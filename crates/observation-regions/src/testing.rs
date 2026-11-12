//! Fakes of the agent's ports, for its own tests.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use http::StatusCode;
use observation_upstream::{TransportError, TransportErrorKind};

use crate::ports::{Download, DownloadBody, DownloadResponse, FileInfo, QueryRunner, Toolchain};

/// One step of a fake download's body.
#[derive(Debug, Clone)]
pub(crate) enum Step {
    Chunk(Bytes),
    Fail(TransportErrorKind),
    /// The next piece never comes.
    Hang,
}

/// What a [`FakeDownload`] answers.
#[derive(Debug, Clone)]
pub(crate) enum Reply {
    Head {
        status: StatusCode,
        reason: Option<String>,
        content_length: Option<u64>,
        steps: Vec<Step>,
    },
    Fail(TransportErrorKind),
    /// The head never comes.
    Hang,
}

impl Reply {
    /// A 200 with these pieces and this declared length.
    pub(crate) fn ok(content_length: Option<u64>, steps: Vec<Step>) -> Self {
        Self::Head {
            status: StatusCode::OK,
            reason: None,
            content_length,
            steps,
        }
    }

    /// An empty reply with `status` and, when given, its own reason phrase.
    pub(crate) fn status(status: u16, reason: Option<&str>) -> Self {
        Self::Head {
            status: StatusCode::from_u16(status).unwrap(),
            reason: reason.map(str::to_owned),
            content_length: None,
            steps: Vec::new(),
        }
    }
}

/// A piece of body.
pub(crate) fn chunk(bytes: &'static [u8]) -> Step {
    Step::Chunk(Bytes::from_static(bytes))
}

/// A [`Download`] answering every request with one scripted [`Reply`],
/// recording the URLs and whether the body was dropped.
#[derive(Debug)]
pub(crate) struct FakeDownload {
    reply: Reply,
    pub(crate) urls: Mutex<Vec<String>>,
    pub(crate) body_dropped: Arc<AtomicBool>,
}

impl FakeDownload {
    pub(crate) fn new(reply: Reply) -> Self {
        Self {
            reply,
            urls: Mutex::default(),
            body_dropped: Arc::default(),
        }
    }
}

impl Download for FakeDownload {
    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<DownloadResponse, TransportError>> {
        self.urls.lock().unwrap().push(url.to_owned());
        async move {
            match self.reply.clone() {
                Reply::Head {
                    status,
                    reason,
                    content_length,
                    steps,
                } => Ok(DownloadResponse {
                    status,
                    reason,
                    content_length,
                    body: Box::new(FakeBody {
                        steps: steps.into(),
                        dropped: Arc::clone(&self.body_dropped),
                    }),
                }),
                Reply::Fail(kind) => Err(TransportError::new(kind, format!("scripted {kind:?}"))),
                Reply::Hang => std::future::pending().await,
            }
        }
        .boxed()
    }
}

#[derive(Debug)]
struct FakeBody {
    steps: VecDeque<Step>,
    dropped: Arc<AtomicBool>,
}

impl DownloadBody for FakeBody {
    fn chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, TransportError>> {
        let step = self.steps.pop_front();
        async move {
            match step {
                None => Ok(None),
                Some(Step::Chunk(b)) => Ok(Some(b)),
                Some(Step::Fail(kind)) => {
                    Err(TransportError::new(kind, format!("scripted {kind:?}")))
                }
                Some(Step::Hang) => std::future::pending().await,
            }
        }
        .boxed()
    }
}

impl Drop for FakeBody {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

/// A [`Toolchain`] that runs nothing: it records its calls, reports 3
/// elements and a replication timestamp, and reports loading 1 then 3.
#[derive(Debug, Default)]
pub(crate) struct FakeToolchain {
    pub(crate) calls: Mutex<Vec<String>>,
}

/// The replication timestamp [`FakeToolchain`] reports.
pub(crate) const FAKE_TIMESTAMP: &str = "2026-10-31T19:43:02Z";

impl Toolchain for FakeToolchain {
    fn tags_filter<'a>(
        &'a self,
        _input: &'a Path,
        _output: &'a Path,
    ) -> BoxFuture<'a, Result<(), String>> {
        self.calls.lock().unwrap().push("tags_filter".into());
        async { Ok(()) }.boxed()
    }

    fn file_info<'a>(&'a self, _file: &'a Path) -> BoxFuture<'a, Result<FileInfo, String>> {
        self.calls.lock().unwrap().push("file_info".into());
        async {
            Ok(FileInfo {
                elements: 3,
                replication_timestamp: Some(FAKE_TIMESTAMP.into()),
            })
        }
        .boxed()
    }

    fn load<'a>(
        &'a self,
        _file: &'a Path,
        _db: &'a Path,
        version: Option<&'a str>,
        progress: &'a mut (dyn FnMut(u64) + Send),
    ) -> BoxFuture<'a, Result<(), String>> {
        self.calls.lock().unwrap().push(format!("load {version:?}"));
        async move {
            progress(1);
            progress(3);
            Ok(())
        }
        .boxed()
    }
}

/// A [`QueryRunner`] whose queries wait until [`GateRunner::release`] lets
/// them answer `{"elements":[]}`.
#[derive(Debug)]
pub(crate) struct GateRunner {
    pub(crate) started: AtomicUsize,
    gate: tokio::sync::Semaphore,
}

impl GateRunner {
    pub(crate) fn new() -> Self {
        Self {
            started: AtomicUsize::new(0),
            gate: tokio::sync::Semaphore::new(0),
        }
    }

    /// Lets `n` queries answer.
    pub(crate) fn release(&self, n: usize) {
        self.gate.add_permits(n);
    }
}

impl QueryRunner for GateRunner {
    fn run<'a>(&'a self, _db: &'a Path, _query: &'a str) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        self.started.fetch_add(1, Ordering::SeqCst);
        async move {
            self.gate.acquire().await.unwrap().forget();
            Ok(b"{\"elements\":[]}".to_vec())
        }
        .boxed()
    }
}
