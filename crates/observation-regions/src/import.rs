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
use crate::registry::{Imported, JobId, RegionDb, Registry, slug};

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
    let db_dir = format!("{}-{}", slug(id), registry.now_unix());
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

pub(crate) async fn fetch_poly(http: &dyn HttpClient, url: &str) -> Result<String, String> {
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
    use crate::catalog::Extract;
    use crate::ports::{ManualClock, SystemClock};
    use crate::testing::{FAKE_TIMESTAMP, FakeDownload, FakeToolchain, Reply, Step, chunk};

    const PBF: &str = "https://download.geofabrik.de/europe/monaco-latest.osm.pbf";

    /// A registry whose `monaco` import is running, allowing `max` bytes
    /// of download.
    async fn running(
        dir: &Path,
        max: u64,
        clock: Arc<dyn crate::ports::Clock>,
    ) -> (Arc<Registry>, JobId) {
        let registry = Arc::new(Registry::open(dir, max, 7, clock).unwrap());
        registry.enqueue("monaco", "Monaco").unwrap();
        let (id, job) = registry.next_job().await;
        let task = tokio::spawn(std::future::pending::<()>());
        assert!(registry.started(&id, job, task.abort_handle()));
        (registry, job)
    }

    fn tools(download: FakeDownload, http: FakeHttp) -> Tools {
        Tools {
            http: Arc::new(http),
            download: Arc::new(download),
            toolchain: Arc::new(FakeToolchain::default()),
        }
    }

    /// Downloads `reply` with a cap of `max` bytes: the result, what was
    /// written, and how long it took on Tokio's clock.
    async fn fetch(reply: Reply, max: u64) -> (Result<(), String>, Option<Vec<u8>>, Duration) {
        let dir = tempfile::tempdir().unwrap();
        let (registry, job) = running(dir.path(), max, Arc::new(SystemClock)).await;
        let tools = tools(FakeDownload::new(reply), FakeHttp::new());
        let dest = dir.path().join("extract.osm.pbf");
        let started = tokio::time::Instant::now();
        let result = download(&tools, &registry, "monaco", job, PBF, &dest).await;
        (result, std::fs::read(&dest).ok(), started.elapsed())
    }

    /// What a failed or finished download leaves on disk.
    #[derive(Debug, Clone, Copy)]
    enum File {
        /// The file holds exactly these bytes.
        Is(&'static [u8]),
        /// The file was never created.
        Absent,
        /// Created; what reached it before the failure is not checked (the
        /// file is dropped unflushed).
        Unchecked,
    }

    /// Every way a download ends, with its sentence: the status texts, the
    /// 60 s waits for the head and for each piece, the size cap (declared
    /// and while streaming) and a body that breaks off. A body ending early
    /// is a body error from hyper ("error decoding response body", as
    /// `a_broken_stream_is_a_body_error_with_its_status` pins), so the
    /// import's own "ended early" check is defensive and not reached here.
    #[tokio::test(start_paused = true)]
    async fn downloads_end_as_described() {
        const MIB: u64 = 1 << 20;
        let fail = |e: &str| Err(e.to_owned());
        // The reply, the cap, the result, what is on disk, the seconds taken.
        type Case = (Reply, u64, Result<(), String>, File, u64);
        let cases: Vec<Case> = vec![
            (
                Reply::ok(Some(6), vec![chunk(b"abc"), chunk(b"def")]),
                MIB,
                Ok(()),
                File::Is(b"abcdef"),
                0,
            ),
            (
                Reply::ok(None, vec![chunk(b"abc")]),
                MIB,
                Ok(()),
                File::Is(b"abc"),
                0,
            ),
            (
                Reply::status(404, None),
                MIB,
                fail("could not download the extract: HTTP status client error (404 Not Found)"),
                File::Absent,
                0,
            ),
            (
                Reply::status(503, Some("Busy")),
                MIB,
                fail("could not download the extract: HTTP status server error (503 Busy)"),
                File::Absent,
                0,
            ),
            (
                Reply::Fail(TransportErrorKind::Connect),
                MIB,
                fail("could not download the extract: scripted Connect"),
                File::Absent,
                0,
            ),
            (
                Reply::Hang,
                MIB,
                fail("the download server did not answer; try again later"),
                File::Absent,
                60,
            ),
            (
                Reply::ok(Some(6), vec![chunk(b"abc"), Step::Hang]),
                MIB,
                fail("the download stalled; try again later"),
                File::Unchecked,
                60,
            ),
            // A body that ends before its declared length, as hyper reports it.
            (
                Reply::ok(
                    Some(6),
                    vec![
                        chunk(b"abc"),
                        Step::Fail(TransportErrorKind::Body, "error decoding response body"),
                    ],
                ),
                MIB,
                fail("the download failed: error decoding response body"),
                File::Unchecked,
                0,
            ),
            // The cap: declared, refused before the file is created ...
            (
                Reply::ok(Some(3 * MIB + 1), vec![]),
                3 * MIB,
                fail(
                    "the extract is 4 MB, more than the 3 MB this server accepts (OBS_REGIONS_MAX_DOWNLOAD_MB)",
                ),
                File::Absent,
                0,
            ),
            (
                Reply::ok(Some(10), vec![chunk(b"0123456789")]),
                10,
                Ok(()),
                File::Is(b"0123456789"),
                0,
            ),
            // ... and while streaming, at exactly one byte over.
            (
                Reply::ok(None, vec![chunk(b"012345"), chunk(b"6789")]),
                10,
                Ok(()),
                File::Is(b"0123456789"),
                0,
            ),
            (
                Reply::ok(None, vec![chunk(b"012345"), chunk(b"6789A")]),
                10,
                fail(
                    "the extract is 1 MB, more than the 0 MB this server accepts (OBS_REGIONS_MAX_DOWNLOAD_MB)",
                ),
                File::Unchecked,
                0,
            ),
        ];
        for (reply, max, want, on_disk, secs) in cases {
            let label = format!("{reply:?}, max {max}");
            // A lost deadline fails here rather than hanging the test.
            let (got, file, took) =
                tokio::time::timeout(Duration::from_secs(3600), fetch(reply, max))
                    .await
                    .unwrap_or_else(|_| panic!("{label}: still waiting after an hour"));
            assert_eq!(got, want, "{label}");
            match on_disk {
                File::Is(bytes) => assert_eq!(file.as_deref(), Some(bytes), "{label}"),
                File::Absent => assert_eq!(file, None, "{label}: the file was created"),
                File::Unchecked => assert!(file.is_some(), "{label}: no file"),
            }
            assert_eq!(took, Duration::from_secs(secs), "{label}");
        }
    }

    /// Dropping the import (a cancel) drops the download's body, which ends
    /// the transfer.
    #[tokio::test(start_paused = true)]
    async fn a_cancelled_download_drops_its_body() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, job) = running(dir.path(), 1 << 20, Arc::new(SystemClock)).await;
        let fake = Arc::new(FakeDownload::new(Reply::ok(
            None,
            vec![chunk(b"abc"), Step::Hang],
        )));
        let tools = Tools {
            http: Arc::new(FakeHttp::new()),
            download: fake.clone(),
            toolchain: Arc::new(FakeToolchain::default()),
        };
        let dest = dir.path().join("extract.osm.pbf");
        let task =
            tokio::spawn(
                async move { download(&tools, &registry, "monaco", job, PBF, &dest).await },
            );
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!fake.body_dropped.load(std::sync::atomic::Ordering::SeqCst));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(fake.body_dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(*fake.urls.lock().unwrap(), [PBF]);
    }

    /// A whole import on fakes: it reports each phase and the load's
    /// progress, the tools run in order with the checked timestamp, the
    /// extract is gone once filtered, and the database lands in
    /// `db/<slug>-<clock's now>` with its boundary.
    #[tokio::test(start_paused = true)]
    async fn an_import_lands_in_a_directory_named_by_the_clock() {
        const SQUARE: &str = "sq\n1\n 0 0\n 1 0\n 1 1\n 0 1\nEND\nEND\n";
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::at_unix(1_700_000_000));
        let (registry, job) = running(dir.path(), 1 << 20, clock).await;
        let toolchain = Arc::new(FakeToolchain::default());
        let tools = Tools {
            http: Arc::new(FakeHttp::replying([FakeReply::ok(SQUARE)])),
            download: Arc::new(FakeDownload::new(Reply::ok(
                Some(3),
                vec![Step::Pause, chunk(b"pbf"), Step::Pause],
            ))),
            toolchain: toolchain.clone(),
        };
        let extract = Extract {
            entry: observation_core::regions::CatalogEntry {
                id: "monaco".into(),
                name: "Monaco".into(),
                parent: None,
            },
            pbf_url: PBF.into(),
            poly_url: "https://download.geofabrik.de/europe/monaco.poly".into(),
        };
        // Every published state of the job, in order.
        let mut published = registry.subscribe();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let watcher = tokio::spawn(async move {
            while published.changed().await.is_ok() {
                let snapshot = published.borrow_and_update().clone();
                if let Some(j) = snapshot.regions.iter().find_map(|r| r.job.clone()) {
                    record.lock().unwrap().push((j.phase, j.done, j.total));
                }
            }
        });
        let imported = run(&tools, &registry, job, &extract).await.unwrap();
        // Let the watcher see the last state.
        tokio::time::sleep(Duration::from_millis(1)).await;
        watcher.abort();
        use observation_core::regions::JobPhase::{Downloading, Filtering, Finishing, Importing};
        assert_eq!(
            *seen.lock().unwrap(),
            [
                (Downloading, 0, Some(3)),
                (Downloading, 3, Some(3)),
                (Filtering, 0, None),
                (Importing, 0, Some(3)),
                (Importing, 1, Some(3)),
                (Importing, 3, Some(3)),
                (Finishing, 0, None),
            ]
        );
        assert_eq!(imported.db_dir, "monaco-1700000000");
        assert_eq!(imported.data_timestamp.as_deref(), Some(FAKE_TIMESTAMP));
        let db = dir.path().join("db/monaco-1700000000");
        assert_eq!(imported.db.dir, db);
        assert_eq!(
            std::fs::read_to_string(db.join("region.poly")).unwrap(),
            SQUARE
        );
        assert!(
            !dir.path().join("work/monaco").exists(),
            "the work directory is gone"
        );
        assert_eq!(
            *toolchain.calls.lock().unwrap(),
            [
                "tags_filter",
                "file_info (extract kept: false)",
                &format!("load Some({FAKE_TIMESTAMP:?})")
            ]
        );
    }

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
