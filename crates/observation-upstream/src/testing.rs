//! Test doubles for [`HttpClient`]: [`FakeHttp`] answers from a script,
//! [`RouterHttp`] hands requests to an in-process tower service (such as an
//! axum `Router`). Both apply the request's caps and timeout the way
//! [`GuardedHttp`](crate::GuardedHttp) does, including a client-level
//! timeout (`with_timeout`, as [`GuardedHttpBuilder::timeout`](crate::GuardedHttpBuilder::timeout))
//! that a request's own timeout replaces. Neither follows redirects: script
//! the final reply.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use http::header::{HeaderName, HeaderValue};
use http::{HeaderMap, Method, StatusCode};
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};

use crate::http::{
    Body, HttpClient, HttpRequest, HttpResponse, TransportError, TransportErrorKind,
};

/// One scripted answer of a [`FakeHttp`].
#[derive(Debug, Clone)]
pub struct FakeReply {
    /// How long the answer takes (on Tokio's clock, so paused-time tests
    /// skip it); past the request's timeout, the request times out instead.
    pub delay: Duration,
    pub outcome: Result<FakeResponse, TransportErrorKind>,
}

/// The response half of a [`FakeReply`].
#[derive(Debug, Clone)]
pub struct FakeResponse {
    pub status: StatusCode,
    /// The reply's own reason phrase (see [`HttpResponse::reason`]).
    pub reason: Option<String>,
    pub headers: HeaderMap,
    pub body: Bytes,
    /// Reading the body fails this way (if it is read at all).
    pub body_failure: Option<TransportErrorKind>,
}

impl FakeReply {
    /// `200 OK` with `body`.
    pub fn ok(body: impl Into<Bytes>) -> Self {
        Self::status(StatusCode::OK).body(body)
    }

    /// `status` with an empty body.
    pub fn status(status: StatusCode) -> Self {
        Self {
            delay: Duration::ZERO,
            outcome: Ok(FakeResponse {
                status,
                reason: None,
                headers: HeaderMap::new(),
                body: Bytes::new(),
                body_failure: None,
            }),
        }
    }

    /// A transport failure of `kind`.
    pub fn error(kind: TransportErrorKind) -> Self {
        Self {
            delay: Duration::ZERO,
            outcome: Err(kind),
        }
    }

    /// An answer that never comes: the request waits for its timeout (a
    /// request with no timeout at all panics rather than hang the test).
    pub fn hang() -> Self {
        Self::status(StatusCode::OK).delay(Duration::MAX)
    }

    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        if let Ok(r) = &mut self.outcome {
            r.body = body.into();
        }
        self
    }

    /// The reply's own reason phrase, in place of the status's canonical one.
    pub fn reason(mut self, reason: &str) -> Self {
        if let Ok(r) = &mut self.outcome {
            r.reason = Some(reason.to_owned());
        }
        self
    }

    /// The body breaks off with `kind` if it is read (a body the request
    /// does not read cannot fail).
    pub fn body_fails(mut self, kind: TransportErrorKind) -> Self {
        if let Ok(r) = &mut self.outcome {
            r.body_failure = Some(kind);
        }
        self
    }

    /// Adds a header value; repeated calls with one name add repeated headers.
    ///
    /// # Panics
    /// If `value` cannot be a header value.
    pub fn header(mut self, name: HeaderName, value: impl AsRef<str>) -> Self {
        if let Ok(r) = &mut self.outcome {
            let value = HeaderValue::from_str(value.as_ref()).expect("a valid header value");
            r.headers.append(name, value);
        }
        self
    }
}

/// Decides a [`FakeHttp`]'s reply from the request.
pub type FakeHandler = dyn Fn(&HttpRequest) -> FakeReply + Send + Sync;

/// An [`HttpClient`] that records every request and answers it from, in
/// order of preference: the replies queued for its method and URL
/// ([`FakeHttp::on`]), the replies queued for any request
/// ([`FakeHttp::push`]), then its handler ([`FakeHttp::from_fn`]). A request
/// none of those answers panics, naming it, so a missing script never passes
/// for an upstream failure; [`FakeHttp::assert_drained`] checks the other way.
/// Requests that race (single-flight, parallel tile fetches) should be
/// answered by key or by handler, since their order is not fixed. Clones share
/// the script and the record.
#[derive(Clone, Default)]
pub struct FakeHttp(Arc<Mutex<FakeState>>);

#[derive(Default)]
struct FakeState {
    keyed: HashMap<(Method, String), VecDeque<FakeReply>>,
    queued: VecDeque<FakeReply>,
    handler: Option<Arc<FakeHandler>>,
    requests: Vec<HttpRequest>,
    /// The client-level timeout.
    timeout: Option<Duration>,
    /// Requests nothing answered (`METHOD redacted-url`), recorded before the
    /// panic, which a spawned task may swallow.
    unexpected: Vec<String>,
}

