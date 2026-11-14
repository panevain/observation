use super::*;
use crate::testing::FakeHttp;
use observation_agent_api::{EndReason, PairingPhase};
use observation_core::camera::Catalog;
use observation_core::scene::AreaRef;
use serde_json::{Value, json};

const TOKEN: &str = "browser-token";

fn created() -> Reply {
    Ok(serde_json::to_vec(&json!({
        "v": 1, "code": "ABCDEFGH", "browser_token": TOKEN,
        "code_expires_in_s": 600, "idle_timeout_s": 1800,
    }))
    .unwrap())
}

fn events_url() -> String {
    format!("{}?token={TOKEN}", paths::EVENTS)
}

fn event(kind: BrowserEventKind) -> StreamEvent {
    StreamEvent::Data(serde_json::to_vec(&BrowserEvent::new(kind)).unwrap())
}

fn claimed() -> StreamEvent {
    event(BrowserEventKind::Status {
        phase: PairingPhase::Claimed,
        agent: Some("claude".into()),
    })
}

fn state(selection: u32) -> impl FnOnce(Option<PinnedPoint>) -> Option<SharedState> {
    move |pinned| {
        Some(SharedState {
            scene: json!({"format": "observation-scene"}),
            scene_name: Some("Home".into()),
            area: Some(AreaRef::Demo),
            selection: vec![selection],
            pinned,
        })
    }
}

/// A link that is waiting with its code shown, over `http`.
fn waiting(http: &FakeHttp) -> AgentLink {
    let mut link = AgentLink {
        available: true,
        ..AgentLink::default()
    };
    link.connect(http);
    assert_eq!(link.phase(), &LinkPhase::Starting);
    assert!(http.respond_to("POST", paths::PAIRINGS, created()));
    link.update(http, 0.0, |_| None);
    link
}

fn posts(http: &FakeHttp) -> Vec<Value> {
    http.sent()
        .iter()
        .filter(|r| r.method == "PUT" && r.url == paths::STATE)
        .map(|r| serde_json::from_str(r.body.as_deref().unwrap()).unwrap())
        .collect()
}

