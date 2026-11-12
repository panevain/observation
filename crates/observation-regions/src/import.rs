//! One region import: download the extract and its boundary, keep only
//! buildings, roads and address nodes (`osmium tags-filter`), and load the
//! result into a new Overpass database (`osmium cat | update_database`),
//! reporting progress.
//!
//! Everything is written below `work/<slug>/` and only the finished database
//! is moved into `db/`, so a cancelled or failed import leaves nothing behind
//! once the work directory is removed. Child processes are killed when the
//! job's future is dropped (cancellation).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use observation_core::regions::JobPhase;
use observation_upstream::{HttpClient, HttpRequest, status_error};
use tokio::io::AsyncWriteExt;

use crate::catalog::Extract;
use crate::poly::{Boundary, MAX_POLY_BYTES};
use crate::ports::{Download, Toolchain, checked_body};
use crate::registry::{Imported, JobId, RegionDb, Registry, now_unix, slug};

/// What an import talks to.
#[derive(Debug, Clone)]
pub struct Tools {
    /// For the boundary.
    pub http: Arc<dyn HttpClient>,
    pub download: Arc<dyn Download>,
    pub toolchain: Arc<dyn Toolchain>,
}

/// Longest wait for the next piece of a download.
const READ_STALL: Duration = Duration::from_secs(60);

/// Runs the import of `extract` and returns the new database. Errors are
/// sentences for the UI.
pub async fn run(
    tools: &Tools,
    registry: &Arc<Registry>,
    job: JobId,
    extract: &Extract,
) -> Result<Imported, String> {
    let id = extract.entry.id.as_str();
    let work = registry.data_dir().join("work").join(slug(id));
    let _ = tokio::fs::remove_dir_all(&work).await;
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|e| format!("could not create a work directory: {e}"))?;

    let pbf = work.join("extract.osm.pbf");
    download(tools, registry, id, job, &extract.pbf_url, &pbf).await?;
    let poly = fetch_poly(&*tools.http, &extract.poly_url).await?;
    let boundary = Boundary::parse(&poly).map_err(|e| e.to_string())?;

    registry.progress(id, job, JobPhase::Filtering, 0, None);
    let filtered = work.join("filtered.osm.pbf");
    tools.toolchain.tags_filter(&pbf, &filtered).await?;
    let _ = tokio::fs::remove_file(&pbf).await;

    let info = tools.toolchain.file_info(&filtered).await?;
    let total = info.elements;
    registry.progress(id, job, JobPhase::Importing, 0, Some(total));
    let db = work.join("db");
    tokio::fs::create_dir_all(&db)
        .await
        .map_err(|e| format!("could not create the database directory: {e}"))?;
    let version = info
        .replication_timestamp
        .as_deref()
        .filter(|t| is_osm_timestamp(t));
    let mut progress = |done| registry.progress(id, job, JobPhase::Importing, done, Some(total));
    tools
        .toolchain
        .load(&filtered, &db, version, &mut progress)
        .await?;
    let _ = tokio::fs::remove_file(&filtered).await;

    registry.progress(id, job, JobPhase::Finishing, 0, None);
    tokio::fs::write(db.join("region.poly"), &poly)
        .await
        .map_err(|e| format!("could not save the region boundary: {e}"))?;
    let db_root = registry.data_dir().join("db");
    tokio::fs::create_dir_all(&db_root)
        .await
        .map_err(|e| format!("could not create the database directory: {e}"))?;
    let db_dir = format!("{}-{}", slug(id), now_unix());
    let dest = db_root.join(&db_dir);
    let _ = tokio::fs::remove_dir_all(&dest).await;
    tokio::fs::rename(&db, &dest)
        .await
        .map_err(|e| format!("could not move the new database into place: {e}"))?;
    let _ = tokio::fs::remove_dir_all(&work).await;
    let disk_bytes = dir_size(&dest);
    Ok(Imported {
        bbox: boundary.bbox(),
        db: Arc::new(RegionDb::new(dest, boundary)),
        db_dir,
        data_timestamp: info.replication_timestamp,
        disk_bytes,
    })
}

