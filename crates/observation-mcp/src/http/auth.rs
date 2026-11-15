//! Bearer-token authentication for the MCP endpoint.

use super::config::Token;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

/// The name of the token that authenticated a request, in its extensions
/// (and so in an MCP handler's `RequestContext` extensions, through the
/// request's `http::request::Parts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientName(pub String);

/// The configured tokens, shared by every request.
#[derive(Debug)]
pub struct Tokens(Vec<Token>);

impl Tokens {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self(tokens)
    }

    /// The tokens, in the order they were configured.
    pub fn iter(&self) -> impl Iterator<Item = &Token> {
        self.0.iter()
    }

    /// The index of the token whose secret is `given`. Every secret is
    /// compared, in constant time for its length, so the time taken does not
    /// show which one (if any) matched or how much of it was right.
    fn find(&self, given: &[u8]) -> Option<usize> {
        let mut found = None;
        for (i, t) in self.0.iter().enumerate() {
            if bool::from(t.secret.as_bytes().ct_eq(given)) {
                found = Some(i);
            }
        }
        found
    }

    /// The index of the token a request's `Authorization: Bearer <secret>`
    /// header names, or why it was refused.
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<usize, Refused> {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(bearer);
        let Some(given) = given else {
            tracing::debug!("refused a request without a bearer token");
            return Err(Refused::Missing);
        };
        self.find(given.as_bytes()).ok_or_else(|| {
            tracing::warn!("refused a request with an unknown bearer token");
            Refused::Unknown
        })
    }
}

/// The credentials of an `Authorization` value with the `Bearer` scheme,
/// which RFC 7235 makes case-insensitive.
fn bearer(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_once(' ')?;
    let token = rest.trim_matches(' ');
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// Why a request was refused. It answers 401 with a `WWW-Authenticate`
/// challenge (RFC 6750).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// No bearer token.
    Missing,
    /// A bearer token that matches none of the configured ones.
    Unknown,
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        let challenge = match self {
            Self::Missing => "Bearer realm=\"observation-mcp\"",
            Self::Unknown => "Bearer realm=\"observation-mcp\", error=\"invalid_token\"",
        };
        let mut response = (
            StatusCode::UNAUTHORIZED,
            "missing or unknown bearer token\n",
        )
            .into_response();
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(challenge),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> Tokens {
        Tokens::new(vec![
            Token {
                name: "laptop".into(),
                secret: "a".repeat(32),
            },
            Token {
                name: "ci".into(),
                secret: "b".repeat(32),
            },
        ])
    }

    #[test]
    fn finds_the_matching_token() {
        let t = tokens();
        assert_eq!(t.find("a".repeat(32).as_bytes()), Some(0));
        assert_eq!(t.find("b".repeat(32).as_bytes()), Some(1));
        assert_eq!(t.find("a".repeat(31).as_bytes()), None);
        assert_eq!(t.find("a".repeat(33).as_bytes()), None);
        assert_eq!(t.find(b""), None);
    }

    #[test]
    fn parses_the_bearer_scheme() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer  abc "), Some("abc"));
        assert_eq!(bearer("Basic abc"), None);
        assert_eq!(bearer("Bearer "), None);
        assert_eq!(bearer("Bearer"), None);
    }
}
