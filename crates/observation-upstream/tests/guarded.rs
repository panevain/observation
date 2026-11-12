//! `GuardedHttp` against real loopback sockets: the guards and caps that the
//! fakes stand in for elsewhere.
#![allow(clippy::unwrap_used)] // integration-test helpers live outside `#[test]` fns

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use observation_upstream::{
    Body, GuardedHttp, HttpClient, HttpRequest, Redirects, TransportErrorKind,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A loopback server that hands each connection, with the request head and
/// body it read, to `handle`. Returns the port and a count of connections.
async fn serve<F, Fut>(handle: F) -> (u16, Arc<AtomicUsize>)
where
    F: Fn(TcpStream, String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let handle = Arc::new(handle);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            let handle = Arc::clone(&handle);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                handle(sock, request).await;
            });
        }
    });
    (port, count)
}

/// Answers every request with `response`.
async fn serve_fixed(response: &'static str) -> (u16, Arc<AtomicUsize>) {
    serve(move |mut sock, _| async move {
        let _ = sock.write_all(response.as_bytes()).await;
    })
    .await
}

/// Sends `head`, then `piece` over and over, never finishing.
async fn serve_endless(head: &'static str, piece: &'static str) -> u16 {
    serve(move |mut sock, _| async move {
        if sock.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        while sock.write_all(piece.as_bytes()).await.is_ok() {}
    })
    .await
    .0
}

/// Sends `head` (and whatever body it starts), then holds the connection.
async fn serve_stalled(head: &'static str) -> u16 {
    serve(move |mut sock, _| async move {
        let _ = sock.write_all(head.as_bytes()).await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    })
    .await
    .0
}

fn client(allow_private: bool, redirects: Redirects) -> GuardedHttp {
    GuardedHttp::builder(allow_private, redirects)
        .user_agent("observation-test/1")
        .build()
        .unwrap()
}

fn lax() -> GuardedHttp {
    client(true, Redirects::None)
}

/// One 1 KiB chunk of a chunked body.
fn chunk() -> &'static str {
    Box::leak(format!("400\r\n{}\r\n", ".".repeat(1024)).into_boxed_str())
}

#[tokio::test]
async fn redirects_are_returned_not_followed() {
    let (port, count) = serve_fixed(
        "HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    let resp = lax()
        .send(HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100))
        .await
        .unwrap();
    assert_eq!(resp.status, 302);
    assert_eq!(resp.headers["location"], "/elsewhere");
    assert_eq!(resp.body, Body::Complete(Default::default()));
    assert_eq!(count.load(Ordering::SeqCst), 1, "the redirect was followed");
}

#[tokio::test]
async fn private_names_are_refused_unless_allowed() {
    let (port, count) =
        serve_fixed("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
    let url = format!("http://localhost:{port}/");
    let err = client(false, Redirects::None)
        .send(HttpRequest::get(&url, 100))
        .await
        .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::Connect);
    assert!(!err.message.contains("localhost"), "{}", err.message);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let ok = lax().send(HttpRequest::get(&url, 100)).await.unwrap();
    assert_eq!(ok.bytes().unwrap(), "ok");
}

#[tokio::test]
async fn the_user_agent_headers_and_body_reach_the_wire() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = std::sync::Mutex::new(Some(tx));
    let (port, _) = serve(move |mut sock, request| {
        if let Some(tx) = tx.lock().unwrap().take() {
            let _ = tx.send(request);
        }
        async move {
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }
    })
    .await;
    lax()
        .send(
            HttpRequest::post(format!("http://127.0.0.1:{port}/api"), 100)
                .bearer("t0ken")
                .unwrap()
                .form([("data", "a b")]),
        )
        .await
        .unwrap();
    let request = rx.await.unwrap().to_ascii_lowercase();
    assert!(request.starts_with("post /api http/1.1\r\n"), "{request}");
    assert!(
        request.contains("\r\nuser-agent: observation-test/1\r\n"),
        "{request}"
    );
    assert!(
        request.contains("\r\nauthorization: bearer t0ken\r\n"),
        "{request}"
    );
    assert!(
        request.contains("\r\ncontent-type: application/x-www-form-urlencoded\r\n"),
        "{request}"
    );
    assert!(request.ends_with("\r\n\r\ndata=a+b"), "{request}");
}

