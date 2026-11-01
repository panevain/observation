//! Local map-data regions: the server's live status (server-sent events), the
//! extract catalog, and import / cancel / delete / settings requests from the UI.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use observation_core::regions::{
    ActionKind, CatalogEntry, RegionAction, RegionView, RegionsStatus,
};

use crate::state::UiState;

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
    pub fn restore_token(&mut self) {
        if let Some(t) = session_get(TOKEN_KEY) {
            self.token = t;
        }
    }

    /// Remembers the token for this session (best effort).
    pub fn store_token(&self) {
        session_set(TOKEN_KEY, &self.token);
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

    /// Takes in a new status and toasts imports that just finished.
    pub fn update(&mut self, status: RegionsStatus, ui: &mut UiState) {
        if let Some(old) = &self.status {
            for new in &status.snapshot.regions {
                let Some(before) = old.snapshot.regions.iter().find(|r| r.id == new.id) else {
                    continue;
                };
                if before.job.is_none() || new.job.is_some() {
                    continue;
                }
                match (&new.last_error, before.last_error == new.last_error) {
                    (Some(e), false) => {
                        ui.error_sticky(format!("Importing {} failed: {e}", new.name));
                    }
                    (None, _) if new.ready && new.imported_unix != before.imported_unix => {
                        ui.info(format!(
                            "{} is ready: areas there now load from this server",
                            new.name
                        ));
                    }
                    _ => {}
                }
            }
        }
        self.status = Some(status);
    }
}

/// Seconds since the Unix epoch by this computer's clock.
pub fn now_unix() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

#[cfg(target_arch = "wasm32")]
fn session_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.session_storage().ok().flatten()
}

#[cfg(target_arch = "wasm32")]
fn session_get(key: &str) -> Option<String> {
    session_storage()?.get_item(key).ok().flatten()
}

#[cfg(target_arch = "wasm32")]
fn session_set(key: &str, value: &str) {
    if let Some(s) = session_storage() {
        let _ = if value.is_empty() {
            s.remove_item(key)
        } else {
            s.set_item(key, value)
        };
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn session_get(_: &str) -> Option<String> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn session_set(_: &str, _: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(r: &Regions) -> Vec<Vec<String>> {
        r.outbox
            .iter()
            .filter_map(|q| match q {
                RegionRequest::Sizes(ids) => Some(ids.clone()),
                _ => None,
            })
            .collect()
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
