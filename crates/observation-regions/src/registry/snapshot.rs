//! The [`RegionsSnapshot`] published from the registry's state.

use observation_core::regions::{RegionView, RegionsSnapshot};

use super::{Refresh, Region, State};

/// A failed or cancelled scheduled refresh is not retried for this long.
const RETRY_REFRESH_AFTER: u64 = 24 * 3600;

/// When `r` is due for a scheduled refresh: its interval after its import,
/// or, when a later attempt failed or was cancelled, a day after that attempt.
/// `None` for a region that is not ready, has an import queued or running, or
/// has auto-refresh off (as have all with the agent's default interval 0,
/// whatever their own).
pub(super) fn next_refresh(r: &Region, refresh: Refresh) -> Option<u64> {
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

pub(super) fn build_snapshot(
    s: &State,
    max_download_bytes: u64,
    refresh: Refresh,
) -> RegionsSnapshot {
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
                last_error_unix: r.record.last_error_unix,
            })
            .collect(),
        max_download_bytes,
        refresh_days: refresh.days,
        min_refresh_days: refresh.min_days,
        max_refresh_days: refresh.max_days,
    }
}
