//! The agent bridge's contract: what the browser, `observation-server` and a
//! local MCP process (`observation-mcp`) send each other.
//!
//! The server relays between one open app and one agent. The browser
//! creates a **pairing** and shows its short code; the user gives the code
//! to the agent, which claims the pairing with it. From then on the browser
//! posts its state (the open scene, the loaded area, the selection and a
//! point the user shared) and the agent reads it, waiting for changes with a
//! long poll. The browser follows the pairing over server-sent events.
//!
//! ```text
//! browser                              server                              agent (MCP)
//!   POST /api/agent/pairings ───────▶  code + browser token
//!   GET  /api/agent/events?token= ──▶  event: agent  {status: waiting}
//!                                        ◀──────────────── POST /api/agent/claim {code}
//!                                      agent token ─────────────────────▶
//!   ◀── event: agent {status: claimed}
//!   PUT  /api/agent/state (bearer) ─▶  revision n
//!                                        ◀──────────────── GET /api/agent/state?since=n-1 (bearer)
//!                                      {revision n, state} ─────────────▶
//!   DELETE /api/agent/pairing ──────▶  ◀────────────────── DELETE /api/agent/pairing
//!   ◀── event: agent {ended: browser}  {ended: browser} to a waiting poll ▶
//! ```
//!
//! Every body is JSON and carries `v`, the protocol version
//! ([`PROTOCOL_VERSION`]). New fields are added with defaults, so a reader
//! ignores what it does not know; `v` changes only when a field changes its
//! meaning or a reader must refuse a body. Browser events are one envelope
//! ([`BrowserEvent`]) whose `type` names the kind: a reader skips a type it
//! does not know ([`BrowserEventKind::Unknown`]), which is how later kinds
//! (the agent's proposals) can be added without breaking older clients.
//!
//! The crate holds only types and constants, with no I/O, so the server, the
//! MCP process and the browser client (in WebAssembly) share them.

use observation_core::scene::AreaRef;
use serde::{Deserialize, Serialize};

/// The protocol version every body carries as `v`.
pub const PROTOCOL_VERSION: u32 = 1;

/// The bridge's routes on the server. Each answers only requests from the
/// same computer (a loopback peer, addressed to a loopback host).
pub mod paths {
    /// `POST` [`CreatePairing`](super::CreatePairing) (browser): a new
    /// pairing, answered with [`PairingCreated`](super::PairingCreated).
    pub const PAIRINGS: &str = "/api/agent/pairings";
    /// `GET` (browser): the pairing's server-sent events, each an
    /// [`EVENT`] message whose data is a [`BrowserEvent`](super::BrowserEvent).
    /// The browser token goes in the [`TOKEN_PARAM`] query parameter, since
    /// `EventSource` cannot send headers.
    pub const EVENTS: &str = "/api/agent/events";
    /// `PUT` [`StatePost`](super::StatePost) (browser, bearer browser
    /// token): the app's state, answered with
    /// [`StateAccepted`](super::StateAccepted). `GET` (agent, bearer agent
    /// token): the [`StateView`](super::StateView); with `?since=<revision>`
    /// it waits up to [`LONG_POLL_SECS`](super::LONG_POLL_SECS) for a newer
    /// revision.
    pub const STATE: &str = "/api/agent/state";
    /// `POST` [`ClaimRequest`](super::ClaimRequest) (agent): claims the
    /// pairing with a code, answered with [`Claimed`](super::Claimed).
    pub const CLAIM: &str = "/api/agent/claim";
    /// `DELETE` (either side, bearer its own token): ends the pairing.
    pub const PAIRING: &str = "/api/agent/pairing";
    /// The `event` name of every server-sent event on [`EVENTS`].
    pub const EVENT: &str = "agent";
    /// The query parameter that carries the browser token on [`EVENTS`].
    pub const TOKEN_PARAM: &str = "token";
}

