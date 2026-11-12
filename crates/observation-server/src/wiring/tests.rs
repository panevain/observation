//! The production wiring over loopback sockets: the settings of the real
//! clients and of the cache, which [`TestApp`](crate::testing::TestApp)
//! replaces with fakes.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use observation_upstream::{HttpRequest, TransportErrorKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::Instant;

use super::*;

const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

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

/// A loopback server answering every request with `response`, or never
/// answering when it is `None`.
async fn serve(response: Option<String>) -> (u16, Arc<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Seen::default());
    let record = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            record.connections.fetch_add(1, Ordering::SeqCst);
            let record = Arc::clone(&record);
            let response = response.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                record.heads.lock().unwrap().push(head);
                match response {
                    Some(r) => drop(sock.write_all(r.as_bytes()).await),
                    None => tokio::time::sleep(Duration::from_secs(60)).await,
                }
            });
        }
    });
    (port, seen)
}

/// The production configuration (no environment) with its cache in `dir`.
fn default_config(dir: &Path) -> Config {
    let mut config = Config::from_lookup(|_| None).unwrap();
    config.cache_dir = dir.to_path_buf();
    config
}

#[tokio::test]
async fn upstream_clients_refuse_private_names_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let config = default_config(dir.path());
    assert!(!config.allow_private_upstreams);
    let ports = production_ports(&config).unwrap().ports;
    let (port, seen) = serve(Some(OK.into())).await;
    for client in [&ports.overpass_http, &ports.tile_http] {
        let err = client
            .send(HttpRequest::get(format!("http://localhost:{port}/"), 100))
            .await
            .unwrap_err();
        assert_eq!(err.kind, TransportErrorKind::Connect);
    }
    assert_eq!(seen.connections(), 0, "a private address was reached");
}

#[tokio::test]
async fn upstream_clients_send_the_user_agent_and_keep_their_timeouts() {
    assert_eq!(TILE_TIMEOUT, Duration::from_secs(20));
    let dir = tempfile::tempdir().unwrap();
    let mut config = default_config(dir.path());
    config.allow_private_upstreams = true;
    config.overpass_timeout = Duration::from_millis(300);
    let ports = production_ports(&config).unwrap().ports;
    let (port, seen) = serve(Some(OK.into())).await;
    for client in [&ports.overpass_http, &ports.tile_http] {
        let resp = client
            .send(HttpRequest::get(format!("http://localhost:{port}/"), 100))
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
    }
    let user_agent = format!("user-agent: {}\r\n", config.user_agent().to_lowercase());
    assert!(user_agent.starts_with("user-agent: observation/"));
    let heads = seen.heads();
    assert_eq!(heads.len(), 2);
    assert!(heads.iter().all(|h| h.contains(&user_agent)), "{heads:?}");

    // A request without a timeout of its own gets the Overpass timeout.
    let (stalled, _) = serve(None).await;
    let started = Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        ports.overpass_http.send(HttpRequest::get(
            format!("http://localhost:{stalled}/"),
            100,
        )),
    )
    .await
    .expect("the Overpass client has a timeout")
    .unwrap_err();
    assert_eq!(err.kind, TransportErrorKind::Timeout);
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(300) && waited < Duration::from_secs(5),
        "{waited:?}"
    );
}

/// The agent client sends the User-Agent and the token, and does not follow
/// a redirect, even to the same host: the token must not go elsewhere.
#[tokio::test]
async fn the_agent_client_keeps_its_token_to_the_first_hop() {
    let (target, target_seen) = serve(Some(OK.into())).await;
    let (agent_port, agent_seen) = serve(Some(format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:{target}/regions\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    )))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut config = default_config(dir.path());
    config.regions_url = Some(format!("http://127.0.0.1:{agent_port}"));
    config.regions_token = Some("agent-secret".into());
    let ports = production_ports(&config).unwrap().ports;
    let agent = ports.agent.expect("an agent client");
    assert!(agent.snapshot(None).await.is_err());
    assert_eq!(target_seen.connections(), 0, "the redirect was followed");
    let heads = agent_seen.heads();
    assert_eq!(heads.len(), 1);
    let user_agent = format!("user-agent: {}\r\n", config.user_agent().to_lowercase());
    assert!(heads[0].contains(&user_agent), "{heads:?}");
    assert!(
        heads[0].contains("authorization: bearer agent-secret\r\n"),
        "{heads:?}"
    );
}

/// The cache is opened with each configured cap and trimmed in the
/// background: only the store over its cap loses its file.
#[tokio::test]
async fn the_cache_gets_its_caps_and_is_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let files = ["bundles/a.obsb", "tiles/1/0/0.png", "dem/1/0/0.png"];
    for file in files {
        let path = dir.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; 1000]).unwrap();
    }
    let mut config = default_config(dir.path());
    // Distinct caps, only the tile cache's below its 1000 bytes: any mix-up
    // trims another store.
    config.bundle_cache_cap_bytes = 2000;
    config.tile_cache_cap_bytes = 500;
    config.dem_cache_cap_bytes = 3000;
    let _ports = production_ports(&config).unwrap().ports;
    let exists = |file: &str| dir.path().join(file).exists();
    let deadline = Instant::now() + Duration::from_secs(5);
    while exists(files[1]) {
        assert!(Instant::now() < deadline, "the tile cache was not trimmed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(exists(files[0]) && exists(files[2]));
}

#[test]
fn at_most_256_event_streams_are_open() {
    assert_eq!(crate::MAX_EVENT_STREAMS, 256);
}
