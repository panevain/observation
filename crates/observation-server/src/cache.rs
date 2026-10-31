//! On-disk cache for bundles and proxied tiles.
//!
//! Layout under the cache root:
//!
//! ```text
//! bundles/<cache_key>.obsb          (freshness = file mtime + TTL)
//! tiles/<z>/<x>/<y>.png             (+ <y>.json with [`TileMeta`])
//! ```
//!
//! Paths are only ever built from validated tile numbers or a sanitised cache
//! key, never from raw request strings. Writes are atomic (temp file in the
//! same directory, then rename). Both caches are capped: a write that would
//! exceed its cap first evicts the oldest entries, which for tiles are the
//! least recently written or revalidated and for bundles the least recently
//! written (and so the most expired, since every bundle has the same TTL).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use observation_core::bundle::{BUNDLE_MAGIC, BUNDLE_VERSION};
use observation_core::tiles::TileId;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// Validators and expiry stored next to every cached tile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileMeta {
    /// Upstream `ETag`, sent back as `If-None-Match`.
    pub etag: Option<String>,
    /// Upstream `Last-Modified`, sent back as `If-Modified-Since`.
    pub last_modified: Option<String>,
    /// Unix time (seconds) after which the tile must be revalidated.
    pub expires_unix: u64,
}

/// A tile read back from disk.
#[derive(Debug, Clone)]
pub struct CachedTile {
    pub bytes: Bytes,
    pub meta: TileMeta,
}

/// Size caps of the two caches, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheCaps {
    pub bundle_bytes: u64,
    pub tile_bytes: u64,
}

/// Disk cache handle. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct Cache {
    root: PathBuf,
    bundle_ttl: Duration,
    bundles: Store,
    tiles: Store,
    tmp_counter: AtomicU64,
}

impl Cache {
    /// Opens (without creating) the cache at `root`. Scans `bundles/` and
    /// `tiles/` once to initialise the size estimates, so call it outside of
    /// request handling.
    pub fn open(root: impl Into<PathBuf>, bundle_ttl: Duration, caps: CacheCaps) -> Self {
        let root = root.into();
        Self {
            bundles: Store::open(root.join("bundles"), Kind::Bundles, caps.bundle_bytes),
            tiles: Store::open(root.join("tiles"), Kind::Tiles, caps.tile_bytes),
            root,
            bundle_ttl,
            tmp_counter: AtomicU64::new(0),
        }
    }

    /// Current estimate of the bundle cache size in bytes.
    pub fn bundle_cache_bytes(&self) -> u64 {
        self.bundles.bytes()
    }

    /// Current estimate of the tile cache size in bytes.
    pub fn tile_cache_bytes(&self) -> u64 {
        self.tiles.bytes()
    }

    // ----- bundles ---------------------------------------------------------

    fn bundle_path(&self, key: &str) -> Option<PathBuf> {
        is_safe_key(key).then(|| self.root.join("bundles").join(format!("{key}.obsb")))
    }

    /// Reads a cached bundle. The flag tells whether it is still within the TTL
    /// and, given `valid_after`, was written after it (newer local data);
    /// expired bundles are returned too so callers can serve them when the
    /// upstream is down.
    pub async fn get_bundle(
        &self,
        key: &str,
        valid_after: Option<SystemTime>,
    ) -> Option<(Bytes, bool)> {
        let path = self.bundle_path(key)?;
        let meta = tokio::fs::metadata(&path).await.ok()?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        if !bytes.starts_with(&BUNDLE_MAGIC)
            || bytes.get(4..6) != Some(&BUNDLE_VERSION.to_le_bytes()[..])
        {
            tracing::warn!(key, "ignoring cached bundle with bad magic or old version");
            return None;
        }
        let modified = meta.modified().ok();
        let age = modified.and_then(|m| SystemTime::now().duration_since(m).ok());
        // An mtime in the future (clock skew) counts as fresh.
        let fresh = age.is_none_or(|a| a < self.bundle_ttl)
            && valid_after.is_none_or(|v| modified.is_none_or(|m| m >= v));
        Some((Bytes::from(bytes), fresh))
    }