/// Characters in a pairing code (40 bits from [`CODE_ALPHABET`]).
pub const CODE_LEN: usize = 8;
/// Crockford's base32: no I, L, O or U, so a code read aloud or typed is
/// hard to get wrong ([`normalise_code`] maps the look-alikes back).
pub const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Hex digits in a token (32 random bytes).
pub const TOKEN_HEX_LEN: usize = 64;
/// Largest [`StatePost`] body accepted, bytes: a scene with 256 cameras and
/// 64 custom models with room to spare.
pub const MAX_STATE_BYTES: usize = 1 << 20;
/// Longest a `GET` of [`paths::STATE`] with `since` waits for a change, seconds.
pub const LONG_POLL_SECS: u64 = 25;
/// An unclaimed code expires after this long, seconds.
pub const CODE_TTL_SECS: u64 = 600;
/// A pairing neither side has used for this long ends, seconds. The agent's
/// long polls and the browser's posts count; an open event stream does not.
pub const IDLE_TIMEOUT_SECS: u64 = 1800;
/// A pairing whose browser has had no event stream open for this long ends
/// (the tab was closed), seconds. Covers the browser's reconnects.
pub const BROWSER_GONE_SECS: u64 = 60;
/// Most pairings a server holds at once.
pub const MAX_PAIRINGS: usize = 8;
/// Wrong codes the server accepts before it burns every unclaimed code.
pub const MAX_FAILED_CLAIMS: u32 = 5;
/// Longest agent name kept, characters.
pub const MAX_AGENT_NAME_CHARS: usize = 64;
/// Longest scene name kept, characters (as the app's list of scenes).
pub const MAX_SCENE_NAME_CHARS: usize = 60;
/// Most selected cameras kept (a scene has at most 256).
pub const MAX_SELECTION: usize = 256;

/// A code as a person typed it, in the form the server issues: upper case,
/// without spaces or hyphens, with `I` and `L` read as `1` and `O` as `0`.
/// `None` unless that leaves [`CODE_LEN`] characters of [`CODE_ALPHABET`].
pub fn normalise_code(raw: &str) -> Option<String> {
    let code: String = raw
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '\t'))
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            c => c,
        })
        .collect();
    (code.len() == CODE_LEN && code.bytes().all(|b| CODE_ALPHABET.contains(&b))).then_some(code)
}

/// A code as the app shows it, in two groups of four: `ABCD-EFGH`.
pub fn display_code(code: &str) -> String {
    if code.len() == CODE_LEN && code.is_ascii() {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_owned()
    }
}

/// Whether a body's `v` is one this build reads.
pub fn version_ok(v: u32) -> bool {
    v == PROTOCOL_VERSION
}

/// `text` trimmed and cut to `max` characters, trimmed again; `None` if
/// nothing is left. Names are drawn as text, never as markup.
pub fn tidy_text(text: &str, max: usize) -> Option<String> {
    let cut: String = text.trim().chars().take(max).collect();
    let cut = cut.trim();
    (!cut.is_empty()).then(|| cut.to_owned())
}

/// Body of `POST` [`paths::PAIRINGS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePairing {
    pub v: u32,
}

impl Default for CreatePairing {
    fn default() -> Self {
        Self {
            v: PROTOCOL_VERSION,
        }
    }
}

/// A new pairing, for the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCreated {
    pub v: u32,
    /// The code the user gives the agent ([`CODE_LEN`] characters of
    /// [`CODE_ALPHABET`]; show it with [`display_code`]).
    pub code: String,
    /// The browser's secret for this pairing. Never shown or logged.
    pub browser_token: String,
    /// Seconds until the code expires unclaimed.
    pub code_expires_in_s: u64,
    /// Seconds without use after which the pairing ends.
    pub idle_timeout_s: u64,
}

/// Body of `POST` [`paths::CLAIM`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub v: u32,
    /// The code as the user typed it (see [`normalise_code`]).
    pub code: String,
    /// What the agent calls itself, shown in the app (cut to
    /// [`MAX_AGENT_NAME_CHARS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// A claimed pairing, for the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claimed {
    pub v: u32,
    /// The agent's secret for this pairing. Never shown or logged.
    pub agent_token: String,
    /// Seconds without use after which the pairing ends.
    pub idle_timeout_s: u64,
    /// Longest a long poll waits, seconds.
    pub long_poll_s: u64,
}

/// A point the user shared with the agent, in the scene's world frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PinnedPoint {
    /// Metres east of the area's origin.
    pub x: f32,
    /// Metres south of the area's origin.
    pub z: f32,
    /// Its latitude and longitude, when the app knows them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    /// Counts the points shared in this pairing, so sharing the same spot
    /// twice is still a change.
    #[serde(default)]
    pub seq: u64,
}

/// What the browser shares: the open scene and what the user is looking at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedState {
    /// The open scene, as the app exports it (a scene file). The server
    /// passes it on unread; the agent checks it as an imported file.
    pub scene: serde_json::Value,
    /// The open scene's name in the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_name: Option<String>,
    /// The loaded area; `None` while none has loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<AreaRef>,
    /// The selected cameras' ids.
    #[serde(default)]
    pub selection: Vec<u32>,
    /// The point the user shared last, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<PinnedPoint>,
}

