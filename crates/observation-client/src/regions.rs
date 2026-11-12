//! Local map-data regions: the server's live status (server-sent events), the
//! extract catalog, and import / cancel / delete / settings requests from the UI.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use observation_core::regions::{
    ActionKind, CatalogEntry, RegionAction, RegionView, RegionsStatus,
};

use crate::platform::KeyValueStore;
use observation_client_model::toasts::Toasts;

/// `sessionStorage` key of the admin token (kept for the browser session only).
const TOKEN_KEY: &str = "observation.admin-token";
/// An extract whose size came back unknown is asked about again after this long.
const SIZE_RETRY_SECS: u64 = 60;

/// The importable extracts, fetched when the dialog first needs them.
#[derive(Debug, Default)]
pub enum CatalogState {
    #[default]
    NotLoaded,
    Loading,
    Loaded(Vec<CatalogEntry>),
    Failed(String),
}

/// Something the UI asked the network layer to do.
#[derive(Debug, Clone)]
pub enum RegionRequest {
    Catalog,
    Action(ActionKind, String),
    /// Change a region's refresh settings (`auto_refresh`, `refresh_days`).
    Settings(RegionAction),
    /// Download sizes of these catalog extracts.
    Sizes(Vec<String>),
}

#[derive(Debug, Default, Resource)]
pub struct Regions {
    /// The server has a region agent (from `/api/config`).
    pub enabled: bool,
    /// Latest status from the event stream.
    pub status: Option<RegionsStatus>,
    /// The event stream is connected (it reconnects by itself when not).
    pub live: bool,
    pub catalog: CatalogState,
    /// Known download sizes of catalog extracts.
    pub sizes: HashMap<String, u64>,
    /// Extracts whose size was asked for and not answered yet.
    pub sizes_pending: HashSet<String>,
    /// Extracts whose size came back unknown, with when (Unix seconds) to ask again.
    sizes_retry: HashMap<String, u64>,
    /// Admin token typed into the dialog.
    pub token: String,
    /// Region with an action in flight (its buttons are disabled).
    pub busy: Option<String>,
    /// Region whose Delete button was pressed once (asks to confirm).
    pub confirm_delete: Option<String>,
    pub search: String,
    /// A refresh interval being dragged or typed: region id and days. Sent
    /// when the edit ends, not on every change.
    pub refresh_edit: Option<(String, u32)>,
    /// Requests for the network layer, drained every frame.
    pub outbox: Vec<RegionRequest>,
}

impl Regions {
    pub fn regions(&self) -> &[RegionView] {
        self.status.as_ref().map_or(&[], |s| &s.snapshot.regions)
    }

    /// The first region with a running or queued import.
    pub fn active_job(&self) -> Option<&RegionView> {
        let r = self.regions();
        r.iter()
            .find(|r| {
                r.job
                    .as_ref()
                    .is_some_and(|j| j.phase != observation_core::regions::JobPhase::Queued)
            })
            .or_else(|| r.iter().find(|r| r.job.is_some()))
    }

