//! The production ports over loopback sockets: the settings of the one
//! client behind the extract download and the HTTP port (the extract list,
//! the sizes and the boundaries), which the other tests replace with fakes.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use observation_upstream::testing::{FakeHttp, FakeReply};
use observation_upstream::{HttpRequest, TransportErrorKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::Instant;

use super::*;

/// What a loopback server saw: connections, and the request heads.
#[derive(Default)]
struct Seen {
    connections: AtomicUsize,
    heads: Mutex<Vec<String>>,
}

impl Seen {
    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// The request heads, lowercased.
    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

/// A loopback server answering every request with what `answer` gives for
/// its request head.
async fn serve(answer: impl Fn(&str) -> String + Send + Sync + 'static) -> (u16, Arc<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Seen::default());
    let record = Arc::clone(&seen);
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            record.connections.fetch_add(1, Ordering::SeqCst);
            let (record, answer) = (Arc::clone(&record), Arc::clone(&answer));
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let response = answer(&head);
                record.heads.lock().unwrap().push(head);
                let _ = sock.write_all(response.as_bytes()).await;
            });
        }
    });
    (port, seen)
}

const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

fn redirect(to: &str) -> String {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// The production configuration (no environment but the data directory).
fn config(dir: &Path, allow_private: bool) -> Config {
    let mut config = Config::from_lookup(|k| match k {
        "OBS_REGIONS_DATA_DIR" => Some(dir.display().to_string()),
        _ => None,
    })
    .unwrap();
    config.allow_private_upstreams = allow_private;
    config
}

#[tokio::test]
async fn the_download_refuses_private_names_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), false);
    let ports = Ports::production(&config).unwrap();
    let (port, seen) = serve(|_| OK.to_owned()).await;
    let err = ports
        .download
        .get(&format!("http://localhost:{port}/x-latest.osm.pbf"))
        .await
        .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::Connect, "{}", err.message);
    let err = ports
        .http
        .send(HttpRequest::head(format!(
            "http://localhost:{port}/x-latest.osm.pbf"
        )))
        .await
        .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::Connect, "{}", err.message);
    assert_eq!(seen.connections(), 0, "a private address was reached");
}

/// Redirects to the same host are followed (to the dated file); a redirect
/// to another host is returned, not followed. The User-Agent goes with
/// every request.
#[tokio::test]
async fn the_download_follows_redirects_only_on_its_host_and_sends_the_user_agent() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), true);
    let ports = Ports::production(&config).unwrap();
    let (other, other_seen) = serve(|_| OK.to_owned()).await;
    let (port, seen) = serve(move |head| {
        if head.starts_with("get /latest ") {
            redirect("/dated")
        } else if head.starts_with("get /away ") {
            // Another host: `localhost` is not `127.0.0.1`.
            redirect(&format!("http://localhost:{other}/dated"))
        } else {
            OK.to_owned()
        }
    })
    .await;

    let mut resp = ports
        .download
        .get(&format!("http://127.0.0.1:{port}/latest"))
        .await
        .unwrap();
    assert_eq!(resp.status, 200, "the same-host redirect was not followed");
    assert_eq!(resp.content_length, Some(2));
    assert_eq!(resp.body.chunk().await.unwrap().unwrap(), "ok");

    let resp = ports
        .download
        .get(&format!("http://127.0.0.1:{port}/away"))
        .await
        .unwrap();
    assert_eq!(resp.status, 302);
    assert_eq!(
        other_seen.connections(),
        0,
        "the cross-host redirect was followed"
    );

    // The same through the HTTP port.
    let resp = ports
        .http
        .send(HttpRequest::get(
            format!("http://127.0.0.1:{port}/latest"),
            2,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status, 200, "the same-host redirect was not followed");
    assert_eq!(resp.bytes().unwrap(), "ok");
    let resp = ports
        .http
        .send(HttpRequest::get(format!("http://127.0.0.1:{port}/away"), 2))
        .await
        .unwrap();
    assert_eq!(resp.status, 302);
    assert_eq!(
        other_seen.connections(),
        0,
        "the cross-host redirect was followed"
    );

    let heads = seen.heads();
    assert_eq!(heads.len(), 6, "{heads:?}");
    let user_agent = format!("user-agent: {}\r\n", config.user_agent().to_lowercase());
    assert!(user_agent.starts_with("user-agent: observation-regions/"));
    assert!(heads.iter().all(|h| h.contains(&user_agent)), "{heads:?}");
}

