//! Bearer-token authentication for the MCP endpoint.

use super::config::Token;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

/// The shortest time between two warnings about refused tokens; refusals in
/// between are counted and reported with the next one, so a client guessing
/// tokens cannot flood the log.
pub const WARN_INTERVAL: Duration = Duration::from_secs(60);

/// The configured tokens, each with what a request that presents it gets
/// (for the server, its own MCP service).
#[derive(Debug)]
pub struct Tokens<T> {
    entries: Vec<(Token, T)>,
    warned: Mutex<Warned>,
}

/// When refused tokens were last reported, and how many since.
#[derive(Debug, Default)]
struct Warned {
    at: Option<Instant>,
    unreported: u64,
}

impl<T> Tokens<T> {
    pub fn new(entries: Vec<(Token, T)>) -> Self {
        Self {
            entries,
            warned: Mutex::new(Warned::default()),
        }
    }

    /// The entry whose secret is `given`. Every secret is compared, in
    /// constant time for its length, so the time taken does not show which
    /// one (if any) matched or how much of it was right.
    fn find(&self, given: &[u8]) -> Option<&(Token, T)> {
        let mut found = None;
        for entry in &self.entries {
            if bool::from(entry.0.secret.as_bytes().ct_eq(given)) {
                found = Some(entry);
            }
        }
        found
    }

    /// The token a request's `Authorization: Bearer <secret>` header names,
    /// with its value, or why the request was refused.
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<(&str, &T), Refused> {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(bearer);
        let Some(given) = given else {
            tracing::debug!("refused a request without a bearer token");
            return Err(Refused::Missing);
        };
        match self.find(given.as_bytes()) {
            Some((token, value)) => Ok((token.name.as_str(), value)),
            None => {
                self.warn_refused(Instant::now());
                Err(Refused::Unknown)
            }
        }
    }

    /// Counts a refused token, and warns about it unless it is less than
    /// [`WARN_INTERVAL`] since the last warning. Returns whether it warned.
    fn warn_refused(&self, now: Instant) -> bool {
        let mut w = self
            .warned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        w.unreported += 1;
        if w.at
            .is_some_and(|at| now.duration_since(at) < WARN_INTERVAL)
        {
            return false;
        }
        tracing::warn!(
            refused = w.unreported,
            "refused requests with an unknown bearer token (reported at most once a minute)"
        );
        w.at = Some(now);
        w.unreported = 0;
        true
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

    fn token(name: &str, secret: String) -> Token {
        Token {
            name: name.into(),
            secret,
        }
    }

    fn tokens() -> Tokens<u8> {
        Tokens::new(vec![
            (token("laptop", "a".repeat(32)), 1),
            (token("ci", "b".repeat(32)), 2),
        ])
    }

    fn bearer_header(secret: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {secret}")).expect("header"),
        );
        h
    }

    #[test]
    fn finds_the_matching_token() {
        let t = tokens();
        assert_eq!(
            t.authenticate(&bearer_header(&"a".repeat(32))),
            Ok(("laptop", &1))
        );
        assert_eq!(
            t.authenticate(&bearer_header(&"b".repeat(32))),
            Ok(("ci", &2))
        );
        for wrong in ["a".repeat(31), "a".repeat(33), "c".repeat(32)] {
            assert_eq!(
                t.authenticate(&bearer_header(&wrong)),
                Err(Refused::Unknown)
            );
        }
        assert_eq!(t.authenticate(&HeaderMap::new()), Err(Refused::Missing));
    }

    #[test]
    fn warns_about_refusals_at_most_once_an_interval() {
        let t = tokens();
        let start = Instant::now();
        assert!(t.warn_refused(start));
        assert!(!t.warn_refused(start + Duration::from_secs(1)));
        assert!(!t.warn_refused(start + WARN_INTERVAL - Duration::from_secs(1)));
        assert!(t.warn_refused(start + WARN_INTERVAL));
        assert!(!t.warn_refused(start + WARN_INTERVAL + Duration::from_secs(1)));
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
