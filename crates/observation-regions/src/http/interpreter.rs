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
use crate::query::run_query;
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
        run_query(&agent.tools.overpass_bin, &db.dir, &form.data),
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

        let square = "sq\n1\n 0 0\n 1 0\n 1 1\n 0 1\nEND\nEND\n";
        let db_dir = data.path().join("db/monaco-1");
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

        let resp = interpreter(
            State(agent),
            Query(BBoxParam {
                bbox: "0.2,0.2,0.3,0.3".into(),
            }),
            Form(QueryForm { data: "q".into() }),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        resp.headers().clone()
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
