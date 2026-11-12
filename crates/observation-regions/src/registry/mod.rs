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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observation_core::geo::BBox;
use observation_core::regions::{JobPhase, JobView, RegionsSnapshot};
use tokio::sync::{Notify, watch};
use tokio::task::AbortHandle;
use tokio::time::Instant;

use crate::poly::Boundary;
use crate::ports::Clock;

mod persist;
mod snapshot;
mod state;

pub use self::persist::{OpenError, RegionRecord};
use self::snapshot::{build_snapshot, next_refresh};

/// Progress is published at most this often (phase changes go out at once).
const PUBLISH_EVERY: Duration = Duration::from_millis(250);

/// Seconds since the Unix epoch at `t` (0 before it).
pub fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
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

/// A finished import, ready to be swapped in.
#[derive(Debug)]
pub struct Imported {
    pub db: Arc<RegionDb>,
    pub db_dir: String,
    pub bbox: Option<BBox>,
    pub data_timestamp: Option<String>,
    pub disk_bytes: u64,
}

/// The region answering an area: see [`Registry::covering`].
#[derive(Debug)]
pub struct Covering {
    pub id: String,
    pub db: Arc<RegionDb>,
    /// The extract's replication timestamp, when it had one.
    pub data_timestamp: Option<String>,
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
    fn new_job(&mut self, now_unix: u64) -> (JobId, JobView) {
        self.next_job += 1;
        (
            self.next_job,
            JobView {
                phase: JobPhase::Queued,
                done: 0,
                total: None,
                since_unix: now_unix,
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

#[derive(Debug)]
pub struct Registry {
    state: Mutex<State>,
    tx: watch::Sender<RegionsSnapshot>,
    wake: Notify,
    data_dir: PathBuf,
    max_download_bytes: u64,
    refresh: Refresh,
    clock: Arc<dyn Clock>,
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

    /// The clock's time in Unix seconds.
    pub fn now_unix(&self) -> u64 {
        unix_secs(self.clock.now())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ManualClock, SystemClock};

    fn now_unix() -> u64 {
        unix_secs(SystemTime::now())
    }

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
        Registry::open(dir, 1, 7, Arc::new(SystemClock)).unwrap()
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
        assert_eq!(reg.covering(&bbox).map(|c| c.id).as_deref(), Some("monaco"));
        assert!(reg.snapshot().regions[0].ready);

        // A refresh swaps the database; the old one goes when unused.
        let held = reg.covering(&bbox).unwrap().db;
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

    /// Every time the registry records comes from its clock; progress is
    /// published at most every 250 ms of Tokio's clock.
    #[tokio::test(start_paused = true)]
    async fn times_come_from_the_clock() {
        const T0: u64 = 1_700_000_000;
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::at_unix(T0));
        let reg = Registry::open(dir.path(), 1, 7, clock.clone()).unwrap();
        let job_view = |reg: &Registry| reg.snapshot().regions[0].job.clone().unwrap();
        reg.enqueue("a", "A").unwrap();
        assert_eq!(job_view(&reg).since_unix, T0);
        let (id, job) = reg.next_job().await;
        assert!(reg.started(&id, job, spawn_dummy()));

        clock.advance(Duration::from_secs(10));
        reg.progress(&id, job, JobPhase::Downloading, 1, None);
        assert_eq!(job_view(&reg).since_unix, T0 + 10, "a new phase starts now");
        reg.progress(&id, job, JobPhase::Downloading, 2, None);
        tokio::time::advance(Duration::from_millis(249)).await;
        reg.progress(&id, job, JobPhase::Downloading, 3, None);
        assert_eq!(job_view(&reg).done, 1, "published less than 250 ms ago");
        tokio::time::advance(Duration::from_millis(1)).await;
        reg.progress(&id, job, JobPhase::Downloading, 4, None);
        assert_eq!(job_view(&reg).done, 4);
        assert_eq!(job_view(&reg).since_unix, T0 + 10);

        clock.advance(Duration::from_secs(10));
        reg.finish(&id, job, Some(Ok(imported(dir.path(), "a-1"))));
        let r = reg.snapshot().regions[0].clone();
        assert_eq!(r.imported_unix, Some(T0 + 20));
        assert_eq!(r.last_attempt_unix, Some(T0 + 20));
        assert_eq!(r.next_refresh_unix, Some(T0 + 20 + 7 * 86_400));
        clock.advance(Duration::from_secs(7 * 86_400));
        assert!(reg.due_for_refresh().is_empty(), "due only after");
        clock.advance(Duration::from_secs(1));
        assert_eq!(reg.due_for_refresh().len(), 1);

        // An import pending at a restart is queued again, at the clock's time.
        reg.enqueue("b", "B").unwrap();
        drop(reg);
        clock.advance(Duration::from_secs(5));
        let reg = Registry::open(dir.path(), 1, 7, clock.clone()).unwrap();
        let b = reg
            .snapshot()
            .regions
            .into_iter()
            .find(|r| r.id == "b")
            .unwrap();
        assert_eq!(b.job.unwrap().since_unix, T0 + 20 + 7 * 86_400 + 1 + 5);
    }

    /// `regions.json` byte for byte: a ready region with its own settings,
    /// and one whose import is pending (queued again after a restart).
    #[tokio::test]
    async fn regions_json_is_these_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::at_unix(1_700_000_000));
        let reg = Registry::open(dir.path(), 1, 7, clock.clone()).unwrap();
        reg.enqueue("europe/monaco", "Monaco").unwrap();
        let (id, job) = reg.next_job().await;
        assert!(reg.started(&id, job, spawn_dummy()));
        clock.advance(Duration::from_secs(100));
        reg.finish(&id, job, Some(Ok(imported(dir.path(), "europe__monaco-1"))));
        reg.change_settings(&id, Some(false), Some(Some(30)))
            .unwrap();
        reg.enqueue("andorra", "Andorra").unwrap();
        let written = std::fs::read_to_string(dir.path().join("regions.json")).unwrap();
        assert_eq!(written, GOLDEN_REGIONS_JSON);
        // And it reads back as written.
        drop(reg);
        let reopened = Registry::open(dir.path(), 1, 7, clock).unwrap();
        let ids: Vec<String> = reopened
            .snapshot()
            .regions
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, ["andorra", "europe/monaco"]);
    }