#[tokio::test]
async fn an_endless_body_is_cut_off_at_the_cap() {
    let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    let port = serve_endless(head, chunk()).await;
    let resp = lax()
        .send(
            HttpRequest::get(format!("http://127.0.0.1:{port}/"), 64 * 1024)
                .timeout(Duration::from_secs(20)),
        )
        .await
        .unwrap();
    assert_eq!((resp.status.as_u16(), resp.body), (200, Body::TooLarge));
}

#[tokio::test]
async fn error_replies_are_capped_by_the_error_cap() {
    let head =
        "HTTP/1.1 502 Bad Gateway\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    let port = serve_endless(head, chunk()).await;
    let resp = lax()
        .send(
            HttpRequest::get(format!("http://127.0.0.1:{port}/"), usize::MAX)
                .timeout(Duration::from_secs(20)),
        )
        .await
        .unwrap();
    assert_eq!((resp.status.as_u16(), resp.body), (502, Body::TooLarge));
}

#[tokio::test]
async fn an_error_cap_of_zero_never_reads_the_body() {
    // The body never comes: reading it would hit the timeout instead.
    let port =
        serve_stalled("HTTP/1.1 502 Bad Gateway\r\nTransfer-Encoding: chunked\r\n\r\n").await;
    let started = Instant::now();
    let resp = lax()
        .send(
            HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100)
                .error_max_bytes(0)
                .timeout(Duration::from_secs(20)),
        )
        .await
        .unwrap();
    assert_eq!((resp.status.as_u16(), resp.body), (502, Body::TooLarge));
    assert!(started.elapsed() < Duration::from_secs(10));
    // A declared-empty error body is complete; a 2xx body is read as usual.
    let (port, _) = serve_fixed(
        "HTTP/1.1 304 Not Modified\r\nETag: \"t\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    let resp = lax()
        .send(HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100).error_max_bytes(0))
        .await
        .unwrap();
    assert_eq!(
        (resp.status.as_u16(), resp.body),
        (304, Body::Complete(Default::default()))
    );
    assert_eq!(resp.headers["etag"], "\"t\"");
    let (port, _) =
        serve_fixed("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
    let resp = lax()
        .send(HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100).error_max_bytes(0))
        .await
        .unwrap();
    assert_eq!(resp.bytes().unwrap(), "ok");
}

#[tokio::test]
async fn a_declared_oversize_body_is_refused_without_reading() {
    // The body never comes: reading it would hit the timeout instead.
    let port = serve_stalled("HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n").await;
    let started = Instant::now();
    let resp = lax()
        .send(
            HttpRequest::get(format!("http://127.0.0.1:{port}/"), 999_999)
                .timeout(Duration::from_secs(20)),
        )
        .await
        .unwrap();
    assert_eq!(resp.body, Body::TooLarge);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn a_stalled_body_times_out() {
    let port = serve_stalled("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabcd").await;
    let err = lax()
        .send(
            HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100)
                .timeout(Duration::from_millis(300)),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::BodyTimeout, "{}", err.message);
    assert_eq!(
        err.status,
        Some(http::StatusCode::OK),
        "the head had arrived"
    );
}

/// A status whose body the request does not want comes back at once,
/// unread, though its body stalls.
#[tokio::test]
async fn an_unwanted_body_is_not_read() {
    let port =
        serve_stalled("HTTP/1.1 206 Partial Content\r\nContent-Length: 10\r\n\r\nabcd").await;
    let resp = lax()
        .send(
            HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100)
                .body_only_for(http::StatusCode::OK)
                .timeout(Duration::from_secs(30)),
        )
        .await
        .unwrap();
    assert_eq!(resp.status, http::StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.body, Body::TooLarge);
}

/// The one status whose body is skipped comes back at once, unread, though
/// its body stalls; another error status is read under its cap.
#[tokio::test]
async fn a_skipped_status_is_not_read() {
    let skipped = |port: u16| {
        HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100)
            .skip_body_of(http::StatusCode::NOT_FOUND)
            .timeout(Duration::from_secs(30))
    };
    let port = serve_stalled("HTTP/1.1 404 Not Found\r\nContent-Length: 10\r\n\r\nabcd").await;
    let resp = lax().send(skipped(port)).await.unwrap();
    assert_eq!(resp.status, http::StatusCode::NOT_FOUND);
    assert_eq!(resp.body, Body::TooLarge);
    let port =
        serve_stalled("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 10\r\n\r\nabcd").await;
    let err = lax()
        .send(skipped(port).timeout(Duration::from_millis(300)))
        .await
        .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::BodyTimeout, "{}", err.message);
    assert_eq!(err.status, Some(http::StatusCode::INTERNAL_SERVER_ERROR));
}