impl fmt::Debug for FakeHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state();
        f.debug_struct("FakeHttp")
            .field(
                "keyed",
                &state.keyed.values().map(VecDeque::len).sum::<usize>(),
            )
            .field("queued", &state.queued.len())
            .field("handler", &state.handler.is_some())
            .field("requests", &state.requests.len())
            .field("timeout", &state.timeout)
            .finish()
    }
}

/// The last clone of a fake that saw unexpected requests fails the test when
/// it is dropped (unless the thread is already panicking).
impl Drop for FakeHttp {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) != 1 || std::thread::panicking() {
            return;
        }
        let unexpected = std::mem::take(&mut self.state().unexpected);
        assert!(
            unexpected.is_empty(),
            "FakeHttp: unexpected requests: {unexpected:?}"
        );
    }
}

impl FakeHttp {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fake that answers every request with `handler`'s reply.
    pub fn from_fn(handler: impl Fn(&HttpRequest) -> FakeReply + Send + Sync + 'static) -> Self {
        let fake = Self::new();
        fake.state().handler = Some(Arc::new(handler));
        fake
    }

    /// A fake that answers with `replies` in order.
    pub fn replying(replies: impl IntoIterator<Item = FakeReply>) -> Self {
        let fake = Self::new();
        for r in replies {
            fake.push(r);
        }
        fake
    }

    /// A timeout for requests that set none, like a client built with
    /// [`GuardedHttpBuilder::timeout`](crate::GuardedHttpBuilder::timeout).
    pub fn with_timeout(self, timeout: Duration) -> Self {
        self.state().timeout = Some(timeout);
        self
    }

    /// Panics if scripted replies were left unused, naming them.
    /// Also panics if any request was unexpected, naming them, even when the
    /// panic it caused was swallowed by a spawned task.
    pub fn assert_drained(&self) {
        let state = self.state();
        assert!(
            state.unexpected.is_empty(),
            "FakeHttp: unexpected requests: {:?}",
            state.unexpected
        );
        let keyed: Vec<String> = state
            .keyed
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .map(|((m, u), q)| format!("{m} {} ({})", crate::redact(u), q.len()))
            .collect();
        assert!(
            keyed.is_empty() && state.queued.is_empty(),
            "FakeHttp: unused replies: keyed {keyed:?}, queued {}",
            state.queued.len()
        );
    }

    /// Queues `reply` for the next request that has no keyed reply.
    pub fn push(&self, reply: FakeReply) {
        self.state().queued.push_back(reply);
    }

    /// Queues `reply` for the next `method` request to exactly `url`.
    pub fn on(&self, method: Method, url: &str, reply: FakeReply) {
        self.state()
            .keyed
            .entry((method, url.to_owned()))
            .or_default()
            .push_back(reply);
    }

    /// The requests sent so far, in the order they arrived.
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.state().requests.clone()
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        // A test that panicked while holding the lock has failed already.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn reply_for(&self, req: &HttpRequest) -> Option<FakeReply> {
        let mut state = self.state();
        state.requests.push(req.clone());
        let key = (req.method.clone(), req.url.clone());
        if let Some(r) = state.keyed.get_mut(&key).and_then(VecDeque::pop_front) {
            return Some(r);
        }
        if let Some(r) = state.queued.pop_front() {
            return Some(r);
        }
        let handler = state.handler.clone()?;
        // Not under the lock: the handler may look at the fake.
        drop(state);
        Some(handler(req))
    }

    async fn answer(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let Some(reply) = self.reply_for(&req) else {
            let what = format!("{} {}", req.method, crate::redact(&req.url));
            self.state().unexpected.push(what.clone());
            panic!("FakeHttp: unexpected request {what}");
        };
        let timeout = req.timeout.or(self.state().timeout);
        let wait = async {
            if reply.delay == Duration::MAX {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(reply.delay).await;
        };
        match timeout {
            Some(t) => tokio::time::timeout(t, wait)
                .await
                .map_err(|_| timed_out())?,
            None if reply.delay == Duration::MAX => panic!(
                "FakeHttp: {} {} would wait forever: it has no timeout",
                req.method,
                crate::redact(&req.url)
            ),
            None => wait.await,
        }
        match reply.outcome {
            Ok(FakeResponse {
                status,
                body_failure: Some(kind),
                ..
            }) if !req.skips_body(status) => Err(TransportError::new(
                kind,
                format!("FakeHttp: scripted {kind:?} in the body"),
            )
            .in_body_of(status)),
            Ok(r) => Ok(HttpResponse {
                body: if req.skips_body(r.status) {
                    req.unread_body(r.status, Some(r.body.len() as u64))
                } else if r.body.len() > req.cap_for(r.status) {
                    Body::TooLarge
                } else {
                    Body::Complete(r.body)
                },
                status: r.status,
                reason: r.reason,
                headers: r.headers,
            }),
            Err(kind) => Err(TransportError::new(
                kind,
                format!("FakeHttp: scripted {kind:?}"),
            )),
        }
    }
}

