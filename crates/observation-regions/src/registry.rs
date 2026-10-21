//! Regions, their databases and the import queue, persisted in
//! `<data>/regions.json` and published as [`RegionsSnapshot`]s.
//!
//! Layout under the data directory:
//!
//! ```text
//! regions.json            region records (survive restarts)
//! catalog.json            cached extract list
//! db/<slug>-<n>/          one Overpass database per import, with region.poly
//! work/<slug>/            downloads and the database being built
//! ```
//!
//! A region's database is shared as an [`Arc<RegionDb>`]; a refresh or delete
//! retires it, and the directory is removed when the last query using it ends.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use observation_core::geo::BBox;
use observation_core::regions::{JobPhase, JobView, RegionView, RegionsSnapshot};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};
use tokio::task::AbortHandle;

use crate::poly::Boundary;

/// Progress is published at most this often (phase changes go out at once).
const PUBLISH_EVERY: Duration = Duration::from_millis(250);

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// File-system-safe form of a region id (`us/california` -> `us__california`).
pub fn slug(id: &str) -> String {
    id.replace('/', "__")
}

/// An imported Overpass database.
#[derive(Debug)]
pub struct RegionDb {
    pub dir: PathBuf,
    pub boundary: Boundary,
    retired: AtomicBool,
}

impl RegionDb {
    pub fn new(dir: PathBuf, boundary: Boundary) -> Self {
        Self {
            dir,
            boundary,
            retired: AtomicBool::new(false),
        }
    }

    /// Removes the directory once nothing uses the database any more.
    fn retire(&self) {
        self.retired.store(true, Ordering::SeqCst);
    }
}

impl Drop for RegionDb {
    fn drop(&mut self) {
        if self.retired.load(Ordering::SeqCst) {
            if let Err(e) = std::fs::remove_dir_all(&self.dir) {
                tracing::warn!(dir = %self.dir.display(), error = %e, "could not remove old region database");
            } else {
                tracing::info!(dir = %self.dir.display(), "removed old region database");
            }
        }
    }
}

/// What is kept about a region across restarts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegionRecord {
    pub id: String,
    pub name: String,
    /// Directory name under `db/` of the current database.
    pub db_dir: Option<String>,
    pub bbox: Option<BBox>,
    pub data_timestamp: Option<String>,
    pub imported_unix: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub last_error: Option<String>,
    /// An import was queued or running; it is queued again after a restart.
    #[serde(default)]
    pub pending: bool,
}

/// A finished import, ready to be swapped in.
#[derive(Debug)]
pub struct Imported {
    pub db: Arc<RegionDb>,
    pub db_dir: String,
    pub bbox: Option<BBox>,
    pub data_timestamp: Option<String>,
    pub disk_bytes: u64,
}

#[derive(Debug)]
struct Region {
    record: RegionRecord,
    db: Option<Arc<RegionDb>>,
    job: Option<JobView>,
}

#[derive(Debug, Default)]
struct State {
    version: u64,
    regions: BTreeMap<String, Region>,
    queue: VecDeque<String>,
    running: Option<(String, AbortHandle)>,
    last_publish: Option<Instant>,
}

/// Why a region action was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// There is no such region (or no job to cancel).
    NotFound,
    /// An import of this region is already queued or running.
    Busy,
}

#[derive(Debug)]
pub struct Registry {
    state: Mutex<State>,
    tx: watch::Sender<RegionsSnapshot>,
    wake: Notify,
    data_dir: PathBuf,
    max_download_bytes: u64,
    refresh_days: u32,
}

