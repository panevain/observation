//! [`GuardedHttp`]: the production [`HttpClient`], a reqwest client built
//! with the guards of [`client_builder`](crate::client_builder).

use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use crate::http::{
    Body, HttpClient, HttpRequest, HttpResponse, TransportError, TransportErrorKind,
};
use crate::{Redirects, client_builder, read_capped};

/// The [`HttpClient`] for real upstreams. It can only be built through
/// [`GuardedHttp::builder`], which starts from [`client_builder`], so every
/// instance resolves names through the address filter, ignores proxy
/// variables unless private upstreams are allowed, and follows only the
/// redirects its [`Redirects`] policy allows.
#[derive(Debug, Clone)]
pub struct GuardedHttp {
    client: reqwest::Client,
}

/// Configures a [`GuardedHttp`]; only settings that cannot weaken the guards
/// are exposed.
#[derive(Debug)]
pub struct GuardedHttpBuilder(reqwest::ClientBuilder);

impl GuardedHttp {
    /// A builder starting from [`client_builder`]`(allow_private, redirects)`.
    pub fn builder(allow_private: bool, redirects: Redirects) -> GuardedHttpBuilder {
        GuardedHttpBuilder(client_builder(allow_private, redirects))
    }

    async fn exchange(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let (headers, body) = req.wire_parts();
        let mut builder = self
            .client
            .request(req.method.clone(), &req.url)
            .headers(headers);
        if let Some(body) = body {
            builder = builder.body(body);
        }
        if let Some(timeout) = req.timeout {
            builder = builder.timeout(timeout);
        }
        let resp = builder.send().await.map_err(transport_error)?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = if req.skips_body(status) {
            // Dropped unread.
            match resp.content_length() {
                Some(0) => Body::Complete(Default::default()),
                _ => Body::TooLarge,
            }
        } else {
            // Read here, under the request's own deadline, as reqwest's timeout covers the body.
            match read_capped(resp, req.cap_for(status))
                .await
                .map_err(body_error)?
            {
                Some(bytes) => Body::Complete(bytes),
                None => Body::TooLarge,
            }
        };
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

impl GuardedHttpBuilder {
    /// The `User-Agent` sent with every request.
    pub fn user_agent(self, user_agent: &str) -> Self {
        Self(self.0.user_agent(user_agent))
    }

    /// A timeout for every request, body included; a request's own
    /// [`HttpRequest::timeout`] replaces it.
    pub fn timeout(self, timeout: Duration) -> Self {
        Self(self.0.timeout(timeout))
    }

    /// Replaces [`client_builder`]'s 10 s connect timeout.
    pub fn connect_timeout(self, timeout: Duration) -> Self {
        Self(self.0.connect_timeout(timeout))
    }

    pub fn build(self) -> Result<GuardedHttp, reqwest::Error> {
        Ok(GuardedHttp {
            client: self.0.build()?,
        })
    }
}

impl HttpClient for GuardedHttp {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        self.exchange(req).boxed()
    }
}

/// [`transport_error`] for a failure while reading the body, where a
/// timeout is a [`TransportErrorKind::BodyTimeout`].
fn body_error(e: reqwest::Error) -> TransportError {
    let mut error = transport_error(e);
    if error.kind == TransportErrorKind::Timeout {
        error.kind = TransportErrorKind::BodyTimeout;
    }
    error
}

/// Classifies a reqwest error. A timeout while connecting stays apart from
/// one after connecting: the host was never reached, which is not the same
/// as being slow.
fn transport_error(e: reqwest::Error) -> TransportError {
    let kind = if e.is_timeout() && e.is_connect() {
        TransportErrorKind::ConnectTimeout
    } else if e.is_timeout() {
        TransportErrorKind::Timeout
    } else if e.is_connect() {
        TransportErrorKind::Connect
    } else if e.is_body() || e.is_decode() {
        TransportErrorKind::Body
    } else {
        TransportErrorKind::Other
    };
    TransportError::new(kind, e.without_url().to_string())
}
