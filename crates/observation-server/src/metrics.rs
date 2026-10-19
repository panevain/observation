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
use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder, PrometheusHandle};

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

/// Latency bucket upper bounds in seconds; the request timeout is 60 s.
const DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

/// Installs the global Prometheus recorder and returns the render handle.
/// Latency is exported as a real histogram (aggregatable across replicas).
pub fn install() -> Result<PrometheusHandle, BuildError> {
    PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Full(HTTP_DURATION.to_owned()), DURATION_BUCKETS)?
        .install_recorder()
}

/// Serves `GET /metrics` on `addr` until the process exits.
pub async fn serve(addr: SocketAddr, handle: PrometheusHandle) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "serving metrics");
    // `install_recorder` does not run upkeep, which drains histogram buffers.
    let upkeep = handle.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            upkeep.run_upkeep();
        }
    });
    let app = Router::new().route("/metrics", get(move || async move { handle.render() }));
    axum::serve(listener, app).await
}

/// Standard methods keep their name; anything else (clients may send any token)
/// becomes `other`, so the label set is fixed.
fn method_label(m: &axum::http::Method) -> &'static str {
    use axum::http::Method;
    match *m {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        Method::PATCH => "PATCH",
        Method::CONNECT => "CONNECT",
        Method::TRACE => "TRACE",
        _ => "other",
    }
}

/// Middleware counting requests and timing them. The route label is the
/// matched pattern (`/api/tiles/{z}/{x}/{y}`), never the raw path, so label
/// cardinality stays bounded.
pub async fn track(req: Request, next: Next) -> Response {
    let method = method_label(req.method());
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let start = Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16().to_string();
    metrics::histogram!(HTTP_DURATION, "method" => method, "route" => route.clone())
        .record(start.elapsed().as_secs_f64());
    metrics::counter!(HTTP_REQUESTS, "method" => method, "route" => route, "status" => status)
        .increment(1);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, StatusCode};
    use std::time::Duration;
    use tower::ServiceExt;
    use tower_http::timeout::TimeoutLayer;

    #[test]
    fn unknown_methods_share_one_label() {
        assert_eq!(method_label(&Method::GET), "GET");
        let custom = Method::from_bytes(b"FOOBAR").unwrap();
        assert_eq!(method_label(&custom), "other");
    }

    // A current-thread runtime keeps the thread-local recorder in place across awaits.
    #[tokio::test]
    async fn timed_out_requests_are_counted() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _guard = metrics::set_default_local_recorder(&recorder);

        // The same order as `crate::app`: metrics outside the timeout.
        let app = Router::new()
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "late"
                }),
            )
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                Duration::from_millis(20),
            ))
            .layer(axum::middleware::from_fn(track));
        let resp = app
            .oneshot(Request::get("/slow").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);

        let text = handle.render();
        assert!(
            text.contains(r#"route="/slow""#) && text.contains(r#"status="408""#),
            "{text}"
        );
    }
}