/// Runs `test` on its own current-thread runtime with paused time, and fails
/// if it takes more than 10 s of real time, so that a probe that hangs cannot
/// hang the suite.
///
/// The connect-timeout probe below is a declared exception to "socket tests
/// run on real time": it uses a loopback socket only, reqwest's timeouts run
/// on Tokio's timer, so paused time makes it exact and instant (the real-time
/// form would wait 20 s), and this guard bounds it in real time.
fn paused_with_a_real_time_guard<F>(test: impl FnOnce() -> F + Send + 'static)
where
    F: std::future::Future<Output = ()>,
{
    let (done, finished) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(test());
        let _ = done.send(());
    });
    match finished.recv_timeout(Duration::from_secs(10)) {
        Ok(()) => runner.join().unwrap(),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            // The test panicked: report its panic.
            if let Err(panic) = runner.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("the probe took more than 10 s of real time")
        }
    }
}

/// The download gives up connecting after 20 s. A loopback listener whose
/// accept queue is full never completes another connection (Linux drops the
/// SYN while `tcp_abort_on_overflow` is off, its default).
#[test]
fn the_download_stops_connecting_after_20_s() {
    paused_with_a_real_time_guard(|| async {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener = socket.listen(1).unwrap();
        let addr = listener.local_addr().unwrap();
        // Fill the accept queue: connect until a connection no longer completes.
        let mut queued = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(1), tokio::net::TcpStream::connect(addr))
                .await
            {
                Ok(stream) => queued.push(stream.unwrap()),
                Err(_) => break,
            }
            assert!(queued.len() < 16, "the accept queue never filled");
        }

        let dir = tempfile::tempdir().unwrap();
        let ports = Ports::production(&config(dir.path(), true)).unwrap();
        let started = Instant::now();
        let err = ports
            .download
            .get(&format!("http://{addr}/x-latest.osm.pbf"))
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            TransportErrorKind::ConnectTimeout,
            "{}",
            err.message
        );
        assert_eq!(started.elapsed(), Duration::from_secs(20));
        // The HTTP port's requests carry their own (shorter) timeouts; with
        // a longer one, the connect timeout ends the request at 20 s.
        let started = Instant::now();
        let req = HttpRequest::head(format!("http://{addr}/x-latest.osm.pbf"))
            .timeout(Duration::from_secs(60));
        let err = ports.http.send(req).await.unwrap_err();
        assert_eq!(
            err.kind,
            TransportErrorKind::ConnectTimeout,
            "{}",
            err.message
        );
        assert_eq!(started.elapsed(), Duration::from_secs(20));
        drop(listener);
    });
}

/// The agent built from its ports sends the extract list, the sizes and the
/// boundary through `ports.http`, and contacts nothing else.
#[tokio::test]
async fn the_catalog_and_the_boundary_go_through_the_http_port() {
    const INDEX: &str = r#"{"features":[{"properties":{"id":"monaco","name":"Monaco",
      "urls":{"pbf":"https://download.geofabrik.de/europe/monaco-latest.osm.pbf"}}}]}"#;
    const SQUARE: &str = "sq\n1\n 0 0\n 1 0\n 1 1\n 0 1\nEND\nEND\n";
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), false);
    assert_eq!(
        config.catalog_url,
        "https://download.geofabrik.de/index-v1-nogeom.json"
    );
    let fake = FakeHttp::new();
    fake.on(http::Method::GET, &config.catalog_url, FakeReply::ok(INDEX));
    let sized =
        FakeReply::status(http::StatusCode::OK).header(http::header::CONTENT_LENGTH, "1234");
    let pbf = "https://download.geofabrik.de/europe/monaco-latest.osm.pbf";
    fake.on(http::Method::HEAD, pbf, sized);
    let poly = "https://download.geofabrik.de/europe/monaco.poly";
    fake.on(http::Method::GET, poly, FakeReply::ok(SQUARE));
    let ports = Ports {
        http: Arc::new(fake.clone()),
        ..Ports::production(&config).unwrap()
    };
    let agent = Agent::from_parts(&config, ports).unwrap();

    let extract = agent.catalog.find("monaco").await.unwrap().unwrap();
    let sizes = agent.catalog.sizes(&["monaco".to_owned()]).await.unwrap();
    assert_eq!(sizes["monaco"], Some(1234));
    let boundary = crate::import::fetch_poly(&*agent.tools.http, &extract.poly_url).await;
    assert_eq!(boundary.unwrap(), SQUARE);
    assert_eq!(fake.requests().len(), 3);
    fake.assert_drained();
}