impl SharedState {
    /// Cuts what a reader shows or keeps to its limits: the scene name to
    /// [`MAX_SCENE_NAME_CHARS`], the selection to [`MAX_SELECTION`], and a
    /// pinned point with a coordinate that is not finite is dropped.
    pub fn tidy(&mut self) {
        self.scene_name = self
            .scene_name
            .as_deref()
            .and_then(|n| tidy_text(n, MAX_SCENE_NAME_CHARS));
        self.selection.truncate(MAX_SELECTION);
        if self.pinned.is_some_and(|p| {
            !(p.x.is_finite()
                && p.z.is_finite()
                && p.lat.is_none_or(f64::is_finite)
                && p.lon.is_none_or(f64::is_finite))
        }) {
            self.pinned = None;
        }
    }
}

/// Body of `PUT` [`paths::STATE`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatePost {
    pub v: u32,
    pub state: SharedState,
}

/// The server took a [`StatePost`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateAccepted {
    pub v: u32,
    /// The revision the state now has.
    pub revision: u64,
}

/// The state as the agent reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateView {
    pub v: u32,
    /// Starts at 0 (nothing posted yet) and grows with every post.
    pub revision: u64,
    /// The latest state; `None` until the browser has posted one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SharedState>,
    /// Set when the pairing ended while the agent was waiting; later
    /// requests with the token are refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<EndReason>,
}

/// Where a pairing stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingPhase {
    /// Waiting for the agent to claim the code.
    Waiting,
    /// An agent holds it.
    Claimed,
}

/// Why a pairing ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// The browser ended it (Disconnect).
    Browser,
    /// The agent ended it (`disconnect_app`).
    Agent,
    /// Neither side used it for [`IDLE_TIMEOUT_SECS`].
    Idle,
    /// The code was not claimed within [`CODE_TTL_SECS`].
    CodeExpired,
    /// Too many wrong codes were tried ([`MAX_FAILED_CLAIMS`]), so every
    /// unclaimed code was burnt.
    CodeBurnt,
    /// The browser had no event stream open for [`BROWSER_GONE_SECS`].
    BrowserGone,
    /// The server is shutting down.
    Shutdown,
    /// A reason this build does not know.
    #[serde(other)]
    Other,
}

impl EndReason {
    /// The reason in words, for the app and the agent.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Browser => "the app disconnected",
            Self::Agent => "the agent disconnected",
            Self::Idle => "neither side used the connection for 30 minutes",
            Self::CodeExpired => "the code was not used within 10 minutes",
            Self::CodeBurnt => "too many wrong codes were tried, so the code was withdrawn",
            Self::BrowserGone => "the app's tab was closed or lost its connection",
            Self::Shutdown => "the server stopped",
            Self::Other => "the connection ended",
        }
    }
}

/// One server-sent event to the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserEvent {
    pub v: u32,
    #[serde(flatten)]
    pub kind: BrowserEventKind,
}

impl BrowserEvent {
    pub fn new(kind: BrowserEventKind) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            kind,
        }
    }
}

/// What a [`BrowserEvent`] says, by its `type`.
///
/// Part 3 of the feature (proposals: the agent sends a whole change set and
/// the user accepts or rejects it) adds a kind here; older clients read it
/// as [`Self::Unknown`] and skip it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserEventKind {
    /// Sent when the stream opens and whenever the pairing changes phase.
    Status {
        phase: PairingPhase,
        /// The agent's name, once claimed (if it gave one).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// The pairing ended; the stream closes after this.
    Ended { reason: EndReason },
    /// A kind this build does not know.
    #[serde(other)]
    Unknown,
}

