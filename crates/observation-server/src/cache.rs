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
//! same directory, then rename).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use observation_core::bundle::{BUNDLE_MAGIC, BUNDLE_VERSION};
use observation_core::tiles::TileId;
use serde::{Deserialize, Serialize};

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
    /// Approximate size of `tiles/`, scanned at start and kept up to date.
    tile_bytes: AtomicU64,
    tmp_counter: AtomicU64,
}

impl Cache {
    /// Opens (without creating) the cache at `root`. Scans `tiles/` once to
    /// initialise the size estimate, so call it outside of request handling.
    pub fn open(root: impl Into<PathBuf>, bundle_ttl: Duration, tile_cap_bytes: u64) -> Self {
        let root = root.into();
        let tile_bytes = AtomicU64::new(dir_size(&root.join("tiles")));
        Self {
            root,
            bundle_ttl,
            tile_cap_bytes,
            tile_bytes,
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

    /// Reads a cached bundle. The flag tells whether it is still within the TTL;
    /// expired bundles are returned too so callers can serve them when the
    /// upstream is down.
    pub async fn get_bundle(&self, key: &str) -> Option<(Bytes, bool)> {
        let path = self.bundle_path(key)?;
        let meta = tokio::fs::metadata(&path).await.ok()?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        if !bytes.starts_with(&BUNDLE_MAGIC)
            || bytes.get(4..6) != Some(&BUNDLE_VERSION.to_le_bytes()[..])
        {
            tracing::warn!(key, "ignoring cached bundle with bad magic or old version");
            return None;
        }
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok());
        // An mtime in the future (clock skew) counts as fresh.
        let fresh = age.is_none_or(|a| a < self.bundle_ttl);
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

    /// Stores a tile. Returns `Ok(false)` (and logs) when the tile cache is over
    /// its size cap and nothing was written.
    pub async fn put_tile(&self, id: TileId, bytes: &[u8], meta: &TileMeta) -> io::Result<bool> {
        if self.tile_bytes.load(Ordering::Relaxed) >= self.tile_cap_bytes {
            tracing::warn!(
                cap = self.tile_cap_bytes,
                "tile cache is full, not caching {id:?}"
            );
            return Ok(false);
        }
        let (png, json) = self.tile_paths(id)?;
        let previous = tokio::fs::metadata(&png).await.map_or(0, |m| m.len());
        self.write_atomic(&png, bytes).await?;
        self.write_atomic(&json, &encode_meta(meta)?).await?;
        let _ = self
            .tile_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
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

/// Total size of all files below `dir` (0 if it does not exist).
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_bytes() -> Vec<u8> {
        let mut v = BUNDLE_MAGIC.to_vec();
        v.extend_from_slice(&[1, 0, 9, 9]);
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
        assert!(cache.get_bundle("1.0_2.0").await.is_none());
        cache.put_bundle("1.0_2.0", &bundle_bytes()).await.unwrap();
        let (bytes, fresh) = cache.get_bundle("1.0_2.0").await.unwrap();
        assert_eq!(bytes.as_ref(), bundle_bytes().as_slice());
        assert!(fresh);
        assert!(cache.put_bundle("../evil", b"x").await.is_err());

        let expired = Cache::open(dir.path(), Duration::ZERO, 1 << 20);
        let (_, fresh) = expired.get_bundle("1.0_2.0").await.unwrap();
        assert!(!fresh, "zero TTL bundles are stale but still readable");
    }

    #[tokio::test]
    async fn rejects_non_bundle_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), Duration::from_secs(3600), 1 << 20);
        cache.put_bundle("1.0", b"<html>oops</html>").await.unwrap();
        assert!(cache.get_bundle("1.0").await.is_none());
    }

    #[tokio::test]
    async fn tiles_store_meta_and_respect_cap() {
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

        // A scan on open picks up existing files; at the cap nothing is written.
        let reopened = Cache::open(dir.path(), Duration::from_secs(1), 80);
        assert!(reopened.tile_cache_bytes() >= 80);
        let other = TileId::new(3, 3, 3).unwrap();
        assert!(!reopened.put_tile(other, &[1], &meta).await.unwrap());
        assert!(reopened.get_tile(other).await.is_none());
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
