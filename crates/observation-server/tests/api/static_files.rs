use axum::http::{StatusCode, header};

use crate::harness::{get_uri, harness_with, json};

#[tokio::test]
async fn static_files_spa_fallback_and_cache_headers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>Observation</title>",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app-50916e44830ffe5e_bg.wasm"),
        b"\0asm\x01\0\0\0",
    )
    .unwrap();
    std::fs::write(dir.path().join("app-50916e44830ffe5e.js"), "export {};").unwrap();
    std::fs::write(
        dir.path().join("coverage-worker_loader.js"),
        "import init from './coverage-worker.js';await init();",
    )
    .unwrap();
    let h = harness_with(
        "http://127.0.0.1:9/o",
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        dir,
        |_| {},
    );

    for uri in ["/", "/index.html", "/some/client/route"] {
        let (status, headers, body) = get_uri(&h.app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(
            String::from_utf8_lossy(&body).contains("<title>Observation</title>"),
            "{uri}"
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache", "{uri}");
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html"),
            "{uri}"
        );
        assert_eq!(headers["x-content-type-options"], "nosniff");
    }

    let (status, headers, body) = get_uri(&h.app, "/app-50916e44830ffe5e_bg.wasm").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/wasm");
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(body.as_ref(), b"\0asm\x01\0\0\0");

    let (status, headers, _) = get_uri(&h.app, "/app-50916e44830ffe5e.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );

    // Files without trunk's content hash, such as the web worker's, are revalidated.
    let (status, headers, _) = get_uri(&h.app, "/coverage-worker_loader.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
}

#[tokio::test]
async fn unknown_api_paths_are_json_404_not_the_spa() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<title>Observation</title>").unwrap();
    let h = harness_with(
        "http://127.0.0.1:9/o",
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        dir,
        |_| {},
    );
    let (status, _, body) = get_uri(&h.app, "/api/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(json(&body)["error"].is_string());
}
