//! Locally imported map-data regions: the JSON shared by the region agent, the
//! server and the client.
//!
//! A region is one Geofabrik extract (a country, a state, a city) that the
//! region agent has downloaded, filtered to buildings and roads, and imported
//! into its own Overpass database. Areas inside a ready region are built from it
//! instead of the public Overpass servers.

use serde::{Deserialize, Serialize};

use crate::geo::BBox;

/// Everything the agent knows about its regions. `version` increases on every
/// change, so watchers can wait for the next one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegionsSnapshot {
    pub version: u64,
    pub regions: Vec<RegionView>,
    /// Largest extract the agent will download, bytes.
    pub max_download_bytes: u64,
    /// Ready regions with [`RegionView::auto_refresh`] on are re-imported
    /// after this many days; 0 means never.
    pub refresh_days: u32,
}

/// What the server tells the client: the agent's snapshot, whether the agent
/// is reachable, and whether region management is turned on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegionsStatus {
    /// The server reached the region agent on its last try.
    pub agent_online: bool,
    /// The server accepts management requests (with the admin token).
    pub manageable: bool,
    pub snapshot: RegionsSnapshot,
}

/// One region: its data (if imported) and the job working on it (if any).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionView {
    /// Geofabrik extract id, e.g. `monaco` or `us/california`.
    pub id: String,
    /// Display name, e.g. `Monaco`.
    pub name: String,
    /// Data is imported and queries are answered from it (also while a refresh runs).
    pub ready: bool,
    /// Bounds of the imported data.
    pub bbox: Option<BBox>,
    /// OSM replication timestamp of the imported data (RFC 3339).
    pub data_timestamp: Option<String>,
    /// When the data was imported, Unix seconds.
    pub imported_unix: Option<u64>,
    /// When the last import ended, however it ended (Unix seconds).
    #[serde(default)]
    pub last_attempt_unix: Option<u64>,
    /// The agent re-imports the region every [`RegionsSnapshot::refresh_days`].
    #[serde(default = "yes")]
    pub auto_refresh: bool,
    /// When the next scheduled refresh is due, Unix seconds (the agent checks
    /// hourly). `None` when none is planned: not ready, auto-refresh off for
    /// the region or the agent, or an import already queued.
    #[serde(default)]
    pub next_refresh_unix: Option<u64>,
    /// Size of the region's database on disk, bytes.
    pub disk_bytes: Option<u64>,
    /// Queued or running import.
    pub job: Option<JobView>,
    /// Why the last import failed.
    pub last_error: Option<String>,
}

fn yes() -> bool {
    true
}

/// Progress of an import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobView {
    pub phase: JobPhase,
    /// Work done in this phase (bytes downloaded, elements imported).
    pub done: u64,
    /// Total work in this phase, when known.
    pub total: Option<u64>,
    /// When the job started (or was queued), Unix seconds.
    pub since_unix: u64,
}

impl JobView {
    /// Fraction of the whole import, `0..=1`, weighting the phases by their
    /// typical share of the run time.
    pub fn overall(&self) -> f32 {
        let (start, span) = self.phase.span();
        let within = match self.total {
            Some(total) if total > 0 => (self.done as f64 / total as f64).min(1.0) as f32,
            _ => 0.0,
        };
        start + span * within
    }
}

/// Steps of an import, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    /// Waiting for another import to finish.
    Queued,
    /// Fetching the extract and its boundary.
    Downloading,
    /// Keeping only buildings and roads.
    Filtering,
    /// Loading the filtered data into a new Overpass database.
    Importing,
    /// Swapping the new database in.
    Finishing,
}

impl JobPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Downloading => "Downloading",
            Self::Filtering => "Filtering",
            Self::Importing => "Importing",
            Self::Finishing => "Finishing",
        }
    }

    /// Start and width of this phase in [`JobView::overall`].
    fn span(self) -> (f32, f32) {
        match self {
            Self::Queued => (0.0, 0.0),
            Self::Downloading => (0.0, 0.4),
            Self::Filtering => (0.4, 0.1),
            Self::Importing => (0.5, 0.45),
            Self::Finishing => (0.95, 0.05),
        }
    }
}

/// An extract that can be imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    /// Id of the enclosing extract (`europe` for `monaco`).
    pub parent: Option<String>,
}

/// A region management action, named by the last part of its route
/// (`/api/regions/import`, agent `/regions/import`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Import,
    Cancel,
    Delete,
    /// Changes the region's settings ([`RegionAction::auto_refresh`]).
    Settings,
}

impl ActionKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "import" => Some(Self::Import),
            "cancel" => Some(Self::Cancel),
            "delete" => Some(Self::Delete),
            "settings" => Some(Self::Settings),
            _ => None,
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Cancel => "cancel",
            Self::Delete => "delete",
            Self::Settings => "settings",
        }
    }

    /// What the action does to a region, for messages: `Could not {verb} monaco`.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Settings => "change the settings of",
            other => other.path(),
        }
    }
}

