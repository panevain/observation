//! [`Store`]: one size-capped directory of the cache and its eviction.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Mutex;

use super::scan::{evict_oldest, scan};
use super::{Kind, file_len};
use crate::clock::Clock;

/// One size-capped directory of the cache, with an estimate of its size.
#[derive(Debug)]
pub(super) struct Store {
    pub(super) dir: PathBuf,
    kind: Kind,
    cap: u64,
    /// Approximate size of the entries in `dir` (for tiles, `.png` files and
    /// `.json` sidecars), scanned at start, kept up to date and reset from a
    /// fresh scan on every eviction.
    bytes: AtomicU64,
    /// Held while entries are being evicted, so that only one write does it at a time.
    evicting: Mutex<()>,
    /// Decides which temp files are stale.
    clock: Arc<dyn Clock>,
}

impl Store {
    pub(super) fn open(dir: PathBuf, kind: Kind, cap: u64, clock: Arc<dyn Clock>) -> Self {
        let bytes = AtomicU64::new(scan(&dir, kind, clock.now()).total);
        Self {
            dir,
            kind,
            cap,
            bytes,
            evicting: Mutex::new(()),
            clock,
        }
    }

    pub(super) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Whether `growth` more bytes fit, by the estimate.
    fn fits(&self, growth: u64) -> bool {
        self.bytes().saturating_add(growth) <= self.cap
    }

    /// Replaces `old` bytes of the size estimate with `new`.
    pub(super) fn resize(&self, old: u64, new: u64) {
        let _ = self
            .bytes
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(old).saturating_add(new))
            });
    }

    /// Whether an entry (`what`, for the log) may be written as `files`, each a
    /// path with the size it is about to have; `keep` is its main file. Evicts
    /// first if it would not fit. Logs why when it may not: the entry is larger
    /// than the cap (nothing is evicted then), or eviction could not make room.
    pub(super) async fn make_room(&self, keep: &Path, files: &[(&Path, u64)], what: &str) -> bool {
        let new: u64 = files.iter().map(|f| f.1).sum();
        let cache = self.kind.name();
        if new > self.cap {
            tracing::warn!(
                cache,
                cap = self.cap,
                bytes = new,
                "{what} is larger than the {cache} cache cap, not caching it"
            );
            return false;
        }
        let mut previous = 0;
        for (path, _) in files {
            previous += file_len(path).await;
        }
        let growth = new.saturating_sub(previous);
        if self.fits(growth) || self.evict(keep, growth).await {
            return true;
        }
        tracing::warn!(
            cache,
            cap = self.cap,
            bytes = new,
            "{cache} cache is full and eviction could not make room, not caching {what}"
        );
        false
    }

    /// Removes leftovers (see [`evict_oldest`]) and, if the cache is over its
    /// cap, evicts down to 90 % of it.
    pub(super) async fn trim(&self) {
        let _guard = self.evicting.lock().await;
        let target = if self.bytes() > self.cap {
            self.cap - self.cap / 10
        } else {
            u64::MAX
        };
        self.run_eviction(PathBuf::new(), target).await;
    }

    /// Deletes the oldest entries (by the mtime of their main file, with their
    /// sidecars) to make room for a write of `growth` more bytes, never
    /// touching `keep`. It evicts in a batch, down to 90 % of the cap, so that
    /// the next writes do not have to walk the directory again, and resets the
    /// size estimate from the walk, so a drifted estimate cannot make every
    /// later write fail. Returns whether the write may go ahead: there is room
    /// now, or another write is already evicting (the write is then not held up
    /// and the cache may overshoot the cap briefly).
    /// The caller makes sure `growth` is at most the cap.
    async fn evict(&self, keep: &Path, growth: u64) -> bool {
        let cap = self.cap;
        let Ok(_guard) = self.evicting.try_lock() else {
            return true;
        };
        // An eviction may have finished between the caller's check and `try_lock`.
        if self.fits(growth) {
            return true;
        }
        // Below the low-water mark, and low enough to fit the new entry.
        let target = (cap - cap / 10).min(cap.saturating_sub(growth));
        self.run_eviction(keep.to_path_buf(), target).await && self.fits(growth)
    }

    /// Runs [`evict_oldest`] off the runtime with the evict lock held by the
    /// caller, and resets the size estimate from its walk. False if it failed.
    async fn run_eviction(&self, keep: PathBuf, target: u64) -> bool {
        let (dir, kind, now) = (self.dir.clone(), self.kind, self.clock.now());
        let evicted =
            match tokio::task::spawn_blocking(move || evict_oldest(&dir, kind, &keep, target, now))
                .await
            {
                Ok(evicted) => evicted,
                Err(e) => {
                    tracing::warn!(cache = kind.name(), error = %e, "could not evict");
                    return false;
                }
            };
        // The walk is the truth; writes racing with it make this slightly off
        // until the next eviction.
        self.bytes.store(evicted.total, Ordering::Relaxed);
        if evicted.bytes > 0 || evicted.temps > 0 {
            tracing::info!(
                cache = kind.name(),
                entries = evicted.entries,
                bytes = evicted.bytes,
                temp_files = evicted.temps,
                "evicted the oldest entries from the cache"
            );
        }
        true
    }
}
