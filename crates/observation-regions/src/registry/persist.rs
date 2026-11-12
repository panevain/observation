//! `regions.json`: the region records kept across restarts, and loading them.

use std::path::Path;
use std::sync::{Arc, Mutex};

use observation_core::geo::BBox;
use observation_core::regions::{DEFAULT_MAX_REFRESH_DAYS, DEFAULT_MIN_REFRESH_DAYS};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Notify, watch};

use super::snapshot::build_snapshot;
use super::{Refresh, Region, RegionDb, Registry, State};
use crate::poly::Boundary;

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
    /// When `last_error` happened (Unix seconds).
    #[serde(default)]
    pub last_error_unix: Option<u64>,
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
}
