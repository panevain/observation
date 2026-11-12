//! The outbound HTTP port: one capped request in, one response out.
//!
//! Services talk to upstreams through [`HttpClient`] rather than a concrete
//! client, so tests can swap in a fake. The size cap is part of the request,
//! not something each caller remembers to apply: an implementation never
//! hands back more than [`HttpRequest::max_bytes`] of body.

use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use http::header::{
    AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderName, HeaderValue, InvalidHeaderValue,
    PROXY_AUTHORIZATION,
};
use http::{HeaderMap, Method, StatusCode};

/// Sends one request to an upstream.
pub trait HttpClient: Send + Sync + fmt::Debug {
    /// Sends `req` and reads the response body up to `req.max_bytes`.
    /// Redirects are returned as responses or followed as the implementation
    /// was configured; a non-success status is a response, not an error.
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>>;
}

/// One outbound request. Its `Debug` output shows the URL without
/// credentials and hides sensitive header values.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    /// Set through [`Self::header`] and [`Self::bearer`] only, so that
    /// credential headers are always marked sensitive; read with
    /// [`Self::headers`].
    headers: HeaderMap,
    pub body: RequestBody,
    /// Longest the whole exchange may take, body included. `None` leaves only
    /// the timeout the client itself was built with, if any.
    pub timeout: Option<Duration>,
    /// Largest body accepted with a 2xx status; a longer one is [`Body::TooLarge`].
    pub max_bytes: usize,
    /// Largest body accepted with any other status, so that an error reply
    /// is not read up to a large success cap. 0 means such a body is never
    /// read: it is [`Body::TooLarge`] unless it is declared empty.
    pub error_max_bytes: usize,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &crate::redact(&self.url))
            .field("headers", &self.headers)
            .field("body", &self.body)
            .field("timeout", &self.timeout)
            .field("max_bytes", &self.max_bytes)
            .field("error_max_bytes", &self.error_max_bytes)
            .finish()
    }
}

/// The default [`HttpRequest::error_max_bytes`].
pub const DEFAULT_ERROR_MAX_BYTES: usize = 64 * 1024;

/// What a request carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestBody {
    None,
    /// Sent as `application/x-www-form-urlencoded`.
    Form(Vec<(String, String)>),
    /// Serialized JSON, sent as `application/json`.
    Json(Bytes),
}

impl RequestBody {
    /// The bytes on the wire and the content type to send unless the request
    /// sets its own (as reqwest's `form` and `json` do).
    pub fn encode(&self) -> Option<(&'static str, Bytes)> {
        match self {
            Self::None => None,
            Self::Form(pairs) => {
                let mut s = form_urlencoded::Serializer::new(String::new());
                for (k, v) in pairs {
                    s.append_pair(k, v);
                }
                Some(("application/x-www-form-urlencoded", Bytes::from(s.finish())))
            }
            Self::Json(bytes) => Some(("application/json", bytes.clone())),
        }
    }
}

impl HttpRequest {
    /// A request with no headers, no body and no timeout of its own.
    pub fn new(method: Method, url: impl Into<String>, max_bytes: usize) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            body: RequestBody::None,
            timeout: None,
            max_bytes,
            error_max_bytes: DEFAULT_ERROR_MAX_BYTES,
        }
    }

    /// The headers set on this request (without the body's content type).
    /// They cannot be written directly, which would bypass the sensitive
    /// marking of credentials:
    ///
    /// ```compile_fail
    /// let mut req = observation_upstream::HttpRequest::get("http://a.example/", 1);
    /// req.headers.insert("authorization", "Bearer s3cret".parse().unwrap());
    /// ```
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Whether a response with `status` must not have its body read at all.
    pub(crate) fn skips_body(&self, status: StatusCode) -> bool {
        !status.is_success() && self.error_max_bytes == 0
    }

    /// The cap that applies to a response with `status`.
    pub fn cap_for(&self, status: StatusCode) -> usize {
        if status.is_success() {
            self.max_bytes
        } else {
            self.error_max_bytes
        }
    }

    pub fn get(url: impl Into<String>, max_bytes: usize) -> Self {
        Self::new(Method::GET, url, max_bytes)
    }

    pub fn post(url: impl Into<String>, max_bytes: usize) -> Self {
        Self::new(Method::POST, url, max_bytes)
    }

    pub fn head(url: impl Into<String>) -> Self {
        Self::new(Method::HEAD, url, 0)
    }

    /// Sets `name`. Credential headers (`Authorization`, `Proxy-Authorization`,
    /// `Cookie`) are marked sensitive, as [`Self::bearer`] does.
    pub fn header(mut self, name: HeaderName, mut value: HeaderValue) -> Self {
        if [AUTHORIZATION, PROXY_AUTHORIZATION, COOKIE].contains(&name) {
            value.set_sensitive(true);
        }
        self.headers.insert(name, value);
        self
    }

    /// `Authorization: Bearer <token>`, marked sensitive so that `Debug`
    /// output (of this request, or of a fake's record) does not show it.
    /// Fails, rather than sending the request without it, if the token
    /// cannot be a header value.
    pub fn bearer(self, token: &str) -> Result<Self, InvalidHeaderValue> {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))?;
        Ok(self.header(AUTHORIZATION, value))
    }

    pub fn error_max_bytes(mut self, max: usize) -> Self {
        self.error_max_bytes = max;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// A form body of `pairs`.
    pub fn form<K: Into<String>, V: Into<String>>(
        mut self,
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.body = RequestBody::Form(
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        );
        self
    }

    /// A JSON body already serialized (with `serde_json::to_vec`, as
    /// reqwest's `json` does).
    pub fn json(mut self, json: impl Into<Bytes>) -> Self {
        self.body = RequestBody::Json(json.into());
        self
    }

    /// The headers to send: [`Self::headers`] plus the body's content type
    /// when the request does not set one; and the body bytes.
    pub fn wire_parts(&self) -> (HeaderMap, Option<Bytes>) {
        let mut headers = self.headers.clone();
        let body = self.body.encode().map(|(content_type, bytes)| {
            headers
                .entry(CONTENT_TYPE)
                .or_insert(HeaderValue::from_static(content_type));
            bytes
        });
        (headers, body)
    }
}