#[test]
fn connecting_shows_the_code_and_follows_the_pairing() {
    let http = FakeHttp::default();
    let mut off = AgentLink::default();
    off.connect(&http);
    assert!(http.sent().is_empty(), "not offered: nothing is asked");

    let link = waiting(&http);
    let create = &http.sent()[0];
    assert_eq!(create.body.as_deref(), Some(r#"{"v":1}"#));
    assert_eq!(link.code().as_deref(), Some("ABCD-EFGH"));
    assert_eq!(http.followed(), [(events_url(), "agent".to_owned())]);
}

#[test]
fn the_link_is_connected_once_claimed_and_ends_when_told() {
    let http = FakeHttp::default();
    let mut link = waiting(&http);
    http.emit_follow(&events_url(), &claimed());
    link.update(&http, 0.0, |_| None);
    assert_eq!(
        link.phase(),
        &LinkPhase::Connected {
            agent: Some("claude".into())
        }
    );
    // A drop is the browser's to reconnect; unknown kinds are skipped.
    http.emit_follow(&events_url(), &StreamEvent::Dropped);
    http.emit_follow(
        &events_url(),
        &StreamEvent::Data(br#"{"v":1,"type":"proposal"}"#.to_vec()),
    );
    link.update(&http, 0.0, |_| None);
    assert!(link.is_connected());

    http.emit_follow(
        &events_url(),
        &event(BrowserEventKind::Ended {
            reason: EndReason::Agent,
        }),
    );
    link.update(&http, 0.0, |_| None);
    assert_eq!(
        link.phase(),
        &LinkPhase::Ended {
            why: "Disconnected: The agent disconnected.".into()
        }
    );
    assert!(http.followed().is_empty(), "the stream is stopped");
}

#[test]
fn a_closed_stream_or_a_failed_start_ends_the_link() {
    let http = FakeHttp::default();
    let mut link = waiting(&http);
    http.emit_follow(&events_url(), &StreamEvent::Closed("refused".into()));
    link.update(&http, 0.0, |_| None);
    assert!(matches!(link.phase(), LinkPhase::Ended { why } if why.contains("refused")));

    let mut link = AgentLink {
        available: true,
        ..AgentLink::default()
    };
    link.connect(&http);
    http.respond_to("POST", paths::PAIRINGS, Err("HTTP 409: too many".into()));
    link.update(&http, 0.0, |_| None);
    assert!(
        matches!(link.phase(), LinkPhase::Ended { why } if why.contains("too many")),
        "{:?}",
        link.phase()
    );
    // It can be tried again.
    link.connect(&http);
    assert_eq!(link.phase(), &LinkPhase::Starting);
}

#[test]
fn the_state_is_posted_when_it_changes_and_not_otherwise() {
    let http = FakeHttp::default();
    let mut link = waiting(&http);
    // Not before the interval is up.
    link.update(&http, POST_INTERVAL / 2.0, state(1));
    assert!(posts(&http).is_empty());
    link.update(&http, POST_INTERVAL / 2.0, state(1));
    let sent = posts(&http);
    assert_eq!(sent.len(), 1, "posted while waiting, ready for the agent");
    assert_eq!(sent[0]["v"], 1);
    assert_eq!(sent[0]["state"]["selection"], json!([1]));
    let put = http.pending().pop().unwrap();
    assert_eq!(put.bearer.as_deref(), Some(TOKEN));
    // One post at a time.
    link.update(&http, POST_INTERVAL, state(2));
    assert_eq!(posts(&http).len(), 1);
    assert!(http.respond_to("PUT", paths::STATE, Ok(b"{}".to_vec())));
    // The same state again: nothing goes out.
    link.update(&http, POST_INTERVAL, state(1));
    assert_eq!(posts(&http).len(), 1);
    // A new selection does.
    link.update(&http, POST_INTERVAL, state(2));
    assert_eq!(posts(&http).len(), 2);
    http.respond_to("PUT", paths::STATE, Ok(b"{}".to_vec()));

    // A shared point goes out with the next post, and again when shared again.
    http.emit_follow(&events_url(), &claimed());
    link.update(&http, 0.0, state(2));
    assert!(link.share_point(1.5, -2.0, Some((47.0, -122.0))));
    link.update(&http, POST_INTERVAL, state(2));
    let pinned = &posts(&http)[2]["state"]["pinned"];
    assert_eq!(
        *pinned,
        json!({"x": 1.5, "z": -2.0, "lat": 47.0, "lon": -122.0, "seq": 1})
    );
    http.respond_to("PUT", paths::STATE, Ok(b"{}".to_vec()));
    assert!(link.share_point(1.5, -2.0, None));
    link.update(&http, POST_INTERVAL, state(2));
    assert_eq!(posts(&http)[3]["state"]["pinned"]["seq"], 2);
}

#[test]
fn a_refused_post_is_tried_again_after_a_wait() {
    let http = FakeHttp::default();
    let mut link = waiting(&http);
    link.update(&http, POST_INTERVAL, state(1));
    http.respond_to("PUT", paths::STATE, Err("HTTP 429: slow down".into()));
    link.update(&http, POST_INTERVAL, state(1));
    assert_eq!(link.post_error.as_deref(), Some("HTTP 429: slow down"));
    assert_eq!(posts(&http).len(), 1, "waits");
    link.update(&http, RETRY_AFTER, state(1));
    assert_eq!(posts(&http).len(), 2, "the same state is sent again");
}

#[test]
fn disconnecting_tells_the_server_and_drops_late_replies() {
    let http = FakeHttp::default();
    let mut link = waiting(&http);
    link.update(&http, POST_INTERVAL, state(1));
    link.disconnect(&http);
    assert_eq!(link.phase(), &LinkPhase::Idle);
    let delete = http
        .sent()
        .into_iter()
        .find(|r| r.method == "DELETE")
        .unwrap();
    assert_eq!(delete.url, paths::PAIRING);
    assert_eq!(delete.bearer.as_deref(), Some(TOKEN));
    assert!(http.followed().is_empty());
    // The post's answer and the stream's last word belong to the old pairing.
    http.respond_to("PUT", paths::STATE, Err("gone".into()));
    link.update(&http, 0.0, |_| None);
    assert_eq!(link.phase(), &LinkPhase::Idle);
    assert_eq!(link.post_error, None);
    assert!(!link.share_point(0.0, 0.0, None), "nothing to share with");
}

#[test]
fn the_shared_state_is_the_exported_scene_with_the_selection() {
    let models = Models::new(Catalog::default());
    let mut doc = Document::default();
    assert!(shared_state(&doc, &models, "Home", None, None).is_none());
    doc.area = Some(AreaRef::Demo);
    let s = shared_state(&doc, &models, "  Home  ", Some(3), None).unwrap();
    assert_eq!(s.scene["format"], "observation-scene");
    assert_eq!(s.scene_name.as_deref(), Some("Home"));
    assert_eq!(s.area, Some(AreaRef::Demo));
    assert_eq!(s.selection, [3]);
}
