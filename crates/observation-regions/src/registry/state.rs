//! The region state machine: actions, import jobs and their results.

use super::*;

impl Registry {
    /// Queues an import (or re-import) of `id`.
    pub fn enqueue(&self, id: &str, name: &str) -> Result<(), ActionError> {
        let mut s = self.lock();
        if s.regions.get(id).is_some_and(|r| r.job.is_some()) {
            return Err(ActionError::Busy);
        }
        let job = s.new_job(self.now_unix());
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
        region.record.last_attempt_unix = Some(self.now_unix());
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
    /// [`Registry::refresh_range`]) or, with `Some(None)`, returns the region
    /// to the agent's default. `None` leaves a setting as it is. A refresh
    /// already queued or running carries on (cancel it to stop it).
    pub fn change_settings(
        &self,
        id: &str,
        auto_refresh: Option<bool>,
        refresh_days: Option<Option<u32>>,
    ) -> Result<(), ActionError> {
        let mut s = self.lock();
        let r = &mut s.regions.get_mut(id).ok_or(ActionError::NotFound)?.record;
        let before = (r.auto_refresh, r.refresh_days);
        r.auto_refresh = auto_refresh.unwrap_or(r.auto_refresh);
        if let Some(days) = refresh_days {
            r.refresh_days = days;
        }
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
            view.since_unix = self.now_unix();
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
            region.record.last_attempt_unix = Some(self.now_unix());
            match result {
                Some(Ok(imported)) => {
                    retired = region.db.replace(Arc::clone(&imported.db));
                    let r = &mut region.record;
                    r.db_dir = Some(imported.db_dir);
                    r.bbox = imported.bbox;
                    r.data_timestamp = imported.data_timestamp;
                    r.imported_unix = Some(self.now_unix());
                    r.disk_bytes = Some(imported.disk_bytes);
                    r.last_error = None;
                    r.last_error_unix = None;
                    tracing::info!(region = id, "region ready");
                }
                // The region stays listed (with its old data, if any) so the error can be read.
                Some(Err(e)) => {
                    tracing::warn!(region = id, error = %e, "region import failed");
                    region.record.last_error = Some(e);
                    region.record.last_error_unix = Some(self.now_unix());
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
    pub fn covering(&self, bbox: &BBox) -> Option<Covering> {
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
            .and_then(|r| {
                Some(Covering {
                    id: r.record.id.clone(),
                    db: Arc::clone(r.db.as_ref()?),
                    data_timestamp: r.record.data_timestamp.clone(),
                })
            })
    }

    /// Regions due for a scheduled refresh (see [`next_refresh`]).
    pub fn due_for_refresh(&self) -> Vec<(String, String)> {
        let now = self.now_unix();
        self.lock()
            .regions
            .values()
            .filter(|r| next_refresh(r, self.refresh).is_some_and(|t| t < now))
            .map(|r| (r.record.id.clone(), r.record.name.clone()))
            .collect()
    }
}