/// The body of every error answer: `{"error": "..."}`, worded for a person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use observation_core::geo::BBox;
    use serde_json::json;

    #[test]
    fn the_routes_are_these() {
        assert_eq!(
            [
                paths::PAIRINGS,
                paths::EVENTS,
                paths::STATE,
                paths::CLAIM,
                paths::PAIRING
            ],
            [
                "/api/agent/pairings",
                "/api/agent/events",
                "/api/agent/state",
                "/api/agent/claim",
                "/api/agent/pairing"
            ]
        );
        assert_eq!(paths::EVENT, "agent");
        assert_eq!(paths::TOKEN_PARAM, "token");
    }

    #[test]
    fn codes_are_read_as_people_type_them() {
        assert_eq!(normalise_code("ABCD-EFGH").as_deref(), Some("ABCDEFGH"));
        assert_eq!(normalise_code(" abcd efgh ").as_deref(), Some("ABCDEFGH"));
        // Look-alikes map to the digits Crockford's alphabet keeps.
        assert_eq!(normalise_code("iLoO-2345").as_deref(), Some("11002345"));
        for bad in [
            "",
            "ABCDEFG",
            "ABCDEFGHJ",
            "ABCD-EFGU",
            "ABCD/EFG",
            "ÄBCDEFGH",
        ] {
            assert_eq!(normalise_code(bad), None, "{bad:?}");
        }
        assert_eq!(display_code("ABCDEFGH"), "ABCD-EFGH");
        assert_eq!(display_code("short"), "short");
        assert_eq!(CODE_ALPHABET.len(), 32);
        assert!(!CODE_ALPHABET.iter().any(|b| b"ILOU".contains(b)));
    }

    #[test]
    fn events_are_one_envelope_and_unknown_kinds_are_skipped() {
        let claimed = BrowserEvent::new(BrowserEventKind::Status {
            phase: PairingPhase::Claimed,
            agent: Some("claude-code".into()),
        });
        let text = serde_json::to_string(&claimed).unwrap();
        assert_eq!(
            text,
            r#"{"v":1,"type":"status","phase":"claimed","agent":"claude-code"}"#
        );
        assert_eq!(
            serde_json::from_str::<BrowserEvent>(&text).unwrap(),
            claimed
        );
        let ended = BrowserEvent::new(BrowserEventKind::Ended {
            reason: EndReason::CodeBurnt,
        });
        assert_eq!(
            serde_json::to_string(&ended).unwrap(),
            r#"{"v":1,"type":"ended","reason":"code_burnt"}"#
        );
        // A later kind, and a later reason, still read.
        let later: BrowserEvent =
            serde_json::from_value(json!({"v": 1, "type": "proposal", "id": 3})).unwrap();
        assert_eq!(later.kind, BrowserEventKind::Unknown);
        let reason: BrowserEvent =
            serde_json::from_value(json!({"v": 1, "type": "ended", "reason": "moon"})).unwrap();
        assert_eq!(
            reason.kind,
            BrowserEventKind::Ended {
                reason: EndReason::Other
            }
        );
    }

    #[test]
    fn a_state_reads_with_only_the_scene_and_round_trips() {
        let minimal: StatePost =
            serde_json::from_value(json!({"v": 1, "state": {"scene": {"format": "x"}}})).unwrap();
        assert_eq!(minimal.state.selection, Vec::<u32>::new());
        assert_eq!(minimal.state.area, None);
        let full = StatePost {
            v: PROTOCOL_VERSION,
            state: SharedState {
                scene: json!({"format": "observation-scene"}),
                scene_name: Some("Home".into()),
                area: Some(AreaRef::Osm {
                    bbox: BBox::new(47.0, -122.0, 47.001, -121.999).unwrap(),
                }),
                selection: vec![3],
                pinned: Some(PinnedPoint {
                    x: 1.5,
                    z: -2.0,
                    lat: Some(47.0005),
                    lon: None,
                    seq: 1,
                }),
            },
        };
        let text = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<StatePost>(&text).unwrap(), full);
        assert!(
            text.contains(r#""area":{"kind":"osm","bbox":{"south":47.0"#),
            "{text}"
        );
    }

    #[test]
    fn tidying_cuts_names_and_drops_unusable_points() {
        let mut s = SharedState {
            scene: json!({}),
            scene_name: Some(format!("  {}  ", "x".repeat(100))),
            area: None,
            selection: (0..300).collect(),
            pinned: Some(PinnedPoint {
                x: f32::NAN,
                z: 0.0,
                lat: None,
                lon: None,
                seq: 0,
            }),
        };
        s.tidy();
        assert_eq!(s.scene_name.as_deref().map(str::len), Some(60));
        assert_eq!(s.selection.len(), MAX_SELECTION);
        assert_eq!(s.pinned, None);
        assert_eq!(tidy_text("   ", 5), None);
        assert_eq!(tidy_text(" ab  c ", 4).as_deref(), Some("ab"));
    }

    #[test]
    fn only_this_version_is_read() {
        assert!(version_ok(1));
        assert!(!version_ok(0) && !version_ok(2));
    }
}
