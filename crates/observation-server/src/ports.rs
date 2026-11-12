//! The server's storage ports: where bundles and proxied tiles are kept.
//!
//! A store only keeps bytes and says when they were written; whether a
//! stored bundle or tile is still good is for the callers to decide. The
//! production implementation of both is
//! the disk [`Cache`](crate::cache::Cache); tests can use the in-memory
//! fakes in `testing` (behind the `testing` feature).

use std::fmt::Debug;
use std::io;
use std::time::SystemTime;

use axum::body::Bytes;
use futures_util::future::BoxFuture;
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

/// A tile read back from a store.
#[derive(Debug, Clone)]
pub struct CachedTile {
    pub bytes: Bytes,
    pub meta: TileMeta,
}

/// Which set of proxied tiles: map images or elevation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileSet {
    Map,
    Dem,
}

/// A bundle read back from a store, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBundle {
    pub bytes: Bytes,
    /// When it was written, if the store knows.
    pub written: Option<SystemTime>,
}

/// Encoded bundles by cache key.
pub trait BundleStore: Send + Sync + Debug {
    /// The bundle stored under `key`, if any. Reading does not change when it
    /// counts as written.
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<StoredBundle>>;

    /// Stores `bytes` under `key`, replacing any earlier bundle. `Ok(false)`
    /// when the store declines to keep it (too large, no room), which is not
    /// an error; an unusable key is.
    fn put<'a>(&'a self, key: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, io::Result<bool>>;
}

/// Proxied map and elevation tiles with their metadata.
pub trait TileStore: Send + Sync + Debug {
    fn get(&self, set: TileSet, id: TileId) -> BoxFuture<'_, Option<CachedTile>>;

    /// Stores a tile and its metadata; `Ok(false)` when the store declines.
    fn put<'a>(
        &'a self,
        set: TileSet,
        id: TileId,
        png: &'a [u8],
        meta: &'a TileMeta,
    ) -> BoxFuture<'a, io::Result<bool>>;

    /// Replaces the metadata of a stored tile after the upstream confirmed it
    /// (a 304), and marks the tile as recently used.
    fn revalidated<'a>(
        &'a self,
        set: TileSet,
        id: TileId,
        meta: &'a TileMeta,
    ) -> BoxFuture<'a, io::Result<()>>;
}
