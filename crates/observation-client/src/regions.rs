//! Local map-data regions: the server's live status (server-sent events), the
//! extract catalog, and import / cancel / delete requests from the UI.

use bevy::prelude::*;
use observation_core::regions::{ActionKind, CatalogEntry, RegionView, RegionsStatus};

use crate::state::UiState;

/// `sessionStorage` key of the admin token (kept for the browser session only).
const TOKEN_KEY: &str = "observation.admin-token";

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
    /// Admin token typed into the dialog.
    pub token: String,
    /// Region with an action in flight (its buttons are disabled).
    pub busy: Option<String>,
    /// Region whose Delete button was pressed once (asks to confirm).
    pub confirm_delete: Option<String>,
    pub search: String,
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

    pub fn request(&mut self, r: RegionRequest) {
        if let RegionRequest::Action(_, id) = &r {
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
