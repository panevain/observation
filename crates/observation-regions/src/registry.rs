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
//!
//! Every queued import gets a [`JobId`]. The worker passes it back with its
//! progress and result, which are ignored once the job was cancelled or
//! replaced, so a stale task can never touch a newer import of the same region.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use observation_core::geo::BBox;
use observation_core::regions::{
    DEFAULT_MAX_REFRESH_DAYS, DEFAULT_MIN_REFRESH_DAYS, JobPhase, JobView, RegionView,
    RegionsSnapshot,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Notify, watch};
use tokio::task::AbortHandle;

use crate::poly::Boundary;

/// Progress is published at most this often (phase changes go out at once).
const PUBLISH_EVERY: Duration = Duration::from_millis(250);
/// A failed or cancelled scheduled refresh is not retried for this long.
const RETRY_REFRESH_AFTER: u64 = 24 * 3600;

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// File-system-safe form of a region id (`us/california` -> `us__california`).
pub fn slug(id: &str) -> String {
    id.replace('/', "__")
}

/// Identifies one queued import.
pub type JobId = u64;

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
    /// Deleting a large database takes a while: it runs on its own thread, so
    /// neither the registry lock nor a runtime thread waits for it.
    fn drop(&mut self) {
        if !self.retired.load(Ordering::SeqCst) {
            return;
        }
        let dir = std::mem::take(&mut self.dir);
        std::thread::spawn(move || match std::fs::remove_dir_all(&dir) {
            Ok(()) => tracing::info!(dir = %dir.display(), "removed old region database"),
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "could not remove old region database");
            }
        });
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
    /// When the last import ended, however it ended (Unix seconds).
    #[serde(default)]
    pub last_attempt_unix: Option<u64>,
    /// An import was queued or running; it is queued again after a restart.
    #[serde(default)]
    pub pending: bool,
    /// Scheduled refreshes are on for this region.
    #[serde(default = "yes")]
    pub auto_refresh: bool,
    /// Days between this region's scheduled refreshes; `None` uses the
    /// agent's `OBS_REGIONS_DEFAULT_REFRESH_DAYS`.
    #[serde(default)]
    pub refresh_days: Option<u32>,
}

fn yes() -> bool {
    true
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
    job: Option<(JobId, JobView)>,
}

#[derive(Debug, Default)]
struct State {
    version: u64,
    next_job: JobId,
    regions: BTreeMap<String, Region>,
    queue: VecDeque<(String, JobId)>,
    running: Option<(JobId, AbortHandle)>,
    last_publish: Option<Instant>,
}

impl State {
    fn new_job(&mut self) -> (JobId, JobView) {
        self.next_job += 1;
        (
            self.next_job,
            JobView {
                phase: JobPhase::Queued,
                done: 0,
                total: None,
                since_unix: now_unix(),
            },
        )
    }

    /// The region whose current job is `job`.
    fn job_region(&mut self, id: &str, job: JobId) -> Option<&mut Region> {
        self.regions
            .get_mut(id)
            .filter(|r| r.job.as_ref().is_some_and(|(j, _)| *j == job))
    }

    /// Aborts the running task if it is `job`.
    fn abort_if_running(&self, job: Option<JobId>) {
        if let (Some(job), Some((running, handle))) = (job, &self.running)
            && *running == job
        {
            handle.abort();
        }
    }
}

/// Why a region action was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// There is no such region (or no job to cancel).
    NotFound,
    /// An import of this region is already queued or running.
    Busy,
}

/// `regions.json` exists but cannot be used. The agent refuses to start
/// rather than treat every database as unreferenced and delete it.
#[derive(Debug, Error)]
#[error(
    "{path} is unreadable ({reason}); fix or remove it to start (removing it deletes all imported regions)"
)]
pub struct OpenError {
    path: String,
    reason: String,
}

#[derive(Debug)]
pub struct Registry {
    state: Mutex<State>,
    tx: watch::Sender<RegionsSnapshot>,
    wake: Notify,
    data_dir: PathBuf,
    max_download_bytes: u64,
    refresh: Refresh,
}

