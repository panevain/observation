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
/// Trunk output without a content hash: a web worker is named after its
/// binary, because the app has to know its URL.
const UNHASHED_PREFIXES: &[&str] = &["/coverage-worker"];

/// Serves `dir`; unknown paths get `index.html` (client-side routing).
///
/// HTML and the unhashed worker files are `no-cache` so deployments are picked
/// up at once; everything else is treated as content-hashed (trunk's output)
/// and cached for a year.
pub fn service(dir: &Path) -> Router {
    let files = ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")));
    Router::new()
        .fallback_service(files)
        .layer(middleware::from_fn(cache_headers))
}

async fn cache_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let unhashed = UNHASHED_PREFIXES.iter().any(|p| path.starts_with(p));
    let mut resp = next.run(req).await;
    if resp.status().is_success() && !resp.headers().contains_key(header::CACHE_CONTROL) {
        let is_html = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"));
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if is_html || unhashed {
                NO_CACHE
            } else {
                IMMUTABLE
            }),
        );
    }
    resp
}