/// Whether an `Authorization` header value is `Bearer <secret>`, compared in
/// time independent of where the token differs.
pub fn bearer_matches(header: Option<&str>, secret: &str) -> bool {
    let Some(given) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    given.len() == secret.len()
        && given
            .bytes()
            .zip(secret.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Body of the region management requests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionAction {
    pub id: String,
    /// For [`ActionKind::Settings`]: turn scheduled refreshes on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_refresh: Option<bool>,
}

/// Whether `id` looks like a Geofabrik extract id: lowercase ASCII letters,
/// digits, `-`, and `/` between non-empty parts. Ids are also checked against
/// the catalog; this only keeps junk out of logs and file names.
pub fn is_valid_region_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id.split('/').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_ids() {
        for ok in ["monaco", "us/california", "isle-of-man", "us/new-york"] {
            assert!(is_valid_region_id(ok), "{ok}");
        }
        for bad in [
            "", "/", "us/", "/us", "us//ca", "../x", "Monaco", "a b", "a.b",
        ] {
            assert!(!is_valid_region_id(bad), "{bad}");
        }
    }

    #[test]
    fn bearer_tokens() {
        assert!(bearer_matches(Some("Bearer s3cret"), "s3cret"));
        for bad in [
            None,
            Some("s3cret"),
            Some("Bearer s3creT"),
            Some("Bearer s3cret2"),
            Some("Basic s3cret"),
        ] {
            assert!(!bearer_matches(bad, "s3cret"), "{bad:?}");
        }
    }

    #[test]
    fn actions_round_trip() {
        for a in [
            ActionKind::Import,
            ActionKind::Cancel,
            ActionKind::Delete,
            ActionKind::Settings,
        ] {
            assert_eq!(ActionKind::parse(a.path()), Some(a));
        }
        assert_eq!(ActionKind::parse("nuke"), None);
    }

    #[test]
    fn overall_progress_is_monotonic_across_phases() {
        let at = |phase, done, total| {
            JobView {
                phase,
                done,
                total,
                since_unix: 0,
            }
            .overall()
        };
        assert_eq!(at(JobPhase::Queued, 0, None), 0.0);
        assert!((at(JobPhase::Downloading, 50, Some(100)) - 0.2).abs() < 1e-6);
        assert_eq!(
            at(JobPhase::Downloading, 100, Some(100)),
            at(JobPhase::Filtering, 0, None)
        );
        assert!(at(JobPhase::Importing, 1, Some(2)) > at(JobPhase::Filtering, 0, None));
        // Overshoot (more elements than counted) stays inside the phase.
        assert!(at(JobPhase::Importing, 9, Some(2)) <= at(JobPhase::Finishing, 0, None));
    }

    #[test]
    fn snapshot_json_round_trip() {
        let s = RegionsSnapshot {
            version: 3,
            regions: vec![RegionView {
                id: "monaco".into(),
                name: "Monaco".into(),
                ready: true,
                bbox: BBox::new(43.72, 7.40, 43.75, 7.44).ok(),
                data_timestamp: Some("2026-10-19T20:21:11Z".into()),
                imported_unix: Some(1),
                last_attempt_unix: Some(1),
                auto_refresh: false,
                next_refresh_unix: None,
                disk_bytes: Some(2),
                job: Some(JobView {
                    phase: JobPhase::Importing,
                    done: 1,
                    total: Some(2),
                    since_unix: 3,
                }),
                last_error: None,
            }],
            max_download_bytes: 10,
            refresh_days: 7,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"phase\":\"importing\""));
        assert_eq!(serde_json::from_str::<RegionsSnapshot>(&json).unwrap(), s);
    }

    #[test]
    fn regions_from_an_older_agent_refresh_automatically() {
        let v: RegionView = serde_json::from_str(
            r#"{"id":"a","name":"A","ready":true,"bbox":null,"data_timestamp":null,
                "imported_unix":1,"disk_bytes":null,"job":null,"last_error":null}"#,
        )
        .unwrap();
        assert!(v.auto_refresh);
        assert_eq!(v.next_refresh_unix, None);
    }

    #[test]
    fn plain_actions_carry_only_the_id() {
        let plain = RegionAction {
            id: "monaco".into(),
            ..RegionAction::default()
        };
        assert_eq!(serde_json::to_string(&plain).unwrap(), r#"{"id":"monaco"}"#);
        let a: RegionAction =
            serde_json::from_str(r#"{"id":"monaco","auto_refresh":false}"#).unwrap();
        assert_eq!(a.auto_refresh, Some(false));
    }
}
