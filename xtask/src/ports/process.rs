//! The ports' real adapters, each running a program (or reading the system clock).

use std::env;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use super::{Brotli, Cargo, Clock, Git, Http, Response};
use crate::core::time::Timestamp;
use crate::{Error, Result};

/// Runs `cmd`, feeding it `input` on stdin, and collects its output whatever its exit status.
fn run(cmd: &mut Command, input: Option<&[u8]>) -> io::Result<Output> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdin = child.stdin.take();
    // A writer thread, so that a large input cannot deadlock against a full stdout pipe.
    std::thread::scope(|s| {
        if let (Some(mut stdin), Some(data)) = (stdin, input) {
            s.spawn(move || stdin.write_all(data));
        }
        child.wait_with_output()
    })
}

fn describe(cmd: &Command) -> String {
    let args = cmd
        .get_args()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>();
    format!("{} {}", cmd.get_program().to_string_lossy(), args.join(" "))
}

fn spawn_error(cmd: &Command, e: &io::Error) -> Error {
    Error::new(format!("could not run `{}`: {e}", describe(cmd)))
}

fn failed(cmd: &Command, out: &Output) -> Error {
    Error::new(format!(
        "`{}` failed ({}): {}",
        describe(cmd),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim_end()
    ))
}

fn utf8(cmd: &Command, bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes)
        .map_err(|_| Error::new(format!("`{}` printed invalid UTF-8", describe(cmd))))
}

/// Whether an executable file called `name` is in a `PATH` directory.
pub fn on_path(name: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|dir| {
            let p = dir.join(name);
            p.metadata().is_ok_and(|m| {
                #[cfg(unix)]
                let exec = std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0;
                #[cfg(not(unix))]
                let exec = true;
                m.is_file() && exec
            })
        })
    })
}

/// `git`, run in the repository at `root`.
#[derive(Debug, Clone)]
pub struct GitCli {
    pub root: PathBuf,
}

impl GitCli {
    fn git(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.root).args(args);
        cmd
    }
}

impl Git for GitCli {
    fn is_commit(&self, rev: &str) -> Result<bool> {
        let mut cmd = self.git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ]);
        let out = run(&mut cmd, None).map_err(|e| spawn_error(&cmd, &e))?;
        Ok(out.status.success())
    }

    fn show(&self, rev: &str, path: &str) -> Result<Option<String>> {
        let mut cmd = self.git(&["show", &format!("{rev}:{path}")]);
        let out = run(&mut cmd, None).map_err(|e| spawn_error(&cmd, &e))?;
        if !out.status.success() {
            return Ok(None);
        }
        utf8(&cmd, out.stdout).map(Some)
    }

    fn grep(&self, args: &[&str]) -> Result<Option<String>> {
        let mut cmd = self.git(&["grep"]);
        cmd.args(args);
        let out = run(&mut cmd, None).map_err(|e| spawn_error(&cmd, &e))?;
        match out.status.code() {
            Some(0) => utf8(&cmd, out.stdout).map(Some),
            Some(1) => Ok(None),
            _ => Err(failed(&cmd, &out)),
        }
    }
}

/// `curl`, HTTPS only (redirects included), identifying itself with `user_agent`.
#[derive(Debug, Clone)]
pub struct Curl {
    pub user_agent: String,
}

/// curl's output with `--write-out '\n%{http_code}'`: the body, then the status on a last line.
fn split_status(stdout: &[u8]) -> Option<(u16, &[u8])> {
    let nl = stdout.iter().rposition(|&b| b == b'\n')?;
    let status = std::str::from_utf8(&stdout[nl + 1..]).ok()?.parse().ok()?;
    Some((status, &stdout[..nl]))
}

