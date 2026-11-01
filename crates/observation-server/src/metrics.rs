//! Prometheus metrics: HTTP request middleware and an optional exporter.
//!
//! Instrumentation uses the `metrics` facade, which does nothing until a
//! recorder is installed, and [`crate::app`] adds the request middleware only when
//! `OBS_METRICS_BIND` is set, so tests and library users pay nothing. The binary
//! installs the Prometheus recorder only when `OBS_METRICS_BIND` is set and
//! serves it on that separate address, keeping `/metrics` off the public port.

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
/// Bundle cache outcomes: `hit` per request served from a fresh file; `built`,
/// `stale_served` and `error` per upstream refresh, which concurrent requests share.
pub const BUNDLE_CACHE: &str = "observation_bundle_cache_total";
/// Tile lookups: `result` is `hit`, `miss` (nothing cached), `refreshed` (expired copy
/// replaced), `revalidated` (304), `stale_served` or `error` (upstream failed, no copy).
pub const TILE_CACHE: &str = "observation_tile_cache_total";
/// Overpass fetches: `outcome` is `ok`, `error` or `rate_limited` (including our own cooldown).
pub const OVERPASS_FETCH: &str = "observation_overpass_fetches_total";
/// Bundle builds tried against a local region: `outcome` is `ok`, `not_covered`
/// (the agent has no region holding the area) or `error`; the last two fall back to Overpass.
pub const REGION_QUERY: &str = "observation_region_queries_total";

/// Latency bucket upper bounds in seconds; the request timeout is 80 s.
const DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 80.0,
];

/// Installs the global Prometheus recorder and returns the render handle.
/// Latency is exported as a real histogram (aggregatable across replicas).
pub fn install() -> Result<PrometheusHandle, BuildError> {
    PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Full(HTTP_DURATION.to_owned()), DURATION_BUCKETS)?
        .install_recorder()
}

/// Serves `GET /metrics` on `listener` until the process exits. The caller binds the
/// listener first so that a busy port stops start-up instead of failing in a background task.
pub async fn serve(
    listener: tokio::net::TcpListener,
    handle: PrometheusHandle,
) -> std::io::Result<()> {
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

/// Records a request when dropped: with its status once the response is ready, or as
/// `cancelled` if the future is dropped first (the client went away or the connection
/// closed), so abandoned requests still show up in the counts and latency.
struct RequestRecord {
    method: &'static str,
    route: String,
    start: Instant,
    status: Option<String>,
}

impl Drop for RequestRecord {
    fn drop(&mut self) {
        let status = self.status.take().unwrap_or_else(|| "cancelled".to_owned());
        let route = std::mem::take(&mut self.route);
        metrics::histogram!(HTTP_DURATION, "method" => self.method, "route" => route.clone())
            .record(self.start.elapsed().as_secs_f64());
        metrics::counter!(HTTP_REQUESTS, "method" => self.method, "route" => route, "status" => status)
            .increment(1);
    }
}

/// Middleware counting requests and timing them. The route label is the
/// matched pattern (`/api/tiles/{z}/{x}/{y}`), never the raw path, so label
/// cardinality stays bounded. [`crate::app`] adds it only when metrics are enabled.
pub async fn track(req: Request, next: Next) -> Response {
    let mut record = RequestRecord {
        method: method_label(req.method()),
        route: req
            .extensions()
            .get::<MatchedPath>()
            .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned()),
        start: Instant::now(),
        status: None,
    };
    let resp = next.run(req).await;
    record.status = Some(resp.status().as_u16().to_string());
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

    #[tokio::test]
    async fn abandoned_requests_are_counted() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _guard = metrics::set_default_local_recorder(&recorder);

        let app = Router::new()
            .route("/hang", get(std::future::pending::<&'static str>))
            .layer(axum::middleware::from_fn(track));
        // The client gives up: the request future is dropped before a response exists.
        let _ = tokio::time::timeout(
            Duration::from_millis(20),
            app.oneshot(Request::get("/hang").body(Body::empty()).unwrap()),
        )
        .await;

        let text = handle.render();
        assert!(
            text.contains(r#"route="/hang""#) && text.contains(r#"status="cancelled""#),
            "{text}"
        );
    }
}
