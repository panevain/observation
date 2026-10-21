//! One region import: download the extract and its boundary, keep only
//! buildings and roads (`osmium tags-filter`), and load the result into a new
//! Overpass database (`osmium cat | update_database`), reporting progress.
//!
//! Everything is written below `work/<slug>/` and only the finished database
//! is moved into `db/`, so a cancelled or failed import leaves nothing behind
//! once the work directory is removed. Child processes are killed when the
//! job's future is dropped (cancellation).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use observation_core::regions::JobPhase;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::catalog::Extract;
use crate::poly::{Boundary, MAX_POLY_BYTES};
use crate::registry::{Imported, RegionDb, Registry, now_unix, slug};

/// The tools and limits an import needs.
#[derive(Debug, Clone)]
pub struct Tools {
    pub client: reqwest::Client,
    pub osmium: PathBuf,
    pub overpass_bin: PathBuf,
}

/// Longest wait for the next piece of a download.
const READ_STALL: Duration = Duration::from_secs(60);

/// Runs the import of `extract` and returns the new database. Errors are
/// sentences for the UI.
pub async fn run(
    tools: &Tools,
    registry: &Arc<Registry>,
    extract: &Extract,
) -> Result<Imported, String> {
    let id = extract.entry.id.as_str();
    let work = registry.data_dir().join("work").join(slug(id));
    let _ = tokio::fs::remove_dir_all(&work).await;
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|e| format!("could not create a work directory: {e}"))?;

    let pbf = work.join("extract.osm.pbf");
    download(tools, registry, id, &extract.pbf_url, &pbf).await?;
    let poly = fetch_poly(tools, &extract.poly_url).await?;
    let boundary = Boundary::parse(&poly).map_err(|e| e.to_string())?;

    registry.progress(id, JobPhase::Filtering, 0, None);
    let filtered = work.join("filtered.osm.pbf");
    let mut filter = Command::new(&tools.osmium);
    filter
        .arg("tags-filter")
        .arg(&pbf)
        .args(["w/building", "r/building", "w/highway"])
        .arg("-o")
        .arg(&filtered)
        .arg("--overwrite");
    run_tool(filter, "osmium tags-filter").await?;
    let _ = tokio::fs::remove_file(&pbf).await;

    let info = file_info(tools, &filtered).await?;
    let total = info.data.count.nodes + info.data.count.ways + info.data.count.relations;
    registry.progress(id, JobPhase::Importing, 0, Some(total));
    let db = work.join("db");
    tokio::fs::create_dir_all(&db)
        .await
        .map_err(|e| format!("could not create the database directory: {e}"))?;
    import(tools, registry, id, &filtered, &db, total).await?;
    let _ = tokio::fs::remove_file(&filtered).await;

    registry.progress(id, JobPhase::Finishing, 0, None);
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
        data_timestamp: info.header.option.osmosis_replication_timestamp,
        disk_bytes,
    })
}

async fn download(
    tools: &Tools,
    registry: &Registry,
    id: &str,
    url: &str,
    dest: &Path,
) -> Result<(), String> {
    let max = registry.max_download_bytes();
    let mut resp = tools
        .client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("could not download the extract: {}", e.without_url()))?;
    let total = resp.content_length();
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
    registry.progress(id, JobPhase::Downloading, 0, total);
    loop {
        let chunk = tokio::time::timeout(READ_STALL, resp.chunk())
            .await
            .map_err(|_| "the download stalled; try again later".to_owned())?
            .map_err(|e| format!("the download failed: {}", e.without_url()))?;
        let Some(chunk) = chunk else { break };
        done += chunk.len() as u64;
        if done > max {
            return Err(too_big(done));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("could not store the download: {e}"))?;
        registry.progress(id, JobPhase::Downloading, done, total);
    }
    file.flush()
        .await
        .map_err(|e| format!("could not store the download: {e}"))?;
    if total.is_some_and(|t| t != done) {
        return Err("the download ended early; try again".into());
    }
    Ok(())
}

async fn fetch_poly(tools: &Tools, url: &str) -> Result<String, String> {
    let fail = |e: String| format!("could not download the region boundary: {e}");
    let resp = tools
        .client
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| fail(e.without_url().to_string()))?;
    let bytes = crate::read_capped(resp, MAX_POLY_BYTES)
        .await
        .map_err(|e| fail(e.without_url().to_string()))?
        .ok_or_else(|| fail("it is unexpectedly large".into()))?;
    String::from_utf8(bytes.to_vec()).map_err(|_| fail("it is not text".into()))
}

#[derive(Debug, Deserialize)]
struct FileInfo {
    header: Header,
    data: Data,
}

#[derive(Debug, Deserialize)]
struct Header {
    #[serde(default)]
    option: HeaderOptions,
}

