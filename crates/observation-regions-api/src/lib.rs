//! The contract between the Observation server and its region agent
//! (`observation-regions`).
//!
//! - [`paths`]: the agent's routes, used by the agent to serve them and by the
//!   server to call them, so the two cannot drift apart.
//! - [`RegionAgent`]: the server's view of the agent, one method per route,
//!   with no state of its own. [`HttpRegionAgent`] implements it over an
//!   [`HttpClient`](observation_upstream::HttpClient).
//!
//! The request and response types are those of [`observation_core::regions`],
//! re-exported here.

use std::fmt::Debug;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use observation_core::geo::BBox;
pub use observation_core::regions::{
    ActionKind, OSM_BASE_HEADER, RegionAction, RegionsSnapshot, SizesRequest,
};

mod client;

pub use crate::client::HttpRegionAgent;

/// The agent's routes.
pub mod paths {
    use super::ActionKind;

    /// `GET`: the [`RegionsSnapshot`](super::RegionsSnapshot); with
    /// `?since=<version>`, waits for a newer one.
    pub const REGIONS: &str = "/regions";
    /// `GET`: the importable extracts (JSON).
    pub const CATALOG: &str = "/catalog";
    /// `POST` a [`SizesRequest`](super::SizesRequest): download sizes of
    /// catalog extracts.
    pub const CATALOG_SIZES: &str = "/catalog/sizes";
    /// `POST` an Overpass QL query as the form field `data`, with
    /// `?bbox=<s,w,n,e>`: Overpass JSON, or 404 when no region holds the box.
    pub const INTERPRETER: &str = "/interpreter";

    /// `POST` a [`RegionAction`](super::RegionAction): the route of `action`,
    /// answered with the new snapshot.
    pub fn action(action: ActionKind) -> &'static str {
        match action {
            ActionKind::Import => "/regions/import",
            ActionKind::Cancel => "/regions/cancel",
            ActionKind::Delete => "/regions/delete",
            ActionKind::Settings => "/regions/settings",
        }
    }
}

/// An Overpass result from a local region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryResult {
    /// The Overpass JSON.
    pub body: Bytes,
    /// The region's data date from [`OSM_BASE_HEADER`], Unix seconds.
    pub osm_base_unix: Option<u64>,
}

/// Why a catalog request or an action failed. The agent's own `error` text
/// is kept where it answered with one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    /// 400 or 404: the request was refused.
    #[error("{0}")]
    BadRequest(String),
    /// 409: it clashes with work in progress.
    #[error("{0}")]
    Conflict(String),
    /// 401: the agent refused the server's token.
    #[error("the agent refused the token")]
    Unauthorized,
    /// No answer: not reachable, or the reply broke off.
    #[error("the agent is not reachable")]
    Unreachable,
    /// The reply was larger than accepted.
    #[error("the agent sent too much data")]
    TooLarge,
    /// Any other error status.
    #[error("{0}")]
    Other(String),
}

/// The server's view of the region agent: one call per route, no state.
pub trait RegionAgent: Send + Sync + Debug {
    /// The current snapshot; with `since`, the agent waits for a newer one
    /// first. The error is a description to log.
    fn snapshot(&self, since: Option<u64>) -> BoxFuture<'_, Result<RegionsSnapshot, String>>;

    /// Runs `query` against the region holding `bbox`. `Ok(None)` when no
    /// region holds it; the error is a description to log.
    fn query<'a>(
        &'a self,
        bbox: &'a BBox,
        query: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Option<QueryResult>, String>>;

    /// The importable extracts (JSON).
    fn catalog(&self) -> BoxFuture<'_, Result<Bytes, AgentError>>;

    /// Download sizes of catalog extracts (JSON, `{"<id>": bytes or null}`).
    fn catalog_sizes<'a>(
        &'a self,
        req: &'a SizesRequest,
    ) -> BoxFuture<'a, Result<Bytes, AgentError>>;

    /// Starts or applies `action`; the agent answers with its new snapshot
    /// (JSON).
    fn act<'a>(
        &'a self,
        action: ActionKind,
        req: &'a RegionAction,
    ) -> BoxFuture<'a, Result<Bytes, AgentError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_routes_are_the_regions_route_and_the_action_name() {
        for kind in [
            ActionKind::Import,
            ActionKind::Cancel,
            ActionKind::Delete,
            ActionKind::Settings,
        ] {
            assert_eq!(
                paths::action(kind),
                format!("{}/{}", paths::REGIONS, kind.path())
            );
            assert_eq!(ActionKind::parse(kind.path()), Some(kind));
        }
        assert_eq!(paths::REGIONS, "/regions");
        assert_eq!(paths::CATALOG, "/catalog");
        assert_eq!(paths::CATALOG_SIZES, "/catalog/sizes");
        assert_eq!(paths::INTERPRETER, "/interpreter");
    }
}
