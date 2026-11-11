//! What the client needs from the page it runs in, as traits: HTTP and
//! server-sent events, key-value storage, file download and upload, the wall
//! clock and the system colour scheme.
//!
//! The client implements each trait once for the browser and once for native
//! builds (`observation-client`'s `platform` module); tests use the fakes in
//! [`crate::testing`]. Results that arrive later (a reply, a picked file, a
//! change of scheme) are handed to a callback, which the client uses to send
//! them down a channel that a system drains every frame.

/// A reply body, or why there is none (already worded for the user).
pub type Reply = Result<Vec<u8>, String>;

/// One HTTP request.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub method: &'static str,
    /// JSON body.
    pub body: Option<String>,
    /// Admin token, sent as `Authorization: Bearer` when not empty.
    pub bearer: Option<String>,
}

/// Leaves the token out, so logs and test output never show it.
impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &self.url)
            .field("method", &self.method)
            .field("body", &self.body)
            .field("bearer", &self.bearer.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl HttpRequest {
    pub fn get(url: String) -> Self {
        Self {
            url,
            method: "GET",
            body: None,
            bearer: None,
        }
    }
}

/// HTTP requests and event streams to the server the page came from.
///
/// Callbacks may run before `fetch` or `subscribe` returns (the native build
/// answers every call at once with an error) or later, from the browser's
/// event loop; callers must accept both.
pub trait Http: Send + Sync + 'static {
    /// Sends the request; `done` is called once with the body of a successful
    /// reply or a description of the failure.
    fn fetch(&self, req: HttpRequest, done: Box<dyn FnOnce(Reply) + Send>);
    /// Follows a server-sent event stream: `on` is called with the data of
    /// every `event` message, and with an error whenever the stream drops;
    /// a stream that drops is reopened and lives as long as the page. If the
    /// stream cannot be opened at all (always in the native build), `on` is
    /// called once with an error, possibly before this returns, and never
    /// again.
    fn subscribe(&self, url: &str, event: &str, on: Box<dyn Fn(Reply) + Send>);
}

/// Why the browser refused the autosave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFailure {
    /// The site's storage quota is used up (or is zero, as in some private modes).
    Full,
    /// The browser denies this page access to storage (site data or cookies blocked).
    Blocked,
    /// The browser offers no storage to this page at all.
    Unavailable,
}

impl SaveFailure {
    /// What went wrong, for the status bar's tooltip.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Full => "This browser's storage for the site is full.",
            Self::Blocked => {
                "This browser blocks the site from storing data (check its site data or cookie settings)."
            }
            Self::Unavailable => {
                "This browser offers no storage to the site (it may be in private mode)."
            }
        }
    }
}

/// String storage that outlives the app (`localStorage`) or the page
/// (`sessionStorage`).
pub trait KeyValueStore: Send + Sync + 'static {
    /// The stored value, if there is one and it can be read.
    fn get(&self, key: &str) -> Option<String>;
    /// Stores the value, or says why it could not.
    fn set(&self, key: &str, value: &str) -> Result<(), SaveFailure>;
    /// Forgets the key (best effort).
    fn remove(&self, key: &str);
}

/// Files the user saves or opens.
pub trait FileDialogs: Send + Sync + 'static {
    /// Offers `text` as a file named `name` of type `mime`.
    fn download(&self, name: &str, mime: &str, text: &str) -> Result<(), String>;
    /// Lets the user pick a file of the `accept` types (an `<input accept>`
    /// list); `done` is called with its text or why it could not be read.
    /// A picker that cannot be shown calls `done` with an error before this
    /// returns. `done` is never called when the user cancels, nor in the
    /// native build, which has no picker.
    fn pick_text(&self, accept: &str, done: Box<dyn FnOnce(Result<String, String>) + Send>);
}

/// The wall clock.
pub trait Clock: Send + Sync + 'static {
    /// Milliseconds since the Unix epoch by this computer's clock.
    fn unix_ms(&self) -> f64;
}

/// Whole seconds since the Unix epoch by `clock`.
pub fn unix_secs(clock: &dyn Clock) -> u64 {
    (clock.unix_ms() / 1000.0) as u64
}

/// The system light/dark setting.
pub trait ColorScheme: Send + Sync + 'static {
    /// Whether the system asks for a dark scheme now.
    fn prefers_dark(&self) -> bool;
    /// Calls `on` with the new setting whenever it changes, for as long as
    /// the page lives. It may never be called: the native build has no
    /// setting to follow, and a browser may not report changes.
    fn watch(&self, on: Box<dyn Fn(bool) + Send>);
}
