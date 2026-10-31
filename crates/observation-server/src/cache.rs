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
//! same directory, then rename). The tile cache is capped: a write that would
//! exceed the cap first evicts the least recently written tiles.

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

/// Disk cache handle. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct Cache {
    root: PathBuf,
    bundle_ttl: Duration,
    tile_cap_bytes: u64,
    /// Approximate size of the `.png` files in `tiles/` (the sidecars are not
    /// counted), scanned at start and kept up to date.
    tile_bytes: AtomicU64,
    /// Held while tiles are being evicted, so that only one put does it at a time.
    evicting: Mutex<()>,
    tmp_counter: AtomicU64,
}

impl Cache {
    /// Opens (without creating) the cache at `root`. Scans `tiles/` once to
    /// initialise the size estimate, so call it outside of request handling.
    pub fn open(root: impl Into<PathBuf>, bundle_ttl: Duration, tile_cap_bytes: u64) -> Self {
        let root = root.into();
        let tile_bytes = AtomicU64::new(png_size(&root.join("tiles")));
        Self {
            root,
            bundle_ttl,
            tile_cap_bytes,
            tile_bytes,
            evicting: Mutex::new(()),
            tmp_counter: AtomicU64::new(0),
        }
    }

    /// Current estimate of the tile cache size in bytes.
    pub fn tile_cache_bytes(&self) -> u64 {
        self.tile_bytes.load(Ordering::Relaxed)
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

    /// Stores a bundle atomically.
    pub async fn put_bundle(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self
            .bundle_path(key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unsafe cache key"))?;
        self.write_atomic(&path, bytes).await
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
    /// least recently written tiles are evicted first (see [`Cache::evict_tiles`]).
    /// Returns `Ok(false)` (and logs) when that cannot make enough room and
    /// nothing was written.
    pub async fn put_tile(&self, id: TileId, bytes: &[u8], meta: &TileMeta) -> io::Result<bool> {
        let (png, json) = self.tile_paths(id)?;
        let previous = tokio::fs::metadata(&png).await.map_or(0, |m| m.len());
        let growth = (bytes.len() as u64).saturating_sub(previous);
        if self
            .tile_bytes
            .load(Ordering::Relaxed)
            .saturating_add(growth)
            > self.tile_cap_bytes
            && !self.evict_tiles(&png, growth).await
        {
            tracing::warn!(
                cap = self.tile_cap_bytes,
                "tile cache is full, not caching {id:?}"
            );
            return Ok(false);
        }
        self.write_atomic(&png, bytes).await?;
        self.write_atomic(&json, &encode_meta(meta)?).await?;
        let _ = self
            .tile_bytes
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(
                    v.saturating_sub(previous)
                        .saturating_add(bytes.len() as u64),
                )
            });
        Ok(true)
    }

    /// Rewrites only the metadata of an already cached tile (after a 304).
    pub async fn update_tile_meta(&self, id: TileId, meta: &TileMeta) -> io::Result<()> {
        let (_, json) = self.tile_paths(id)?;
        self.write_atomic(&json, &encode_meta(meta)?).await
    }

    // ----- helpers ---------------------------------------------------------

    /// Deletes the oldest tiles (by `.png` mtime, with their sidecars) to make
    /// room for a write of `growth` more bytes, never touching `keep`. It evicts
    /// in a batch, down to 90 % of the cap, so that the next puts do not have to
    /// walk `tiles/` again. Returns whether the write may go ahead: there is room
    /// now, or another put is already evicting (the write is then not held up and
    /// the cache may overshoot the cap briefly).
    async fn evict_tiles(&self, keep: &Path, growth: u64) -> bool {
        let cap = self.tile_cap_bytes;
        if growth > cap {
            return false;
        }
        let Ok(_guard) = self.evicting.try_lock() else {
            return true;
        };
        // Another put may have evicted while this one waited for the guard.
        if self
            .tile_bytes
            .load(Ordering::Relaxed)
            .saturating_add(growth)
            <= cap
        {
            return true;
        }
        // Below the low-water mark, and low enough to fit the new tile.
        let target = (cap - cap / 10).min(cap - growth);
        let tiles_dir = self.root.join("tiles");
        let mut files = match tokio::task::spawn_blocking(move || tile_files(&tiles_dir)).await {
            Ok(files) => files,
            Err(e) => {
                tracing::warn!(error = %e, "could not list tiles to evict");
                return false;
            }
        };
        files.sort_unstable_by(|a, b| (a.modified, &a.path).cmp(&(b.modified, &b.path)));
        let (mut tiles, mut bytes) = (0u64, 0u64);
        for file in files.iter().filter(|f| f.path != keep) {
            if self.tile_bytes.load(Ordering::Relaxed) <= target {
                break;
            }
            // Sidecar first: a tile left without one is only an orphan `.png`,
            // which the next eviction still finds.
            let _ = tokio::fs::remove_file(file.path.with_extension("json")).await;
            match tokio::fs::remove_file(&file.path).await {
                Ok(()) => {
                    let _ = self
                        .tile_bytes
                        .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                            Some(v.saturating_sub(file.len))
                        });
                    tiles += 1;
                    bytes += file.len;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(error = %e, "could not evict a cached tile"),
            }
        }
        tracing::info!(tiles, bytes, "evicted old tiles from the tile cache");
        self.tile_bytes
            .load(Ordering::Relaxed)
            .saturating_add(growth)
            <= cap
    }

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

/// A cached tile image found by [`tile_files`].
struct TileFile {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

/// The `.png` files below `dir` (none if it does not exist). Everything else,
/// such as sidecars and `.tmp-*` files, is skipped, and so are symlinks.
fn tile_files(dir: &Path) -> Vec<TileFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        match e.metadata() {
            Ok(m) if m.is_dir() => files.extend(tile_files(&path)),
            Ok(m) if m.is_file() && path.extension().is_some_and(|x| x == "png") => {
                files.push(TileFile {
                    path,
                    len: m.len(),
                    modified: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
            _ => {}
        }
    }
    files
}

/// Total size of the `.png` files below `dir` (0 if it does not exist), the
/// same files [`Cache::put_tile`] counts.
fn png_size(dir: &Path) -> u64 {
    tile_files(dir).iter().map(|f| f.len).sum()
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
        let cache = Cache::open(dir.path(), Duration::from_secs(3600), 1 << 20);
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

        let expired = Cache::open(dir.path(), Duration::ZERO, 1 << 20);
        let (_, fresh) = expired.get_bundle("1.0_2.0", None).await.unwrap();
        assert!(!fresh, "zero TTL bundles are stale but still readable");
    }

    #[tokio::test]
    async fn rejects_non_bundle_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(3600), 1 << 20);
        cache.put_bundle("1.0", b"<html>oops</html>").await.unwrap();
        assert!(cache.get_bundle("1.0", None).await.is_none());
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
        let cache = Cache::open(dir.path(), Duration::from_secs(1), 100);
        assert!(cache.put_tile(id, &[0u8; 60], &meta).await.unwrap());
        let got = cache.get_tile(id).await.unwrap();
        assert_eq!(got.bytes.len(), 60);
        assert_eq!(got.meta, meta);
        assert!(dir.path().join("tiles/3/2/1.png").exists());
        assert_eq!(cache.tile_cache_bytes(), 60);

