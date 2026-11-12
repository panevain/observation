//! The production [`Toolchain`]: `osmium` and Overpass's `update_database`,
//! run as child processes that are killed when their future is dropped.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::ports::{self, Toolchain};

/// `osmium` and the Overpass binaries.
#[derive(Debug, Clone)]
pub struct OsmTools {
    pub osmium: PathBuf,
    pub overpass_bin: PathBuf,
}

impl Toolchain for OsmTools {
    fn tags_filter<'a>(
        &'a self,
        input: &'a Path,
        output: &'a Path,
    ) -> BoxFuture<'a, Result<(), String>> {
        async move {
            let mut filter = Command::new(&self.osmium);
            filter
                .arg("tags-filter")
                .arg(input)
                .args([
                    "w/building",
                    "r/building",
                    "w/highway",
                    "n/addr:housenumber",
                    "n/addr:housename",
                ])
                .arg("-o")
                .arg(output)
                .arg("--overwrite");
            run_tool(filter, "osmium tags-filter").await.map(drop)
        }
        .boxed()
    }

    fn file_info<'a>(&'a self, file: &'a Path) -> BoxFuture<'a, Result<ports::FileInfo, String>> {
        async move {
            let info = file_info(&self.osmium, file).await?;
            Ok(ports::FileInfo {
                elements: info.data.count.nodes + info.data.count.ways + info.data.count.relations,
                replication_timestamp: info.header.option.osmosis_replication_timestamp,
            })
        }
        .boxed()
    }

    fn load<'a>(
        &'a self,
        file: &'a Path,
        db: &'a Path,
        version: Option<&'a str>,
        progress: &'a mut (dyn FnMut(u64) + Send),
    ) -> BoxFuture<'a, Result<(), String>> {
        import(self, file, db, version, progress).boxed()
    }
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

async fn file_info(osmium: &Path, file: &Path) -> Result<FileInfo, String> {
    let mut cmd = Command::new(osmium);
    cmd.arg("fileinfo").arg("-e").arg("-j").arg(file);
    let out = run_tool(cmd, "osmium fileinfo").await?;
    serde_json::from_slice(&out).map_err(|e| format!("osmium fileinfo output unreadable: {e}"))
}

/// Pipes `osmium cat -f osm` into `update_database`, counting elements on the
/// way for progress. With a `version` (the extract's replication timestamp,
/// see [`is_osm_timestamp`](crate::import::is_osm_timestamp)) the database
/// records it, and Overpass reports it as `timestamp_osm_base`.
async fn import(
    tools: &OsmTools,
    filtered: &Path,
    db: &Path,
    version: Option<&str>,
    progress: &mut (dyn FnMut(u64) + Send),
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
    if let Some(version) = version {
        update.arg(format!("--version={version}"));
    }
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
            progress(counter.count);
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

    /// A directory with stand-in `osmium` and `update_database` scripts.
    /// Each writes its arguments to `<name>.args` beside itself.
    #[cfg(unix)]
    fn stand_ins(osmium: &str, update_database: &str) -> (tempfile::TempDir, OsmTools) {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [("osmium", osmium), ("update_database", update_database)] {
            let script = format!("echo \"$@\" > \"$0.args\"\n{body}");
            crate::testing::write_script(&dir.path().join(name), &script);
        }
        let tools = OsmTools {
            osmium: dir.path().join("osmium"),
            overpass_bin: dir.path().to_owned(),
        };
        (dir, tools)
    }

    #[cfg(unix)]
    fn args(dir: &tempfile::TempDir, name: &str) -> String {
        std::fs::read_to_string(dir.path().join(format!("{name}.args"))).unwrap()
    }

    /// A tool's exit status decides: its stdout on success, its trimmed
    /// stderr in the sentence on failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn tools_succeed_or_fail_by_exit_status() {
        let (dir, ok) = stand_ins("exit 0", "exit 0");
        let (input, output) = (Path::new("/in.pbf"), Path::new("/out.pbf"));
        ok.tags_filter(input, output).await.unwrap();
        assert_eq!(
            args(&dir, "osmium"),
            "tags-filter /in.pbf w/building r/building w/highway n/addr:housenumber n/addr:housename -o /out.pbf --overwrite\n"
        );
        let (_dir, bad) = stand_ins("echo '  no such file  ' >&2; exit 2", "exit 0");
        assert_eq!(
            bad.tags_filter(input, output).await.unwrap_err(),
            "osmium tags-filter failed: no such file"
        );

        let json = r#"{"header":{"option":{"osmosis_replication_timestamp":"2026-10-19T20:21:11Z"}},"data":{"count":{"nodes":5,"ways":3,"relations":1}}}"#;
        let (dir, info) = stand_ins(&format!("echo '{json}'"), "exit 0");
        let got = info.file_info(input).await.unwrap();
        assert_eq!(args(&dir, "osmium"), "fileinfo -e -j /in.pbf\n");
        assert_eq!(
            got,
            ports::FileInfo {
                elements: 9,
                replication_timestamp: Some("2026-10-19T20:21:11Z".into()),
            }
        );
    }

    /// The load pipes `osmium cat` into `update_database`, reporting the
    /// elements counted so far; a failure names `update_database` first,
    /// since osmium then fails too, on a broken pipe.
    #[cfg(unix)]
    #[tokio::test]
    async fn loads_report_progress_and_name_the_failed_tool() {
        const CAT: &str =
            "printf '<osm>\\n<node id=\"1\"/>\\n<node id=\"2\"/>\\n<way id=\"3\"/>\\n</osm>\\n'";
        let load = |tools: OsmTools| async move {
            let mut seen = Vec::new();
            let mut progress = |n| seen.push(n);
            let result = tools
                .load(
                    Path::new("/f.pbf"),
                    Path::new("/db"),
                    Some("2026-10-19T20:21:11Z"),
                    &mut progress,
                )
                .await;
            (result, seen)
        };

        let (dir, tools) = stand_ins(CAT, "cat >/dev/null");
        let (result, seen) = load(tools).await;
        result.unwrap();
        assert_eq!(seen.last(), Some(&3), "{seen:?}");
        assert_eq!(args(&dir, "osmium"), "cat /f.pbf -f osm -o -\n");
        assert_eq!(
            args(&dir, "update_database"),
            "--db-dir=/db/ --meta=no --compression-method=lz4 --map-compression-method=lz4 --version=2026-10-19T20:21:11Z\n"
        );

        let (_dir, tools) = stand_ins(CAT, "cat >/dev/null; echo 'disk full' >&2; exit 1");
        assert_eq!(
            load(tools).await.0.unwrap_err(),
            "update_database failed: disk full"
        );
        let (_dir, tools) = stand_ins(
            "echo 'broken pipe' >&2; exit 1",
            "cat >/dev/null; echo 'disk full' >&2; exit 1",
        );
        assert_eq!(
            load(tools).await.0.unwrap_err(),
            "update_database failed: disk full"
        );
        let (_dir, tools) = stand_ins("echo 'bad input' >&2; exit 1", "cat >/dev/null");
        assert_eq!(
            load(tools).await.0.unwrap_err(),
            "osmium cat failed: bad input"
        );
    }

    #[test]
    fn last_chars_is_char_safe() {
        assert_eq!(last_chars("  héllo wörld ", 5), "wörld");
    }
}
