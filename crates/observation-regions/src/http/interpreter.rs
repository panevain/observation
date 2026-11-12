//! `/interpreter`: Overpass queries against the region holding a box.

use std::sync::Arc;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use observation_core::geo::BBox;
use observation_upstream::QueueError;
use serde::Deserialize;

use super::{AgentError, err};
use crate::agent::Agent;
use crate::import;
use crate::registry::Covering;

/// Largest query text accepted.
const MAX_QUERY_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize)]
pub(super) struct BBoxParam {
    bbox: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct QueryForm {
    data: String,
}

pub(super) async fn interpreter(
    State(agent): State<Arc<Agent>>,
    Query(p): Query<BBoxParam>,
    Form(form): Form<QueryForm>,
) -> Result<Response, AgentError> {
    let bbox = BBox::parse(&p.bbox).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    if form.data.len() > MAX_QUERY_BYTES {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "query too long"));
    }
    let Covering {
        id: region,
        db,
        data_timestamp,
    } = agent
        .registry
        .covering(&bbox)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no imported region holds this area"))?;
    let _permit = agent.queries.enter().await.map_err(|e| match e {
        QueueError::Full => err(StatusCode::SERVICE_UNAVAILABLE, "too many queries waiting"),
        QueueError::Closed(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    })?;
    let started = std::time::Instant::now();
    let out = tokio::time::timeout(
        agent.query_timeout,
        agent.query_runner.run(&db.dir, &form.data),
    )
    .await
    .map_err(|_| err(StatusCode::GATEWAY_TIMEOUT, "the query took too long"))?
    .map_err(|e| {
        tracing::warn!(%region, error = %e, "query failed");
        err(StatusCode::INTERNAL_SERVER_ERROR, e)
    })?;
    tracing::debug!(%region, ms = started.elapsed().as_millis() as u64, bytes = out.len(), "query");
    let mut resp = ([(header::CONTENT_TYPE, "application/json")], out).into_response();
    if let Ok(v) = HeaderValue::from_str(&region) {
        resp.headers_mut().insert("x-region", v);
    }
    // Databases imported before they recorded their own date have no
    // `timestamp_osm_base` in the output, so the date travels in a header.
    if let Some(v) = data_timestamp
        .as_deref()
        .filter(|t| import::is_osm_timestamp(t))
        .and_then(|t| HeaderValue::from_str(t).ok())
    {
        resp.headers_mut()
            .insert(observation_core::regions::OSM_BASE_HEADER, v);
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::query::tests::fake_query_bin;
    use crate::{poly, registry};

    /// Answers `POST /interpreter` for a box inside a ready region that has
    /// `data_timestamp`, through a stand-in `osm3s_query`.
    #[cfg(unix)]
    async fn interpreter_headers(data_timestamp: Option<&str>) -> axum::http::HeaderMap {
        let bin = fake_query_bin("echo '{\"elements\":[]}'");
        let data = tempfile::tempdir().unwrap();
        let config = Config::from_lookup(|k| match k {
            "OBS_REGIONS_DATA_DIR" => Some(data.path().display().to_string()),
            "OBS_REGIONS_OVERPASS_BIN" => Some(bin.path().display().to_string()),
            _ => None,
        })
        .unwrap();
        let agent = Agent::new(&config).unwrap();
        install_monaco(&agent, data.path(), data_timestamp).await;
        let resp = ask(agent).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        resp.headers().clone()
    }

    /// Makes `monaco`, a unit square, a ready region with `data_timestamp`.
    async fn install_monaco(agent: &Agent, data: &std::path::Path, data_timestamp: Option<&str>) {
        let square = "sq\n1\n 0 0\n 1 0\n 1 1\n 0 1\nEND\nEND\n";
        let db_dir = data.join("db/monaco-1");
        std::fs::create_dir_all(&db_dir).unwrap();
        agent.registry.enqueue("monaco", "Monaco").unwrap();
        let (id, job) = agent.registry.next_job().await;
        assert!(agent.registry.started(
            &id,
            job,
            tokio::spawn(std::future::pending::<()>()).abort_handle()
        ));
        let boundary = poly::Boundary::parse(square).unwrap();
        agent.registry.finish(
            &id,
            job,
            Some(Ok(registry::Imported {
                bbox: boundary.bbox(),
                db: Arc::new(registry::RegionDb::new(db_dir, boundary)),
                db_dir: "monaco-1".into(),
                data_timestamp: data_timestamp.map(str::to_owned),
                disk_bytes: 0,
            })),
        );
    }

    /// Asks for a box inside `monaco`.
    async fn ask(agent: Arc<Agent>) -> Result<Response, AgentError> {
        interpreter(
            State(agent),
            Query(BBoxParam {
                bbox: "0.2,0.2,0.3,0.3".into(),
            }),
            Form(QueryForm { data: "q".into() }),
        )
        .await
    }

    /// With one query running and 32 waiting, the next is turned away at
    /// once with 503 "too many queries waiting"; the others all answer.
    #[tokio::test]
    async fn a_full_queue_answers_503() {
        use futures_util::FutureExt;
        use observation_upstream::testing::FakeHttp;
        use std::sync::atomic::Ordering;

        use crate::agent::Ports;
        use crate::ports::SystemClock;
        use crate::testing::{FakeDownload, FakeToolchain, GateRunner, Reply};

        let data = tempfile::tempdir().unwrap();
        let config = Config::from_lookup(|k| match k {
            "OBS_REGIONS_DATA_DIR" => Some(data.path().display().to_string()),
            "OBS_REGIONS_QUERY_CONCURRENCY" => Some("1".into()),
            _ => None,
        })
        .unwrap();
        let runner = Arc::new(GateRunner::new());
        let ports = Ports {
            http: Arc::new(FakeHttp::new()),
            download: Arc::new(FakeDownload::new(Reply::Hang)),
            toolchain: Arc::new(FakeToolchain::default()),
            query_runner: runner.clone(),
            clock: Arc::new(SystemClock),
        };
        let agent = Agent::from_parts(&config, ports).unwrap();
        install_monaco(&agent, data.path(), None).await;

        let queued: Vec<_> = (0..33)
            .map(|_| tokio::spawn(ask(Arc::clone(&agent))))
            .collect();
        // Let every task reach the queue (this runtime has one thread).
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert_eq!(runner.started.load(Ordering::SeqCst), 1);
        let refused = ask(Arc::clone(&agent))
            .now_or_never()
            .expect("turned away at once")
            .unwrap_err();
        assert_eq!(refused.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(refused.1, "too many queries waiting");

        runner.release(33);
        for task in queued {
            assert_eq!(task.await.unwrap().unwrap().status(), StatusCode::OK);
        }
        assert_eq!(runner.started.load(Ordering::SeqCst), 33);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn answers_carry_the_regions_data_date() {
        let h = interpreter_headers(Some("2026-10-31T19:43:02Z")).await;
        assert_eq!(h["x-region"], "monaco");
        assert_eq!(h["x-osm-base"], "2026-10-31T19:43:02Z");

        let h = interpreter_headers(None).await;
        assert_eq!(h["x-region"], "monaco");
        assert!(!h.contains_key("x-osm-base"));

        let h = interpreter_headers(Some("yesterday")).await;
        assert!(!h.contains_key("x-osm-base"));
    }
}