async fn download(
    tools: &Tools,
    registry: &Registry,
    id: &str,
    job: JobId,
    url: &str,
    dest: &Path,
) -> Result<(), String> {
    let max = registry.max_download_bytes();
    // The body may take hours, so only the wait for the headers is bounded here
    // (and each piece of the body below).
    let resp = tokio::time::timeout(READ_STALL, tools.download.get(url))
        .await
        .map_err(|_| "the download server did not answer; try again later".to_owned())?
        .map_err(|e| format!("could not download the extract: {e}"))?;
    if resp.status.is_client_error() || resp.status.is_server_error() {
        let e = status_error(resp.status, resp.reason.as_deref());
        return Err(format!("could not download the extract: {e}"));
    }
    let total = resp.content_length;
    let mut body = resp.body;
    let too_big = |n: u64| {
        format!(
            "the extract is {} MB, more than the {} MB this server accepts (OBS_REGIONS_MAX_DOWNLOAD_MB)",
            n.div_ceil(1 << 20),
            max >> 20
        )
    };
    if let Some(n) = total.filter(|&n| n > max) {
        return Err(too_big(n));
    }
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| format!("could not store the download: {e}"))?;
    let mut done = 0u64;
    registry.progress(id, job, JobPhase::Downloading, 0, total);
    loop {
        let chunk = tokio::time::timeout(READ_STALL, body.chunk())
            .await
            .map_err(|_| "the download stalled; try again later".to_owned())?
            .map_err(|e| format!("the download failed: {e}"))?;
        let Some(chunk) = chunk else { break };
        done += chunk.len() as u64;
        if done > max {
            return Err(too_big(done));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("could not store the download: {e}"))?;
        registry.progress(id, job, JobPhase::Downloading, done, total);
    }
    file.flush()
        .await
        .map_err(|e| format!("could not store the download: {e}"))?;
    if total.is_some_and(|t| t != done) {
        return Err("the download ended early; try again".into());
    }
    Ok(())
}

async fn fetch_poly(http: &dyn HttpClient, url: &str) -> Result<String, String> {
    let fail = |e: String| format!("could not download the region boundary: {e}");
    let req = HttpRequest::get(url, MAX_POLY_BYTES)
        .timeout(Duration::from_secs(60))
        .skip_error_bodies();
    let resp = http.send(req).await.map_err(|e| fail(e.to_string()))?;
    let bytes = checked_body(resp)
        .map_err(fail)?
        .ok_or_else(|| fail("it is unexpectedly large".into()))?;
    String::from_utf8(bytes.to_vec()).map_err(|_| fail("it is not text".into()))
}

/// Whether `s` is an OSM data date as Overpass reports it,
/// `YYYY-MM-DDTHH:MM:SSZ` with a real date and time (the same check the
/// server applies: [`observation_core::provenance::parse_utc`]). Checked
/// before a timestamp reaches a command line or a response header.
pub fn is_osm_timestamp(s: &str) -> bool {
    observation_core::provenance::parse_utc(s).is_some()
}

/// Total size of the files below `dir`.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use observation_upstream::TransportErrorKind;
    use observation_upstream::testing::{FakeHttp, FakeReply};

    use super::*;

    /// The boundary: a 60 s GET whose error bodies are not read, capped at
    /// 32 MiB, and the sentence for each way it fails.
    #[tokio::test]
    async fn boundary_failures_are_described() {
        const URL: &str = "https://download.geofabrik.de/europe/monaco.poly";
        let cases = [
            (FakeReply::ok("sq\nEND\n"), Ok("sq\nEND\n".to_owned())),
            (
                FakeReply::status(StatusCode::NOT_FOUND),
                Err("HTTP status client error (404 Not Found)"),
            ),
            (
                FakeReply::status(StatusCode::SERVICE_UNAVAILABLE).reason("Back Soon"),
                Err("HTTP status server error (503 Back Soon)"),
            ),
            (
                FakeReply::ok(vec![b'x'; MAX_POLY_BYTES + 1]),
                Err("it is unexpectedly large"),
            ),
            (
                FakeReply::ok(vec![b'x'; MAX_POLY_BYTES]),
                Ok("x".repeat(MAX_POLY_BYTES)),
            ),
            (FakeReply::ok(vec![0xff]), Err("it is not text")),
            (FakeReply::ok(""), Ok(String::new())),
            (
                FakeReply::error(TransportErrorKind::Connect),
                Err("FakeHttp: scripted Connect"),
            ),
        ];
        for (reply, want) in cases {
            let fake = FakeHttp::replying([reply]);
            let got = fetch_poly(&fake, URL).await;
            let want = want.map_err(|e| format!("could not download the region boundary: {e}"));
            assert_eq!(got, want);
            let req = &fake.requests()[0];
            assert_eq!((&req.method, req.url.as_str()), (&http::Method::GET, URL));
            assert_eq!(req.timeout, Some(Duration::from_secs(60)));
            assert_eq!(req.max_bytes, 32 * 1024 * 1024);
            assert!(req.skip_error_bodies);
            fake.assert_drained();
        }
    }

    #[test]
    fn accepts_only_iso_utc_timestamps() {
        assert!(is_osm_timestamp("2026-10-31T19:43:02Z"));
        for bad in [
            "",
            "2026-10-31T19:43:02",
            "2026-10-31 19:43:02Z",
            "2026-10-31T19:43:02+00:00",
            "2026-10-31T19:43:02.5Z",
            "2026-10-31T19:43:02Zx",
            "2026-10-31T19:43:0aZ",
            "2026-10-31T19:43:02Z --db-dir=/",
            "２０２６-10-31T19:43:02Z",
            "2026-10-31T19:43:02\nZ",
            "2026-02-30T12:00:00Z",
            "2026-10-31T25:61:61Z",
        ] {
            assert!(!is_osm_timestamp(bad), "{bad:?}");
        }
    }
}