impl Http for Curl {
    fn get(&self, url: &str) -> Result<Response> {
        let mut cmd = Command::new("curl");
        cmd.args(["--silent", "--show-error", "--location"])
            .args(["--proto", "=https", "--proto-redir", "=https"])
            .args(["--connect-timeout", "30", "--max-time", "120"])
            .args(["--user-agent", &self.user_agent])
            .args(["--write-out", "\n%{http_code}", "--", url]);
        let out = run(&mut cmd, None).map_err(|e| spawn_error(&cmd, &e))?;
        if !out.status.success() {
            return Err(failed(&cmd, &out));
        }
        let (status, body) = split_status(&out.stdout)
            .ok_or_else(|| Error::new(format!("`{}`: no status in the output", describe(&cmd))))?;
        let body = utf8(&cmd, body.to_vec())?;
        Ok(Response { status, body })
    }
}

/// `cargo-audit`, from `PATH`.
#[derive(Debug, Clone, Default)]
pub struct CargoCli;

impl Cargo for CargoCli {
    fn audit_json(&self, lockfile: &Path) -> Result<Option<String>> {
        let mut cmd = Command::new("cargo-audit");
        cmd.args(["audit", "--json", "--file"]).arg(lockfile);
        if let Some(dir) = lockfile.parent().filter(|d| !d.as_os_str().is_empty()) {
            cmd.current_dir(dir);
        }
        match run(&mut cmd, None) {
            // cargo-audit exits non-zero when it finds something; the JSON is what counts.
            Ok(out) => utf8(&cmd, out.stdout).map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(spawn_error(&cmd, &e)),
        }
    }
}

/// `brotli`, from `PATH`.
#[derive(Debug, Clone, Default)]
pub struct BrotliCli;

impl Brotli for BrotliCli {
    fn available(&self) -> bool {
        on_path("brotli")
    }

    fn compressed_len(&self, data: &[u8]) -> Result<usize> {
        let mut cmd = Command::new("brotli");
        cmd.args(["-q", "11", "-c"]);
        let out = run(&mut cmd, Some(data)).map_err(|e| spawn_error(&cmd, &e))?;
        if !out.status.success() {
            return Err(failed(&cmd, &out));
        }
        Ok(out.stdout.len())
    }
}

/// The system clock.
#[derive(Debug, Clone, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let micros = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_micros()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_micros()).unwrap_or(i64::MAX),
        };
        Timestamp::from_unix_micros(micros)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_status_line() {
        assert_eq!(split_status(b"a\nb\n200"), Some((200, &b"a\nb"[..])));
        assert_eq!(split_status(b"\n404"), Some((404, &b""[..])));
        assert_eq!(split_status(b"200"), None);
    }

    #[test]
    fn finds_programs_on_path() {
        assert!(on_path("git"));
        assert!(!on_path("no-such-program-xtask"));
    }

    #[test]
    fn git_reads_a_repository() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let sh = |script: &str| {
            let ok = Command::new("sh")
                .args(["-c", script])
                .current_dir(dir.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "{script}");
        };
        sh("git init -q -b main && printf 'one\\ntwo\\n' > a.md \
            && git -c user.name=t -c user.email=t@example.com add a.md \
            && git -c user.name=t -c user.email=t@example.com commit -q -m init \
            && printf 'three\\n' > b.md");
        let git = GitCli {
            root: dir.path().to_owned(),
        };
        fn ok<T>(r: Result<T>) -> T {
            r.unwrap_or_else(|e| panic!("{e}"))
        }
        assert!(ok(git.is_commit("HEAD")));
        assert!(!ok(git.is_commit("nope")));
        assert_eq!(ok(git.show("HEAD", "a.md")).as_deref(), Some("one\ntwo\n"));
        assert_eq!(ok(git.show("HEAD", "b.md")), None);
        assert_eq!(
            ok(git.grep(&["--untracked", "-n", "-e", "t[wh]", "--", "*.md"])).as_deref(),
            Some("a.md:2:two\nb.md:1:three\n")
        );
        assert_eq!(ok(git.grep(&["-e", "four"])), None);
        assert!(git.grep(&["--no-such-flag"]).is_err());
    }
}