    /// Stores a bundle atomically. When that would take the bundle cache over
    /// its size cap, the least recently written bundles are evicted first (see
    /// [`Store::evict`]). Returns `Ok(false)` (and logs) when that cannot make
    /// enough room and nothing was written.
    pub async fn put_bundle(&self, key: &str, bytes: &[u8]) -> io::Result<bool> {
        let path = self
            .bundle_path(key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unsafe cache key"))?;
        let previous = tokio::fs::metadata(&path).await.map_or(0, |m| m.len());
        let new = bytes.len() as u64;
        if !self
            .bundles
            .make_room(&path, new.saturating_sub(previous))
            .await
        {
            tracing::warn!(
                cap = self.bundles.cap,
                key,
                "bundle cache is full, not caching the bundle"
            );
            return Ok(false);
        }
        self.write_atomic(&path, bytes).await?;
        self.bundles.resize(previous, new);
        Ok(true)
    }

    // ----- tiles -----------------------------------------------------------

    /// `tiles/<z>/<x>/<y>.{png,json}`. The relative part is digits and `/` by
    /// construction; the `..` check (the same rule as [`is_safe_key`]) keeps the
    /// path inside `tiles/` even if that ever changes, and lets path-injection
    /// analysis see the guard.
    fn tile_paths(&self, id: TileId) -> io::Result<(PathBuf, PathBuf)> {
        let rel = format!("{}/{}/{}", id.z, id.x, id.y);
        if rel.contains("..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe tile path",
            ));
        }
        let base = self.root.join("tiles").join(rel);
        Ok((base.with_extension("png"), base.with_extension("json")))
    }

    /// Reads a cached tile together with its metadata.
    pub async fn get_tile(&self, id: TileId) -> Option<CachedTile> {
        let (png, json) = self.tile_paths(id).ok()?;
        let bytes = tokio::fs::read(&png).await.ok()?;
        let meta = tokio::fs::read(&json).await.ok()?;
        let meta = serde_json::from_slice(&meta).ok()?;
        Some(CachedTile {
            bytes: Bytes::from(bytes),
            meta,
        })
    }

    /// Stores a tile. When that would take the tile cache over its size cap, the
    /// least recently written or revalidated tiles are evicted first (see
    /// [`Store::evict`]). Returns `Ok(false)` (and logs) when that cannot make
    /// enough room and nothing was written.
    pub async fn put_tile(&self, id: TileId, bytes: &[u8], meta: &TileMeta) -> io::Result<bool> {
        let (png, json) = self.tile_paths(id)?;
        let encoded = encode_meta(meta)?;
        let previous_png = tokio::fs::metadata(&png).await.map_or(0, |m| m.len());
        let previous_json = tokio::fs::metadata(&json).await.map_or(0, |m| m.len());
        let (new_png, new_json) = (bytes.len() as u64, encoded.len() as u64);
        let growth = (new_png + new_json).saturating_sub(previous_png + previous_json);
        if !self.tiles.make_room(&png, growth).await {
            tracing::warn!(
                cap = self.tiles.cap,
                "tile cache is full, not caching {id:?}"
            );
            return Ok(false);
        }
        // The size is updated after each rename, so a failed second write
        // leaves the estimate in step with what is on disk.
        self.write_atomic(&png, bytes).await?;
        self.tiles.resize(previous_png, new_png);
        self.write_atomic(&json, &encoded).await?;
        self.tiles.resize(previous_json, new_json);
        Ok(true)
    }

    /// Rewrites only the metadata of an already cached tile (after a 304). Also
    /// touches the `.png`, so that tiles that are still in use are evicted last.
    pub async fn update_tile_meta(&self, id: TileId, meta: &TileMeta) -> io::Result<()> {
        let (png, json) = self.tile_paths(id)?;
        let encoded = encode_meta(meta)?;
        let previous = tokio::fs::metadata(&json).await.map_or(0, |m| m.len());
        self.write_atomic(&json, &encoded).await?;
        self.tiles.resize(previous, encoded.len() as u64);
        let touch = move || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(png)?
                .set_modified(SystemTime::now())
        };
        // Best effort: the tile may have been evicted meanwhile.
        let _ = tokio::task::spawn_blocking(touch).await;
        Ok(())
    }

    // ----- helpers ---------------------------------------------------------

    async fn write_atomic(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
        tokio::fs::create_dir_all(dir).await?;
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".tmp-{}-{n}", std::process::id()));
        let result = async {
            tokio::fs::write(&tmp, bytes).await?;
            tokio::fs::rename(&tmp, path).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        result
    }
}

fn encode_meta(meta: &TileMeta) -> io::Result<Vec<u8>> {
    serde_json::to_vec(meta).map_err(io::Error::other)
}

