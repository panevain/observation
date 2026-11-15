use std::sync::Arc;
use std::time::Duration;

use observation_agent_api::{
    BROWSER_GONE_SECS, CODE_ALPHABET, CODE_LEN, CODE_TTL_SECS, EndReason, IDLE_TIMEOUT_SECS,
    MAX_FAILED_CLAIMS, MAX_PAIRINGS, PairingPhase, SharedState, TOKEN_HEX_LEN,
};
use observation_core::scene::changes::Change;
use serde_json::json;

use super::*;

fn state(n: u32) -> SharedState {
    SharedState {
        scene: json!({ "n": n }),
        scene_name: None,
        area: None,
        selection: vec![n],
        pinned: None,
    }
}

/// A bridge with one claimed pairing whose browser holds a stream open:
/// `(bridge, browser token, agent token, stream)`.
fn claimed() -> (Arc<AgentBridge>, String, String, BrowserStream) {
    let bridge = Arc::new(AgentBridge::new());
    let created = bridge.create().unwrap();
    let stream = bridge.open_stream(&created.browser_token).unwrap();
    let claimed = bridge
        .claim(Some(&created.code), Some("  test agent  "))
        .unwrap();
    (bridge, created.browser_token, claimed.agent_token, stream)
}

async fn advance(secs: u64) {
    tokio::time::advance(Duration::from_secs(secs)).await;
}

#[test]
fn secrets_compare_by_every_byte() {
    assert!(same_secret("abc", "abc"));
    assert!(!same_secret("abc", "abx"));
    assert!(!same_secret("abc", "abcd"));
    assert!(!same_secret("", "a"));
}

