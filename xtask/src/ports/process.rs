//! The ports' real adapters, each running a program (or reading the system clock).

use std::env;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use super::{AuditOutput, Brotli, Cargo, Clock, Git, Http, Response};
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

/// Whether an executable file called `name` is in a `PATH` directory (`shutil.which`, except
/// that an unset `PATH` finds nothing).
pub(crate) fn on_path(name: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|dir| executable(&dir.join(name))))
}

fn executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| {
        #[cfg(unix)]
        let exec = std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0;
        #[cfg(not(unix))]
        let exec = true;
        m.is_file() && exec
    })
}

/// Whether `program` can be run: a bare name is looked up on `PATH`, a path is checked as is.
fn installed(program: &Path) -> bool {
    match program.to_str() {
        Some(name) if program.components().count() == 1 => on_path(name),
        _ => executable(program),
    }
}

/// `git`, run in the repository at `root`, with `env` added to its environment. No
/// caller-supplied revision, path or pattern can be read as an option.
#[derive(Debug, Clone)]
pub struct GitCli {
    pub root: PathBuf,
    pub env: Vec<(String, String)>,
}

impl GitCli {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            env: Vec::new(),
        }
    }

    fn git(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.root)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .args(args);
        cmd
    }
}

impl Git for GitCli {
    fn is_commit(&self, rev: &str) -> Result<bool> {
        let rev = format!("{rev}^{{commit}}");
        let mut cmd = self.git(&["rev-parse", "--verify", "--quiet", "--end-of-options", &rev]);
        let out = run(&mut cmd, None).map_err(|e| spawn_error(&cmd, &e))?;
        Ok(out.status.success())
    }

    /// The text as git prints it: CRLF stays CRLF (the scripts' text mode turned it into LF).
    fn show(&self, rev: &str, path: &str) -> Result<Option<String>> {
        let mut cmd = self.git(&["show", "--end-of-options", &format!("{rev}:{path}")]);
        let out = run(&mut cmd, None).map_err(|e| spawn_error(&cmd, &e))?;
        if !out.status.success() {
            return Ok(None);
        }
        utf8(&cmd, out.stdout).map(Some)
    }

    fn grep(&self, flags: &[&str], pattern: &str, pathspecs: &[&str]) -> Result<Option<String>> {
        let mut cmd = self.git(&["grep", "--no-color"]);
        cmd.args(flags).args(["-e", pattern, "--"]).args(pathspecs);
        let out = run(&mut cmd, None).map_err(|e| spawn_error(&cmd, &e))?;
        match out.status.code() {
            Some(0) => utf8(&cmd, out.stdout).map(Some),
            Some(1) => Ok(None),
            _ => Err(failed(&cmd, &out)),
        }
    }
}

/// `curl`, HTTPS only (redirects included), identifying itself with `user_agent`. It reads no
/// `.curlrc`.
#[derive(Debug, Clone)]
pub struct Curl {
    pub user_agent: String,
}