/// A cache key is safe as a file name if it only has the characters produced by
/// `BBox::cache_key` (digits, `-`, `.`, `_`) and cannot form `..`.
pub fn is_safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && !key.contains("..")
        && key
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '.' | '_'))
}

/// What a [`Store`] holds, which decides the files that are its entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `bundles/<key>.obsb`, one file per entry.
    Bundles,
    /// `tiles/<z>/<x>/<y>.png`, each with a `.json` sidecar.
    Tiles,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Bundles => "bundle",
            Self::Tiles => "tile",
        }
    }

    /// The sidecar that belongs to an entry's main file, if entries have one.
    fn sidecar(self, main: &Path) -> Option<PathBuf> {
        match self {
            Self::Bundles => None,
            Self::Tiles => Some(main.with_extension("json")),
        }
    }
}

/// One size-capped directory of the cache, with an estimate of its size.
#[derive(Debug)]
struct Store {
    dir: PathBuf,
    kind: Kind,
    cap: u64,
    /// Approximate size of the entries in `dir` (for tiles, `.png` files and
    /// `.json` sidecars), scanned at start, kept up to date and reset from a
    /// fresh scan on every eviction.
    bytes: AtomicU64,
    /// Held while entries are being evicted, so that only one write does it at a time.
    evicting: Mutex<()>,
}

impl Store {
    fn open(dir: PathBuf, kind: Kind, cap: u64) -> Self {
        let bytes = AtomicU64::new(scan(&dir, kind).total);
        Self {
            dir,
            kind,
            cap,
            bytes,
            evicting: Mutex::new(()),
        }
    }

    fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Whether `growth` more bytes fit, by the estimate.
    fn fits(&self, growth: u64) -> bool {
        self.bytes().saturating_add(growth) <= self.cap
    }

    /// Replaces `old` bytes of the size estimate with `new`.
    fn resize(&self, old: u64, new: u64) {
        let _ = self
            .bytes
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(old).saturating_add(new))
            });
    }

    /// Whether a write of `growth` more bytes to `keep` may go ahead, evicting
    /// first if it would not fit.
    async fn make_room(&self, keep: &Path, growth: u64) -> bool {
        self.fits(growth) || self.evict(keep, growth).await
    }

    /// Deletes the oldest entries (by the mtime of their main file, with their
    /// sidecars) to make room for a write of `growth` more bytes, never
    /// touching `keep`. It evicts in a batch, down to 90 % of the cap, so that
    /// the next writes do not have to walk the directory again, and resets the
    /// size estimate from the walk, so a drifted estimate cannot make every
    /// later write fail. Returns whether the write may go ahead: there is room
    /// now, or another write is already evicting (the write is then not held up
    /// and the cache may overshoot the cap briefly).
    async fn evict(&self, keep: &Path, growth: u64) -> bool {
        let cap = self.cap;
        if growth > cap {
            return false;
        }
        let Ok(_guard) = self.evicting.try_lock() else {
            return true;
        };
        // An eviction may have finished between the caller's check and `try_lock`.
        if self.fits(growth) {
            return true;
        }
        // Below the low-water mark, and low enough to fit the new entry.
        let target = (cap - cap / 10).min(cap - growth);
        let (dir, kind, keep) = (self.dir.clone(), self.kind, keep.to_path_buf());
        let evicted = match tokio::task::spawn_blocking(move || {
            evict_oldest(&dir, kind, &keep, target)
        })
        .await
        {
            Ok(evicted) => evicted,
            Err(e) => {
                tracing::warn!(cache = self.kind.name(), error = %e, "could not evict");
                return false;
            }
        };
        // The walk is the truth; writes racing with it make this slightly off
        // until the next eviction.
        self.bytes.store(evicted.total, Ordering::Relaxed);
        tracing::info!(
            cache = self.kind.name(),
            entries = evicted.entries,
            bytes = evicted.bytes,
            "evicted the oldest entries from the cache"
        );
        self.fits(growth)
    }
}

/// A cached entry found by [`scan`].
struct Entry {
    /// The `.obsb` or `.png`.
    path: PathBuf,
    len: u64,
    /// 0 if the entry has no sidecar or it is missing.
    sidecar_len: u64,
    modified: SystemTime,
}

/// What is in a store's directory: entries, sidecars without their main file,
/// and the size of both. Other files, such as `.tmp-*`, are neither listed
/// nor counted.
#[derive(Default)]
struct Scan {
    entries: Vec<Entry>,
    /// `(path, length)` of the sidecars that have no main file.
    orphans: Vec<(PathBuf, u64)>,
    total: u64,
}

