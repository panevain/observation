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

/// What `state(selection)` is built from.
fn key(selection: u32) -> StateKey {
    StateKey {
        selected: Some(selection),
        ..StateKey::default()
    }
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
    link.update(http, 0.0, StateKey::default(), |_| None);
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
    link.update(&http, 0.0, StateKey::default(), |_| None);
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
        &StreamEvent::Data(br#"{"v":1,"type":"telepathy"}"#.to_vec()),
    );
    link.update(&http, 0.0, StateKey::default(), |_| None);
    assert!(link.is_connected());

    http.emit_follow(
        &events_url(),
        &event(BrowserEventKind::Ended {
            reason: EndReason::Agent,
        }),
    );
    link.update(&http, 0.0, StateKey::default(), |_| None);
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
    link.update(&http, 0.0, StateKey::default(), |_| None);
    assert!(matches!(link.phase(), LinkPhase::Ended { why } if why.contains("refused")));

    let mut link = AgentLink {
        available: true,
        ..AgentLink::default()
    };
    link.connect(&http);
    http.respond_to("POST", paths::PAIRINGS, Err("HTTP 409: too many".into()));
    link.update(&http, 0.0, StateKey::default(), |_| None);
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
    link.update(&http, POST_INTERVAL / 2.0, key(1), state(1));
    assert!(posts(&http).is_empty());
    link.update(&http, POST_INTERVAL / 2.0, key(1), state(1));
    let sent = posts(&http);
    assert_eq!(sent.len(), 1, "posted while waiting, ready for the agent");
    assert_eq!(sent[0]["v"], 1);
    assert_eq!(sent[0]["state"]["selection"], json!([1]));
    let put = http.pending().pop().unwrap();
    assert_eq!(put.bearer.as_deref(), Some(TOKEN));
    // One post at a time.
    link.update(&http, POST_INTERVAL, key(2), state(2));
    assert_eq!(posts(&http).len(), 1);
    assert!(http.respond_to("PUT", paths::STATE, Ok(b"{}".to_vec())));
    // The same state again: nothing goes out.
    link.update(&http, POST_INTERVAL, key(1), state(1));
    assert_eq!(posts(&http).len(), 1);
    // A new selection does.
    link.update(&http, POST_INTERVAL, key(2), state(2));
    assert_eq!(posts(&http).len(), 2);
    http.respond_to("PUT", paths::STATE, Ok(b"{}".to_vec()));

    // A shared point goes out with the next post, and again when shared again.
    http.emit_follow(&events_url(), &claimed());
    link.update(&http, 0.0, key(2), state(2));
    assert!(link.share_point(1.5, -2.0, Some((47.0, -122.0))));
    link.update(&http, POST_INTERVAL, key(2), state(2));
    let pinned = &posts(&http)[2]["state"]["pinned"];
    assert_eq!(
        *pinned,
        json!({"x": 1.5, "z": -2.0, "lat": 47.0, "lon": -122.0, "seq": 1})
    );
    http.respond_to("PUT", paths::STATE, Ok(b"{}".to_vec()));
    assert!(link.share_point(1.5, -2.0, None));
    link.update(&http, POST_INTERVAL, key(2), state(2));
    assert_eq!(posts(&http)[3]["state"]["pinned"]["seq"], 2);
}

#[test]
fn a_refused_post_is_tried_again_after_a_wait() {
    let http = FakeHttp::default();
    let mut link = waiting(&http);
    link.update(&http, POST_INTERVAL, key(1), state(1));
    http.respond_to("PUT", paths::STATE, Err("HTTP 429: slow down".into()));
    link.update(&http, POST_INTERVAL, key(1), state(1));
    assert_eq!(link.post_error.as_deref(), Some("HTTP 429: slow down"));
    assert_eq!(posts(&http).len(), 1, "waits");
    link.update(&http, RETRY_AFTER, key(1), state(1));
    assert_eq!(posts(&http).len(), 2, "the same state is sent again");
}

