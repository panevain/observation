//! On-disk cache for bundles and proxied tiles.
//!
//! Layout under the cache root:
//!
//! ```text
//! bundles/<cache_key>.obsb          (written = file mtime; freshness in domain::freshness)
//! tiles/<z>/<x>/<y>.png             (+ <y>.json with [`TileMeta`])
//! dem/<z>/<x>/<y>.png               (elevation tiles, laid out as `tiles/`)
//! ```
//!
//! Paths are only ever built from validated tile numbers or a sanitised cache
//! key, never from raw request strings. Writes are atomic (temp file in the
//! same directory, then rename). Both caches are capped: a write that would
//! exceed its cap first evicts the oldest entries, which for tiles are the
//! least recently written or revalidated and for bundles the least recently
//! written (and so the most expired, since every bundle has the same TTL).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Bytes;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use observation_core::tiles::TileId;

use self::store::Store;
use crate::clock::{Clock, SystemClock};
use crate::ports::{BundleStore, StoredBundle, TileStore};
pub use crate::ports::{CachedTile, TileMeta, TileSet};

mod scan;
mod store;

/// Size caps of the caches, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheCaps {
    pub bundle_bytes: u64,
    pub tile_bytes: u64,
    pub dem_bytes: u64,
}

/// Disk cache handle. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct Cache {
    bundles: Store,
    tiles: Store,
    dem: Store,
    tmp_counter: AtomicU64,
    clock: Arc<dyn Clock>,
}

impl Cache {
    /// Opens (without creating) the cache at `root`. Scans `bundles/`,
    /// `tiles/` and `dem/` once to initialise the size estimates, so call it outside of
    /// request handling.
    pub fn open(root: impl Into<PathBuf>, caps: CacheCaps) -> Self {
        Self::open_with_clock(root, caps, Arc::new(SystemClock))
    }

    /// [`Self::open`] with `clock` as "now" for touches and stale temp files.
    pub fn open_with_clock(
        root: impl Into<PathBuf>,
        caps: CacheCaps,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let root = root.into();
        let store =
            |dir: &str, kind, cap| Store::open(root.join(dir), kind, cap, Arc::clone(&clock));
        Self {
            bundles: store("bundles", Kind::Bundles, caps.bundle_bytes),
            tiles: store("tiles", Kind::Tiles, caps.tile_bytes),
            dem: store("dem", Kind::Tiles, caps.dem_bytes),
            tmp_counter: AtomicU64::new(0),
            clock,
        }
    }

    /// Brings every cache under their caps (a cache directory from before a
    /// cap, or from a larger one, can be far over it) and removes leftovers:
    /// sidecars without a tile and temp files of interrupted writes. Run it in
    /// the background at start, so the first write does not do it all.
    pub async fn trim(&self) {
        self.bundles.trim().await;
        self.tiles.trim().await;
        self.dem.trim().await;
    }

    /// Current estimate of the bundle cache size in bytes.
    pub fn bundle_cache_bytes(&self) -> u64 {
        self.bundles.bytes()
    }

    /// Current estimate of the tile cache size in bytes.
    pub fn tile_cache_bytes(&self) -> u64 {
        self.tiles.bytes()
    }

    /// Current estimate of the elevation tile cache size in bytes.
    pub fn dem_cache_bytes(&self) -> u64 {
        self.dem.bytes()
    }

    fn store(&self, set: TileSet) -> &Store {
        match set {
            TileSet::Map => &self.tiles,
            TileSet::Dem => &self.dem,
        }
    }

    // ----- bundles ---------------------------------------------------------

    fn bundle_path(&self, key: &str) -> Option<PathBuf> {
        is_safe_key(key).then(|| self.bundles.dir.join(format!("{key}.obsb")))
    }

    /// Reads a cached bundle, with its file's mtime as the time it was
    /// written (its freshness; see [`crate::domain::freshness`]). Reading
    /// never touches the file.
    pub async fn get_bundle(&self, key: &str) -> Option<StoredBundle> {
        let path = self.bundle_path(key)?;
        let meta = tokio::fs::metadata(&path).await.ok()?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        Some(StoredBundle {
            bytes: Bytes::from(bytes),
            written: meta.modified().ok(),
        })
    }