/// The agent's scheduled-refresh settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Refresh {
    /// Default interval, days; 0 turns scheduled refreshes off for all regions.
    days: u32,
    /// Range a region's own interval must lie in.
    min_days: u32,
    max_days: u32,
}

impl Refresh {
    /// The interval of a region with `own` days set (clamped, in case the
    /// range was narrowed since), or the default.
    fn days_for(self, own: Option<u32>) -> u32 {
        own.map_or(self.days, |d| d.clamp(self.min_days, self.max_days))
    }
}

impl Registry {
    /// Loads `regions.json`, opens the databases it names, deletes leftovers
    /// (unreferenced databases, interrupted downloads) and queues imports that
    /// were pending when the agent stopped.
    pub fn open(
        data_dir: &Path,
        max_download_bytes: u64,
        refresh_days: u32,
    ) -> Result<Self, OpenError> {
        let path = data_dir.join("regions.json");
        let records: Vec<RegionRecord> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| OpenError {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(OpenError {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                });
            }
        };
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
            let job = record.pending.then(|| state.new_job());
            if let Some((j, _)) = &job {
                state.queue.push_back((record.id.clone(), *j));
            }
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
        let refresh = Refresh {
            days: refresh_days,
            min_days: DEFAULT_MIN_REFRESH_DAYS,
            max_days: DEFAULT_MAX_REFRESH_DAYS,
        };
        let snapshot = build_snapshot(&state, max_download_bytes, refresh);
        let (tx, _) = watch::channel(snapshot);
        Ok(Self {
            state: Mutex::new(state),
            tx,
            wake: Notify::new(),
            data_dir: data_dir.to_owned(),
            max_download_bytes,
            refresh,
        })
    }

    /// Sets the range a region's own refresh interval must lie in
    /// (`1 <= min_days <= max_days`, checked by the configuration).
    pub fn with_refresh_range(mut self, min_days: u32, max_days: u32) -> Self {
        self.refresh.min_days = min_days;
        self.refresh.max_days = max_days;
        let snapshot = build_snapshot(&self.lock(), self.max_download_bytes, self.refresh);
        self.tx.send_replace(snapshot);
        self
    }

    /// The range a region's own refresh interval must lie in, days.
    pub fn refresh_range(&self) -> std::ops::RangeInclusive<u32> {
        self.refresh.min_days..=self.refresh.max_days
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

    /// Publishes the state and, if `persist`, writes `regions.json`. The file
    /// is written before the lock is released, so writes cannot overtake each
    /// other (it is small, and only written on actions and job ends).
    fn commit(&self, mut state: MutexGuard<'_, State>, persist: bool) {
        state.version += 1;
        state.last_publish = Some(Instant::now());
        let snapshot = build_snapshot(&state, self.max_download_bytes, self.refresh);
        if persist {
            let records: Vec<&RegionRecord> = state.regions.values().map(|r| &r.record).collect();
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
        drop(state);
        self.tx.send_replace(snapshot);
    }

    /// Queues an import (or re-import) of `id`.
    pub fn enqueue(&self, id: &str, name: &str) -> Result<(), ActionError> {
        let mut s = self.lock();
        if s.regions.get(id).is_some_and(|r| r.job.is_some()) {
            return Err(ActionError::Busy);
        }
        let job = s.new_job();
        let job_id = job.0;
        let region = s.regions.entry(id.to_owned()).or_insert_with(|| Region {
            record: RegionRecord {
                id: id.to_owned(),
                auto_refresh: true,
                ..RegionRecord::default()
            },
            db: None,
            job: None,
        });
        region.record.name = name.to_owned();
        region.record.pending = true;
        region.job = Some(job);
        s.queue.push_back((id.to_owned(), job_id));
        self.commit(s, true);
        self.wake.notify_one();
        Ok(())
    }

    /// Cancels the queued or running import of `id`. A region that never
    /// finished an import is forgotten.
    pub fn cancel(&self, id: &str) -> Result<(), ActionError> {
        let mut s = self.lock();
        let region = s.regions.get_mut(id).ok_or(ActionError::NotFound)?;
        let (job, _) = region.job.take().ok_or(ActionError::NotFound)?;
        region.record.pending = false;
        region.record.last_attempt_unix = Some(now_unix());
        if region.db.is_none() {
            s.regions.remove(id);
        }
        s.queue.retain(|(_, j)| *j != job);
        s.abort_if_running(Some(job));
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
        let job = region.job.as_ref().map(|(j, _)| *j);
        s.queue.retain(|(_, j)| Some(*j) != job);
        s.abort_if_running(job);
        self.commit(s, true);
        drop(region);
        Ok(())
    }

    /// Changes the refresh settings of `id`: turns scheduled refreshes on or
    /// off, and sets the days between them (the caller checks them against
    /// [`Registry::refresh_range`]). `None` leaves a setting as it is. A
    /// refresh already queued or running carries on (cancel it to stop it).
    pub fn change_settings(
        &self,
        id: &str,
        auto_refresh: Option<bool>,
        refresh_days: Option<u32>,
    ) -> Result<(), ActionError> {
        let mut s = self.lock();
        let r = &mut s.regions.get_mut(id).ok_or(ActionError::NotFound)?.record;
        let before = (r.auto_refresh, r.refresh_days);
        r.auto_refresh = auto_refresh.unwrap_or(r.auto_refresh);
        r.refresh_days = refresh_days.or(r.refresh_days);
        if (r.auto_refresh, r.refresh_days) != before {
            tracing::info!(region = id, on = r.auto_refresh, days = ?r.refresh_days, "refresh settings changed");
            self.commit(s, true);
        }
        Ok(())
    }

    /// Waits until nothing runs and an import is queued; returns it.
    pub async fn next_job(&self) -> (String, JobId) {
        loop {
            let notified = self.wake.notified();
            {
                let mut s = self.lock();
                if s.running.is_none()
                    && let Some(next) = s.queue.pop_front()
                {
                    return next;
                }
            }
            notified.await;
        }
    }

    /// Records the running job's abort handle (call right after spawning it).
    /// Returns `false`, and aborts it, if the job was cancelled meanwhile.
    pub fn started(&self, id: &str, job: JobId, handle: AbortHandle) -> bool {
        let mut s = self.lock();
        if s.job_region(id, job).is_none() {
            handle.abort();
            return false;
        }
        s.running = Some((job, handle));
        true
    }

    /// Updates a job's progress; published when the phase changes or
    /// [`PUBLISH_EVERY`] has passed. Ignored for a cancelled or replaced job.
    pub fn progress(&self, id: &str, job: JobId, phase: JobPhase, done: u64, total: Option<u64>) {
        let mut s = self.lock();
        let due = s.last_publish.is_none_or(|t| t.elapsed() >= PUBLISH_EVERY);
        let Some((_, view)) = s.job_region(id, job).and_then(|r| r.job.as_mut()) else {
            return;
        };
        let phase_changed = view.phase != phase;
        if phase_changed {
            view.since_unix = now_unix();
        }
        view.phase = phase;
        view.done = done;
        view.total = total;
        if phase_changed || due {
            self.commit(s, false);
        }
    }

    /// Ends a job: installs `result` (retiring the database it replaces) or
    /// records its error. `None` means it was cancelled. A result for a job
    /// that is no longer current is dropped.
    pub fn finish(&self, id: &str, job: JobId, result: Option<Result<Imported, String>>) {
        let mut s = self.lock();
        if s.running.as_ref().is_some_and(|(j, _)| *j == job) {
            s.running = None;
        }
        let mut retired = None;
        if let Some(region) = s.job_region(id, job) {
            region.job = None;
            region.record.pending = false;
            region.record.last_attempt_unix = Some(now_unix());
            match result {
                Some(Ok(imported)) => {
                    retired = region.db.replace(Arc::clone(&imported.db));
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
            if let Some(old) = &retired {
                old.retire();
            }
        } else if let Some(Ok(imported)) = result {
            imported.db.retire();
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
            // Cheap rejection by bounds before the polygon test.
            .filter(|r| r.record.bbox.is_none_or(|b| b.contains_box(bbox)))
            .filter(|r| {
                r.db.as_ref()
                    .is_some_and(|db| db.boundary.contains_bbox(bbox))
            })
            .min_by(|a, b| area(a).total_cmp(&area(b)))
            .and_then(|r| Some((r.record.id.clone(), Arc::clone(r.db.as_ref()?))))
    }

    /// Regions due for a scheduled refresh (see [`next_refresh`]).
    pub fn due_for_refresh(&self) -> Vec<(String, String)> {
        let now = now_unix();
        self.lock()
            .regions
            .values()
            .filter(|r| next_refresh(r, self.refresh).is_some_and(|t| t < now))
            .map(|r| (r.record.id.clone(), r.record.name.clone()))
            .collect()
    }
}

/// When `r` is due for a scheduled refresh: its interval after its import,
/// or, when a later attempt failed or was cancelled, a day after that attempt.
/// `None` for a region that is not ready, has an import queued or running, or
/// has auto-refresh off (as have all with the agent's default interval 0,
/// whatever their own).
fn next_refresh(r: &Region, refresh: Refresh) -> Option<u64> {
    if refresh.days == 0 || !r.record.auto_refresh || r.db.is_none() || r.job.is_some() {
        return None;
    }
    let imported = r.record.imported_unix?;
    let days = refresh.days_for(r.record.refresh_days);
    let due = imported.saturating_add(u64::from(days) * 86_400);
    Some(match r.record.last_attempt_unix {
        Some(t) if t > imported => due.max(t.saturating_add(RETRY_REFRESH_AFTER)),
        _ => due,
    })
}

fn build_snapshot(s: &State, max_download_bytes: u64, refresh: Refresh) -> RegionsSnapshot {
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
                last_attempt_unix: r.record.last_attempt_unix,
                auto_refresh: r.record.auto_refresh,
                refresh_days: r.record.refresh_days.map(|d| refresh.days_for(Some(d))),
                next_refresh_unix: next_refresh(r, refresh),
                disk_bytes: r.record.disk_bytes,
                job: r.job.as_ref().map(|(_, v)| v.clone()),
                last_error: r.record.last_error.clone(),
            })
            .collect(),
        max_download_bytes,
        refresh_days: refresh.days,
        min_refresh_days: refresh.min_days,
        max_refresh_days: refresh.max_days,
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

    fn open(dir: &Path) -> Registry {
        Registry::open(dir, 1, 7).unwrap()
    }

    /// Database removal runs on its own thread; wait for it.
    fn gone(path: &Path) -> bool {
        for _ in 0..200 {
            if !path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[tokio::test]
    async fn import_lifecycle_persists_and_swaps() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(dir.path());
        reg.enqueue("monaco", "Monaco").unwrap();
        assert_eq!(reg.enqueue("monaco", "Monaco"), Err(ActionError::Busy));
        let (id, job) = reg.next_job().await;
        assert_eq!(id, "monaco");
        assert!(reg.started(&id, job, spawn_dummy()));
        reg.progress(&id, job, JobPhase::Downloading, 1, Some(2));
        let snap = reg.snapshot();
        assert_eq!(
            snap.regions[0].job.as_ref().map(|j| j.phase),
            Some(JobPhase::Downloading)
        );

        reg.finish(&id, job, Some(Ok(imported(dir.path(), "monaco-1"))));
        let bbox = BBox::new(0.2, 0.2, 0.3, 0.3).unwrap();
        assert_eq!(reg.covering(&bbox).map(|c| c.0).as_deref(), Some("monaco"));
        assert!(reg.snapshot().regions[0].ready);

        // A refresh swaps the database; the old one goes when unused.
        let (_, held) = reg.covering(&bbox).unwrap();
        reg.enqueue("monaco", "Monaco").unwrap();
        let (id, job) = reg.next_job().await;
        assert!(reg.started(&id, job, spawn_dummy()));
        reg.finish(&id, job, Some(Ok(imported(dir.path(), "monaco-2"))));
        assert!(dir.path().join("db/monaco-1").exists(), "still in use");
        drop(held);
        assert!(gone(&dir.path().join("db/monaco-1")));

        // Reopening restores the region from regions.json.
        let reopened = open(dir.path());
        assert!(reopened.covering(&bbox).is_some());
        reopened.delete("monaco").unwrap();
        assert!(reopened.covering(&bbox).is_none());
        assert!(gone(&dir.path().join("db/monaco-2")));
    }

    #[tokio::test]
    async fn a_stale_job_cannot_touch_a_newer_import() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(dir.path());
        reg.enqueue("a", "A").unwrap();
        let (id, old) = reg.next_job().await;
        assert!(reg.started(&id, old, spawn_dummy()));
        // Cancelled, then imported again before the old task has wound down.
        reg.cancel("a").unwrap();
        reg.enqueue("a", "A").unwrap();
        reg.progress("a", old, JobPhase::Importing, 5, Some(9));
        reg.finish("a", old, Some(Ok(imported(dir.path(), "a-old"))));
        let snap = reg.snapshot();
        let job = snap.regions[0]
            .job
            .as_ref()
            .expect("the new import is still queued");
        assert_eq!(job.phase, JobPhase::Queued);
        assert!(!snap.regions[0].ready, "the stale result was not installed");
        assert!(gone(&dir.path().join("db/a-old")));
        let (_, new) = reg.next_job().await;
        assert_ne!(new, old);
        assert!(reg.started("a", new, spawn_dummy()));
    }

    #[tokio::test]
    async fn cancel_forgets_new_regions_and_failures_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(dir.path());
        reg.enqueue("a", "A").unwrap();
        reg.cancel("a").unwrap();
        assert!(reg.snapshot().regions.is_empty());
        assert_eq!(reg.cancel("a"), Err(ActionError::NotFound));

        reg.enqueue("b", "B").unwrap();
        let (id, job) = reg.next_job().await;
        assert_eq!(id, "b");
        assert!(reg.started(&id, job, spawn_dummy()));
        reg.finish(&id, job, Some(Err("no space left".into())));
        let snap = reg.snapshot();
        assert_eq!(snap.regions[0].last_error.as_deref(), Some("no space left"));
        assert!(!snap.regions[0].ready);
    }

    #[tokio::test]
    async fn failed_refreshes_are_not_retried_hourly() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path(), 1, 1).unwrap();
        reg.enqueue("a", "A").unwrap();
        let (id, job) = reg.next_job().await;
        reg.finish(&id, job, Some(Ok(imported(dir.path(), "a-1"))));
        // Pretend the import is two days old.
        {
            let mut state = reg.lock();
            let r = &mut state.regions.get_mut("a").unwrap().record;
            r.imported_unix = Some(now_unix() - 2 * 86_400);
            r.last_attempt_unix = r.imported_unix;
        }
        assert_eq!(reg.due_for_refresh().len(), 1);
        reg.enqueue("a", "A").unwrap();
        let (id, job) = reg.next_job().await;
        reg.finish(&id, job, Some(Err("gone".into())));
        assert!(
            reg.due_for_refresh().is_empty(),
            "retried after a day, not every hour"
        );
    }

    #[tokio::test]
    async fn auto_refresh_can_be_turned_off_per_region() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path(), 1, 1).unwrap();
        assert_eq!(
            reg.change_settings("a", Some(false), None),
            Err(ActionError::NotFound)
        );
        reg.enqueue("a", "A").unwrap();
        let (id, job) = reg.next_job().await;
        reg.finish(&id, job, Some(Ok(imported(dir.path(), "a-1"))));
        let r = &reg.snapshot().regions[0];
        assert!(r.auto_refresh, "on by default");
        let imported = r.imported_unix.unwrap();
        assert_eq!(r.next_refresh_unix, Some(imported + 86_400));
        {
            let mut state = reg.lock();
            let r = &mut state.regions.get_mut("a").unwrap().record;
            r.imported_unix = Some(now_unix() - 2 * 86_400);
            r.last_attempt_unix = r.imported_unix;
        }
        assert_eq!(reg.due_for_refresh().len(), 1);

        let version = reg.snapshot().version;
        reg.change_settings("a", Some(false), None).unwrap();
        assert!(reg.due_for_refresh().is_empty());
        let r = &reg.snapshot().regions[0];
        assert!(!r.auto_refresh);
        assert_eq!(r.next_refresh_unix, None);
        reg.change_settings("a", Some(false), None).unwrap();
        assert_eq!(reg.snapshot().version, version + 1, "no change, no publish");

        // Persisted across restarts.
        drop(reg);
        let reopened = Registry::open(dir.path(), 1, 1).unwrap();
        assert!(!reopened.snapshot().regions[0].auto_refresh);
        assert!(reopened.due_for_refresh().is_empty());
        reopened.change_settings("a", Some(true), None).unwrap();
        assert_eq!(reopened.due_for_refresh().len(), 1);
    }

    #[tokio::test]
    async fn a_region_can_have_its_own_interval() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path(), 1, 7).unwrap();
        assert_eq!(reg.refresh_range(), 7..=365, "the defaults");
        reg.enqueue("a", "A").unwrap();
        let (id, job) = reg.next_job().await;
        reg.finish(&id, job, Some(Ok(imported(dir.path(), "a-1"))));
        let imported_at = now_unix() - 10 * 86_400;
        {
            let mut state = reg.lock();
            let r = &mut state.regions.get_mut("a").unwrap().record;
            r.imported_unix = Some(imported_at);
            r.last_attempt_unix = r.imported_unix;
        }
        assert_eq!(
            reg.due_for_refresh().len(),
            1,
            "the agent's 7 days have passed"
        );

        reg.change_settings("a", None, Some(30)).unwrap();
        assert!(reg.due_for_refresh().is_empty(), "its own 30 days have not");
        let r = &reg.snapshot().regions[0];
        assert_eq!(r.refresh_days, Some(30));
        assert!(r.auto_refresh, "untouched");
        assert_eq!(r.next_refresh_unix, Some(imported_at + 30 * 86_400));

        // Turning it off and on keeps the interval, and it survives a restart.
        reg.change_settings("a", Some(false), None).unwrap();
        reg.change_settings("a", Some(true), None).unwrap();
        drop(reg);
        // A narrower range clamps it.
        let narrow = Registry::open(dir.path(), 1, 7)
            .unwrap()
            .with_refresh_range(1, 14);
        let snap = narrow.snapshot();
        assert_eq!((snap.min_refresh_days, snap.max_refresh_days), (1, 14));
        assert_eq!(snap.regions[0].refresh_days, Some(14));
        assert_eq!(narrow.due_for_refresh().len(), 0, "14 days have not passed");
        drop(narrow);
        // A default interval of 0 still turns every scheduled refresh off.
        let off = Registry::open(dir.path(), 1, 0).unwrap();
        assert_eq!(off.snapshot().regions[0].next_refresh_unix, None);
    }

    #[test]
    fn records_from_before_auto_refresh_keep_refreshing() {
        let r: RegionRecord = serde_json::from_str(r#"{"id":"a","name":"A","db_dir":null,"bbox":null,"data_timestamp":null,"imported_unix":null,"disk_bytes":null,"last_error":null}"#).unwrap();
        assert!(r.auto_refresh);
    }

    #[tokio::test]
    async fn pending_imports_resume_and_leftovers_go() {
        let dir = tempfile::tempdir().unwrap();
        {
            let reg = open(dir.path());
            reg.enqueue("a", "A").unwrap();
        }
        std::fs::create_dir_all(dir.path().join("db/stray-1")).unwrap();
        std::fs::create_dir_all(dir.path().join("work/a")).unwrap();
        let reg = open(dir.path());
        assert_eq!(reg.next_job().await.0, "a");
        assert!(!dir.path().join("db/stray-1").exists());
        assert!(!dir.path().join("work").exists());
    }

    #[test]
    fn a_corrupt_state_file_stops_start_up_and_keeps_the_databases() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("db/a-1")).unwrap();
        std::fs::write(dir.path().join("regions.json"), b"[{\"id\": \"a\", ").unwrap();
        let err = Registry::open(dir.path(), 1, 7).unwrap_err();
        assert!(err.to_string().contains("regions.json"));
        assert!(dir.path().join("db/a-1").exists());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("us/california"), "us__california");
    }
}
