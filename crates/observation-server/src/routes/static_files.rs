//! Static client files with SPA fallback and cache headers.

use std::path::Path;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use tower_http::services::{ServeDir, ServeFile};

const NO_CACHE: &str = "no-cache";
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Serves `dir`; unknown paths get `index.html` (client-side routing).
///
/// Files whose names carry trunk's content hash (`filehash = true`) are cached
/// for a year. Everything else, HTML and the web worker files trunk names
/// without a hash included, is `no-cache` so deployments are picked up at once.
pub fn service(dir: &Path) -> Router {
    let files = ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")));
    Router::new()
        .fallback_service(files)
        .layer(middleware::from_fn(cache_headers))
}

async fn cache_headers(req: Request, next: Next) -> Response {
    let hashed = is_content_hashed(req.uri().path());
    let mut resp = next.run(req).await;
    if resp.status().is_success() && !resp.headers().contains_key(header::CACHE_CONTROL) {
        let is_html = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"));
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if hashed && !is_html {
                IMMUTABLE
            } else {
                NO_CACHE
            }),
        );
    }
    resp
}

/// Whether a file name carries trunk's content hash: `<stem>-<hash>` before
/// `_bg.wasm` or the extension (`observation-client-50916e44830ffe5e.js`,
/// `.._bg.wasm`, `style-<hash>.css`), where the hash is a 64-bit SeaHash in
/// lower-case hex without leading zeros. Eight digits are required, so a short
/// hex word after a dash does not count (a hash that short has odds of 1 in 4
/// billion).
fn is_content_hashed(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name
        .strip_suffix("_bg.wasm")
        .or_else(|| name.rsplit_once('.').map(|(stem, _)| stem))
        .unwrap_or(name);
    stem.rsplit_once('-').is_some_and(|(_, hash)| {
        (8..=16).contains(&hash.len())
            && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

#[cfg(test)]
mod tests {
    use super::is_content_hashed;

    #[test]
    fn recognises_trunk_hashes() {
        for hashed in [
            "/observation-client-50916e44830ffe5e.js",
            "/observation-client-50916e44830ffe5e_bg.wasm",
            "/style-0a1b2c3d4e5f.css",
        ] {
            assert!(is_content_hashed(hashed), "{hashed}");
        }
        for unhashed in [
            "/",
            "/index.html",
            "/coverage-worker.js",
            "/coverage-worker_bg.wasm",
            "/coverage-worker_loader.js",
            "/app-abc123.js",
            "/observation-client-50916E44830FFE5E.js",
            "/favicon.ico",
        ] {
            assert!(!is_content_hashed(unhashed), "{unhashed}");
        }
    }
}
