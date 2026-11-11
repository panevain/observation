//! In-memory ports for tests. Each answers only what the test set up, records every call, and
//! panics on anything else, so a test cannot pass by reaching the real world or by a call it
//! did not expect.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use crate::core::time::Timestamp;
use crate::ports::{Brotli, Cargo, Clock, Git, Http, Response};
use crate::{Error, Result};

/// The calls a fake received, in order, written like the command they stand for.
#[derive(Debug, Default)]
pub struct Calls(RefCell<Vec<String>>);

impl Calls {
    fn record(&self, call: String) {
        self.0.borrow_mut().push(call);
    }

    pub fn take(&self) -> Vec<String> {
        self.0.take()
    }
}

fn unexpected(call: &str) -> ! {
    panic!("fake: no answer set up for `{call}`")
}

#[derive(Debug, Default)]
pub struct FakeGit {
    pub commits: Vec<String>,
    /// (rev, path) -> text; a missing entry panics, a `None` is "git cannot show it".
    pub files: HashMap<(String, String), Option<String>>,
    /// The arguments joined by spaces -> output.
    pub greps: HashMap<String, Option<String>>,
    pub calls: Calls,
}

impl Git for FakeGit {
    fn is_commit(&self, rev: &str) -> Result<bool> {
        self.calls.record(format!("git rev-parse {rev}"));
        Ok(self.commits.iter().any(|c| c == rev))
    }

    fn show(&self, rev: &str, path: &str) -> Result<Option<String>> {
        let call = format!("git show {rev}:{path}");
        self.calls.record(call.clone());
        let key = (rev.to_owned(), path.to_owned());
        Ok(self
            .files
            .get(&key)
            .cloned()
            .unwrap_or_else(|| unexpected(&call)))
    }

    fn grep(&self, args: &[&str]) -> Result<Option<String>> {
        let call = format!("git grep {}", args.join(" "));
        self.calls.record(call.clone());
        Ok(self
            .greps
            .get(&args.join(" "))
            .cloned()
            .unwrap_or_else(|| unexpected(&call)))
    }
}

#[derive(Debug, Default)]
pub struct FakeHttp {
    /// URL -> response; `Err` stands for a network failure.
    pub responses: HashMap<String, Result<Response>>,
    pub calls: Calls,
}

impl FakeHttp {
    pub fn ok(mut self, url: &str, body: &str) -> Self {
        let response = Response {
            status: 200,
            body: body.to_owned(),
        };
        self.responses.insert(url.to_owned(), Ok(response));
        self
    }

    pub fn status(mut self, url: &str, status: u16) -> Self {
        let response = Response {
            status,
            body: String::new(),
        };
        self.responses.insert(url.to_owned(), Ok(response));
        self
    }
}

impl Http for FakeHttp {
    fn get(&self, url: &str) -> Result<Response> {
        let call = format!("GET {url}");
        self.calls.record(call.clone());
        self.responses
            .get(url)
            .cloned()
            .unwrap_or_else(|| unexpected(&call))
    }
}

#[derive(Debug, Default)]
pub struct FakeCargo {
    /// Whether cargo-audit is installed; without it `audit_json` answers `None`.
    pub audit_installed: bool,
    /// Lockfile text -> cargo-audit's JSON for it.
    pub audits: HashMap<String, String>,
    pub calls: Calls,
}

impl Cargo for FakeCargo {
    fn audit_json(&self, lockfile: &Path) -> Result<Option<String>> {
        self.calls.record(format!(
            "cargo-audit audit --json --file {}",
            lockfile.display()
        ));
        if !self.audit_installed {
            return Ok(None);
        }
        let text = std::fs::read_to_string(lockfile).map_err(|e| Error::new(e.to_string()))?;
        let json = self.audits.get(&text).cloned();
        Ok(Some(json.unwrap_or_else(|| {
            unexpected(&format!("cargo-audit on\n{text}"))
        })))
    }
}

#[derive(Debug, Default)]
pub struct FakeBrotli {
    pub installed: bool,
    /// Input -> compressed size.
    pub sizes: HashMap<Vec<u8>, usize>,
    pub calls: Calls,
}

impl Brotli for FakeBrotli {
    fn available(&self) -> bool {
        self.installed
    }

    fn compressed_len(&self, data: &[u8]) -> Result<usize> {
        let call = format!("brotli -q 11 ({} bytes)", data.len());
        self.calls.record(call.clone());
        assert!(
            self.installed,
            "fake: brotli is not installed, yet `{call}` ran"
        );
        Ok(*self.sizes.get(data).unwrap_or_else(|| unexpected(&call)))
    }
}

/// A clock stopped at one instant.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fakes_answer_what_was_set_up_and_record_it() {
        let http = FakeHttp::default()
            .ok("https://index.crates.io/3/s/syn", "{}")
            .status("https://index.crates.io/3/z/zzz", 404);
        let get = |url| http.get(url).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(get("https://index.crates.io/3/s/syn").body, "{}");
        assert_eq!(get("https://index.crates.io/3/z/zzz").status, 404);
        assert_eq!(
            http.calls.take(),
            [
                "GET https://index.crates.io/3/s/syn",
                "GET https://index.crates.io/3/z/zzz"
            ]
        );
    }

    #[test]
    #[should_panic(expected = "no answer set up for `git show HEAD:Cargo.lock`")]
    fn fakes_panic_on_calls_nobody_set_up() {
        let _ = FakeGit::default().show("HEAD", "Cargo.lock");
    }
}