#[test]
fn disconnecting_tells_the_server_and_drops_late_replies() {
    let http = FakeHttp::default();
    let mut link = waiting(&http);
    link.update(&http, POST_INTERVAL, key(1), state(1));
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
    link.update(&http, 0.0, StateKey::default(), |_| None);
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

/// The demo area with cameras 1 and 2.
fn document() -> (Document, Models) {
    let models = Models::new(Catalog::default());
    let mut doc = Document::default();
    doc.area = Some(AreaRef::Demo);
    for x in [0.0, 10.0] {
        doc.add_camera(observation_core::camera::CameraPlacement {
            id: 0,
            label: String::new(),
            model_id: "m".into(),
            position: [x, 3.0, -6.0],
            yaw_deg: 180.0,
            pitch_deg: -10.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        });
    }
    (doc, models)
}

/// A proposal event that turns camera 1, made against `doc`.
fn proposal_event(id: u64, doc: &Document, models: &Models) -> StreamEvent {
    let base = crate::proposal::current_scene(doc, models).unwrap();
    let mut proposed = base.clone();
    proposed.cameras[0].yaw_deg = 90.0;
    let changes = observation_core::scene::changes::ChangeSet::between(&base, &proposed).unwrap();
    event(BrowserEventKind::Proposal {
        proposal: Proposal {
            id,
            summary: "Face camera 1 east".into(),
            base_revision: 1,
            changes,
        },
    })
}

fn decisions(http: &FakeHttp) -> Vec<Value> {
    http.sent()
        .iter()
        .filter(|r| r.method == "POST" && r.url == paths::DECISION)
        .map(|r| {
            assert_eq!(r.bearer.as_deref(), Some(TOKEN));
            serde_json::from_str(r.body.as_deref().unwrap()).unwrap()
        })
        .collect()
}

/// A connected link over `http` with proposal 1 on its card.
fn with_proposal(http: &FakeHttp, doc: &Document, models: &Models) -> AgentLink {
    let mut link = waiting(http);
    http.emit_follow(&events_url(), &claimed());
    http.emit_follow(&events_url(), &proposal_event(1, doc, models));
    link.update(http, 0.0, StateKey::default(), |_| None);
    link.follow_proposal(http, doc, models);
    link
}

#[test]
fn a_proposal_opens_the_card_and_accepting_applies_it_and_tells_the_agent() {
    let http = FakeHttp::default();
    let (mut doc, mut models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    assert!(link.card_open);
    assert_eq!(
        link.take_notices(),
        ["The agent proposes: Face camera 1 east"]
    );
    let p = link.proposal().unwrap();
    assert_eq!(p.id, 1);
    assert!(
        p.proposed().is_some(),
        "ready for the figures and the ghost"
    );
    assert_eq!(link.agent_name(), Some("claude"));

    link.accept(&http, &mut doc, &mut models, 100).unwrap();
    assert_eq!(doc.camera(1).unwrap().yaw_deg, 90.0);
    assert_eq!(doc.agent_log.len(), 1);
    assert_eq!(
        decisions(&http),
        [json!({"v": 1, "id": 1, "decision": "accepted"})]
    );
    // Accepting twice does nothing more.
    assert!(link.accept(&http, &mut doc, &mut models, 100).is_err());
    assert_eq!(decisions(&http).len(), 1);
    // The card closes and forgets a decided proposal.
    link.close_card();
    assert!(link.proposal().is_none() && !link.card_open);
}

#[test]
fn a_stale_proposal_is_reported_once_and_cannot_be_accepted() {
    let http = FakeHttp::default();
    let (mut doc, mut models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    doc.camera_mut(1).unwrap().position[0] = 2.0;
    link.follow_proposal(&http, &doc, &models);
    link.follow_proposal(&http, &doc, &models);
    assert_eq!(
        decisions(&http),
        [json!({"v": 1, "id": 1, "decision": "stale",
                "reason": "The user changed camera 1 before deciding"})]
    );
    assert!(link.accept(&http, &mut doc, &mut models, 0).is_err());
    assert_eq!(decisions(&http).len(), 1);
    assert_eq!(doc.camera(1).unwrap().yaw_deg, 180.0, "nothing applied");
}

#[test]
fn rejecting_sends_the_reason_and_a_failed_send_is_shown() {
    let http = FakeHttp::default();
    let (doc, models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    link.proposal_mut().unwrap().reason = " not that one ".into();
    link.reject(&http);
    assert_eq!(
        decisions(&http),
        [json!({"v": 1, "id": 1, "decision": "rejected", "reason": "not that one"})]
    );
    let why = "that proposal is no longer pending: it is withdrawn";
    assert!(http.fail_to("POST", paths::DECISION, 409, None, why));
    link.update(&http, 0.0, StateKey::default(), |_| None);
    let p = link.proposal().unwrap();
    assert_eq!(p.decision_error.as_deref(), Some(why));
    assert!(!p.decision_retrying);
    // Not tried again.
    link.update(&http, 60.0, StateKey::default(), |_| None);
    assert_eq!(decisions(&http).len(), 1);
}

#[test]
fn a_new_proposal_replaces_the_shown_one_and_a_withdrawal_closes_it() {
    let http = FakeHttp::default();
    let (doc, models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    http.emit_follow(&events_url(), &proposal_event(2, &doc, &models));
    link.update(&http, 0.0, StateKey::default(), |_| None);
    link.follow_proposal(&http, &doc, &models);
    assert_eq!(link.proposal().unwrap().id, 2);
    link.take_notices();
    // A withdrawal of another proposal changes nothing.
    http.emit_follow(
        &events_url(),
        &event(BrowserEventKind::ProposalWithdrawn { id: 1 }),
    );
    link.update(&http, 0.0, StateKey::default(), |_| None);
    assert!(link.proposal().unwrap().standing.is_open());
    http.emit_follow(
        &events_url(),
        &event(BrowserEventKind::ProposalWithdrawn { id: 2 }),
    );
    link.update(&http, 0.0, StateKey::default(), |_| None);
    assert_eq!(
        link.proposal().unwrap().standing,
        crate::proposal::Standing::Closed {
            why: "The agent withdrew this proposal.".into()
        }
    );
    assert_eq!(link.take_notices(), ["The agent withdrew its proposal"]);
    assert!(decisions(&http).is_empty());
}

#[test]
fn a_proposal_left_undecided_closes_when_the_connection_ends() {
    let http = FakeHttp::default();
    let (mut doc, mut models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    link.disconnect(&http);
    let p = link.proposal().unwrap();
    assert!(!p.standing.is_open());
    assert!(link.accept(&http, &mut doc, &mut models, 0).is_err());
    assert!(decisions(&http).is_empty());
}

#[test]
fn the_state_is_built_only_when_its_key_or_the_pinned_point_changes() {
    let http = FakeHttp::default();
    let mut link = waiting(&http);
    let mut built = 0;
    let mut update = |link: &mut AgentLink, key: StateKey| {
        link.update(&http, POST_INTERVAL, key, |pinned| {
            built += 1;
            state(1)(pinned)
        });
        http.respond_to("PUT", paths::STATE, Ok(b"{}".to_vec()));
    };
    update(&mut link, key(1));
    update(&mut link, key(1));
    update(&mut link, key(1));
    assert!(link.share_point(1.0, 2.0, None));
    update(&mut link, key(1));
    update(&mut link, key(1));
    update(
        &mut link,
        StateKey {
            revision: 7,
            ..key(1)
        },
    );
    assert_eq!(built, 3, "once, then for the point, then for the revision");
    // The new revision changed nothing in the JSON: two posts.
    assert_eq!(posts(&http).len(), 2);
}

#[test]
fn a_proposal_sent_again_after_a_reconnect_keeps_the_card() {
    let http = FakeHttp::default();
    let (mut doc, mut models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    let again = proposal_event(1, &doc, &models);
    assert_eq!(link.take_notices().len(), 1);
    let resend = |link: &mut AgentLink, doc: &Document, models: &Models| {
        http.emit_follow(&events_url(), &StreamEvent::Dropped);
        http.emit_follow(&events_url(), &again);
        link.update(&http, 0.0, StateKey::default(), |_| None);
        link.follow_proposal(&http, doc, models);
    };

    // Open, with a reason being typed: kept as it is.
    link.proposal_mut().unwrap().reason = "maybe".into();
    resend(&mut link, &doc, &models);
    assert_eq!(link.proposal().unwrap().reason, "maybe");
    assert!(link.take_notices().is_empty(), "no second toast");

    // Accepted, its decision not answered yet: no stale decision follows,
    // although the scene now differs from the proposal's base.
    link.accept(&http, &mut doc, &mut models, 100).unwrap();
    resend(&mut link, &doc, &models);
    let p = link.proposal().unwrap();
    assert_eq!(p.standing, crate::proposal::Standing::Accepted);
    assert_eq!(
        decisions(&http),
        [json!({"v": 1, "id": 1, "decision": "accepted"})]
    );
    assert!(link.take_notices().is_empty());

    // Nor once the card is closed and the proposal forgotten.
    assert!(http.respond_to("POST", paths::DECISION, Ok(Vec::new())));
    link.update(&http, 0.0, StateKey::default(), |_| None);
    link.close_card();
    resend(&mut link, &doc, &models);
    assert!(link.proposal().is_none() && !link.card_open);
    assert!(link.take_notices().is_empty());
    assert_eq!(decisions(&http).len(), 1);

    // A newer proposal is still taken.
    http.emit_follow(&events_url(), &proposal_event(2, &doc, &models));
    link.update(&http, 0.0, StateKey::default(), |_| None);
    link.follow_proposal(&http, &doc, &models);
    assert_eq!(link.proposal().map(|p| p.id), Some(2));
    assert_eq!(link.take_notices().len(), 1);
}

#[test]
fn a_stale_proposal_sent_again_stays_stale_and_is_reported_once() {
    let http = FakeHttp::default();
    let (mut doc, models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    let again = proposal_event(1, &doc, &models);
    doc.camera_mut(1).unwrap().yaw_deg = 0.0;
    link.follow_proposal(&http, &doc, &models);
    http.emit_follow(&events_url(), &again);
    link.update(&http, 0.0, StateKey::default(), |_| None);
    link.follow_proposal(&http, &doc, &models);
    assert!(matches!(
        link.proposal().unwrap().standing,
        crate::proposal::Standing::Stale { .. }
    ));
    assert_eq!(decisions(&http).len(), 1);
}

#[test]
fn a_decision_refused_for_now_is_tried_again_before_any_state_post() {
    let http = FakeHttp::default();
    let (mut doc, mut models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    link.accept(&http, &mut doc, &mut models, 100).unwrap();
    assert!(http.fail_to("POST", paths::DECISION, 429, Some(2), "too many requests"));
    link.update(&http, 0.0, key(1), state(1));
    let card =
        |link: &AgentLink| crate::proposal::ProposalCardVm::new(link.proposal().unwrap(), None);
    assert!(card(&link).telling_agent, "telling the agent…");
    assert_eq!(card(&link).decision_error, None, "no error while it tries");

    // The server's Retry-After is honoured, and no state goes out first.
    link.update(&http, 1.0, key(1), state(1));
    assert_eq!(decisions(&http).len(), 1);
    assert!(posts(&http).is_empty());
    link.update(&http, 1.0, key(1), state(1));
    let accepted = json!({"v": 1, "id": 1, "decision": "accepted"});
    assert_eq!(decisions(&http), [accepted.clone(), accepted]);
    assert!(posts(&http).is_empty());
    assert!(http.respond_to("POST", paths::DECISION, Ok(Vec::new())));
    link.update(&http, 0.0, key(1), state(1));
    assert!(!card(&link).telling_agent);
    assert_eq!(card(&link).decision_error, None);

    // Delivered: the state follows.
    link.update(&http, POST_INTERVAL, key(1), state(1));
    assert_eq!(posts(&http).len(), 1);
}

#[test]
fn an_unanswered_decision_backs_off_and_a_late_409_means_it_got_through() {
    let http = FakeHttp::default();
    let (mut doc, mut models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    link.accept(&http, &mut doc, &mut models, 100).unwrap();
    // No answer at all: tried again after 1 s, then 2 s.
    assert!(http.respond_to("POST", paths::DECISION, Err("offline".into())));
    link.update(&http, 0.0, StateKey::default(), |_| None);
    link.update(&http, DECISION_RETRY_FIRST, StateKey::default(), |_| None);
    assert_eq!(decisions(&http).len(), 2);
    assert!(http.fail_to("POST", paths::DECISION, 503, None, "restarting"));
    link.update(&http, 0.0, StateKey::default(), |_| None);
    link.update(&http, 1.5, StateKey::default(), |_| None);
    assert_eq!(decisions(&http).len(), 2, "waits 2 s now");
    link.update(&http, 0.5, StateKey::default(), |_| None);
    assert_eq!(decisions(&http).len(), 3);
    // The first try had reached the server after all.
    let decided = "that proposal is no longer pending: it is accepted";
    assert!(http.fail_to("POST", paths::DECISION, 409, None, decided));
    link.update(&http, 0.0, StateKey::default(), |_| None);
    let p = link.proposal().unwrap();
    assert_eq!(
        (p.decision_error.as_deref(), p.decision_retrying),
        (None, false)
    );
    link.update(&http, DECISION_RETRY_MAX, StateKey::default(), |_| None);
    assert_eq!(decisions(&http).len(), 3, "stopped");
}

#[test]
fn a_decision_still_trying_when_the_pairing_ends_says_so() {
    let http = FakeHttp::default();
    let (mut doc, mut models) = document();
    let mut link = with_proposal(&http, &doc, &models);
    link.accept(&http, &mut doc, &mut models, 100).unwrap();
    assert!(http.fail_to("POST", paths::DECISION, 500, None, "oops"));
    link.update(&http, 0.0, StateKey::default(), |_| None);
    assert!(link.proposal().unwrap().decision_retrying);
    link.disconnect(&http);
    let p = link.proposal().unwrap();
    assert!(!p.decision_retrying);
    assert_eq!(
        p.decision_error.as_deref(),
        Some("the connection to the agent ended before it was told")
    );
    assert_eq!(p.standing, crate::proposal::Standing::Accepted);
}
