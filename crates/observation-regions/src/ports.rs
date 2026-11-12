//! The agent's outside world: the extract download and the programs it runs
//! on an import and a query. The import and the query route see only these
//! traits, so tests can run them on fakes. The production implementations
//! are `ExtractDownload` (below), `tools.rs` and `query.rs`. The extract
//! list, the boundaries and the download sizes go through
//! [`HttpClient`](observation_upstream::HttpClient).

use std::fmt;
use std::path::Path;

use bytes::Bytes;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use http::StatusCode;
use observation_upstream::{
    Body, ByteStream, GuardedHttp, HttpResponse, TransportError, status_error,
};

/// Downloads an extract. Nothing here bounds the size or the time a body
/// takes: the import caps the size, and times out the wait for the head and
/// for each piece.
pub trait Download: Send + Sync + fmt::Debug {
    /// Sends a GET for `url`; resolves once the status and headers arrived,
    /// whatever the status.
    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<DownloadResponse, TransportError>>;
}

/// The head of a download, and its body.
#[derive(Debug)]
pub struct DownloadResponse {
    pub status: StatusCode,
    /// The reply's own reason phrase, when not the canonical one
    /// (see [`observation_upstream::status_error`]).
    pub reason: Option<String>,
    /// The body's declared length.
    pub content_length: Option<u64>,
    pub body: Box<dyn DownloadBody>,
}

/// A download's body, read piece by piece; dropping it ends the download.
pub trait DownloadBody: Send + fmt::Debug {
    /// The next piece, `None` at the end. It may never resolve.
    fn chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, TransportError>>;
}

/// The production [`Download`]: [`GuardedHttp::get_stream`], whose body has
/// no cap and no deadline. Private to this crate, so that no other crate
/// reaches an uncapped stream through the port.
#[derive(Debug, Clone)]
pub(crate) struct ExtractDownload(pub(crate) GuardedHttp);

impl Download for ExtractDownload {
    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<DownloadResponse, TransportError>> {
        async move {
            let resp = self.0.get_stream(url).await?;
            Ok(DownloadResponse {
                status: resp.status,
                reason: resp.reason,
                content_length: resp.body.content_length(),
                body: Box::new(resp.body),
            })
        }
        .boxed()
    }
}

impl DownloadBody for ByteStream {
    fn chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, TransportError>> {
        ByteStream::chunk(self).boxed()
    }
}

/// The body of a response to a request sent with
/// [`skip_error_bodies`](observation_upstream::HttpRequest::skip_error_bodies),
/// as reqwest's `error_for_status` followed by a capped read gave it: a 4xx
/// or 5xx is an error with its status text, and `None` is a body over the cap.
pub(crate) fn checked_body(resp: HttpResponse) -> Result<Option<Bytes>, String> {
    if resp.status.is_client_error() || resp.status.is_server_error() {
        return Err(status_error(resp.status, resp.reason.as_deref()));
    }
    match resp.body {
        Body::Complete(bytes) => Ok(Some(bytes)),
        // `Skipped` comes only with a 4xx or 5xx, which is an error above.
        Body::TooLarge | Body::Skipped => Ok(None),
    }
}

/// Runs an Overpass query against an imported database.
pub trait QueryRunner: Send + Sync + fmt::Debug {
    /// The query's output, or a sentence saying why it failed. Dropping the
    /// future stops the query.
    fn run<'a>(&'a self, db: &'a Path, query: &'a str) -> BoxFuture<'a, Result<Vec<u8>, String>>;
}

/// What an import needs to know about the filtered extract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// Nodes, ways and relations in the file.
    pub elements: u64,
    /// The extract's replication timestamp, as the file's header has it
    /// (unchecked).
    pub replication_timestamp: Option<String>,
}

/// The programs that turn an extract into an Overpass database. Errors are
/// sentences for the UI; dropping a future stops the program it runs.
pub trait Toolchain: Send + Sync + fmt::Debug {
    /// Keeps only buildings, roads and address nodes of `input`, in `output`.
    fn tags_filter<'a>(
        &'a self,
        input: &'a Path,
        output: &'a Path,
    ) -> BoxFuture<'a, Result<(), String>>;

    /// Counts the elements of `file` and reads its header.
    fn file_info<'a>(&'a self, file: &'a Path) -> BoxFuture<'a, Result<FileInfo, String>>;

    /// Loads `file` into a new database in `db`, recording `version` (a
    /// checked replication timestamp) when given, and calling `progress`
    /// with the number of elements loaded so far.
    fn load<'a>(
        &'a self,
        file: &'a Path,
        db: &'a Path,
        version: Option<&'a str>,
        progress: &'a mut (dyn FnMut(u64) + Send),
    ) -> BoxFuture<'a, Result<(), String>>;
}
