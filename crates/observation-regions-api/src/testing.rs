//! A scripted [`RegionAgent`] for tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use observation_core::geo::BBox;
use tokio::time::Instant;

use crate::{
    ActionKind, AgentError, QueryResult, RegionAction, RegionAgent, RegionsSnapshot, SizesRequest,
};

/// One call a [`FakeRegionAgent`] received.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentCall {
    Snapshot {
        since: Option<u64>,
    },
    Query {
        bbox: BBox,
        query: String,
        timeout: Duration,
    },
    Catalog,
    CatalogSizes(SizesRequest),
    Act(ActionKind, RegionAction),
}

/// A [`RegionAgent`] that answers from queues: snapshots, queries, and the
/// answers to catalog requests and actions (one queue for those three).
/// Every call is recorded with the time it came (Tokio's clock). A call
/// with nothing queued answers an error and is counted, which
/// [`Self::assert_drained`] reports; with [`Self::idle_when_drained`], an
/// unscripted snapshot instead never answers, like a long poll with no
/// change. Clones share the script.
#[derive(Debug, Clone, Default)]
pub struct FakeRegionAgent(Arc<Mutex<State>>);

#[derive(Debug, Default)]
struct State {
    snapshots: VecDeque<Result<RegionsSnapshot, String>>,
    queries: VecDeque<Result<Option<QueryResult>, String>>,
    answers: VecDeque<Result<Bytes, AgentError>>,
    calls: Vec<(Instant, AgentCall)>,
    unscripted: Vec<AgentCall>,
    idle_when_drained: bool,
}

impl FakeRegionAgent {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues the answer to a snapshot request.
    pub fn snapshot_reply(&self, reply: Result<RegionsSnapshot, String>) {
        self.state().snapshots.push_back(reply);
    }

    /// Queues the answer to a query.
    pub fn query_reply(&self, reply: Result<Option<QueryResult>, String>) {
        self.state().queries.push_back(reply);
    }

    /// Queues the answer to a catalog request or an action.
    pub fn answer(&self, reply: Result<Bytes, AgentError>) {
        self.state().answers.push_back(reply);
    }

    /// From now on, a snapshot request with nothing queued waits forever.
    pub fn idle_when_drained(&self) {
        self.state().idle_when_drained = true;
    }

    /// The calls so far, in order.
    pub fn calls(&self) -> Vec<AgentCall> {
        self.state().calls.iter().map(|c| c.1.clone()).collect()
    }

    /// When each call came, in order.
    pub fn call_times(&self) -> Vec<Instant> {
        self.state().calls.iter().map(|c| c.0).collect()
    }

    /// Panics if a reply is left or a call found none.
    #[track_caller]
    pub fn assert_drained(&self) {
        let s = self.state();
        assert!(
            s.unscripted.is_empty(),
            "unscripted calls: {:?}",
            s.unscripted
        );
        assert!(
            s.snapshots.is_empty() && s.queries.is_empty() && s.answers.is_empty(),
            "replies left"
        );
    }

    /// Records `call` and takes its reply from `queue`.
    fn take<T>(&self, call: AgentCall, queue: impl FnOnce(&mut State) -> Option<T>) -> Option<T> {
        let mut s = self.state();
        s.calls.push((Instant::now(), call.clone()));
        let reply = queue(&mut s);
        if reply.is_none() && !(s.idle_when_drained && matches!(call, AgentCall::Snapshot { .. })) {
            s.unscripted.push(call);
        }
        reply
    }

    fn answer_for(&self, call: AgentCall) -> BoxFuture<'static, Result<Bytes, AgentError>> {
        let reply = self
            .take(call, |s| s.answers.pop_front())
            .unwrap_or_else(|| Err(AgentError::Other("unscripted fake agent call".into())));
        async move { reply }.boxed()
    }
}

impl RegionAgent for FakeRegionAgent {
    fn snapshot(&self, since: Option<u64>) -> BoxFuture<'_, Result<RegionsSnapshot, String>> {
        let idle = self.state().idle_when_drained;
        match self.take(AgentCall::Snapshot { since }, |s| s.snapshots.pop_front()) {
            Some(reply) => async move { reply }.boxed(),
            None if idle => futures_util::future::pending().boxed(),
            None => async { Err("unscripted fake agent call".to_owned()) }.boxed(),
        }
    }

    fn query<'a>(
        &'a self,
        bbox: &'a BBox,
        query: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Option<QueryResult>, String>> {
        let call = AgentCall::Query {
            bbox: *bbox,
            query: query.to_owned(),
            timeout,
        };
        let reply = self
            .take(call, |s| s.queries.pop_front())
            .unwrap_or_else(|| Err("unscripted fake agent call".to_owned()));
        async move { reply }.boxed()
    }

    fn catalog(&self) -> BoxFuture<'_, Result<Bytes, AgentError>> {
        self.answer_for(AgentCall::Catalog)
    }

    fn catalog_sizes<'a>(
        &'a self,
        req: &'a SizesRequest,
    ) -> BoxFuture<'a, Result<Bytes, AgentError>> {
        self.answer_for(AgentCall::CatalogSizes(req.clone()))
    }

    fn act<'a>(
        &'a self,
        action: ActionKind,
        req: &'a RegionAction,
    ) -> BoxFuture<'a, Result<Bytes, AgentError>> {
        self.answer_for(AgentCall::Act(action, req.clone()))
    }
}