    const GOLDEN_REGIONS_JSON: &str = r#"[
  {
    "id": "andorra",
    "name": "Andorra",
    "db_dir": null,
    "bbox": null,
    "data_timestamp": null,
    "imported_unix": null,
    "disk_bytes": null,
    "last_error": null,
    "last_error_unix": null,
    "last_attempt_unix": null,
    "pending": true,
    "auto_refresh": true,
    "refresh_days": null
  },
  {
    "id": "europe/monaco",
    "name": "Monaco",
    "db_dir": "europe__monaco-1",
    "bbox": {
      "south": 0.0,
      "west": 0.0,
      "north": 1.0,
      "east": 1.0
    },
    "data_timestamp": "2026-01-01T00:00:00Z",
    "imported_unix": 1700000100,
    "disk_bytes": 5,
    "last_error": null,
    "last_error_unix": null,
    "last_attempt_unix": 1700000100,
    "pending": false,
    "auto_refresh": false,
    "refresh_days": 30
  }
]"#;

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
        assert!(
            snap.regions[0]
                .last_error_unix
                .is_some_and(|t| t + 5 >= now_unix())
        );
        assert!(!snap.regions[0].ready);
    }

    #[tokio::test]
    async fn failed_refreshes_are_not_retried_hourly() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path(), 1, 1, Arc::new(SystemClock)).unwrap();
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
        let reg = Registry::open(dir.path(), 1, 1, Arc::new(SystemClock)).unwrap();
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
        let reopened = Registry::open(dir.path(), 1, 1, Arc::new(SystemClock)).unwrap();
        assert!(!reopened.snapshot().regions[0].auto_refresh);
        assert!(reopened.due_for_refresh().is_empty());
        reopened.change_settings("a", Some(true), None).unwrap();
        assert_eq!(reopened.due_for_refresh().len(), 1);
    }

    #[tokio::test]
    async fn a_region_can_have_its_own_interval() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path(), 1, 7, Arc::new(SystemClock)).unwrap();
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

        reg.change_settings("a", None, Some(Some(30))).unwrap();
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
        let narrow = Registry::open(dir.path(), 1, 7, Arc::new(SystemClock))
            .unwrap()
            .with_refresh_range(1, 14);
        let snap = narrow.snapshot();
        assert_eq!((snap.min_refresh_days, snap.max_refresh_days), (1, 14));
        assert_eq!(snap.regions[0].refresh_days, Some(14));
        assert_eq!(narrow.due_for_refresh().len(), 0, "14 days have not passed");
        drop(narrow);
        // A default interval of 0 still turns every scheduled refresh off.
        let off = Registry::open(dir.path(), 1, 0, Arc::new(SystemClock)).unwrap();
        assert_eq!(off.snapshot().regions[0].next_refresh_unix, None);
    }

    #[tokio::test]
    async fn a_cancel_does_not_move_the_failure_time() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(dir.path());
        reg.enqueue("a", "A").unwrap();
        let (id, job) = reg.next_job().await;
        reg.finish(&id, job, Some(Ok(imported(dir.path(), "a-1"))));
        reg.enqueue("a", "A").unwrap();
        let (id, job) = reg.next_job().await;
        reg.finish(&id, job, Some(Err("gone".into())));
        // Pretend the failure was long ago.
        reg.lock()
            .regions
            .get_mut("a")
            .unwrap()
            .record
            .last_error_unix = Some(1);
        reg.enqueue("a", "A").unwrap();
        reg.cancel("a").unwrap();
        let r = &reg.snapshot().regions[0];
        assert_eq!(r.last_error.as_deref(), Some("gone"));
        assert_eq!(r.last_error_unix, Some(1), "still when it failed");
        assert!(
            r.last_attempt_unix.is_some_and(|t| t > 1),
            "the cancel is the last attempt"
        );

        // A successful import clears both.
        reg.enqueue("a", "A").unwrap();
        let (id, job) = reg.next_job().await;
        reg.finish(&id, job, Some(Ok(imported(dir.path(), "a-2"))));
        let r = &reg.snapshot().regions[0];
        assert_eq!((r.last_error.as_deref(), r.last_error_unix), (None, None));
    }

    #[tokio::test]
    async fn an_interval_can_be_cleared_back_to_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path(), 1, 7, Arc::new(SystemClock)).unwrap();
        reg.enqueue("a", "A").unwrap();
        let (id, job) = reg.next_job().await;
        reg.finish(&id, job, Some(Ok(imported(dir.path(), "a-1"))));
        {
            let mut state = reg.lock();
            let r = &mut state.regions.get_mut("a").unwrap().record;
            r.imported_unix = Some(now_unix() - 10 * 86_400);
            r.last_attempt_unix = r.imported_unix;
        }
        reg.change_settings("a", None, Some(Some(30))).unwrap();
        assert!(reg.due_for_refresh().is_empty());
        reg.change_settings("a", Some(true), None).unwrap();
        assert_eq!(
            reg.snapshot().regions[0].refresh_days,
            Some(30),
            "absent: left alone"
        );
        reg.change_settings("a", None, Some(None)).unwrap();
        assert_eq!(reg.snapshot().regions[0].refresh_days, None);
        assert_eq!(reg.due_for_refresh().len(), 1, "back on the 7-day default");
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
        let err = Registry::open(dir.path(), 1, 7, Arc::new(SystemClock)).unwrap_err();
        assert!(err.to_string().contains("regions.json"));
        assert!(dir.path().join("db/a-1").exists());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("us/california"), "us__california");
    }
}