        // Rewriting the same tile does not double count.
        assert!(cache.put_tile(id, &[0u8; 80], &meta).await.unwrap());
        assert_eq!(cache.tile_cache_bytes(), 80);

        // A scan on open counts the `.png` files only, as puts do.
        let reopened = Cache::open(dir.path(), Duration::from_secs(1), 80);
        assert_eq!(reopened.tile_cache_bytes(), 80);
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

    /// Bytes of the tiles in `ids` that are on disk.
    fn png_bytes_on_disk(root: &Path, ids: &[TileId]) -> u64 {
        ids.iter()
            .filter_map(|id| std::fs::metadata(root.join(format!("tiles/10/0/{}.png", id.y))).ok())
            .map(|m| m.len())
            .sum()
    }

    #[tokio::test]
    async fn full_tile_cache_evicts_the_oldest_tiles() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(1), 1000);
        let mut ids = fill_tiles(&cache, dir.path(), 20, 50).await;
        assert_eq!(cache.tile_cache_bytes(), 1000);

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
        assert_eq!(cache.tile_cache_bytes(), 950);
        assert_eq!(png_bytes_on_disk(dir.path(), &ids), 950);

        // The batch left room: the next put needs no eviction.
        let next = TileId::new(10, 0, 21).unwrap();
        assert!(
            cache
                .put_tile(next, &[3u8; 50], &TileMeta::default())
                .await
                .unwrap()
        );
        assert!(cache.get_tile(ids[2]).await.is_some());
        assert_eq!(cache.tile_cache_bytes(), 1000);
    }

    #[tokio::test]
    async fn eviction_keeps_the_tile_being_rewritten_and_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(1), 1000);
        let ids = fill_tiles(&cache, dir.path(), 10, 100).await;
        let tiles = dir.path().join("tiles");
        std::fs::write(tiles.join("10/0/.tmp-1-1"), [0u8; 500]).unwrap();
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
        assert_eq!(
            cache.tile_cache_bytes(),
            png_bytes_on_disk(dir.path(), &ids)
        );
        assert!(tiles.join("10/0/.tmp-1-1").exists());
        assert!(dir.path().join("bundles/1.0.obsb").exists());
    }

    #[tokio::test]
    async fn tile_larger_than_the_cap_is_refused_without_evicting() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(1), 100);
        let ids = fill_tiles(&cache, dir.path(), 1, 60).await;
        let big = TileId::new(10, 0, 9).unwrap();
        assert!(
            !cache
                .put_tile(big, &[0u8; 101], &TileMeta::default())
                .await
                .unwrap()
        );
        assert!(cache.get_tile(big).await.is_none());
        assert!(cache.get_tile(ids[0]).await.is_some());
        assert_eq!(cache.tile_cache_bytes(), 60);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_puts_on_a_full_cache_keep_the_byte_count_in_step() {
        let dir = tempfile::tempdir().unwrap();
        let cache = std::sync::Arc::new(Cache::open(dir.path(), Duration::from_secs(1), 1000));
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
        let ids: Vec<_> = (0..60).map(|i| TileId::new(10, 0, i).unwrap()).collect();
        let on_disk = png_bytes_on_disk(dir.path(), &ids);
        assert_eq!(cache.tile_cache_bytes(), on_disk);
        assert!(on_disk < 60 * 50, "nothing was evicted");
        assert!(
            cache.get_tile(ids[0]).await.is_none(),
            "the oldest tile stayed"
        );
    }

    #[tokio::test]
    async fn no_temp_files_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(1), 1 << 20);
        cache.put_bundle("1.0", &bundle_bytes()).await.unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path().join("bundles"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("1.0.obsb")]);
    }
}