/// curl's arguments for a GET of `url`. `--disable` must come first to keep `.curlrc` out. The
/// timeouts are urllib's `timeout=30`: 30 s to connect, and no 30 s without data. The URL is
/// requested as written, as urllib did: `--globoff` keeps `[]` and `{}` from being expanded
/// into other URLs, and `--path-as-is` keeps `/../` and `/./` in the path.
fn curl_args<'a>(user_agent: &'a str, url: &'a str) -> Vec<&'a str> {
    vec![
        "--disable",
        "--globoff",
        "--path-as-is",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "30",
        "--speed-limit",
        "1",
        "--speed-time",
        "30",
        "--user-agent",
        user_agent,
        "--write-out",
        "\n%{http_code}",
        "--",
        url,
    ]
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
        cmd.args(curl_args(&self.user_agent, url));
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

/// cargo-audit, as `program` (default: `cargo-audit` from `PATH`).
#[derive(Debug, Clone)]
pub struct CargoCli {
    pub program: PathBuf,
}

impl Default for CargoCli {
    fn default() -> Self {
        Self {
            program: "cargo-audit".into(),
        }
    }
}

impl Cargo for CargoCli {
    /// Runs in the lockfile's directory and names the lockfile by its file name there. `None`
    /// when the program cannot be found, which is also what a missing directory gives (as in
    /// the script).
    fn audit_json(&self, lockfile: &Path) -> Result<Option<AuditOutput>> {
        let name = lockfile
            .file_name()
            .ok_or_else(|| Error::new(format!("{}: not a file", lockfile.display())))?;
        let dir = lockfile
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut cmd = Command::new(&self.program);
        cmd.current_dir(dir)
            .args(["audit", "--json", "--file"])
            .arg(name);
        match run(&mut cmd, None) {
            // cargo-audit exits non-zero when it finds something; the caller reads the JSON.
            Ok(out) => Ok(Some(AuditOutput {
                stdout: utf8(&cmd, out.stdout)?,
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(spawn_error(&cmd, &e)),
        }
    }
}

/// brotli, as `program` (default: `brotli` from `PATH`).
#[derive(Debug, Clone)]
pub struct BrotliCli {
    pub program: PathBuf,
}

impl Default for BrotliCli {
    fn default() -> Self {
        Self {
            program: "brotli".into(),
        }
    }
}

impl Brotli for BrotliCli {
    fn available(&self) -> bool {
        installed(&self.program)
    }

    fn compressed_len(&self, data: &[u8]) -> Result<usize> {
        let mut cmd = Command::new(&self.program);
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
    fn curl_reads_no_curlrc_and_takes_the_url_as_an_argument() {
        let args = curl_args("ua", "-o/tmp/x");
        assert_eq!(args[0], "--disable");
        assert_eq!(&args[args.len() - 2..], ["--", "-o/tmp/x"]);
    }

    /// `[]` and `{}` in a URL are not curl globs, and dot segments are sent as written.
    #[test]
    fn curl_requests_the_url_as_written() {
        let args = curl_args("ua", "https://example.com/se/rd/serd[e-e]");
        assert!(args.contains(&"--globoff"));
        assert!(args.contains(&"--path-as-is"));
    }

    fn ok<T>(r: Result<T>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    /// An executable shell script `name` in `dir`.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap_or_else(|e| panic!("{e}"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .unwrap_or_else(|e| panic!("{e}"));
        }
        path
    }

    #[test]
    fn cargo_audit_runs_beside_the_lockfile_and_keeps_stderr() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let program = script(
            tmp.path(),
            "cargo-audit",
            r#"[ "$1 $2 $3" = "audit --json --file" ] || exit 9; cat "$4"; echo "fetch failed" >&2; exit 1"#,
        );
        std::fs::create_dir(tmp.path().join("sub")).unwrap_or_else(|e| panic!("{e}"));
        let lock = tmp.path().join("sub/Cargo.lock");
        std::fs::write(&lock, "{}").unwrap_or_else(|e| panic!("{e}"));
        let cargo = CargoCli { program };
        let out = ok(cargo.audit_json(&lock));
        assert_eq!(
            out,
            Some(AuditOutput {
                stdout: "{}".into(),
                stderr: "fetch failed\n".into()
            })
        );
        let missing = CargoCli {
            program: tmp.path().join("no-cargo-audit"),
        };
        assert_eq!(ok(missing.audit_json(&lock)), None);
    }

    #[test]
    fn brotli_reports_the_compressed_size() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let program = script(
            tmp.path(),
            "brotli",
            r#"[ "$*" = "-q 11 -c" ] || exit 9; cat >/dev/null; printf abc"#,
        );
        let brotli = BrotliCli { program };
        assert!(brotli.available());
        assert_eq!(ok(brotli.compressed_len(&[0; 100_000])), 3);
        assert!(
            !BrotliCli {
                program: tmp.path().join("none")
            }
            .available()
        );
        assert!(
            !BrotliCli {
                program: "no-such-program-xtask".into()
            }
            .available()
        );
    }

    #[test]
    fn finds_programs_on_path() {
        assert!(on_path("git"));
        assert!(!on_path("no-such-program-xtask"));
    }

    #[test]
    fn git_reads_a_repository() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let isolated = [
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ];
        let sh = |script: &str| {
            let ok = Command::new("sh")
                .args(["-c", script])
                .current_dir(dir.path())
                .envs(isolated)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "{script}");
        };
        sh("git init -q -b main && printf 'one\\ntwo\\n' > a.md \
            && git -c user.name=t -c user.email=t@example.com add a.md \
            && git -c user.name=t -c user.email=t@example.com commit -q -m init \
            && printf 'three\\n' > b.md");
        let mut git = GitCli::new(dir.path());
        git.env = isolated
            .iter()
            .map(|&(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        assert!(ok(git.is_commit("HEAD")));
        assert!(!ok(git.is_commit("nope")));
        assert_eq!(ok(git.show("HEAD", "a.md")).as_deref(), Some("one\ntwo\n"));
        assert_eq!(ok(git.show("HEAD", "b.md")), None);
        assert_eq!(
            ok(git.grep(&["--untracked", "-n"], "t[wh]", &["*.md"])).as_deref(),
            Some("a.md:2:two\nb.md:1:three\n")
        );
        assert_eq!(ok(git.grep(&[], "four", &[])), None);
        assert_eq!(ok(git.grep(&[], "--four", &[])), None);
        assert!(git.grep(&["--no-such-flag"], "x", &[]).is_err());
        // Revisions that look like options stay revisions.
        assert!(!ok(git.is_commit("--all")));
        assert_eq!(ok(git.show("--output=written", "a.md")), None);
        assert!(!dir.path().join("written:a.md").exists());
    }
}