impl HttpClient for FakeHttp {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        self.answer(req).boxed()
    }
}

fn timed_out() -> TransportError {
    TransportError::new(TransportErrorKind::Timeout, "operation timed out")
}

/// An [`HttpClient`] that serves requests from an in-process tower service,
/// such as an axum `Router`, without a socket. The URL's path and query become
/// the request URI and its host (and port, unless the default) the `Host`
/// header; unlike reqwest, it adds no `User-Agent`, `Accept` or
/// `Content-Length` of its own. Its timeout covers the whole exchange, so a
/// body that stalls is reported as `Timeout`, not `BodyTimeout`.
#[derive(Clone)]
pub struct RouterHttp<S> {
    service: S,
    /// The client-level timeout.
    timeout: Option<Duration>,
}

impl<S> fmt::Debug for RouterHttp<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RouterHttp").finish_non_exhaustive()
    }
}

impl<S> RouterHttp<S> {
    pub fn new(service: S) -> Self {
        Self {
            service,
            timeout: None,
        }
    }

    /// A timeout for requests that set none, like a client built with
    /// [`GuardedHttpBuilder::timeout`](crate::GuardedHttpBuilder::timeout).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

impl<S, B> RouterHttp<S>
where
    S: tower::Service<http::Request<Full<Bytes>>, Response = http::Response<B>>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Future: Send,
    S::Error: fmt::Display,
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    async fn serve(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let other =
            |e: &dyn fmt::Display| TransportError::new(TransportErrorKind::Other, e.to_string());
        let url = reqwest::Url::parse(&req.url).map_err(|e| other(&e))?;
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_owned(),
        };
        let (headers, body) = req.wire_parts();
        let mut request = http::Request::builder()
            .method(req.method.clone())
            .uri(path)
            .body(Full::new(body.unwrap_or_default()))
            .map_err(|e| other(&e))?;
        *request.headers_mut() = headers;
        let host = url.host_str().map(|h| match url.port() {
            Some(port) => format!("{h}:{port}"),
            None => h.to_owned(),
        });
        if let Some(host) = host
            && let Ok(v) = HeaderValue::from_str(&host)
        {
            request.headers_mut().insert(http::header::HOST, v);
        }
        let exchange = async {
            let mut service = self.service.clone();
            std::future::poll_fn(|cx| service.poll_ready(cx))
                .await
                .map_err(|e| other(&e))?;
            let resp = service.call(request).await.map_err(|e| other(&e))?;
            let (parts, body) = resp.into_parts();
            let cap = req.cap_for(parts.status);
            // The body's own size, as reqwest sees it (a HEAD reply's
            // `Content-Length` header describes a body that is not there).
            let declared = http_body::Body::size_hint(&body).exact();
            let body = if req.skips_body(parts.status) {
                req.unread_body(parts.status, declared)
            } else if declared.is_some_and(|n| n > cap as u64) {
                Body::TooLarge
            } else {
                match Limited::new(body, cap).collect().await {
                    Ok(collected) => Body::Complete(collected.to_bytes()),
                    Err(e) if e.is::<LengthLimitError>() => Body::TooLarge,
                    // A body that breaks off keeps its status. One that stalls
                    // is caught by the timeout around the whole exchange
                    // below: `Timeout` with no status, where `GuardedHttp`
                    // gives `BodyTimeout` with the status.
                    Err(e) => {
                        return Err(TransportError::new(TransportErrorKind::Body, e.to_string())
                            .in_body_of(parts.status));
                    }
                }
            };
            Ok(HttpResponse {
                status: parts.status,
                reason: parts
                    .extensions
                    .get::<hyper::ext::ReasonPhrase>()
                    .map(|r| String::from_utf8_lossy(r.as_bytes()).into_owned()),
                headers: parts.headers,
                body,
            })
        };
        match req.timeout.or(self.timeout) {
            Some(t) => tokio::time::timeout(t, exchange)
                .await
                .map_err(|_| timed_out())?,
            None => exchange.await,
        }
    }
}