/// One response, with at most the request's `max_bytes` of body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Body,
}

impl HttpResponse {
    /// The body, or `None` if it was over the cap.
    pub fn bytes(&self) -> Option<&Bytes> {
        match &self.body {
            Body::Complete(b) => Some(b),
            Body::TooLarge => None,
        }
    }

    /// The raw `content-length` header, if present and numeric (for `HEAD`).
    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get(http::header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    }
}

/// A response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Complete(Bytes),
    /// Longer than the request's `max_bytes` (declared or while streaming);
    /// reading stopped there.
    TooLarge,
}

/// How a request failed to produce a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind {
    /// No connection (refused, unresolvable, filtered name, TLS).
    Connect,
    /// No connection within the connect timeout: the host was never reached.
    ConnectTimeout,
    /// Connected, but no response (status and headers) within the timeout.
    Timeout,
    /// The response arrived, but its body did not finish within the timeout.
    BodyTimeout,
    /// The response body broke off or could not be decoded.
    Body,
    Other,
}

/// A failed request. The message never contains the URL, which may carry
/// credentials.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub message: String,
}

impl TransportError {
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_bodies_encode_as_reqwest_does() {
        let req = HttpRequest::post("http://o.example/api", 10).form([("data", "a b&c=d/é")]);
        let (headers, body) = req.wire_parts();
        assert_eq!(headers[CONTENT_TYPE], "application/x-www-form-urlencoded");
        assert_eq!(body.unwrap(), "data=a+b%26c%3Dd%2F%C3%A9");
    }

    #[test]
    fn json_bodies_are_sent_as_given_and_an_explicit_content_type_wins() {
        let req = HttpRequest::post("http://a.example/x", 10).json(r#"{"z":1,"a":"x"}"#);
        let (headers, body) = req.wire_parts();
        assert_eq!(headers[CONTENT_TYPE], "application/json");
        assert_eq!(body.unwrap(), r#"{"z":1,"a":"x"}"#);

        let req = req.header(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert_eq!(req.wire_parts().0[CONTENT_TYPE], "text/plain");
        assert_eq!(
            HttpRequest::get("http://a.example/", 1).wire_parts().1,
            None
        );
    }

    #[test]
    fn credentials_stay_out_of_debug_output() {
        let req = HttpRequest::get("https://user:pw@a.example/x", 10)
            .bearer("s3cret")
            .unwrap();
        assert_eq!(req.headers()[AUTHORIZATION], "Bearer s3cret");
        let shown = format!("{req:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(!shown.contains("pw"), "{shown}");
        assert!(shown.contains("https://a.example/x"), "{shown}");
        for name in [AUTHORIZATION, PROXY_AUTHORIZATION, COOKIE] {
            let req = HttpRequest::get("http://a.example/", 1)
                .header(name, HeaderValue::from_static("hidden-value"));
            assert!(!format!("{req:?}").contains("hidden-value"));
        }
        let req = HttpRequest::get("http://a.example/", 1)
            .header(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert!(format!("{req:?}").contains("text/plain"));
    }

    #[test]
    fn an_invalid_bearer_token_is_an_error_not_a_missing_header() {
        assert!(
            HttpRequest::get("http://a.example/", 1)
                .bearer("bad\ntoken")
                .is_err()
        );
    }

    #[test]
    fn error_statuses_get_their_own_cap() {
        let req = HttpRequest::get("http://a.example/", 1 << 20).error_max_bytes(10);
        assert_eq!(req.cap_for(StatusCode::OK), 1 << 20);
        assert_eq!(req.cap_for(StatusCode::NO_CONTENT), 1 << 20);
        assert_eq!(req.cap_for(StatusCode::FOUND), 10);
        assert_eq!(req.cap_for(StatusCode::BAD_GATEWAY), 10);
        assert_eq!(
            HttpRequest::get("http://a.example/", 1).error_max_bytes,
            DEFAULT_ERROR_MAX_BYTES
        );
        let req = HttpRequest::get("http://a.example/", 10).error_max_bytes(0);
        assert!(req.skips_body(StatusCode::BAD_GATEWAY));
        assert!(req.skips_body(StatusCode::NOT_MODIFIED));
        assert!(!req.skips_body(StatusCode::OK));
        assert!(!HttpRequest::get("http://a.example/", 0).skips_body(StatusCode::BAD_GATEWAY));
    }

    #[test]
    fn content_length_is_read_from_the_header() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, HeaderValue::from_static("42"));
        let resp = HttpResponse {
            status: StatusCode::OK,
            headers,
            body: Body::Complete(Bytes::new()),
        };
        assert_eq!(resp.content_length(), Some(42));
        assert_eq!(resp.bytes(), Some(&Bytes::new()));
    }
}