/// Under `skip_error_bodies` an error status comes back at once, unread,
/// though its body stalls; a 3xx is read, under the main cap.
#[tokio::test]
async fn skipped_error_bodies_are_not_read_but_redirects_are() {
    let skipping = |port: u16| {
        HttpRequest::get(format!("http://127.0.0.1:{port}/"), 10)
            .skip_error_bodies()
            .timeout(Duration::from_millis(300))
    };
    for head in [
        "HTTP/1.1 404 Not Found\r\nContent-Length: 10\r\n\r\nabcd",
        "HTTP/1.1 503 Back Soon\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nabcd\r\n",
    ] {
        let port = serve_stalled(head).await;
        let started = Instant::now();
        let resp = lax().send(skipping(port)).await.unwrap();
        assert!(started.elapsed() < Duration::from_millis(300), "{head}");
        assert_eq!(resp.body, Body::Skipped, "{head}");
    }
    let (port, _) = serve_fixed(
        "HTTP/1.1 302 Found\r\nLocation: /x\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789",
    )
    .await;
    let resp = lax().send(skipping(port)).await.unwrap();
    assert_eq!(resp.status, http::StatusCode::FOUND);
    assert_eq!(resp.bytes().unwrap(), "0123456789");
    let (port, _) = serve_fixed(
        "HTTP/1.1 302 Found\r\nLocation: /x\r\nContent-Length: 11\r\nConnection: close\r\n\r\n0123456789A",
    )
    .await;
    let resp = lax().send(skipping(port)).await.unwrap();
    assert_eq!(resp.body, Body::TooLarge, "a 3xx gets the main cap");
    let port = serve_stalled("HTTP/1.1 302 Found\r\nContent-Length: 10\r\n\r\nabcd").await;
    let err = lax().send(skipping(port)).await.unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::BodyTimeout, "{}", err.message);
}

#[tokio::test]
async fn a_slow_answer_times_out_under_the_client_timeout() {
    let port = serve_stalled("").await;
    let client = GuardedHttp::builder(true, Redirects::None)
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let err = client
        .send(HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100))
        .await
        .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::Timeout, "{}", err.message);
}

#[tokio::test]
async fn refused_connections_are_connect_errors() {
    // Bound but not listening, and held for the whole test, so no other
    // socket can take the port meanwhile: connecting is refused.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = socket.local_addr().unwrap().port();
    let err = lax()
        .send(HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100))
        .await
        .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::Connect, "{}", err.message);
    assert_eq!(err.status, None, "no response");
    drop(socket);
}

#[tokio::test]
async fn a_connect_timeout_stays_apart_from_a_read_timeout() {
    // A listener that never accepts, with its backlog filled: further
    // handshakes get no answer, so connecting times out.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = socket.listen(1).unwrap();
    let addr = listener.local_addr().unwrap();
    let mut held = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(200), TcpStream::connect(addr)).await {
            Ok(Ok(s)) => held.push(s),
            Ok(Err(e)) => panic!("could not fill the backlog: {e}"),
            Err(_) => break,
        }
        assert!(held.len() < 64, "the backlog never filled");
    }
    let client = GuardedHttp::builder(true, Redirects::None)
        .connect_timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let err = client
        .send(HttpRequest::get(format!("http://{addr}/"), 100).timeout(Duration::from_secs(20)))
        .await
        .unwrap_err();
    assert_eq!(
        err.kind,
        TransportErrorKind::ConnectTimeout,
        "{}",
        err.message
    );
    drop(listener);
}

#[tokio::test]
async fn head_sizes_come_from_the_header() {
    let (port, _) =
        serve_fixed("HTTP/1.1 200 OK\r\nContent-Length: 123456789\r\nConnection: close\r\n\r\n")
            .await;
    let resp = lax()
        .send(HttpRequest::head(format!(
            "http://127.0.0.1:{port}/x.osm.pbf"
        )))
        .await
        .unwrap();
    assert_eq!(resp.body, Body::Complete(Default::default()));
    assert_eq!(resp.content_length(), Some(123_456_789));
}