#[tokio::test(start_paused = true)]
async fn a_pairing_has_a_code_and_tokens_of_the_documented_form() {
    let bridge = Arc::new(AgentBridge::new());
    let a = bridge.create().unwrap();
    let b = bridge.create().unwrap();
    assert_eq!(a.code.len(), CODE_LEN);
    assert!(a.code.bytes().all(|c| CODE_ALPHABET.contains(&c)));
    assert_eq!(a.browser_token.len(), TOKEN_HEX_LEN);
    assert!(a.browser_token.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a.code, b.code);
    assert_ne!(a.browser_token, b.browser_token);
    assert_eq!(a.code_expires_in_s, CODE_TTL_SECS);
    assert_eq!(bridge.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn claiming_publishes_the_agent_and_uses_the_code_up() {
    let bridge = Arc::new(AgentBridge::new());
    let created = bridge.create().unwrap();
    let stream = bridge.open_stream(&created.browser_token).unwrap();
    assert_eq!(stream.signal.borrow().phase, PairingPhase::Waiting);
    let claimed = bridge
        .claim(Some(&created.code), Some("  claude  "))
        .unwrap();
    assert_eq!(claimed.agent_token.len(), TOKEN_HEX_LEN);
    assert_ne!(claimed.agent_token, created.browser_token);
    let s = stream.signal.borrow().clone();
    assert_eq!(s.phase, PairingPhase::Claimed);
    assert_eq!(s.agent.as_deref(), Some("claude"));
    // One agent per pairing: the code is gone.
    assert_eq!(
        bridge.claim(Some(&created.code), None),
        Err(BridgeError::NoSuchCode)
    );
    // The browser's token is not the agent's.
    assert_eq!(
        bridge
            .read_state(&created.browser_token, None, Duration::ZERO)
            .await,
        Err(BridgeError::Unauthorized)
    );
}

#[tokio::test(start_paused = true)]
async fn wrong_codes_burn_every_unclaimed_code() {
    let bridge = Arc::new(AgentBridge::new());
    let held = bridge.create().unwrap();
    let held_agent = bridge.claim(Some(&held.code), None).unwrap();
    let created = bridge.create().unwrap();
    let stream = bridge.open_stream(&created.browser_token).unwrap();
    for _ in 0..MAX_FAILED_CLAIMS - 1 {
        assert_eq!(
            bridge.claim(Some("00000000"), None),
            Err(BridgeError::NoSuchCode)
        );
    }
    assert_eq!(stream.signal.borrow().ended, None);
    // A code that could never be valid counts too, and this one burns.
    assert_eq!(bridge.claim(None, None), Err(BridgeError::NoSuchCode));
    assert_eq!(stream.signal.borrow().ended, Some(EndReason::CodeBurnt));
    assert_eq!(
        bridge.claim(Some(&created.code), None),
        Err(BridgeError::NoSuchCode)
    );
    // A claimed pairing has no code to burn.
    assert!(
        bridge
            .read_state(&held_agent.agent_token, None, Duration::ZERO)
            .await
            .is_ok()
    );
}

#[tokio::test(start_paused = true)]
async fn a_right_code_resets_the_count_of_wrong_ones() {
    let bridge = Arc::new(AgentBridge::new());
    let first = bridge.create().unwrap();
    let second = bridge.create().unwrap();
    for _ in 0..MAX_FAILED_CLAIMS - 1 {
        assert!(bridge.claim(Some("00000000"), None).is_err());
    }
    bridge.claim(Some(&first.code), None).unwrap();
    for _ in 0..MAX_FAILED_CLAIMS - 1 {
        assert!(bridge.claim(Some("00000000"), None).is_err());
    }
    assert!(bridge.claim(Some(&second.code), None).is_ok());
}

#[tokio::test(start_paused = true)]
async fn an_unclaimed_code_expires() {
    let bridge = Arc::new(AgentBridge::new());
    let created = bridge.create().unwrap();
    let stream = bridge.open_stream(&created.browser_token).unwrap();
    advance(CODE_TTL_SECS - 1).await;
    assert_eq!(bridge.len(), 1);
    advance(1).await;
    assert_eq!(
        bridge.claim(Some(&created.code), None),
        Err(BridgeError::NoSuchCode)
    );
    assert_eq!(stream.signal.borrow().ended, Some(EndReason::CodeExpired));
}

#[tokio::test(start_paused = true)]
async fn a_claimed_pairing_ends_when_idle_and_use_keeps_it() {
    let (bridge, browser, agent, stream) = claimed();
    advance(IDLE_TIMEOUT_SECS - 10).await;
    bridge.post_state(&browser, state(1)).unwrap();
    advance(IDLE_TIMEOUT_SECS - 10).await;
    bridge
        .read_state(&agent, None, Duration::ZERO)
        .await
        .unwrap();
    advance(IDLE_TIMEOUT_SECS - 1).await;
    assert_eq!(bridge.len(), 1);
    advance(1).await;
    assert!(bridge.is_empty());
    assert_eq!(stream.signal.borrow().ended, Some(EndReason::Idle));
    assert_eq!(
        bridge.post_state(&browser, state(2)),
        Err(BridgeError::Unauthorized)
    );
}

#[tokio::test(start_paused = true)]
async fn a_pairing_ends_a_minute_after_its_browser_goes() {
    let (bridge, browser, _agent, stream) = claimed();
    let BrowserStream { signal, guard } = stream;
    drop(guard);
    advance(BROWSER_GONE_SECS - 1).await;
    // A reconnect within the minute keeps it.
    let again = bridge.open_stream(&browser).unwrap();
    advance(BROWSER_GONE_SECS * 3).await;
    assert_eq!(bridge.len(), 1);
    drop(again);
    advance(BROWSER_GONE_SECS).await;
    assert!(bridge.is_empty());
    assert_eq!(signal.borrow().ended, Some(EndReason::BrowserGone));
}

#[tokio::test(start_paused = true)]
async fn either_side_ends_it_and_its_tokens_stop_working() {
    let (bridge, browser, agent, stream) = claimed();
    assert_eq!(bridge.end(&agent), Ok(EndReason::Agent));
    assert_eq!(stream.signal.borrow().ended, Some(EndReason::Agent));
    assert_eq!(bridge.end(&browser), Err(BridgeError::Unauthorized));

    let (bridge, browser, agent, stream) = claimed();
    assert_eq!(bridge.end(&browser), Ok(EndReason::Browser));
    assert_eq!(stream.signal.borrow().ended, Some(EndReason::Browser));
    // An agent whose poll was between requests is still told why, for a
    // while; then its token is simply unknown.
    for since in [None, Some(0)] {
        let view = bridge
            .read_state(&agent, since, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!((view.state, view.ended), (None, Some(EndReason::Browser)));
    }
    assert_eq!(
        bridge.read_state("not a token", None, Duration::ZERO).await,
        Err(BridgeError::Unauthorized)
    );
    advance(ENDED_MEMORY.as_secs()).await;
    assert_eq!(
        bridge.read_state(&agent, None, Duration::ZERO).await,
        Err(BridgeError::Unauthorized)
    );
    assert_eq!(bridge.end("not a token"), Err(BridgeError::Unauthorized));
}

#[tokio::test(start_paused = true)]
async fn posts_raise_the_revision_and_the_agent_reads_the_latest() {
    let (bridge, browser, agent, _stream) = claimed();
    let first = bridge
        .read_state(&agent, None, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!((first.revision, first.state), (0, None));
    assert_eq!(bridge.post_state(&browser, state(1)), Ok(1));
    assert_eq!(bridge.post_state(&browser, state(2)), Ok(2));
    let view = bridge
        .read_state(&agent, Some(0), Duration::from_secs(25))
        .await
        .unwrap();
    assert_eq!(view.revision, 2);
    assert_eq!(view.state, Some(state(2)));
    assert_eq!(view.ended, None);
}

#[tokio::test(start_paused = true)]
async fn a_long_poll_waits_for_a_newer_revision_or_its_time() {
    let (bridge, browser, agent, _stream) = claimed();
    bridge.post_state(&browser, state(1)).unwrap();
    let poll = {
        let (bridge, agent) = (Arc::clone(&bridge), agent.clone());
        tokio::spawn(async move {
            bridge
                .read_state(&agent, Some(1), Duration::from_secs(25))
                .await
        })
    };
    advance(5).await;
    assert!(!poll.is_finished());
    bridge.post_state(&browser, state(2)).unwrap();
    let view = poll.await.unwrap().unwrap();
    assert_eq!(view.revision, 2);

    // Nothing new: it answers the same revision when its time is up.
    let started = tokio::time::Instant::now();
    let view = bridge
        .read_state(&agent, Some(2), Duration::from_secs(25))
        .await
        .unwrap();
    assert_eq!(view.revision, 2);
    assert_eq!(started.elapsed(), Duration::from_secs(25));
}

#[tokio::test(start_paused = true)]
async fn a_waiting_poll_hears_that_the_pairing_ended() {
    let (bridge, browser, agent, _stream) = claimed();
    let poll = {
        let (bridge, agent) = (Arc::clone(&bridge), agent.clone());
        tokio::spawn(async move {
            bridge
                .read_state(&agent, Some(0), Duration::from_secs(25))
                .await
        })
    };
    advance(1).await;
    bridge.end(&browser).unwrap();
    let view = poll.await.unwrap().unwrap();
    assert_eq!(view.ended, Some(EndReason::Browser));
    assert_eq!(view.state, None);
}

#[tokio::test(start_paused = true)]
async fn the_number_of_pairings_is_capped() {
    let bridge = Arc::new(AgentBridge::new());
    for _ in 0..MAX_PAIRINGS {
        bridge.create().unwrap();
    }
    assert_eq!(bridge.create(), Err(BridgeError::Full));
    // Expired codes make room.
    advance(CODE_TTL_SECS).await;
    assert!(bridge.create().is_ok());
}

#[tokio::test(start_paused = true)]
async fn posts_streams_and_polls_are_limited_per_pairing() {
    let (bridge, browser, agent, _stream) = claimed();
    for n in 0..MAX_POSTS_PER_WINDOW {
        bridge.post_state(&browser, state(n as u32)).unwrap();
    }
    assert!(matches!(
        bridge.post_state(&browser, state(0)),
        Err(BridgeError::TooMany(s)) if (1..=10).contains(&s)
    ));
    advance(POST_WINDOW.as_secs()).await;
    assert!(bridge.post_state(&browser, state(0)).is_ok());

    let streams: Vec<_> = (1..MAX_STREAMS_PER_PAIRING)
        .map(|_| bridge.open_stream(&browser).unwrap())
        .collect();
    assert!(matches!(
        bridge.open_stream(&browser),
        Err(BridgeError::TooMany(_))
    ));
    drop(streams);
    assert!(bridge.open_stream(&browser).is_ok());

    let since = bridge
        .read_state(&agent, None, Duration::ZERO)
        .await
        .unwrap()
        .revision;
    let polls: Vec<_> = (0..MAX_POLLS_PER_PAIRING)
        .map(|_| {
            let (bridge, agent) = (Arc::clone(&bridge), agent.clone());
            tokio::spawn(async move {
                bridge
                    .read_state(&agent, Some(since), Duration::from_secs(25))
                    .await
            })
        })
        .collect();
    tokio::task::yield_now().await;
    advance(1).await;
    assert_eq!(
        bridge
            .read_state(&agent, Some(since), Duration::from_secs(25))
            .await,
        Err(BridgeError::TooMany(1))
    );
    for p in polls {
        p.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn ending_everything_tells_every_follower() {
    let (bridge, _browser, _agent, stream) = claimed();
    bridge.end_all(EndReason::Shutdown);
    assert_eq!(stream.signal.borrow().ended, Some(EndReason::Shutdown));
    assert!(bridge.is_empty());
}

fn changes(new: u32) -> ChangeSet {
    ChangeSet {
        changes: vec![Change::LimitFiguresTo {
            base: None,
            new: Some(new),
        }],
    }
}

fn statuses(view: &StateView) -> Vec<(u64, ProposalStatus)> {
    view.proposals.iter().map(|o| (o.id, o.status)).collect()
}

#[tokio::test(start_paused = true)]
async fn a_new_proposal_supersedes_the_pending_one_and_wakes_the_poll() {
    let (bridge, _browser, agent, stream) = claimed();
    let poll = {
        let (bridge, agent) = (Arc::clone(&bridge), agent.clone());
        tokio::spawn(async move {
            bridge
                .read_state(&agent, Some(0), Duration::from_secs(25))
                .await
        })
    };
    advance(1).await;
    assert_eq!(bridge.propose(&agent, "first".into(), 0, changes(1)), Ok(1));
    let view = poll.await.unwrap().unwrap();
    assert_eq!(view.revision, 1);
    assert_eq!(statuses(&view), [(1, ProposalStatus::Pending)]);
    assert_eq!(
        stream.signal.borrow().proposal.as_ref().map(|p| p.id),
        Some(1)
    );

    assert_eq!(
        bridge.propose(&agent, "second".into(), 1, changes(2)),
        Ok(2)
    );
    let view = bridge
        .read_state(&agent, Some(1), Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(
        statuses(&view),
        [
            (1, ProposalStatus::Superseded),
            (2, ProposalStatus::Pending)
        ]
    );
    let pending = stream.signal.borrow().proposal.clone().unwrap();
    assert_eq!((pending.id, pending.summary.as_str()), (2, "second"));
    // The browser's token cannot propose.
    assert_eq!(
        bridge.propose(&_browser, "x".into(), 0, changes(3)),
        Err(BridgeError::Unauthorized)
    );
}

#[tokio::test(start_paused = true)]
async fn the_users_decision_reaches_the_agent_and_is_final() {
    let (bridge, browser, agent, stream) = claimed();
    bridge.propose(&agent, "p".into(), 0, changes(1)).unwrap();
    assert_eq!(
        bridge.decide(&agent, 1, Decision::Accepted, None),
        Err(BridgeError::Unauthorized),
        "only the browser decides"
    );
    bridge
        .decide(&browser, 1, Decision::Rejected, Some("too close".into()))
        .unwrap();
    assert_eq!(stream.signal.borrow().proposal, None);
    let view = bridge
        .read_state(&agent, None, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(statuses(&view), [(1, ProposalStatus::Rejected)]);
    assert_eq!(view.proposals[0].reason.as_deref(), Some("too close"));
    assert_eq!(
        bridge.decide(&browser, 1, Decision::Accepted, None),
        Err(BridgeError::Decided(ProposalStatus::Rejected))
    );
    assert_eq!(
        bridge.decide(&browser, 9, Decision::Accepted, None),
        Err(BridgeError::NoSuchProposal)
    );
    // A proposal superseded after the user accepted it: the acceptance counts,
    // and the newer one stays pending.
    bridge.propose(&agent, "a".into(), 0, changes(1)).unwrap();
    bridge.propose(&agent, "b".into(), 0, changes(2)).unwrap();
    bridge
        .decide(&browser, 2, Decision::Accepted, None)
        .unwrap();
    let view = bridge
        .read_state(&agent, None, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(
        statuses(&view)[1..],
        [(2, ProposalStatus::Accepted), (3, ProposalStatus::Pending)]
    );
    assert_eq!(
        stream.signal.borrow().proposal.as_ref().map(|p| p.id),
        Some(3)
    );
}

#[tokio::test(start_paused = true)]
async fn the_agent_withdraws_a_pending_proposal_only() {
    let (bridge, browser, agent, stream) = claimed();
    bridge.propose(&agent, "p".into(), 0, changes(1)).unwrap();
    assert_eq!(bridge.withdraw(&agent, 1), Ok(()));
    let s = stream.signal.borrow().clone();
    assert_eq!((s.proposal, s.withdrawn), (None, Some(1)));
    assert_eq!(
        bridge.withdraw(&agent, 1),
        Err(BridgeError::Decided(ProposalStatus::Withdrawn))
    );
    assert_eq!(bridge.withdraw(&agent, 7), Err(BridgeError::NoSuchProposal));
    bridge.propose(&agent, "q".into(), 0, changes(1)).unwrap();
    bridge.decide(&browser, 2, Decision::Stale, None).unwrap();
    assert_eq!(
        bridge.withdraw(&agent, 2),
        Err(BridgeError::Decided(ProposalStatus::Stale))
    );
}

#[tokio::test(start_paused = true)]
async fn proposals_count_against_the_post_limit_and_outcomes_are_capped() {
    let (bridge, _browser, agent, _stream) = claimed();
    for n in 0..MAX_POSTS_PER_WINDOW {
        bridge
            .propose(&agent, format!("p{n}"), 0, changes(1))
            .unwrap();
    }
    assert!(matches!(
        bridge.propose(&agent, "over".into(), 0, changes(1)),
        Err(BridgeError::TooMany(_))
    ));
    let view = bridge
        .read_state(&agent, None, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(view.proposals.len(), MAX_OUTCOMES);
    let last = view.proposals.last().unwrap();
    assert_eq!(
        (last.id, last.status),
        (MAX_POSTS_PER_WINDOW as u64, ProposalStatus::Pending)
    );
}
