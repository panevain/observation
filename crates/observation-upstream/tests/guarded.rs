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
    assert_eq!(err.kind, TransportErrorKind::Timeout, "{}", err.message);
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
    // Bound, then closed: nothing listens on the port.
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let err = lax()
        .send(HttpRequest::get(format!("http://127.0.0.1:{port}/"), 100))
        .await
        .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::Connect, "{}", err.message);
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
