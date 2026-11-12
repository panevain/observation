//! Running Overpass queries against an imported database.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::ports::QueryRunner;

/// Largest query result returned (the server accepts 32 MB).
const MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;

/// The production [`QueryRunner`]: Overpass's `osm3s_query` in `bin`.
#[derive(Debug, Clone)]
pub struct Osm3sQuery {
    pub bin: PathBuf,
}

impl QueryRunner for Osm3sQuery {
    fn run<'a>(&'a self, db: &'a Path, query: &'a str) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        run_query(&self.bin, db, query).boxed()
    }
}

/// Runs `osm3s_query` directly on a database directory (no dispatcher: the
/// databases are read-only once imported).
pub(crate) async fn run_query(bin: &Path, db: &Path, query: &str) -> Result<Vec<u8>, String> {
    let mut child = Command::new(bin.join("osm3s_query"))
        .arg(format!("--db-dir={}/", db.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start osm3s_query: {e}"))?;
    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("could not talk to osm3s_query".into());
    };
    let query = query.as_bytes().to_vec();
    let write = tokio::spawn(async move {
        stdin.write_all(&query).await?;
        stdin.shutdown().await
    });
    // Drained to the end (so the child never blocks on it); the start is kept.
    let errs = tokio::spawn(async move {
        let mut kept = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            if kept.len() < 64 * 1024 {
                kept.extend_from_slice(&buf[..n]);
            }
        }
        kept
    });
    let read = async {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = stdout.read(&mut buf).await?;
            if n == 0 {
                return Ok(out);
            }
            if out.len() + n > MAX_RESULT_BYTES {
                return Err(std::io::Error::other("the result is too large"));
            }
            out.extend_from_slice(&buf[..n]);
        }
    }
    .await;
    let out = match read {
        Ok(out) => out,
        Err(e) => {
            // Stop it now rather than let it block on the pipe until the timeout.
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(format!("osm3s_query output: {e}"));
        }
    };
    let status = child
        .wait()
        .await
        .map_err(|e| format!("osm3s_query failed: {e}"))?;
    let wrote = write.await.is_ok_and(|w| w.is_ok());
    let errs = errs.await.unwrap_or_default();
    if !status.success() || !wrote {
        let errs = String::from_utf8_lossy(&errs);
        return Err(format!(
            "osm3s_query failed ({status}): {}",
            errs.lines()
                .filter(|l| l.contains("error"))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;

    /// A stand-in `osm3s_query` running `script`.
    #[cfg(unix)]
    pub(crate) fn fake_query_bin(script: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("cat >/dev/null\n{script}");
        crate::testing::write_script(&dir.path().join("osm3s_query"), &body);
        dir
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn queries_return_output_and_errors() {
        let dir = fake_query_bin("echo '{\"elements\":[]}'");
        let ok = Osm3sQuery {
            bin: dir.path().to_owned(),
        };
        let out = ok.run(Path::new("/db"), "q").await.unwrap();
        assert_eq!(out, b"{\"elements\":[]}\n");
        let bad = fake_query_bin("echo 'runtime error: broken' >&2; exit 3");
        let e = run_query(bad.path(), Path::new("/db"), "q")
            .await
            .unwrap_err();
        assert!(e.contains("runtime error: broken"), "{e}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_results_fail_at_once() {
        // Endless output: must stop at the cap, not wait for the query timeout.
        let big = fake_query_bin("yes '{\"padding\":1}'");
        let started = std::time::Instant::now();
        let e = run_query(big.path(), Path::new("/db"), "q")
            .await
            .unwrap_err();
        assert!(e.contains("too large"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