/// Runs in a child process that has `HTTP_PROXY` set (the workspace forbids
/// `unsafe`, so the test cannot set it in-process); a no-op otherwise.
#[tokio::test]
async fn proxy_probe_child() {
    let Some(host) = std::env::var_os("OBSERVATION_PROXY_PROBE") else {
        return;
    };
    let target = format!("http://{}/", host.to_string_lossy());
    let err = client(false, Redirects::None)
        .send(HttpRequest::get(&target, 100))
        .await
        .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::Connect, "{}", err.message);
    let resp = lax().send(HttpRequest::get(&target, 100)).await.unwrap();
    assert_eq!(
        resp.status, 200,
        "the proxy variable should apply once the filter is off"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_variables_are_ignored_while_filtering() {
    // A proxy that answers 200 to anything: a filtering client must not use it.
    let (proxy, _) =
        serve_fixed("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "proxy_probe_child", "--nocapture"])
            .env("OBSERVATION_PROXY_PROBE", "metadata.invalid")
            .env("HTTP_PROXY", format!("http://127.0.0.1:{proxy}"))
            .env("http_proxy", format!("http://127.0.0.1:{proxy}"))
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("1 passed"), "{text}");
}

/// Answers every request with `response`, which need not be UTF-8.
async fn serve_bytes(response: &'static [u8]) -> u16 {
    serve(move |mut sock, _| async move {
        let _ = sock.write_all(response).await;
    })
    .await
    .0
}

#[tokio::test]
async fn status_errors_read_as_reqwest_words_them() {
    let plain = reqwest::Client::builder().no_proxy().build().unwrap();
    for (response, own_reason) in [
        (&b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..], false),
        (b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", false),
        (b"HTTP/1.1 404 Gone \"Fishing\"\t\\ x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", true),
        // Not ASCII: hyper keeps an empty phrase, and reqwest prints it so.
        (b"HTTP/1.1 410 Gone \xff\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", true),
        (b"HTTP/1.1 503 Back Soon\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", true),
    ] {
        let url = format!("http://127.0.0.1:{}/x", serve_bytes(response).await);
        let resp = lax().get_stream(&url).await.unwrap();
        assert_eq!(resp.reason.is_some(), own_reason, "{resp:?}");
        let ours = observation_upstream::status_error(resp.status, resp.reason.as_deref());
        // The port carries the same phrase.
        let sent = lax()
            .send(HttpRequest::get(&url, 100).skip_error_bodies())
            .await
            .unwrap();
        assert_eq!((sent.status, &sent.reason), (resp.status, &resp.reason));
        assert_eq!(sent.body, Body::Skipped);
        let theirs = plain
            .get(&url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap_err()
            .without_url()
            .to_string();
        assert_eq!(ours, theirs);
    }
    assert_eq!(
        observation_upstream::status_error(reqwest::StatusCode::NOT_FOUND, None),
        "HTTP status client error (404 Not Found)"
    );
}

#[tokio::test]
async fn streams_are_read_piece_by_piece_with_no_cap() {
    // Far past every cap the port applies: get_stream applies none.
    let port = serve_endless(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        chunk(),
    )
    .await;
    let mut resp = lax()
        .get_stream(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body.content_length(), None);
    let mut read = 0;
    while read < 4 << 20 {
        read += resp.body.chunk().await.unwrap().unwrap().len();
    }

    let (port, _) = serve_fixed(
        "HTTP/1.1 302 Found\r\nLocation: http://a.example/\r\nContent-Length: 5\r\nConnection: close\r\n\r\nmoved",
    )
    .await;
    let mut resp = lax()
        .get_stream(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    assert_eq!(resp.status, 302, "returned, not followed");
    assert_eq!(resp.headers["location"], "http://a.example/");
    assert_eq!(resp.body.content_length(), Some(5));
    let mut body = Vec::new();
    while let Some(piece) = resp.body.chunk().await.unwrap() {
        body.extend_from_slice(&piece);
    }
    assert_eq!(body, b"moved");
}

#[tokio::test]
async fn a_broken_stream_is_a_body_error_with_its_status() {
    let (port, _) =
        serve_fixed("HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabc").await;
    let mut resp = lax()
        .get_stream(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    let err = loop {
        match resp.body.chunk().await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the short body ended cleanly"),
            Err(e) => break e,
        }
    };
    assert_eq!(err.kind, TransportErrorKind::Body, "{}", err.message);
    assert_eq!(err.status, Some(reqwest::StatusCode::OK));

    let err = lax().get_stream("http://127.0.0.1:1/").await.unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::Connect, "{}", err.message);
    assert!(!err.message.contains("127.0.0.1"), "{}", err.message);
}
