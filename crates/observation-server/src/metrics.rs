//! Prometheus metrics: HTTP request middleware and an optional exporter.
//!
//! Instrumentation uses the `metrics` facade, which does nothing until a
//! recorder is installed, so tests and library users pay nothing. The binary
//! installs the Prometheus recorder only when `OBS_METRICS_BIND` is set and
//! serves it on that separate address, keeping `/metrics` off the public port.

use std::net::SocketAddr;
use std::time::Instant;

use axum::Router;
use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::get;
use metrics_exporter_prometheus::{BuildError, PrometheusBuilder, PrometheusHandle};

/// Requests received, labelled by method, matched route and status.
pub const HTTP_REQUESTS: &str = "observation_http_requests_total";
/// Request latency in seconds, labelled by method and matched route.
pub const HTTP_DURATION: &str = "observation_http_request_duration_seconds";
/// Bundle lookups: `result` is `hit`, `miss` or `stale_served`.
pub const BUNDLE_CACHE: &str = "observation_bundle_cache_total";
/// Tile lookups: `result` is `hit`, `miss`, `revalidated` or `stale_served`.
pub const TILE_CACHE: &str = "observation_tile_cache_total";
/// Overpass fetches: `outcome` is `ok`, `error` or `rate_limited` (including our own cooldown).
pub const OVERPASS_FETCH: &str = "observation_overpass_fetches_total";

/// Installs the global Prometheus recorder and returns the render handle.
pub fn install() -> Result<PrometheusHandle, BuildError> {
    PrometheusBuilder::new().install_recorder()
}

/// Serves `GET /metrics` on `addr` until the process exits.
pub async fn serve(addr: SocketAddr, handle: PrometheusHandle) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "serving metrics");
    let app = Router::new().route("/metrics", get(move || async move { handle.render() }));
    axum::serve(listener, app).await
}

/// Middleware counting requests and timing them. The route label is the
/// matched pattern (`/api/tiles/{z}/{x}/{y}`), never the raw path, so label
/// cardinality stays bounded.
pub async fn track(req: Request, next: Next) -> Response {
    let method = req.method().to_string();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let start = Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16().to_string();
    metrics::histogram!(HTTP_DURATION, "method" => method.clone(), "route" => route.clone())
        .record(start.elapsed().as_secs_f64());
    metrics::counter!(HTTP_REQUESTS, "method" => method, "route" => route, "status" => status)
        .increment(1);
    resp
}
