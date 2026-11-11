//! Test doubles for [`HttpClient`]: [`FakeHttp`] answers from a script,
//! [`RouterHttp`] hands requests to an in-process tower service (such as an
//! axum `Router`). Both apply the request's `max_bytes` and `timeout` the way
//! [`GuardedHttp`](crate::GuardedHttp) does.

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
    pub headers: HeaderMap,
    pub body: Bytes,
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
                headers: HeaderMap::new(),
                body: Bytes::new(),
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

    /// An answer that never comes: the request waits for its timeout.
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

    pub fn header(mut self, name: HeaderName, value: &'static str) -> Self {
        if let Ok(r) = &mut self.outcome {
            r.headers.insert(name, HeaderValue::from_static(value));
        }
        self
    }
}

/// Decides a [`FakeHttp`]'s reply from the request.
pub type FakeHandler = dyn Fn(&HttpRequest) -> FakeReply + Send + Sync;

/// An [`HttpClient`] that records every request and answers it from, in
/// order of preference: the replies queued for its method and URL
/// ([`FakeHttp::on`]), the replies queued for any request
/// ([`FakeHttp::push`]), then its handler ([`FakeHttp::from_fn`]). With none
/// of those it fails with [`TransportErrorKind::Other`]. Requests that race
/// (single-flight, parallel tile fetches) should be answered by key or by
/// handler, since their order is not fixed. Clones share the script and the
/// record.
#[derive(Clone, Default)]
pub struct FakeHttp(Arc<Mutex<FakeState>>);

#[derive(Default)]
struct FakeState {
    keyed: HashMap<(Method, String), VecDeque<FakeReply>>,
    queued: VecDeque<FakeReply>,
    handler: Option<Arc<FakeHandler>>,
    requests: Vec<HttpRequest>,
}

impl fmt::Debug for FakeHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state();
        f.debug_struct("FakeHttp")
            .field("keyed", &state.keyed)
            .field("queued", &state.queued)
            .field("handler", &state.handler.is_some())
            .field("requests", &state.requests)
            .finish()
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
            return Err(TransportError::new(
                TransportErrorKind::Other,
                "FakeHttp: no reply scripted for this request",
            ));
        };
        let wait = async {
            if reply.delay == Duration::MAX {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(reply.delay).await;
        };
        match req.timeout {
            Some(t) => tokio::time::timeout(t, wait)
                .await
                .map_err(|_| timed_out())?,
            None => wait.await,
        }
        match reply.outcome {
            Ok(r) => Ok(HttpResponse {
                body: if r.body.len() > req.cap_for(r.status) {
                    Body::TooLarge
                } else {
                    Body::Complete(r.body)
                },
                status: r.status,
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
/// such as an axum `Router`, without a socket. The URL's scheme and host are
/// dropped; its path and query become the request URI.
#[derive(Clone)]
pub struct RouterHttp<S> {
    service: S,
}

impl<S> fmt::Debug for RouterHttp<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RouterHttp").finish_non_exhaustive()
    }
}

impl<S> RouterHttp<S> {
    pub fn new(service: S) -> Self {
        Self { service }
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
        if let Some(host) = url.host_str()
            && let Ok(v) = HeaderValue::from_str(host)
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
            let declared = parts
                .headers
                .get(http::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
                .or(http_body::Body::size_hint(&body).exact());
            let body = if declared.is_some_and(|n| n > cap as u64) {
                Body::TooLarge
            } else {
                match Limited::new(body, cap).collect().await {
                    Ok(collected) => Body::Complete(collected.to_bytes()),
                    Err(e) if e.is::<LengthLimitError>() => Body::TooLarge,
                    Err(e) => {
                        return Err(TransportError::new(TransportErrorKind::Body, e.to_string()));
                    }
                }
            };
            Ok(HttpResponse {
                status: parts.status,
                headers: parts.headers,
                body,
            })
        };
        match req.timeout {
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
        let d = client
            .send(HttpRequest::get("http://o.example/i", 100))
            .await;
        assert_eq!(d.unwrap_err().kind, TransportErrorKind::Other);

        let sent = fake.requests();
        assert_eq!(sent.len(), 4);
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
            text.resize(text.len().max(pad), b'.');
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
            "POST /interpreter?bbox=1,2,3,4 agent.internal application/x-www-form-urlencoded data=x+y"
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
}
