//! [`GuardedHttp::get_stream`]: a GET whose body is read piece by piece, for
//! downloads too large to hold in memory, and [`status_error`], the text a
//! 4xx or 5xx reply is reported with.

use std::fmt::{self, Write as _};

use bytes::Bytes;
use http::{HeaderMap, StatusCode};

use crate::guarded::{GuardedHttp, body_error, transport_error};
use crate::http::TransportError;

/// The head of a streamed response; its body is read with [`ByteStream::chunk`].
#[derive(Debug)]
pub struct StreamingResponse {
    pub status: StatusCode,
    /// The reply's own reason phrase, when it sent one other than the
    /// status's canonical phrase (HTTP/1 only); see [`status_error`].
    pub reason: Option<Bytes>,
    pub headers: HeaderMap,
    pub body: ByteStream,
}

/// A response body read piece by piece. Dropping it closes the connection.
#[derive(Debug)]
pub struct ByteStream(reqwest::Response);

impl ByteStream {
    /// The body's length as the reply declared it.
    pub fn content_length(&self) -> Option<u64> {
        self.0.content_length()
    }

    /// The next piece of the body, `None` at its end. It has no deadline.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        let status = self.0.status();
        self.0
            .chunk()
            .await
            .map_err(|e| body_error(e).in_body_of(status))
    }
}

impl GuardedHttp {
    /// Sends a GET for `url` and returns once the status and headers have
    /// arrived, whatever the status. Unlike
    /// [`HttpClient::send`](crate::HttpClient::send) nothing bounds the body:
    /// the caller must cap its size and time out a stall itself. Only the region agent's extract
    /// download may use it (a test in this crate checks that).
    pub async fn get_stream(&self, url: &str) -> Result<StreamingResponse, TransportError> {
        let resp = self.client.get(url).send().await.map_err(transport_error)?;
        Ok(StreamingResponse {
            status: resp.status(),
            reason: resp
                .extensions()
                .get::<hyper::ext::ReasonPhrase>()
                .map(|r| Bytes::copy_from_slice(r.as_bytes())),
            headers: resp.headers().clone(),
            body: ByteStream(resp),
        })
    }
}

/// The text reqwest's `error_for_status` gives a 4xx or 5xx reply (without
/// the URL): `HTTP status client error (404 Not Found)`, with the reply's
/// own `reason` phrase in place of the canonical one when it sent one.
pub fn status_error(status: StatusCode, reason: Option<&[u8]>) -> String {
    let class = if status.is_client_error() {
        "client"
    } else {
        "server"
    };
    match reason {
        Some(reason) => format!(
            "HTTP status {class} error ({} {})",
            status.as_str(),
            Escaped(reason)
        ),
        None => format!("HTTP status {class} error ({status})"),
    }
}

/// Bytes shown as reqwest shows a reason phrase: Rust byte escapes for
/// anything but printable ASCII.
struct Escaped<'a>(&'a [u8]);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &c in self.0 {
            match c {
                b'\n' => f.write_str("\\n")?,
                b'\r' => f.write_str("\\r")?,
                b'\t' => f.write_str("\\t")?,
                b'\\' | b'"' => write!(f, "\\{}", c as char)?,
                b'\0' => f.write_str("\\0")?,
                0x20..=0x7e => f.write_char(c as char)?,
                _ => write!(f, "\\x{c:02x}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_phrases_are_escaped_as_reqwest_escapes_them() {
        // The loopback test pins the phrases hyper passes on against reqwest;
        // these are the escapes it does not (hyper keeps only ASCII phrases).
        assert_eq!(
            status_error(StatusCode::BAD_GATEWAY, Some(b"a\0\r\n\t\\\"~\x7f\xff")),
            r#"HTTP status server error (502 a\0\r\n\t\\\"~\x7f\xff)"#
        );
        assert_eq!(
            status_error(StatusCode::IM_A_TEAPOT, None),
            "HTTP status client error (418 I'm a teapot)"
        );
    }
}