    pub fn manageable(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.manageable)
    }

    /// Loads the token remembered for this session.
    pub fn restore_token(&mut self, session: &dyn KeyValueStore) {
        if let Some(t) = session.get(TOKEN_KEY) {
            self.token = t;
        }
    }

    /// Remembers the token for this session (best effort: a refused write
    /// only means it is asked for again); an empty token is forgotten.
    pub fn store_token(&self, session: &dyn KeyValueStore) {
        if self.token.is_empty() {
            session.remove(TOKEN_KEY);
        } else {
            let _ = session.set(TOKEN_KEY, &self.token);
        }
    }

    /// Asks for the sizes of those of `ids` that are not known, not asked for
    /// already, and not found unknown within the last [`SIZE_RETRY_SECS`].
    pub fn want_sizes<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>, now: u64) {
        let new: Vec<String> = ids
            .into_iter()
            .filter(|id| {
                !self.sizes.contains_key(*id)
                    && !self.sizes_pending.contains(*id)
                    && self.sizes_retry.get(*id).is_none_or(|&at| now >= at)
            })
            .map(str::to_owned)
            .collect();
        if !new.is_empty() {
            self.sizes_pending.extend(new.iter().cloned());
            self.outbox.push(RegionRequest::Sizes(new));
        }
    }

    /// Takes in a sizes answer for `asked` (`None`: the request failed).
    /// Sizes still unknown (the answer says `null` or leaves the extract out,
    /// or the request failed) are asked again after [`SIZE_RETRY_SECS`].
    pub fn take_sizes(
        &mut self,
        asked: &[String],
        answer: Option<HashMap<String, Option<u64>>>,
        now: u64,
    ) {
        for id in asked {
            self.sizes_pending.remove(id);
            match answer.as_ref().and_then(|a| a.get(id).copied().flatten()) {
                Some(bytes) => {
                    self.sizes.insert(id.clone(), bytes);
                    self.sizes_retry.remove(id);
                }
                None => {
                    self.sizes_retry.insert(id.clone(), now + SIZE_RETRY_SECS);
                }
            }
        }
    }

    pub fn request(&mut self, r: RegionRequest) {
        if let RegionRequest::Action(_, id) | RegionRequest::Settings(RegionAction { id, .. }) = &r
        {
            self.busy = Some(id.clone());
        }
        self.outbox.push(r);
    }

    /// Takes in a new status; returns the imports it shows just ended, in
    /// region order.
    #[must_use]
    pub fn update(&mut self, status: RegionsStatus) -> Vec<RegionEvent> {
        let mut events = Vec::new();
        if let Some(old) = &self.status {
            for new in &status.snapshot.regions {
                let Some(before) = old.snapshot.regions.iter().find(|r| r.id == new.id) else {
                    continue;
                };
                if before.job.is_none() || new.job.is_some() {
                    continue;
                }
                match (&new.last_error, before.last_error == new.last_error) {
                    (Some(e), false) => events.push(RegionEvent::Failed {
                        name: new.name.clone(),
                        error: e.clone(),
                    }),
                    (None, _) if new.ready && new.imported_unix != before.imported_unix => {
                        events.push(RegionEvent::Ready {
                            name: new.name.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
        self.status = Some(status);
        events
    }
}

/// An import that a new status shows just ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionEvent {
    /// It failed, with a new error.
    Failed { name: String, error: String },
    /// It finished, and the region's data is in use.
    Ready { name: String },
}

/// Tells the user about `events`, in order: a failure stays until dismissed.
pub fn announce(events: Vec<RegionEvent>, toasts: &mut Toasts) {
    for event in events {
        match event {
            RegionEvent::Failed { name, error } => {
                toasts.error_sticky(format!("Importing {name} failed: {error}"));
            }
            RegionEvent::Ready { name } => {
                toasts.info(format!(
                    "{name} is ready: areas there now load from this server"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_client_model::toasts::{INFO_SECS, ToastKind};

    fn asked(r: &Regions) -> Vec<Vec<String>> {
        r.outbox
            .iter()
            .filter_map(|q| match q {
                RegionRequest::Sizes(ids) => Some(ids.clone()),
                _ => None,
            })
            .collect()
    }

    /// One region of a test status: id, name, importing, ready, imported at,
    /// last error.
    type Row<'a> = (&'a str, &'a str, bool, bool, Option<u64>, Option<&'a str>);

    fn status(regions: &[Row<'_>]) -> RegionsStatus {
        let regions = regions
            .iter()
            .map(|&(id, name, importing, ready, imported, error)| {
                serde_json::json!({
                    "id": id, "name": name, "ready": ready, "bbox": null,
                    "data_timestamp": null, "imported_unix": imported, "disk_bytes": null,
                    "job": importing.then(|| serde_json::json!(
                        {"phase": "importing", "done": 0, "total": null, "since_unix": 0}
                    )),
                    "last_error": error,
                })
            })
            .collect::<Vec<_>>();
        serde_json::from_value(serde_json::json!({
            "agent_online": true, "manageable": true,
            "snapshot": {"version": 1, "regions": regions, "max_download_bytes": 0, "refresh_days": 0},
        }))
        .expect("a valid status")
    }

    #[test]
    fn a_status_reports_the_imports_that_just_ended_in_region_order() {
        let mut r = Regions::default();
        let first = status(&[
            ("a", "Alpha", true, false, None, None),
            ("b", "Beta", true, true, Some(1), None),
            ("c", "Gamma", true, false, None, Some("old")),
            ("d", "Delta", false, false, None, Some("old")),
        ]);
        assert!(
            r.update(first.clone()).is_empty(),
            "nothing to compare with"
        );
        assert_eq!(r.status.as_ref(), Some(&first));
        let events = r.update(status(&[
            ("a", "Alpha", false, false, None, Some("disk full")),
            ("b", "Beta", false, true, Some(2), None),
            ("c", "Gamma", false, false, None, Some("old")),
            ("d", "Delta", false, false, None, Some("new")),
            ("e", "Epsilon", false, true, Some(3), None),
        ]));
        assert_eq!(
            events,
            [
                RegionEvent::Failed {
                    name: "Alpha".into(),
                    error: "disk full".into()
                },
                RegionEvent::Ready {
                    name: "Beta".into()
                },
            ],
            "a repeated error, a region with no import running and a new region say nothing"
        );
        let mut toasts = Toasts::default();
        announce(events, &mut toasts);
        let shown: Vec<_> = toasts
            .iter()
            .map(|t| (t.text.as_str(), t.kind, t.ttl))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "Importing Alpha failed: disk full",
                    ToastKind::Error,
                    f32::INFINITY
                ),
                (
                    "Beta is ready: areas there now load from this server",
                    ToastKind::Info,
                    INFO_SECS
                ),
            ]
        );
    }

    #[test]
    fn unknown_sizes_are_asked_again_a_minute_later() {
        let mut r = Regions::default();
        r.want_sizes(["a", "b"], 0);
        r.want_sizes(["a", "b"], 0);
        assert_eq!(
            asked(&r),
            vec![vec!["a".to_owned(), "b".to_owned()]],
            "once while pending"
        );
        r.outbox.clear();
        let answer = HashMap::from([("a".to_owned(), Some(5)), ("b".to_owned(), None)]);
        r.take_sizes(&["a".into(), "b".into()], Some(answer), 100);
        assert_eq!(r.sizes.get("a"), Some(&5));
        r.want_sizes(["a", "b"], 100 + SIZE_RETRY_SECS - 1);
        assert!(asked(&r).is_empty(), "known, or unknown just now");
        r.want_sizes(["a", "b"], 100 + SIZE_RETRY_SECS);
        assert_eq!(asked(&r), vec![vec!["b".to_owned()]]);
    }
}