    /// Stores a bundle atomically. When that would take the bundle cache over
    /// its size cap, the least recently written bundles are evicted first (see
    /// `Store::make_room`). Returns `Ok(false)` (and logs why) when the
    /// bundle is larger than the cap or eviction cannot make enough room, and
    /// nothing was written.
    pub async fn put_bundle(&self, key: &str, bytes: &[u8]) -> io::Result<bool> {
        let path = self
            .bundle_path(key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unsafe cache key"))?;
        let new = bytes.len() as u64;
        if !self.bundles.make_room(&path, &[(&path, new)], key).await {
            return Ok(false);
        }
        // Measured again: an eviction may have removed the old file meanwhile.
        let previous = file_len(&path).await;
        self.write_atomic(&path, bytes).await?;
        self.bundles.resize(previous, new);
        Ok(true)
    }

    // ----- tiles -----------------------------------------------------------

    /// `tiles/<z>/<x>/<y>.{png,json}` (or under `dem/`). The relative part is
    /// digits and `/` by construction; the `..` check (the same rule as
    /// [`is_safe_key`]) keeps the path inside `tiles/` even if that ever
    /// changes, and lets path-injection analysis see the guard.
    fn tile_paths(&self, set: TileSet, id: TileId) -> io::Result<(PathBuf, PathBuf)> {
        let rel = format!("{}/{}/{}", id.z, id.x, id.y);
        if rel.contains("..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe tile path",
            ));
        }
        let base = self.store(set).dir.join(rel);
        Ok((base.with_extension("png"), base.with_extension("json")))
    }

    /// Reads a cached map tile together with its metadata.
    pub async fn get_tile(&self, id: TileId) -> Option<CachedTile> {
        self.get_tile_in(TileSet::Map, id).await
    }

    /// Reads a cached tile of `set` together with its metadata.
    pub async fn get_tile_in(&self, set: TileSet, id: TileId) -> Option<CachedTile> {
        let (png, json) = self.tile_paths(set, id).ok()?;
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
    /// `Store::make_room`). Returns `Ok(false)` (and logs why) when the tile
    /// is larger than the cap or eviction cannot make enough room, and nothing
    /// was written.
    pub async fn put_tile(&self, id: TileId, bytes: &[u8], meta: &TileMeta) -> io::Result<bool> {
        self.put_tile_in(TileSet::Map, id, bytes, meta).await
    }

    /// [`Self::put_tile`] for a tile of `set`.
    pub async fn put_tile_in(
        &self,
        set: TileSet,
        id: TileId,
        bytes: &[u8],
        meta: &TileMeta,
    ) -> io::Result<bool> {
        let store = self.store(set);
        let (png, json) = self.tile_paths(set, id)?;
        let encoded = encode_meta(meta)?;
        let (new_png, new_json) = (bytes.len() as u64, encoded.len() as u64);
        let what = format!("{id:?}");
        let files = [(png.as_path(), new_png), (json.as_path(), new_json)];
        if !store.make_room(&png, &files, &what).await {
            return Ok(false);
        }
        // Each size is measured just before its write (an eviction may have
        // removed the old file meanwhile) and updated after its rename, so a
        // failed second write leaves the estimate in step with what is on disk.
        let previous = file_len(&png).await;
        self.write_atomic(&png, bytes).await?;
        store.resize(previous, new_png);
        let previous = file_len(&json).await;
        self.write_atomic(&json, &encoded).await?;
        store.resize(previous, new_json);
        Ok(true)
    }

    /// Rewrites only the metadata of an already cached tile (after a 304). Also
    /// touches the `.png`, so that tiles that are still in use are evicted last.
    pub async fn update_tile_meta(&self, id: TileId, meta: &TileMeta) -> io::Result<()> {
        self.update_tile_meta_in(TileSet::Map, id, meta).await
    }

    /// [`Self::update_tile_meta`] for a tile of `set`.
    pub async fn update_tile_meta_in(
        &self,
        set: TileSet,
        id: TileId,
        meta: &TileMeta,
    ) -> io::Result<()> {
        let (png, json) = self.tile_paths(set, id)?;
        let encoded = encode_meta(meta)?;
        let previous = file_len(&json).await;
        self.write_atomic(&json, &encoded).await?;
        self.store(set).resize(previous, encoded.len() as u64);
        let now = self.clock.now();
        let touch = move || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(png)?
                .set_modified(now)
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

impl BundleStore for Cache {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<StoredBundle>> {
        self.get_bundle(key).boxed()
    }

    fn put<'a>(&'a self, key: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, io::Result<bool>> {
        self.put_bundle(key, bytes).boxed()
    }
}

impl TileStore for Cache {
    fn get(&self, set: TileSet, id: TileId) -> BoxFuture<'_, Option<CachedTile>> {
        self.get_tile_in(set, id).boxed()
    }

    fn put<'a>(
        &'a self,
        set: TileSet,
        id: TileId,
        png: &'a [u8],
        meta: &'a TileMeta,
    ) -> BoxFuture<'a, io::Result<bool>> {
        self.put_tile_in(set, id, png, meta).boxed()
    }

    fn revalidated<'a>(
        &'a self,
        set: TileSet,
        id: TileId,
        meta: &'a TileMeta,
    ) -> BoxFuture<'a, io::Result<()>> {
        self.update_tile_meta_in(set, id, meta).boxed()
    }
}