#[derive(Debug, Default, Deserialize)]
struct HeaderOptions {
    osmosis_replication_timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Data {
    count: Counts,
}

#[derive(Debug, Deserialize)]
struct Counts {
    nodes: u64,
    ways: u64,
    relations: u64,
}

async fn file_info(tools: &Tools, file: &Path) -> Result<FileInfo, String> {
    let mut cmd = Command::new(&tools.osmium);
    cmd.arg("fileinfo").arg("-e").arg("-j").arg(file);
    let out = run_tool(cmd, "osmium fileinfo").await?;
    serde_json::from_slice(&out).map_err(|e| format!("osmium fileinfo output unreadable: {e}"))
}

/// Pipes `osmium cat -f osm` into `update_database`, counting elements on the
/// way for progress.
async fn import(
    tools: &Tools,
    registry: &Registry,
    id: &str,
    filtered: &Path,
    db: &Path,
    total: u64,
) -> Result<(), String> {
    let mut cat = Command::new(&tools.osmium);
    cat.arg("cat")
        .arg(filtered)
        .args(["-f", "osm", "-o", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut update = Command::new(tools.overpass_bin.join("update_database"));
    update
        .arg(format!("--db-dir={}/", db.display()))
        .args([
            "--meta=no",
            "--compression-method=lz4",
            "--map-compression-method=lz4",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut cat = cat
        .spawn()
        .map_err(|e| format!("could not start osmium: {e}"))?;
    let mut update = update
        .spawn()
        .map_err(|e| format!("could not start update_database: {e}"))?;
    let cat_err = tokio::spawn(tail(cat.stderr.take()));
    let update_err = tokio::spawn(tail(update.stderr.take()));
    let (Some(mut xml), Some(mut sink)) = (cat.stdout.take(), update.stdin.take()) else {
        return Err("could not connect osmium to update_database".into());
    };

    let mut counter = ElementCounter::default();
    let mut buf = vec![0u8; 256 * 1024];
    let relay = async {
        loop {
            let n = xml.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            counter.feed(&buf[..n]);
            sink.write_all(&buf[..n]).await?;
            registry.progress(id, JobPhase::Importing, counter.count, Some(total));
        }
        sink.shutdown().await
    }
    .await;
    // Unblock osmium if update_database stopped reading (and vice versa).
    drop(xml);
    drop(sink);
    let cat_status = cat.wait().await;
    let update_status = update.wait().await;
    let cat_err = cat_err.await.unwrap_or_default();
    let update_err = update_err.await.unwrap_or_default();
    // update_database first: when it dies, osmium fails too, on a broken pipe.
    match (update_status, cat_status) {
        (Ok(u), _) if !u.success() => Err(format!("update_database failed: {update_err}")),
        (_, Ok(c)) if !c.success() => Err(format!("osmium cat failed: {cat_err}")),
        (Err(e), _) | (_, Err(e)) => Err(format!("import tool failed: {e}")),
        _ => relay.map_err(|e| format!("the import was interrupted: {e}")),
    }
}

/// Runs a tool to completion; its stdout on success, its stderr in the error.
async fn run_tool(mut cmd: Command, what: &str) -> Result<Vec<u8>, String> {
    let out = cmd
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("could not start {what}: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "{what} failed: {}",
            last_chars(&String::from_utf8_lossy(&out.stderr), 500)
        ))
    }
}

/// The last 500 characters a child wrote to stderr.
async fn tail(stream: Option<impl AsyncRead + Unpin>) -> String {
    let mut out = Vec::new();
    if let Some(mut s) = stream {
        let mut buf = [0u8; 4096];
        while let Ok(n) = s.read(&mut buf).await {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if out.len() > 64 * 1024 {
                out.drain(..out.len() - 4096);
            }
        }
    }
    last_chars(&String::from_utf8_lossy(&out), 500)
}

fn last_chars(s: &str, n: usize) -> String {
    let s = s.trim();
    let skip = s.chars().count().saturating_sub(n);
    s.chars().skip(skip).collect()
}

/// Counts `<node `, `<way ` and `<relation ` starts in an XML stream fed in
/// arbitrary chunks.
#[derive(Debug, Default)]
pub struct ElementCounter {
    pub count: u64,
    /// The end of the previous chunk, for matches split across chunks.
    carry: Vec<u8>,
}

impl ElementCounter {
    const PATTERNS: [&'static [u8]; 3] = [b"<node ", b"<way ", b"<relation "];
    const KEEP: usize = 9; // longest pattern - 1

    pub fn feed(&mut self, chunk: &[u8]) {
        let mut window = std::mem::take(&mut self.carry);
        let carried = window.len();
        window.extend_from_slice(chunk);
        for p in Self::PATTERNS {
            // Matches lying wholly in the carry were counted last time.
            self.count += window
                .windows(p.len())
                .enumerate()
                .filter(|(i, w)| *w == p && i + p.len() > carried)
                .count() as u64;
        }
        let keep = window.len().min(Self::KEEP);
        self.carry = window[window.len() - keep..].to_vec();
    }
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
    use super::*;

    #[test]
    fn counts_elements_across_chunk_boundaries() {
        let xml = b"<osm>\n  <node id=\"1\"/>\n  <node id=\"2\">\n    <tag k=\"a\" v=\"b\"/>\n  </node>\n  <way id=\"3\">\n    <nd ref=\"1\"/>\n  </way>\n  <relation id=\"4\">\n    <member type=\"node\" ref=\"1\"/>\n  </relation>\n</osm>\n";
        let mut whole = ElementCounter::default();
        whole.feed(xml);
        assert_eq!(whole.count, 4);
        for size in 1..12 {
            let mut c = ElementCounter::default();
            for chunk in xml.chunks(size) {
                c.feed(chunk);
            }
            assert_eq!(c.count, 4, "chunk size {size}");
        }
    }

    #[test]
    fn parses_fileinfo() {
        let json = br#"{"file":{},"header":{"boxes":[],"option":{"osmosis_replication_timestamp":"2026-10-19T20:21:11Z"}},
          "data":{"count":{"changesets":0,"nodes":27352,"ways":5321,"relations":32}}}"#;
        let info: FileInfo = serde_json::from_slice(json).unwrap();
        assert_eq!(info.data.count.nodes, 27352);
        assert_eq!(
            info.header.option.osmosis_replication_timestamp.as_deref(),
            Some("2026-10-19T20:21:11Z")
        );
    }

    #[test]
    fn last_chars_is_char_safe() {
        assert_eq!(last_chars("  héllo wörld ", 5), "wörld");
    }
}