impl Registry {
    /// Loads `regions.json`, opens the databases it names, deletes leftovers
    /// (unreferenced databases, interrupted downloads) and queues imports that
    /// were pending when the agent stopped.
    pub fn open(data_dir: &Path, max_download_bytes: u64, refresh_days: u32) -> Self {
        let records: Vec<RegionRecord> = std::fs::read(data_dir.join("regions.json"))
            .ok()
            .and_then(|b| match serde_json::from_slice(&b) {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::error!(error = %e, "regions.json is unreadable; starting without regions");
                    None
                }
            })
            .unwrap_or_default();
        let db_root = data_dir.join("db");
        let mut state = State::default();
        for mut record in records {
            let db = record.db_dir.as_ref().and_then(|name| {
                let dir = db_root.join(name);
                let poly = std::fs::read_to_string(dir.join("region.poly")).ok()?;
                match Boundary::parse(&poly) {
                    Ok(b) => Some(Arc::new(RegionDb::new(dir, b))),
                    Err(e) => {
                        tracing::warn!(region = %record.id, error = %e, "region boundary unreadable");
                        None
                    }
                }
            });
            if db.is_none() && record.db_dir.take().is_some() {
                record.last_error =
                    Some("the region's database was missing; import it again".into());
            }
            if record.pending {
                state.queue.push_back(record.id.clone());
            }
            let job = record.pending.then(queued_job);
            state
                .regions
                .insert(record.id.clone(), Region { record, db, job });
        }
        // Leftovers of interrupted imports and replaced databases.
        let _ = std::fs::remove_dir_all(data_dir.join("work"));
        let referenced: Vec<String> = state
            .regions
            .values()
            .filter_map(|r| r.record.db_dir.clone())
            .collect();
        if let Ok(entries) = std::fs::read_dir(&db_root) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !referenced.contains(&name) {
                    tracing::info!(dir = %name, "removing unreferenced region database");
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let snapshot = build_snapshot(&state, max_download_bytes, refresh_days);
        let (tx, _) = watch::channel(snapshot);
        Self {
            state: Mutex::new(state),
            tx,
            wake: Notify::new(),
            data_dir: data_dir.to_owned(),
            max_download_bytes,
            refresh_days,
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn max_download_bytes(&self) -> u64 {
        self.max_download_bytes
    }

    pub fn subscribe(&self) -> watch::Receiver<RegionsSnapshot> {
        self.tx.subscribe()
    }

    pub fn snapshot(&self) -> RegionsSnapshot {
        self.tx.borrow().clone()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Publishes the state now and, if `persist`, writes `regions.json`.
    fn commit(&self, mut state: MutexGuard<'_, State>, persist: bool) {
        state.version += 1;
        state.last_publish = Some(Instant::now());
        let snapshot = build_snapshot(&state, self.max_download_bytes, self.refresh_days);
        let records: Vec<RegionRecord> = state.regions.values().map(|r| r.record.clone()).collect();
        drop(state);
        self.tx.send_replace(snapshot);
        if persist {
            // Small and rare (on actions and phase ends), so written in place.
            let path = self.data_dir.join("regions.json");
            match serde_json::to_vec_pretty(&records) {
                Ok(bytes) => {
                    if let Err(e) = crate::write_atomic_blocking(&path, &bytes) {
                        tracing::error!(error = %e, "could not save regions.json");
                    }
                }
                Err(e) => tracing::error!(error = %e, "could not encode regions.json"),
            }
        }
    }

    /// Queues an import (or re-import) of `id`.
    pub fn enqueue(&self, id: &str, name: &str) -> Result<(), ActionError> {
        let mut s = self.lock();
        let region = s.regions.entry(id.to_owned()).or_insert_with(|| Region {
            record: RegionRecord {
                id: id.to_owned(),
                name: name.to_owned(),
                ..RegionRecord::default()
            },
            db: None,
            job: None,
        });
        if region.job.is_some() {
            return Err(ActionError::Busy);
        }
        region.record.name = name.to_owned();
        region.record.pending = true;
        region.job = Some(queued_job());
        s.queue.push_back(id.to_owned());
        self.commit(s, true);
        self.wake.notify_one();
        Ok(())
    }

    /// Cancels the queued or running import of `id`. A region that never
    /// finished an import is forgotten.
    pub fn cancel(&self, id: &str) -> Result<(), ActionError> {
        let mut s = self.lock();
        let region = s.regions.get_mut(id).ok_or(ActionError::NotFound)?;
        if region.job.is_none() {
            return Err(ActionError::NotFound);
        }
        region.job = None;
        region.record.pending = false;
        if region.db.is_none() {
            s.regions.remove(id);
        }
        s.queue.retain(|q| q != id);
        if let Some((running, handle)) = &s.running
            && running == id
        {
            handle.abort();
        }
        self.commit(s, true);
        Ok(())
    }

    /// Cancels any import of `id` and deletes its data.
    pub fn delete(&self, id: &str) -> Result<(), ActionError> {
        let mut s = self.lock();
        let region = s.regions.remove(id).ok_or(ActionError::NotFound)?;
        if let Some(db) = &region.db {
            db.retire();
        }
        s.queue.retain(|q| q != id);
        if let Some((running, handle)) = &s.running
            && running == id
        {
            handle.abort();
        }
        self.commit(s, true);
        Ok(())
    }

    /// Waits for the next queued import and marks it running. `start` spawns
    /// the job; its abort handle is kept for [`Self::cancel`].
    pub async fn next_job(&self) -> String {
        loop {
            let notified = self.wake.notified();
            {
                let mut s = self.lock();
                if s.running.is_none()
                    && let Some(id) = s.queue.pop_front()
                {
                    return id;
                }
            }
            notified.await;
        }
    }

    /// Records the running job's abort handle (call right after spawning it).
    /// Returns `false`, and aborts it, if the region was cancelled meanwhile.
    pub fn started(&self, id: &str, handle: AbortHandle) -> bool {
        let mut s = self.lock();
        let alive = s.regions.get(id).is_some_and(|r| r.job.is_some());
        if !alive {
            handle.abort();
            return false;
        }
        s.running = Some((id.to_owned(), handle));
        true
    }

    /// Updates the running job's progress; published when the phase changes
    /// or [`PUBLISH_EVERY`] has passed.
    pub fn progress(&self, id: &str, phase: JobPhase, done: u64, total: Option<u64>) {
        let mut s = self.lock();
        let due = s.last_publish.is_none_or(|t| t.elapsed() >= PUBLISH_EVERY);
        let Some(job) = s.regions.get_mut(id).and_then(|r| r.job.as_mut()) else {
            return;
        };
        let phase_changed = job.phase != phase;
        if phase_changed {
            job.since_unix = now_unix();
        }
        job.phase = phase;
        job.done = done;
        job.total = total;
        if phase_changed || due {
            self.commit(s, false);
        }
    }

    /// Ends the running job: installs `result` (retiring the database it
    /// replaces) or records its error. `None` means it was cancelled.
    pub fn finish(&self, id: &str, result: Option<Result<Imported, String>>) {
        let mut s = self.lock();
        s.running = None;
        // A cancelled or deleted region has no job any more: drop the result.
        let Some(region) = s.regions.get_mut(id).filter(|r| r.job.is_some()) else {
            if let Some(Ok(imported)) = result {
                imported.db.retire();
            }
            self.commit(s, false);
            self.wake.notify_one();
            return;
        };
        region.job = None;
        region.record.pending = false;
        match result {
            Some(Ok(imported)) => {
                if let Some(old) = region.db.replace(Arc::clone(&imported.db)) {
                    old.retire();
                }
                let r = &mut region.record;
                r.db_dir = Some(imported.db_dir);
                r.bbox = imported.bbox;
                r.data_timestamp = imported.data_timestamp;
                r.imported_unix = Some(now_unix());
                r.disk_bytes = Some(imported.disk_bytes);
                r.last_error = None;
                tracing::info!(region = id, "region ready");
            }
            // The region stays listed (with its old data, if any) so the error can be read.
            Some(Err(e)) => {
                tracing::warn!(region = id, error = %e, "region import failed");
                region.record.last_error = Some(e);
            }
            None => {}
        }
        self.commit(s, true);
        self.wake.notify_one();
    }

    /// The ready database whose boundary holds `bbox` (the smallest, if several).
    pub fn covering(&self, bbox: &BBox) -> Option<(String, Arc<RegionDb>)> {
        let s = self.lock();
        let area = |r: &Region| {
            r.record
                .bbox
                .map_or(f64::MAX, |b| (b.north - b.south) * (b.east - b.west))
        };
        s.regions
            .values()
            .filter(|r| {
                r.db.as_ref()
                    .is_some_and(|db| db.boundary.contains_bbox(bbox))
            })
            .min_by(|a, b| area(a).total_cmp(&area(b)))
            .and_then(|r| Some((r.record.id.clone(), Arc::clone(r.db.as_ref()?))))
    }

    /// Ready regions older than the refresh interval, without a job.
    pub fn due_for_refresh(&self) -> Vec<(String, String)> {
        if self.refresh_days == 0 {
            return Vec::new();
        }
        let cutoff = now_unix().saturating_sub(u64::from(self.refresh_days) * 86_400);
        self.lock()
            .regions
            .values()
            .filter(|r| r.db.is_some() && r.job.is_none())
            .filter(|r| r.record.imported_unix.is_some_and(|t| t < cutoff))
            .map(|r| (r.record.id.clone(), r.record.name.clone()))
            .collect()
    }
}

fn queued_job() -> JobView {
    JobView {
        phase: JobPhase::Queued,
        done: 0,
        total: None,
        since_unix: now_unix(),
    }
}

fn build_snapshot(s: &State, max_download_bytes: u64, refresh_days: u32) -> RegionsSnapshot {
    RegionsSnapshot {
        version: s.version,
        regions: s
            .regions
            .values()
            .map(|r| RegionView {
                id: r.record.id.clone(),
                name: r.record.name.clone(),
                ready: r.db.is_some(),
                bbox: r.record.bbox,
                data_timestamp: r.record.data_timestamp.clone(),
                imported_unix: r.record.imported_unix,
                disk_bytes: r.record.disk_bytes,
                job: r.job.clone(),
                last_error: r.record.last_error.clone(),
            })
            .collect(),
        max_download_bytes,
        refresh_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: &str = "sq\n1\n 0 0\n 1 0\n 1 1\n 0 1\nEND\nEND\n";

    fn imported(dir: &Path, name: &str) -> Imported {
        let db_dir = dir.join("db").join(name);
        std::fs::create_dir_all(&db_dir).unwrap();
        std::fs::write(db_dir.join("region.poly"), SQUARE).unwrap();
        Imported {
            db: Arc::new(RegionDb::new(db_dir, Boundary::parse(SQUARE).unwrap())),
            db_dir: name.to_owned(),
            bbox: BBox::new(0.0, 0.0, 1.0, 1.0).ok(),
            data_timestamp: Some("2026-01-01T00:00:00Z".into()),
            disk_bytes: 5,
        }
    }

    fn spawn_dummy() -> AbortHandle {
        tokio::spawn(std::future::pending::<()>()).abort_handle()
    }

    #[tokio::test]
    async fn import_lifecycle_persists_and_swaps() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path(), 1, 7);
        reg.enqueue("monaco", "Monaco").unwrap();
        assert_eq!(reg.enqueue("monaco", "Monaco"), Err(ActionError::Busy));
        assert_eq!(reg.next_job().await, "monaco");
        assert!(reg.started("monaco", spawn_dummy()));
        reg.progress("monaco", JobPhase::Downloading, 1, Some(2));
        let snap = reg.snapshot();
        assert_eq!(
            snap.regions[0].job.as_ref().map(|j| j.phase),
            Some(JobPhase::Downloading)
        );

        reg.finish("monaco", Some(Ok(imported(dir.path(), "monaco-1"))));
        let bbox = BBox::new(0.2, 0.2, 0.3, 0.3).unwrap();
        assert_eq!(reg.covering(&bbox).map(|c| c.0).as_deref(), Some("monaco"));
        assert!(reg.snapshot().regions[0].ready);

        // A refresh swaps the database; the old one goes when unused.
        let (_, held) = reg.covering(&bbox).unwrap();
        reg.enqueue("monaco", "Monaco").unwrap();
        assert_eq!(reg.next_job().await, "monaco");
        assert!(reg.started("monaco", spawn_dummy()));
        reg.finish("monaco", Some(Ok(imported(dir.path(), "monaco-2"))));
        assert!(dir.path().join("db/monaco-1").exists(), "still in use");
        drop(held);
        assert!(!dir.path().join("db/monaco-1").exists());

        // Reopening restores the region from regions.json.
        let reopened = Registry::open(dir.path(), 1, 7);
        assert!(reopened.covering(&bbox).is_some());
        reopened.delete("monaco").unwrap();
        assert!(reopened.covering(&bbox).is_none());
        assert!(!dir.path().join("db/monaco-2").exists());
    }

    #[tokio::test]
    async fn cancel_forgets_new_regions_and_failures_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path(), 1, 7);
        reg.enqueue("a", "A").unwrap();
        reg.cancel("a").unwrap();
        assert!(reg.snapshot().regions.is_empty());
        assert_eq!(reg.cancel("a"), Err(ActionError::NotFound));

        reg.enqueue("b", "B").unwrap();
        assert_eq!(reg.next_job().await, "b");
        assert!(reg.started("b", spawn_dummy()));
        reg.finish("b", Some(Err("no space left".into())));
        let snap = reg.snapshot();
        assert_eq!(snap.regions[0].last_error.as_deref(), Some("no space left"));
        assert!(!snap.regions[0].ready);
    }

    #[tokio::test]
    async fn pending_imports_resume_and_leftovers_go() {
        let dir = tempfile::tempdir().unwrap();
        {
            let reg = Registry::open(dir.path(), 1, 7);
            reg.enqueue("a", "A").unwrap();
        }
        std::fs::create_dir_all(dir.path().join("db/stray-1")).unwrap();
        std::fs::create_dir_all(dir.path().join("work/a")).unwrap();
        let reg = Registry::open(dir.path(), 1, 7);
        assert_eq!(reg.next_job().await, "a");
        assert!(!dir.path().join("db/stray-1").exists());
        assert!(!dir.path().join("work").exists());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("us/california"), "us__california");
    }
}