/// Length of the file at `path`, 0 if there is none.
async fn file_len(path: &Path) -> u64 {
    tokio::fs::metadata(path).await.map_or(0, |m| m.len())
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

#[cfg(test)]
mod tests {
    use super::scan::{STALE_TEMP_AGE, scan};
    use std::time::{Duration, SystemTime};

    use observation_core::bundle::{BUNDLE_MAGIC, BUNDLE_VERSION};

    use super::*;
    use crate::domain::freshness::{bundle_is_fresh, usable_bundle};

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

    /// Every box's cache key is safe; a key is safe exactly when it is 1 to
    /// 128 of `0-9 - . _` without `..`; and a safe key's file name is one
    /// plain path component (it cannot leave the cache directory).
    #[test]
    fn safe_keys_are_exactly_plain_file_names() {
        let mut rng = crate::test_rng::Rng::new(3);
        for _ in 0..2000 {
            let south = rng.f64(-85.0, 85.0);
            let west = rng.f64(-180.0, 179.0);
            let bbox = observation_core::geo::BBox::new(
                south,
                west,
                south + rng.f64(0.0001, 0.004),
                west + rng.f64(0.0001, 0.006),
            );
            if let Ok(bbox) = bbox {
                assert!(is_safe_key(&bbox.cache_key()), "{}", bbox.cache_key());
            }
        }
        let alphabet = ['0', '7', '-', '.', '_', '/', '\\', 'a', '%', ' ', '\0', 'é'];
        // Mostly safe characters too, so that the length rule decides often.
        let safe = ['0', '7', '-', '_', '.'];
        for i in 0..20_000 {
            let len = rng.below(140) as usize;
            let chars: &[char] = if i % 2 == 0 { &alphabet } else { &safe[..4] };
            let key: String = (0..len).map(|_| *rng.pick(chars)).collect();
            let plain = key
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '-' | '.' | '_'));
            let want = (1..=128).contains(&key.len()) && !key.contains("..") && plain;
            assert_eq!(is_safe_key(&key), want, "{key:?}");
            if want {
                let name = format!("{key}.obsb");
                let parts: Vec<_> = Path::new(&name).components().collect();
                assert!(
                    matches!(parts[..], [std::path::Component::Normal(_)]),
                    "{key:?}"
                );
            }
        }
    }

    /// The disk store with the freshness rules on top, as the bundle service
    /// uses them (the rules' boundaries are in `domain::freshness`).
    #[tokio::test]
    async fn bundle_round_trip_and_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), roomy());
        let hour = Duration::from_secs(3600);
        let read = |valid_after, ttl| {
            let cache = &cache;
            async move {
                let stored = cache.get_bundle("1.0_2.0").await?;
                usable_bundle("1.0_2.0", stored, SystemTime::now(), ttl, valid_after)
            }
        };
        assert!(read(None, hour).await.is_none());
        cache.put_bundle("1.0_2.0", &bundle_bytes()).await.unwrap();
        let (bytes, fresh) = read(None, hour).await.unwrap();
        assert_eq!(bytes.as_ref(), bundle_bytes().as_slice());
        assert!(fresh);
        // Data imported after the bundle was written makes it stale.
        let later = SystemTime::now() + Duration::from_secs(60);
        let (_, fresh) = read(Some(later), hour).await.unwrap();
        assert!(!fresh);
        let earlier = SystemTime::now() - Duration::from_secs(60);
        assert!(read(Some(earlier), hour).await.unwrap().1);
        assert!(cache.put_bundle("../evil", b"x").await.is_err());

        let (_, fresh) = read(None, Duration::ZERO).await.unwrap();
        assert!(!fresh, "zero TTL bundles are stale but still readable");
    }

    /// The on-disk layout and sidecar format, which caches written by
    /// earlier versions must keep matching.
    #[tokio::test]
    async fn layout_and_sidecar_format_are_stable() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), roomy());
        let key = observation_core::geo::BBox {
            south: 47.7075,
            west: -122.208,
            north: 47.7098,
            east: -122.204,
        }
        .cache_key();
        assert!(cache.put_bundle(&key, &bundle_bytes()).await.unwrap());
        assert_eq!(
            std::fs::read(
                dir.path()
                    .join("bundles/47.70750_-122.20800_47.70980_-122.20400.obsb")
            )
            .unwrap(),
            bundle_bytes()
        );

        let meta = TileMeta {
            etag: Some("\"abc\"".into()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
            expires_unix: 1_700_000_000,
        };
        let sidecar = r#"{"etag":"\"abc\"","last_modified":"Wed, 21 Oct 2015 07:28:00 GMT","expires_unix":1700000000}"#;
        let id = TileId::new(12, 654, 1583).unwrap();
        assert!(cache.put_tile(id, b"png", &meta).await.unwrap());
        let read = |rel: &str| std::fs::read_to_string(dir.path().join(rel)).unwrap();
        assert_eq!(read("tiles/12/654/1583.png"), "png");
        assert_eq!(read("tiles/12/654/1583.json"), sidecar);
        assert!(
            cache
                .put_tile_in(TileSet::Dem, id, b"dem", &TileMeta::default())
                .await
                .unwrap()
        );
        assert_eq!(read("dem/12/654/1583.png"), "dem");
        assert_eq!(
            read("dem/12/654/1583.json"),
            r#"{"etag":null,"last_modified":null,"expires_unix":0}"#
        );

        // A sidecar as earlier versions wrote it reads back.
        std::fs::write(dir.path().join("tiles/12/654/1583.json"), sidecar).unwrap();
        assert_eq!(cache.get_tile(id).await.unwrap().meta, meta);
    }

    /// Freshness is the file's age: the store reports the mtime as the write
    /// time, and the rule makes it fresh below the TTL, stale from exactly the
    /// TTL on, and fresh while the mtime is ahead.
    #[tokio::test]
    async fn bundle_freshness_at_the_ttl_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let ttl = Duration::from_secs(3600);
        let cache = Cache::open(dir.path(), roomy());
        assert!(cache.put_bundle("1.0", &bundle_bytes()).await.unwrap());
        let written = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_mtime(&dir.path().join("bundles/1.0.obsb"), written);
        let fresh_at = |offset: i64| {
            let at = if offset >= 0 {
                written + Duration::from_secs(offset as u64)
            } else {
                written - Duration::from_secs(offset.unsigned_abs())
            };
            let cache = &cache;
            async move {
                let stored = cache.get_bundle("1.0").await.unwrap();
                assert_eq!(stored.written, Some(written));
                bundle_is_fresh(stored.written, at, ttl, None)
            }
        };
        assert!(fresh_at(0).await);
        assert!(fresh_at(3599).await);
        assert!(!fresh_at(3600).await, "exactly the TTL is stale");
        assert!(!fresh_at(3601).await);
        assert!(
            fresh_at(-60).await,
            "an mtime in the future counts as fresh"
        );
        // Reading never touches the file.
        assert_eq!(
            std::fs::metadata(dir.path().join("bundles/1.0.obsb"))
                .unwrap()
                .modified()
                .unwrap(),
            written
        );
    }

    #[tokio::test]
    async fn revalidation_touches_the_tile_at_the_clocks_time() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(crate::clock::ManualClock::at_unix(1_700_000_000));
        let cache = Cache::open_with_clock(dir.path(), roomy(), clock.clone());
        let id = TileId::new(3, 2, 1).unwrap();
        let meta = TileMeta::default();
        assert!(cache.put_tile(id, b"png", &meta).await.unwrap());
        clock.advance(Duration::from_secs(86_400));
        cache.update_tile_meta(id, &meta).await.unwrap();
        let png = dir.path().join("tiles/3/2/1.png");
        assert_eq!(
            std::fs::metadata(png).unwrap().modified().unwrap(),
            clock.now()
        );
    }

    #[tokio::test]
    async fn the_disk_cache_meets_the_store_contracts() {
        use crate::testing::{
            bundle_store_contract, bundle_store_declines, tile_store_contract, tile_store_declines,
        };
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), roomy());
        bundle_store_contract(&cache).await;
        tile_store_contract(&cache).await;
        // Larger than the caps: declined without an error.
        let small = tempfile::tempdir().unwrap();
        let caps = CacheCaps {
            bundle_bytes: 10,
            tile_bytes: 10,
            dem_bytes: 10,
        };
        let cache = Cache::open(small.path(), caps);
        bundle_store_declines(&cache, &[0; 11]).await;
        tile_store_declines(&cache, &[0; 11]).await;
    }

    #[tokio::test]
    async fn rejects_non_bundle_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), roomy());
        cache.put_bundle("1.0", b"<html>oops</html>").await.unwrap();
        // The store keeps bytes; the reader refuses them.
        let stored = cache.get_bundle("1.0").await.unwrap();
        let now = SystemTime::now();
        assert!(usable_bundle("1.0", stored, now, Duration::from_secs(3600), None).is_none());
    }

    /// 1 MiB for both caches, more than any test fills.
    fn roomy() -> CacheCaps {
        CacheCaps {
            bundle_bytes: 1 << 20,
            tile_bytes: 1 << 20,
            dem_bytes: 1 << 20,
        }
    }

    /// Sets the mtime of the file at `path`.
    fn set_mtime(path: &Path, at: SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(at).unwrap();
    }

    /// `i` seconds after a fixed time in the past.
    fn old_time(i: u32) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + u64::from(i))
    }

    /// A bundle cache of `bundle_bytes` and a 1 MiB tile cache.
    fn bundle_cap(bundle_bytes: u64) -> CacheCaps {
        CacheCaps {
            bundle_bytes,
            tile_bytes: 1 << 20,
            dem_bytes: 1 << 20,
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
        scan(&root.join("bundles"), Kind::Bundles, SystemTime::now()).total
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
            set_mtime(&root.join(format!("bundles/{i}.obsb")), old_time(i));
        }
    }

    #[tokio::test]
    async fn full_bundle_cache_evicts_the_oldest_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), bundle_cap(2000));
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
                cache.get_bundle(&i.to_string()).await.is_none(),
                "{i} survived"
            );
        }
        for i in 2..=20 {
            assert!(
                cache.get_bundle(&i.to_string()).await.is_some(),
                "{i} was evicted"
            );
        }
        assert_eq!(cache.bundle_cache_bytes(), 1900);
        assert_eq!(bundle_disk_bytes(dir.path()), 1900);
        assert!(bundles.join(".tmp-1-1").exists());
        assert!(bundles.join("sub/9.obsb").exists());

        // The batch left room: the next put needs no eviction.
        assert!(cache.put_bundle("21", &sized_bundle(100, 3)).await.unwrap());
        assert!(cache.get_bundle("2").await.is_some());
        assert_eq!(cache.bundle_cache_bytes(), 2000);

        // A scan on open counts the same bundles.
        let reopened = Cache::open(dir.path(), bundle_cap(2000));
        assert_eq!(reopened.bundle_cache_bytes(), 2000);
    }

    #[tokio::test]
    async fn rewriting_the_oldest_bundle_evicts_the_next_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), bundle_cap(1000));
        fill_bundles(&cache, dir.path(), 10, 100).await;
        // A same-size rewrite fits without evicting anything.
        assert!(cache.put_bundle("0", &sized_bundle(100, 4)).await.unwrap());
        assert_eq!(cache.bundle_cache_bytes(), 1000);
        assert!(cache.get_bundle("1").await.is_some());

        // Bundle 0 was the oldest, but is the one being replaced (larger), so 1 goes.
        set_mtime(&dir.path().join("bundles/0.obsb"), SystemTime::UNIX_EPOCH);
        assert!(cache.put_bundle("0", &sized_bundle(150, 5)).await.unwrap());
        assert_eq!(cache.get_bundle("0").await.unwrap().bytes.len(), 150);
        assert!(cache.get_bundle("1").await.is_none());
        assert!(cache.get_bundle("2").await.is_some());
        assert_eq!(cache.bundle_cache_bytes(), bundle_disk_bytes(dir.path()));
        assert_eq!(cache.bundle_cache_bytes(), 950);
    }

    #[tokio::test]
    async fn bundle_larger_than_the_cap_is_refused_without_evicting() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), bundle_cap(200));
        fill_bundles(&cache, dir.path(), 1, 60).await;
        assert!(!cache.put_bundle("99", &sized_bundle(201, 0)).await.unwrap());
        assert!(cache.get_bundle("99").await.is_none());
        assert!(cache.get_bundle("0").await.is_some());
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

    #[tokio::test]
    async fn rewriting_a_bundle_larger_than_the_cap_evicts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), bundle_cap(1000));
        fill_bundles(&cache, dir.path(), 10, 100).await;
        // Growth (950) is within the cap, but the bundle itself is not.
        assert!(!cache.put_bundle("9", &sized_bundle(1050, 0)).await.unwrap());
        for i in 0..10 {
            assert!(
                cache.get_bundle(&i.to_string()).await.is_some(),
                "{i} was evicted"
            );
        }
        assert_eq!(cache.get_bundle("9").await.unwrap().bytes.len(), 100);
        assert_eq!(cache.bundle_cache_bytes(), 1000);
    }

    #[tokio::test]
    async fn trim_brings_an_oversized_cache_under_its_cap_and_removes_stale_temps() {
        let dir = tempfile::tempdir().unwrap();
        let big = Cache::open(dir.path(), bundle_cap(10_000));
        fill_bundles(&big, dir.path(), 30, 100).await;
        let bundles = dir.path().join("bundles");
        let (stale, fresh) = (bundles.join(".tmp-1-1"), bundles.join(".tmp-1-2"));
        std::fs::write(&stale, [0u8; 50]).unwrap();
        std::fs::write(&fresh, [0u8; 50]).unwrap();
        set_mtime(&stale, SystemTime::now() - 2 * STALE_TEMP_AGE);

        // Reopened with a smaller cap, as after an upgrade or a lower OBS_BUNDLE_CACHE_MB.
        let cache = Cache::open(dir.path(), bundle_cap(2000));
        assert_eq!(cache.bundle_cache_bytes(), 3000);
        cache.trim().await;
        assert_eq!(cache.bundle_cache_bytes(), 1800);
        assert_eq!(bundle_disk_bytes(dir.path()), 1800);
        assert!(cache.get_bundle("11").await.is_none());
        assert!(cache.get_bundle("12").await.is_some());
        assert!(!stale.exists());
        assert!(fresh.exists(), "a write may still be using it");

        // Under the cap, trim evicts nothing.
        cache.trim().await;
        assert_eq!(bundle_disk_bytes(dir.path()), 1800);
    }

    /// A 1 MiB bundle cache and a tile cache of `tile_bytes`.
    fn tile_cap(tile_bytes: u64) -> CacheCaps {
        CacheCaps {
            bundle_bytes: 1 << 20,
            tile_bytes,
            dem_bytes: 1 << 20,
        }
    }

    /// Size of the tile cache on disk, by the module's own scan.
    fn disk_bytes(root: &Path) -> u64 {
        scan(&root.join("tiles"), Kind::Tiles, SystemTime::now()).total
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
        let cache = Cache::open(dir.path(), tile_cap(1000));
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
        let reopened = Cache::open(dir.path(), tile_cap(1000));
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
            set_mtime(&root.join(format!("tiles/10/0/{i}.png")), old_time(i));
            ids.push(id);
        }
        ids
    }

    #[tokio::test]
    async fn full_tile_cache_evicts_the_oldest_tiles() {
        let dir = tempfile::tempdir().unwrap();
        let unit = tile_len(50);
        let cache = Cache::open(dir.path(), tile_cap(20 * unit));
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
        let cache = Cache::open(dir.path(), tile_cap(10 * tile_len(100)));
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
    async fn rewriting_a_tile_larger_than_the_cap_evicts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let unit = tile_len(100);
        let cache = Cache::open(dir.path(), tile_cap(10 * unit));
        let ids = fill_tiles(&cache, dir.path(), 10, 100).await;
        let big = vec![0u8; 10 * unit as usize];
        assert!(
            !cache
                .put_tile(ids[9], &big, &TileMeta::default())
                .await
                .unwrap()
        );
        for id in &ids {
            assert!(cache.get_tile(*id).await.is_some(), "{id:?} was evicted");
        }
        assert_eq!(cache.tile_cache_bytes(), 10 * unit);
    }

    #[tokio::test]
    async fn tile_larger_than_the_cap_is_refused_without_evicting() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), tile_cap(200));
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
        let cache = Cache::open(dir.path(), tile_cap(10 * unit));
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
        let cache = Cache::open(dir.path(), roomy());
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
        let cache = std::sync::Arc::new(Cache::open(dir.path(), tile_cap(20 * unit)));
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
        let cache = Cache::open(dir.path(), roomy());
        cache.put_bundle("1.0", &bundle_bytes()).await.unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path().join("bundles"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("1.0.obsb")]);
    }
}