/// Walks `dir` (nothing is found if it does not exist): `bundles/` is flat,
/// `tiles/` is walked recursively. Symlinks are skipped.
fn scan(dir: &Path, kind: Kind) -> Scan {
    let mut scan = Scan::default();
    match kind {
        Kind::Bundles => scan_bundles(dir, &mut scan),
        Kind::Tiles => scan_tiles(dir, &mut scan),
    }
    let entries: u64 = scan.entries.iter().map(|e| e.len + e.sidecar_len).sum();
    scan.total = entries + scan.orphans.iter().map(|o| o.1).sum::<u64>();
    scan
}

fn entry(path: PathBuf, m: &std::fs::Metadata, sidecar_len: u64) -> Entry {
    Entry {
        path,
        len: m.len(),
        sidecar_len,
        modified: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    }
}

/// `bundles/` holds only `<key>.obsb` and temp files; nothing below it is read.
fn scan_bundles(dir: &Path, scan: &mut Scan) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        // `DirEntry::metadata` does not follow symlinks.
        let (path, Ok(m)) = (e.path(), e.metadata()) else {
            continue;
        };
        if m.is_file() && path.extension().and_then(|x| x.to_str()) == Some("obsb") {
            scan.entries.push(entry(path, &m, 0));
        }
    }
}

fn scan_tiles(dir: &Path, scan: &mut Scan) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut pngs = Vec::new();
    // Keyed by the `.png` path they belong to.
    let mut sidecars = HashMap::new();
    for e in entries.flatten() {
        let (path, Ok(m)) = (e.path(), e.metadata()) else {
            continue;
        };
        if m.is_dir() {
            scan_tiles(&path, scan);
        } else if m.is_file() {
            match path.extension().and_then(|x| x.to_str()) {
                Some("png") => pngs.push((path, m)),
                Some("json") => {
                    sidecars.insert(path.with_extension("png"), (path, m.len()));
                }
                _ => {}
            }
        }
    }
    for (png, m) in pngs {
        let json_len = sidecars.remove(&png).map_or(0, |s| s.1);
        scan.entries.push(entry(png, &m, json_len));
    }
    scan.orphans.extend(sidecars.into_values());
}

/// Result of [`evict_oldest`].
struct Evicted {
    /// Size of what is left, as scanned.
    total: u64,
    entries: u64,
    bytes: u64,
}

/// Scans `dir`, deletes sidecars without a main file, then the oldest entries
/// until at most `target` bytes are left. Never deletes `keep` (a main file
/// that is being written) or its sidecar.
fn evict_oldest(dir: &Path, kind: Kind, keep: &Path, target: u64) -> Evicted {
    let scan = scan(dir, kind);
    let mut total = scan.total;
    let keep_sidecar = kind.sidecar(keep);
    for (path, len) in scan
        .orphans
        .iter()
        .filter(|o| Some(&o.0) != keep_sidecar.as_ref())
    {
        if remove_file(path) {
            total = total.saturating_sub(*len);
        }
    }
    let mut entries = scan.entries;
    entries.sort_unstable_by(|a, b| (a.modified, &a.path).cmp(&(b.modified, &b.path)));
    let mut count = 0;
    for e in entries.iter().filter(|e| e.path != keep) {
        if total <= target {
            break;
        }
        // Sidecar first: an entry left without one is only an orphan main
        // file, which the next eviction still finds.
        if let Some(sidecar) = kind.sidecar(&e.path)
            && remove_file(&sidecar)
        {
            total = total.saturating_sub(e.sidecar_len);
        }
        if remove_file(&e.path) {
            total = total.saturating_sub(e.len);
            count += 1;
        }
    }
    Evicted {
        total,
        entries: count,
        bytes: scan.total.saturating_sub(total),
    }
}