impl<S, B> HttpClient for RouterHttp<S>
where
    S: tower::Service<http::Request<Full<Bytes>>, Response = http::Response<B>>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Future: Send,
    S::Error: fmt::Display,
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        self.serve(req).boxed()
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use http::header::CONTENT_TYPE;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn fake_answers_in_order_and_records_requests() {
        let fake = FakeHttp::replying([
            FakeReply::ok("first").header(CONTENT_TYPE, "text/plain"),
            FakeReply::status(StatusCode::TOO_MANY_REQUESTS),
            FakeReply::error(TransportErrorKind::Connect),
        ]);
        let client: Arc<dyn HttpClient> = Arc::new(fake.clone());
        let a = client
            .send(HttpRequest::post("http://o.example/i", 100).form([("data", "q")]))
            .await
            .unwrap();
        assert_eq!(a.status, StatusCode::OK);
        assert_eq!(a.headers[CONTENT_TYPE], "text/plain");
        assert_eq!(a.bytes().unwrap(), "first");
        let b = client
            .send(HttpRequest::get("http://o.example/i", 100))
            .await;
        assert_eq!(b.unwrap().status, StatusCode::TOO_MANY_REQUESTS);
        let c = client
            .send(HttpRequest::get("http://o.example/i", 100))
            .await;
        assert_eq!(c.unwrap_err().kind, TransportErrorKind::Connect);
        fake.assert_drained();

        let sent = fake.requests();
        assert_eq!(sent.len(), 3);
        assert_eq!(
            sent[0].body,
            crate::RequestBody::Form(vec![("data".into(), "q".into())])
        );
    }

    #[tokio::test]
    async fn fake_prefers_keyed_replies_then_queued_then_the_handler() {
        let fake = FakeHttp::from_fn(|req| FakeReply::ok(format!("handler {}", req.url)));
        fake.on(Method::GET, "http://t.example/a", FakeReply::ok("keyed a"));
        fake.push(FakeReply::ok("queued"));
        let get = |u: &str| fake.send(HttpRequest::get(u, 100));
        let body = |r: Result<HttpResponse, TransportError>| r.unwrap().bytes().cloned().unwrap();
        assert_eq!(body(get("http://t.example/b").await), "queued");
        assert_eq!(body(get("http://t.example/a").await), "keyed a");
        assert_eq!(
            body(get("http://t.example/a").await),
            "handler http://t.example/a"
        );
        let urls: Vec<_> = fake.requests().into_iter().map(|r| r.url).collect();
        assert_eq!(
            urls,
            [
                "http://t.example/b",
                "http://t.example/a",
                "http://t.example/a"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fake_applies_the_cap_and_the_timeout() {
        let fake = FakeHttp::replying([
            FakeReply::ok(vec![1u8; 16]),
            FakeReply::ok(vec![1u8; 17]),
            FakeReply::ok("late").delay(Duration::from_secs(30)),
            FakeReply::ok("in time").delay(Duration::from_secs(5)),
            FakeReply::hang(),
        ]);
        let get = || HttpRequest::get("http://t.example/", 16).timeout(Duration::from_secs(10));
        assert!(matches!(
            fake.send(get()).await.unwrap().body,
            Body::Complete(_)
        ));
        assert_eq!(fake.send(get()).await.unwrap().body, Body::TooLarge);
        // An error reply has its own, smaller cap; the status still shows.
        let url = "http://t.example/error";
        fake.on(
            Method::GET,
            url,
            FakeReply::status(StatusCode::BAD_GATEWAY).body(vec![1u8; 8]),
        );
        let err = fake
            .send(HttpRequest::get(url, 16).error_max_bytes(4))
            .await
            .unwrap();
        assert_eq!(
            (err.status, err.body),
            (StatusCode::BAD_GATEWAY, Body::TooLarge)
        );
        let started = tokio::time::Instant::now();
        let late = fake.send(get()).await.unwrap_err();
        assert_eq!(late.kind, TransportErrorKind::Timeout);
        assert_eq!(started.elapsed(), Duration::from_secs(10));
        assert_eq!(fake.send(get()).await.unwrap().bytes().unwrap(), "in time");
        assert_eq!(
            fake.send(get()).await.unwrap_err().kind,
            TransportErrorKind::Timeout
        );
    }

    /// Echoes the method, URI, host, content type and body back as text,
    /// padded to `?pad=n` bytes.
    fn echo() -> impl tower::Service<
        http::Request<Full<Bytes>>,
        Response = http::Response<Full<Bytes>>,
        Error = Infallible,
        Future = impl Send,
    > + Clone
    + Send
    + Sync {
        /// The most `?pad=n` may ask for.
        const MAX_PAD: usize = 64 * 1024;
        tower::service_fn(|req: http::Request<Full<Bytes>>| async move {
            let pad: usize = req
                .uri()
                .query()
                .and_then(|q| q.strip_prefix("pad="))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let head = format!(
                "{} {} {} {}",
                req.method(),
                req.uri(),
                req.headers()[http::header::HOST].to_str().unwrap_or(""),
                req.headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-"),
            );
            let body = req.into_body().collect().await.unwrap().to_bytes();
            let mut text = format!("{head} {}", String::from_utf8_lossy(&body)).into_bytes();
            // `?pad=n` pads the answer to n bytes. The size comes from the request, so it is
            // bounded here: the tests ask for a few thousand bytes at most.
            if pad > text.len() && pad <= MAX_PAD {
                text.resize(pad, b'.');
            }
            Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from(text))))
        })
    }

    #[tokio::test]
    async fn router_serves_path_query_and_body() {
        let client = RouterHttp::new(echo());
        let resp = client
            .send(
                HttpRequest::post("http://agent.internal:9/interpreter?bbox=1,2,3,4", 1024)
                    .form([("data", "x y")]),
            )
            .await
            .unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(
            resp.bytes().unwrap(),
            "POST /interpreter?bbox=1,2,3,4 agent.internal:9 application/x-www-form-urlencoded data=x+y"
        );
        let capped = client
            .send(HttpRequest::get("http://a.internal/?pad=2000", 1024))
            .await
            .unwrap();
        assert_eq!(capped.body, Body::TooLarge);
        let exact = client
            .send(HttpRequest::get("http://a.internal/?pad=1024", 1024))
            .await
            .unwrap();
        assert_eq!(exact.bytes().map(Bytes::len), Some(1024));
    }

    #[tokio::test(start_paused = true)]
    async fn router_applies_the_timeout_on_tokio_time() {
        let slow = tower::service_fn(|_req: http::Request<Full<Bytes>>| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from_static(b"late"))))
        });
        let client = RouterHttp::new(slow);
        let started = tokio::time::Instant::now();
        let err = client
            .send(
                HttpRequest::get("http://a.internal/regions?since=3", 100)
                    .timeout(Duration::from_secs(25)),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, TransportErrorKind::Timeout);
        assert_eq!(started.elapsed(), Duration::from_secs(25));
        let ok = client
            .send(
                HttpRequest::get("http://a.internal/regions", 100).timeout(Duration::from_secs(35)),
            )
            .await
            .unwrap();
        assert_eq!(ok.bytes().unwrap(), "late");
    }

    #[tokio::test]
    #[should_panic(expected = "unexpected request GET https://t.example/x")]
    async fn an_unscripted_request_panics_naming_it_without_credentials() {
        let fake = FakeHttp::new();
        let _ = fake
            .send(HttpRequest::get("https://u:secret@t.example/x", 1))
            .await;
    }

    #[test]
    #[should_panic(expected = "unused replies")]
    fn leftover_replies_fail_assert_drained() {
        let fake = FakeHttp::new();
        fake.on(Method::GET, "http://t.example/a", FakeReply::ok("a"));
        fake.assert_drained();
    }

    #[tokio::test(start_paused = true)]
    async fn keyed_replies_answer_racing_requests_in_any_order() {
        let fake = FakeHttp::new();
        for n in 0..8 {
            let url = format!("http://t.example/{n}");
            // Later URLs answer sooner, so they finish in reverse order.
            let delay = Duration::from_millis(100 * (8 - n));
            fake.on(Method::GET, &url, FakeReply::ok(n.to_string()).delay(delay));
        }
        let sends = (0..8).rev().map(|n| {
            let fake = &fake;
            async move {
                let r = fake
                    .send(HttpRequest::get(format!("http://t.example/{n}"), 10))
                    .await
                    .unwrap();
                (n, r.bytes().cloned().unwrap())
            }
        });
        for (n, body) in futures_util::future::join_all(sends).await {
            assert_eq!(body, n.to_string());
        }
        fake.assert_drained();
    }

    #[tokio::test]
    async fn fake_headers_take_dynamic_and_repeated_values() {
        let max_age = 3600;
        let fake = FakeHttp::replying([FakeReply::ok("x")
            .header(http::header::CACHE_CONTROL, format!("max-age={max_age}"))
            .header(http::header::VARY, "a")
            .header(http::header::VARY, "b")]);
        let r = fake
            .send(HttpRequest::get("http://t.example/", 10))
            .await
            .unwrap();
        assert_eq!(r.headers[http::header::CACHE_CONTROL], "max-age=3600");
        let vary: Vec<_> = r.headers.get_all(http::header::VARY).iter().collect();
        assert_eq!(vary, ["a", "b"]);
    }

    /// The client-level timeout applies when a request sets none, and a
    /// request's own timeout replaces it (longer or shorter), as in reqwest.
    #[tokio::test(start_paused = true)]
    async fn fake_client_timeout_applies_unless_the_request_sets_one() {
        let fake = FakeHttp::new().with_timeout(Duration::from_secs(20));
        let url = "http://t.example/tile";
        fake.push(FakeReply::ok("late").delay(Duration::from_secs(25)));
        let started = tokio::time::Instant::now();
        let err = fake.send(HttpRequest::get(url, 10)).await.unwrap_err();
        assert_eq!(err.kind, TransportErrorKind::Timeout);
        assert_eq!(started.elapsed(), Duration::from_secs(20));
        fake.push(FakeReply::ok("late").delay(Duration::from_secs(25)));
        let longer = HttpRequest::get(url, 10).timeout(Duration::from_secs(30));
        assert_eq!(fake.send(longer).await.unwrap().bytes().unwrap(), "late");
        fake.push(FakeReply::ok("soon").delay(Duration::from_secs(5)));
        let shorter = HttpRequest::get(url, 10).timeout(Duration::from_secs(1));
        assert_eq!(
            fake.send(shorter).await.unwrap_err().kind,
            TransportErrorKind::Timeout
        );
        fake.push(FakeReply::hang());
        assert_eq!(
            fake.send(HttpRequest::get(url, 10)).await.unwrap_err().kind,
            TransportErrorKind::Timeout
        );
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "would wait forever")]
    async fn a_hang_without_any_timeout_panics() {
        let fake = FakeHttp::replying([FakeReply::hang()]);
        let _ = fake.send(HttpRequest::get("http://t.example/", 10)).await;
    }

    #[tokio::test]
    async fn fake_error_cap_of_zero_keeps_only_empty_error_bodies() {
        let fake = FakeHttp::replying([
            FakeReply::status(StatusCode::BAD_GATEWAY).body("oops"),
            FakeReply::status(StatusCode::NOT_MODIFIED),
        ]);
        let get = || HttpRequest::get("http://t.example/", 10).error_max_bytes(0);
        assert_eq!(fake.send(get()).await.unwrap().body, Body::TooLarge);
        assert_eq!(
            fake.send(get()).await.unwrap().body,
            Body::Complete(Bytes::new())
        );
    }

    /// A scripted body failure happens only if the body is read.
    #[tokio::test]
    async fn fake_body_failures_happen_only_when_the_body_is_read() {
        let fake = FakeHttp::replying([
            FakeReply::ok("abc").body_fails(TransportErrorKind::Body),
            FakeReply::status(StatusCode::PARTIAL_CONTENT)
                .body("abc")
                .body_fails(TransportErrorKind::BodyTimeout),
        ]);
        let get = || HttpRequest::get("http://t.example/", 10).body_only_for(StatusCode::OK);
        assert_eq!(
            fake.send(get()).await.unwrap_err().kind,
            TransportErrorKind::Body
        );
        let resp = fake.send(get()).await.unwrap();
        assert_eq!(resp.status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.body, Body::TooLarge, "not read");
        fake.assert_drained();
    }

    /// A scripted body failure comes with the status that preceded it, and
    /// only when the body is read.
    #[tokio::test]
    async fn fake_body_failures_carry_the_status() {
        let fake = FakeHttp::replying([
            FakeReply::status(StatusCode::NOT_FOUND).body_fails(TransportErrorKind::BodyTimeout),
            FakeReply::status(StatusCode::NOT_FOUND).body_fails(TransportErrorKind::Body),
            FakeReply::status(StatusCode::NOT_FOUND)
                .body("gone")
                .body_fails(TransportErrorKind::Body),
        ]);
        let err = fake
            .send(HttpRequest::get("http://t.example/", 10))
            .await
            .unwrap_err();
        assert_eq!(err.kind, TransportErrorKind::BodyTimeout);
        assert_eq!(err.status, Some(StatusCode::NOT_FOUND));
        let unread = HttpRequest::get("http://t.example/", 10).error_max_bytes(0);
        let resp = fake.send(unread).await.unwrap();
        assert_eq!(resp.status, StatusCode::NOT_FOUND);
        assert_eq!(resp.body, Body::Complete(Bytes::new()), "never read");
        let skipped = HttpRequest::get("http://t.example/", 10).skip_body_of(StatusCode::NOT_FOUND);
        let resp = fake.send(skipped).await.unwrap();
        assert_eq!(resp.status, StatusCode::NOT_FOUND);
        assert_eq!(resp.body, Body::TooLarge, "never read");
        fake.assert_drained();
    }

    type Frames = Box<
        dyn futures_util::Stream<Item = Result<http_body::Frame<Bytes>, std::io::Error>>
            + Send
            + Unpin,
    >;

    /// A service answering every request with a streamed body of no known size.
    fn streaming(
        make: fn() -> Frames,
    ) -> impl tower::Service<
        http::Request<Full<Bytes>>,
        Response = http::Response<http_body_util::StreamBody<Frames>>,
        Error = Infallible,
        Future = impl Send,
    > + Clone
    + Send
    + Sync {
        tower::service_fn(move |_req: http::Request<Full<Bytes>>| async move {
            Ok::<_, Infallible>(http::Response::new(http_body_util::StreamBody::new(make())))
        })
    }

    fn frame(len: usize) -> Result<http_body::Frame<Bytes>, std::io::Error> {
        Ok(http_body::Frame::data(Bytes::from(vec![b'.'; len])))
    }

    #[tokio::test(start_paused = true)]
    async fn router_caps_streamed_bodies_and_reports_their_errors() {
        use futures_util::StreamExt;
        let get = || HttpRequest::get("http://a.internal/", 1024).timeout(Duration::from_secs(5));
        let fits = RouterHttp::new(streaming(|| {
            Box::new(futures_util::stream::iter([frame(512), frame(512)]))
        }));
        assert_eq!(
            fits.send(get()).await.unwrap().bytes().map(Bytes::len),
            Some(1024)
        );
        let over = RouterHttp::new(streaming(|| {
            Box::new(futures_util::stream::iter([frame(512), frame(513)]))
        }));
        assert_eq!(over.send(get()).await.unwrap().body, Body::TooLarge);
        let endless = RouterHttp::new(streaming(|| {
            Box::new(futures_util::stream::repeat_with(|| frame(100)))
        }));
        assert_eq!(endless.send(get()).await.unwrap().body, Body::TooLarge);
        let broken = RouterHttp::new(streaming(|| {
            Box::new(futures_util::stream::iter([
                frame(10),
                Err(std::io::Error::other("reset")),
            ]))
        }));
        let err = broken.send(get()).await.unwrap_err();
        assert_eq!(err.kind, TransportErrorKind::Body);
        assert_eq!(err.status, Some(StatusCode::OK), "the head had arrived");
        // A long poll or event stream that stops sending: the timeout ends it,
        // the request's own or else the client's.
        let stalled = RouterHttp::new(streaming(|| {
            Box::new(futures_util::stream::iter([frame(10)]).chain(futures_util::stream::pending()))
        }));
        let started = tokio::time::Instant::now();
        assert_eq!(
            stalled.send(get()).await.unwrap_err().kind,
            TransportErrorKind::Timeout
        );
        assert_eq!(started.elapsed(), Duration::from_secs(5));
        let stalled = stalled.with_timeout(Duration::from_secs(20));
        let started = tokio::time::Instant::now();
        assert_eq!(
            stalled
                .send(HttpRequest::get("http://a.internal/", 1024))
                .await
                .unwrap_err()
                .kind,
            TransportErrorKind::Timeout
        );
        assert_eq!(started.elapsed(), Duration::from_secs(20));
    }

    /// A HEAD answer's `Content-Length` describes a body that is not sent:
    /// the response is complete and empty, as from `GuardedHttp`.
    #[tokio::test]
    async fn router_head_answers_are_empty_and_complete() {
        let svc = tower::service_fn(|_req: http::Request<Full<Bytes>>| async {
            let mut resp = http::Response::new(Full::new(Bytes::new()));
            resp.headers_mut().insert(
                http::header::CONTENT_LENGTH,
                HeaderValue::from_static("123456789"),
            );
            Ok::<_, Infallible>(resp)
        });
        let resp = RouterHttp::new(svc)
            .send(HttpRequest::head("http://a.internal/x.osm.pbf"))
            .await
            .unwrap();
        assert_eq!(resp.body, Body::Complete(Bytes::new()));
        assert_eq!(resp.content_length(), Some(123_456_789));
    }

    /// A request from a spawned task whose panic nobody sees still fails
    /// `assert_drained`.
    #[tokio::test]
    #[should_panic(expected = "unexpected requests: [\"GET https://t.example/lost\"]")]
    async fn unexpected_requests_in_spawned_tasks_are_not_lost() {
        let fake = FakeHttp::new();
        let task_fake = fake.clone();
        drop(tokio::spawn(async move {
            let _ = task_fake
                .send(HttpRequest::get("https://u:pw@t.example/lost", 1))
                .await;
        }));
        while fake.requests().is_empty() {
            tokio::task::yield_now().await;
        }
        fake.assert_drained();
    }

    #[tokio::test]
    #[should_panic(expected = "unexpected requests")]
    async fn dropping_a_fake_that_saw_unexpected_requests_fails() {
        let fake = FakeHttp::new();
        let task_fake = fake.clone();
        let _ = tokio::spawn(async move {
            let _ = task_fake
                .send(HttpRequest::get("http://t.example/lost", 1))
                .await;
        })
        .await;
        drop(fake);
    }

    #[tokio::test]
    async fn router_error_cap_of_zero_keeps_only_empty_error_bodies() {
        let svc = tower::service_fn(|req: http::Request<Full<Bytes>>| async move {
            let (status, body) = match req.uri().path() {
                "/bad" => (StatusCode::BAD_GATEWAY, "oops"),
                "/not-modified" => (StatusCode::NOT_MODIFIED, ""),
                _ => (StatusCode::OK, "ok"),
            };
            let mut resp = http::Response::new(Full::new(Bytes::from_static(body.as_bytes())));
            *resp.status_mut() = status;
            Ok::<_, Infallible>(resp)
        });
        let client = RouterHttp::new(svc);
        let get = |path: &str| {
            HttpRequest::get(format!("http://a.internal{path}"), 10).error_max_bytes(0)
        };
        let bad = client.send(get("/bad")).await.unwrap();
        assert_eq!(
            (bad.status, bad.body),
            (StatusCode::BAD_GATEWAY, Body::TooLarge)
        );
        let nm = client.send(get("/not-modified")).await.unwrap();
        assert_eq!(nm.body, Body::Complete(Bytes::new()));
        assert_eq!(client.send(get("/")).await.unwrap().bytes().unwrap(), "ok");
        // With the default error cap the same error body is read.
        let read = client
            .send(HttpRequest::get("http://a.internal/bad", 10))
            .await
            .unwrap();
        assert_eq!(read.bytes().unwrap(), "oops");
    }

    /// Under `skip_error_bodies` an error body is skipped, never read (it
    /// cannot fail), and comes with the reply's reason; a 3xx is read under
    /// the main cap.
    #[tokio::test]
    async fn fake_skips_error_bodies_and_reads_redirects_under_the_main_cap() {
        let fake = FakeHttp::replying([
            FakeReply::status(StatusCode::NOT_FOUND)
                .reason("Gone Fishing")
                .body("gone")
                .body_fails(TransportErrorKind::BodyTimeout),
            FakeReply::status(StatusCode::SERVICE_UNAVAILABLE),
            FakeReply::status(StatusCode::FOUND).body(vec![1u8; 100]),
            FakeReply::status(StatusCode::FOUND).body(vec![1u8; 101]),
        ]);
        let get = || {
            HttpRequest::get("http://t.example/", 100)
                .error_max_bytes(0)
                .skip_error_bodies()
        };
        let resp = fake.send(get()).await.unwrap();
        assert_eq!(resp.status, StatusCode::NOT_FOUND);
        assert_eq!(resp.reason.as_deref(), Some("Gone Fishing"));
        assert_eq!(resp.body, Body::Skipped);
        assert_eq!(resp.bytes(), None);
        let resp = fake.send(get()).await.unwrap();
        assert_eq!((resp.reason, resp.body), (None, Body::Skipped));
        let resp = fake.send(get()).await.unwrap();
        assert_eq!(resp.bytes().map(Bytes::len), Some(100));
        assert_eq!(fake.send(get()).await.unwrap().body, Body::TooLarge);
        fake.assert_drained();
    }

    #[tokio::test(start_paused = true)]
    async fn router_skips_error_bodies_even_endless_ones() {
        let endless = |status: StatusCode| {
            tower::service_fn(move |_req: http::Request<Full<Bytes>>| async move {
                let frames: Frames = Box::new(futures_util::stream::repeat_with(|| frame(100)));
                let mut resp = http::Response::new(http_body_util::StreamBody::new(frames));
                *resp.status_mut() = status;
                resp.extensions_mut()
                    .insert(hyper::ext::ReasonPhrase::from_static(b"Back Soon"));
                Ok::<_, Infallible>(resp)
            })
        };
        let get = || {
            HttpRequest::get("http://a.internal/", 1024)
                .skip_error_bodies()
                .timeout(Duration::from_secs(5))
        };
        let resp = RouterHttp::new(endless(StatusCode::SERVICE_UNAVAILABLE))
            .send(get())
            .await
            .unwrap();
        assert_eq!(resp.body, Body::Skipped);
        assert_eq!(resp.reason.as_deref(), Some("Back Soon"));
        let resp = RouterHttp::new(endless(StatusCode::MOVED_PERMANENTLY))
            .send(get())
            .await
            .unwrap();
        assert_eq!(
            resp.body,
            Body::TooLarge,
            "a 3xx is read, up to the main cap"
        );
    }
}