/// Removes a file; true if it is gone afterwards.
fn remove_file(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!(error = %e, "could not delete a cached file");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_bytes() -> Vec<u8> {
        let mut v = BUNDLE_MAGIC.to_vec();
        v.extend_from_slice(&BUNDLE_VERSION.to_le_bytes());
        v.extend_from_slice(&[9, 9]);
        v
    }

    #[test]
    fn key_safety() {
        assert!(is_safe_key("47.70750_-122.20800_47.70980_-122.20400"));
        for bad in ["", "..", "a/b", "../x", "1..2", "x y", "1\\2"] {
            assert!(!is_safe_key(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn bundle_round_trip_and_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(3600), tile_cap(1 << 20));
        assert!(cache.get_bundle("1.0_2.0", None).await.is_none());
        cache.put_bundle("1.0_2.0", &bundle_bytes()).await.unwrap();
        let (bytes, fresh) = cache.get_bundle("1.0_2.0", None).await.unwrap();
        assert_eq!(bytes.as_ref(), bundle_bytes().as_slice());
        assert!(fresh);
        // Data imported after the bundle was written makes it stale.
        let later = SystemTime::now() + Duration::from_secs(60);
        let (_, fresh) = cache.get_bundle("1.0_2.0", Some(later)).await.unwrap();
        assert!(!fresh);
        let earlier = SystemTime::now() - Duration::from_secs(60);
        assert!(cache.get_bundle("1.0_2.0", Some(earlier)).await.unwrap().1);
        assert!(cache.put_bundle("../evil", b"x").await.is_err());

        let expired = Cache::open(dir.path(), Duration::ZERO, tile_cap(1 << 20));
        let (_, fresh) = expired.get_bundle("1.0_2.0", None).await.unwrap();
        assert!(!fresh, "zero TTL bundles are stale but still readable");
    }

    #[tokio::test]
    async fn rejects_non_bundle_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(3600), tile_cap(1 << 20));
        cache.put_bundle("1.0", b"<html>oops</html>").await.unwrap();
        assert!(cache.get_bundle("1.0", None).await.is_none());
    }

    /// A bundle cache of `bundle_bytes` and a 1 MiB tile cache.
    fn bundle_cap(bundle_bytes: u64) -> CacheCaps {
        CacheCaps {
            bundle_bytes,
            tile_bytes: 1 << 20,
        }
    }

    /// A valid bundle header padded to `len` bytes with `fill`.
    fn sized_bundle(len: usize, fill: u8) -> Vec<u8> {
        let mut v = bundle_bytes();
        v.resize(len, fill);
        v
    }

    /// Size of the bundle cache on disk, by the module's own scan.
    fn bundle_disk_bytes(root: &Path) -> u64 {
        scan(&root.join("bundles"), Kind::Bundles).total
    }

    /// Writes bundles `0` to `<n-1>` of `len` bytes with the mtime of bundle `i`
    /// set to `i` seconds after a fixed time, oldest first.
    async fn fill_bundles(cache: &Cache, root: &Path, n: u32, len: usize) {
        for i in 0..n {
            assert!(
                cache
                    .put_bundle(&i.to_string(), &sized_bundle(len, 1))
                    .await
                    .unwrap()
            );
            let path = root.join(format!("bundles/{i}.obsb"));
            let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + u64::from(i));
            file.set_modified(at).unwrap();
        }
    }

    #[tokio::test]
    async fn full_bundle_cache_evicts_the_oldest_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(3600), bundle_cap(2000));
        fill_bundles(&cache, dir.path(), 20, 100).await;
        assert_eq!(cache.bundle_cache_bytes(), 2000);
        // Not bundles, so neither counted nor evicted.
        let bundles = dir.path().join("bundles");
        std::fs::write(bundles.join(".tmp-1-1"), [0u8; 500]).unwrap();
        std::fs::create_dir(bundles.join("sub")).unwrap();
        std::fs::write(bundles.join("sub/9.obsb"), [0u8; 500]).unwrap();
        assert_eq!(bundle_disk_bytes(dir.path()), 2000);

        // One more does not fit: evicts down to 90 % (two bundles), then writes.
        assert!(cache.put_bundle("20", &sized_bundle(100, 2)).await.unwrap());
        for i in 0..2 {
            assert!(
                cache.get_bundle(&i.to_string(), None).await.is_none(),
                "{i} survived"
            );
        }
        for i in 2..=20 {
            assert!(
                cache.get_bundle(&i.to_string(), None).await.is_some(),
                "{i} was evicted"
            );
        }
        assert_eq!(cache.bundle_cache_bytes(), 1900);
        assert_eq!(bundle_disk_bytes(dir.path()), 1900);
        assert!(bundles.join(".tmp-1-1").exists());
        assert!(bundles.join("sub/9.obsb").exists());

        // The batch left room: the next put needs no eviction.
        assert!(cache.put_bundle("21", &sized_bundle(100, 3)).await.unwrap());
        assert!(cache.get_bundle("2", None).await.is_some());
        assert_eq!(cache.bundle_cache_bytes(), 2000);

        // A scan on open counts the same bundles.
        let reopened = Cache::open(dir.path(), Duration::from_secs(3600), bundle_cap(2000));
        assert_eq!(reopened.bundle_cache_bytes(), 2000);
    }

    #[tokio::test]
    async fn rewriting_the_oldest_bundle_evicts_the_next_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(3600), bundle_cap(1000));
        fill_bundles(&cache, dir.path(), 10, 100).await;
        // A same-size rewrite fits without evicting anything.
        assert!(cache.put_bundle("0", &sized_bundle(100, 4)).await.unwrap());
        assert_eq!(cache.bundle_cache_bytes(), 1000);
        assert!(cache.get_bundle("1", None).await.is_some());

        // Bundle 0 was the oldest, but is the one being replaced (larger), so 1 goes.
        let touch = |i: u32| {
            let path = dir.path().join(format!("bundles/{i}.obsb"));
            let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            file.set_modified(SystemTime::UNIX_EPOCH).unwrap();
        };
        touch(0);
        assert!(cache.put_bundle("0", &sized_bundle(150, 5)).await.unwrap());
        assert_eq!(cache.get_bundle("0", None).await.unwrap().0.len(), 150);
        assert!(cache.get_bundle("1", None).await.is_none());
        assert!(cache.get_bundle("2", None).await.is_some());
        assert_eq!(cache.bundle_cache_bytes(), bundle_disk_bytes(dir.path()));
        assert_eq!(cache.bundle_cache_bytes(), 950);
    }

    #[tokio::test]
    async fn bundle_larger_than_the_cap_is_refused_without_evicting() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(3600), bundle_cap(200));
        fill_bundles(&cache, dir.path(), 1, 60).await;
        assert!(!cache.put_bundle("99", &sized_bundle(201, 0)).await.unwrap());
        assert!(cache.get_bundle("99", None).await.is_none());
        assert!(cache.get_bundle("0", None).await.is_some());
        assert_eq!(cache.bundle_cache_bytes(), 60);
        // Tiles and bundles are capped separately.
        let id = TileId::new(1, 0, 0).unwrap();
        assert!(
            cache
                .put_tile(id, &[0u8; 500], &TileMeta::default())
                .await
                .unwrap()
        );
        assert_eq!(cache.bundle_cache_bytes(), 60);
    }

    /// A 1 MiB bundle cache and a tile cache of `tile_bytes`.
    fn tile_cap(tile_bytes: u64) -> CacheCaps {
        CacheCaps {
            bundle_bytes: 1 << 20,
            tile_bytes,
        }
    }

    /// Size of the tile cache on disk, by the module's own scan.
    fn disk_bytes(root: &Path) -> u64 {
        scan(&root.join("tiles"), Kind::Tiles).total
    }

    /// Bytes one tile with a `len`-byte image and default metadata takes.
    fn tile_len(len: u64) -> u64 {
        len + encode_meta(&TileMeta::default()).unwrap().len() as u64
    }

    #[tokio::test]
    async fn tiles_store_meta_and_count_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let id = TileId::new(3, 2, 1).unwrap();
        let meta = TileMeta {
            etag: Some("\"abc\"".into()),
            last_modified: None,
            expires_unix: 42,
        };
        let json = encode_meta(&meta).unwrap().len() as u64;
        let cache = Cache::open(dir.path(), Duration::from_secs(1), tile_cap(1000));
        assert!(cache.put_tile(id, &[0u8; 60], &meta).await.unwrap());
        let got = cache.get_tile(id).await.unwrap();
        assert_eq!(got.bytes.len(), 60);
        assert_eq!(got.meta, meta);
        assert!(dir.path().join("tiles/3/2/1.png").exists());
        assert_eq!(cache.tile_cache_bytes(), 60 + json);

        // Rewriting the same tile does not double count.
        assert!(cache.put_tile(id, &[0u8; 80], &meta).await.unwrap());
        assert_eq!(cache.tile_cache_bytes(), 80 + json);

        // A scan on open counts the images and sidecars, as puts do.
        let reopened = Cache::open(dir.path(), Duration::from_secs(1), tile_cap(1000));
        assert_eq!(reopened.tile_cache_bytes(), 80 + json);
        assert_eq!(disk_bytes(dir.path()), 80 + json);
    }

    /// Writes `n` tiles of `len` bytes (`10/0/0` to `10/0/<n-1>`) with the mtime of
    /// tile `i` set to `i` seconds after a fixed time, oldest first.
    async fn fill_tiles(cache: &Cache, root: &Path, n: u32, len: usize) -> Vec<TileId> {
        let meta = TileMeta::default();
        let mut ids = Vec::new();
        for i in 0..n {
            let id = TileId::new(10, 0, i).unwrap();
            assert!(cache.put_tile(id, &vec![1u8; len], &meta).await.unwrap());
            let png = root.join(format!("tiles/10/0/{i}.png"));
            let file = std::fs::OpenOptions::new().write(true).open(png).unwrap();
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + u64::from(i));
            file.set_modified(at).unwrap();
            ids.push(id);
        }
        ids
    }

    #[tokio::test]
    async fn full_tile_cache_evicts_the_oldest_tiles() {
        let dir = tempfile::tempdir().unwrap();
        let unit = tile_len(50);
        let cache = Cache::open(dir.path(), Duration::from_secs(1), tile_cap(20 * unit));
        let mut ids = fill_tiles(&cache, dir.path(), 20, 50).await;
        assert_eq!(cache.tile_cache_bytes(), 20 * unit);

        // One more tile does not fit: evicts down to 90 % (two tiles), then writes.
        let new = TileId::new(10, 0, 20).unwrap();
        assert!(
            cache
                .put_tile(new, &[2u8; 50], &TileMeta::default())
                .await
                .unwrap()
        );
        ids.push(new);
        for id in &ids[..2] {
            assert!(cache.get_tile(*id).await.is_none(), "{id:?} survived");
            assert!(
                !dir.path()
                    .join(format!("tiles/10/0/{}.json", id.y))
                    .exists()
            );
        }
        for id in &ids[2..] {
            assert!(cache.get_tile(*id).await.is_some(), "{id:?} was evicted");
        }
        assert_eq!(cache.get_tile(new).await.unwrap().bytes.as_ref(), [2u8; 50]);
        assert_eq!(cache.tile_cache_bytes(), 19 * unit);
        assert_eq!(disk_bytes(dir.path()), 19 * unit);

        // The batch left room: the next put needs no eviction.
        let next = TileId::new(10, 0, 21).unwrap();
        assert!(
            cache
                .put_tile(next, &[3u8; 50], &TileMeta::default())
                .await
                .unwrap()
        );
        assert!(cache.get_tile(ids[2]).await.is_some());
        assert_eq!(cache.tile_cache_bytes(), 20 * unit);
    }

    #[tokio::test]
    async fn eviction_keeps_the_tile_being_rewritten_and_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(
            dir.path(),
            Duration::from_secs(1),
            tile_cap(10 * tile_len(100)),
        );
        let ids = fill_tiles(&cache, dir.path(), 10, 100).await;
        let tiles = dir.path().join("tiles");
        std::fs::write(tiles.join("10/0/.tmp-1-1"), [0u8; 500]).unwrap();
        std::fs::write(tiles.join("10/0/99.json"), [0u8; 40]).unwrap();
        cache.put_bundle("1.0", &bundle_bytes()).await.unwrap();

        // Tile 0 is the oldest but is the one being replaced (larger), so tile 1 goes.
        assert!(
            cache
                .put_tile(ids[0], &[4u8; 150], &TileMeta::default())
                .await
                .unwrap()
        );
        assert_eq!(cache.get_tile(ids[0]).await.unwrap().bytes.len(), 150);
        assert!(cache.get_tile(ids[1]).await.is_none());
        assert!(cache.get_tile(ids[2]).await.is_some());
        // The sidecar without an image is removed and counted out.
        assert!(!tiles.join("10/0/99.json").exists());
        assert_eq!(cache.tile_cache_bytes(), disk_bytes(dir.path()));
        assert!(tiles.join("10/0/.tmp-1-1").exists());
        assert!(dir.path().join("bundles/1.0.obsb").exists());
    }

    #[tokio::test]
    async fn tile_larger_than_the_cap_is_refused_without_evicting() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(1), tile_cap(200));
        let ids = fill_tiles(&cache, dir.path(), 1, 60).await;
        let before = cache.tile_cache_bytes();
        let big = TileId::new(10, 0, 9).unwrap();
        assert!(
            !cache
                .put_tile(big, &[0u8; 201], &TileMeta::default())
                .await
                .unwrap()
        );
        assert!(cache.get_tile(big).await.is_none());
        assert!(cache.get_tile(ids[0]).await.is_some());
        assert_eq!(cache.tile_cache_bytes(), before);
    }

    #[tokio::test]
    async fn an_overcounted_estimate_is_reset_by_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let unit = tile_len(50);
        let cache = Cache::open(dir.path(), Duration::from_secs(1), tile_cap(10 * unit));
        let ids = fill_tiles(&cache, dir.path(), 10, 50).await;
        assert_eq!(cache.tile_cache_bytes(), 10 * unit);
        // Files vanish behind the cache's back: the estimate says full, the disk is not.
        for id in &ids[..8] {
            std::fs::remove_file(dir.path().join(format!("tiles/10/0/{}.png", id.y))).unwrap();
            std::fs::remove_file(dir.path().join(format!("tiles/10/0/{}.json", id.y))).unwrap();
        }
        assert_eq!(disk_bytes(dir.path()), 2 * unit);

        let new = TileId::new(10, 0, 10).unwrap();
        assert!(
            cache
                .put_tile(new, &[2u8; 50], &TileMeta::default())
                .await
                .unwrap()
        );
        // Nothing live was evicted, and the estimate is back in step.
        assert!(cache.get_tile(ids[8]).await.is_some());
        assert!(cache.get_tile(ids[9]).await.is_some());
        assert!(cache.get_tile(new).await.is_some());
        assert_eq!(cache.tile_cache_bytes(), 3 * unit);
        assert_eq!(disk_bytes(dir.path()), 3 * unit);

        // And it keeps working once the cache really fills up.
        for i in 11..30 {
            let id = TileId::new(10, 0, i).unwrap();
            assert!(
                cache
                    .put_tile(id, &[3u8; 50], &TileMeta::default())
                    .await
                    .unwrap()
            );
        }
        assert!(cache.tile_cache_bytes() <= 10 * unit);
        assert_eq!(cache.tile_cache_bytes(), disk_bytes(dir.path()));
    }

    #[tokio::test]
    async fn revalidating_a_tile_refreshes_its_mtime_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(1), tile_cap(1 << 20));
        let ids = fill_tiles(&cache, dir.path(), 2, 50).await;
        let meta = TileMeta {
            etag: Some("x".repeat(100)),
            ..TileMeta::default()
        };
        cache.update_tile_meta(ids[0], &meta).await.unwrap();
        let modified = |i: u32| {
            std::fs::metadata(dir.path().join(format!("tiles/10/0/{i}.png")))
                .unwrap()
                .modified()
                .unwrap()
        };
        assert!(modified(0) > modified(1), "tile 0 should now be the newest");
        assert_eq!(cache.get_tile(ids[0]).await.unwrap().meta, meta);
        assert_eq!(cache.tile_cache_bytes(), disk_bytes(dir.path()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_puts_on_a_full_cache_keep_the_byte_count_in_step() {
        let dir = tempfile::tempdir().unwrap();
        let unit = tile_len(50);
        let cache = std::sync::Arc::new(Cache::open(
            dir.path(),
            Duration::from_secs(1),
            tile_cap(20 * unit),
        ));
        fill_tiles(&cache, dir.path(), 20, 50).await;
        let tasks: Vec<_> = (20..60)
            .map(|i| {
                let cache = cache.clone();
                tokio::spawn(async move {
                    let id = TileId::new(10, 0, i).unwrap();
                    cache.put_tile(id, &[1u8; 50], &TileMeta::default()).await
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        assert!(disk_bytes(dir.path()) < 60 * unit, "nothing was evicted");
        let oldest = TileId::new(10, 0, 0).unwrap();
        assert!(
            cache.get_tile(oldest).await.is_none(),
            "the oldest tile stayed"
        );

        // Puts racing with an eviction's scan can leave the estimate slightly
        // off; the next eviction (here forced by a large tile) corrects it.
        let large = TileId::new(10, 0, 100).unwrap();
        assert!(
            cache
                .put_tile(large, &vec![5u8; 15 * unit as usize], &TileMeta::default())
                .await
                .unwrap()
        );
        assert_eq!(cache.tile_cache_bytes(), disk_bytes(dir.path()));
        assert!(cache.tile_cache_bytes() <= 20 * unit);
    }

    #[tokio::test]
    async fn no_temp_files_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(1), tile_cap(1 << 20));
        cache.put_bundle("1.0", &bundle_bytes()).await.unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path().join("bundles"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("1.0.obsb")]);
    }
}
