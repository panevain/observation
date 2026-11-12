//! The list of importable extracts: Geofabrik's `index-v1-nogeom.json`,
//! fetched on demand and kept in memory and on disk for a day, and the
//! extracts' download sizes, asked for with `HEAD` and kept in memory.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use observation_core::regions::{CatalogEntry, is_valid_region_id};
use observation_upstream::{HttpClient, HttpRequest, status_error};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::ports::{Clock, checked_body};

/// How long a fetched index is used before it is fetched again.
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// After a failed fetch, an old list is used for this long before trying again.
const RETRY_AFTER: Duration = Duration::from_secs(600);
/// Largest index accepted (the real one is about 0.5 MB).
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
/// How long a download size is reused (Geofabrik publishes daily).
const SIZE_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// How long an unknown size (the `HEAD` failed) is kept before asking again.
const SIZE_RETRY_AFTER: Duration = Duration::from_secs(600);
/// `HEAD` requests in flight at once.
const SIZE_CONCURRENCY: usize = 4;
/// Longest one `HEAD` may take.
const SIZE_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest one [`Catalog::sizes`] call spends waiting for the cache and
/// asking; sizes not asked for by then are answered as unknown (and not
/// cached, so they are asked next time).
const SIZES_BUDGET: Duration = Duration::from_secs(10);

/// An extract with the URLs needed to import it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extract {
    pub entry: CatalogEntry,
    /// The `.osm.pbf` download.
    pub pbf_url: String,
    /// The boundary (`.poly`), next to the download.
    pub poly_url: String,
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("could not fetch the extract list: {0}")]
    Fetch(String),
    #[error("the extract list could not be read: {0}")]
    Parse(String),
}

#[derive(Debug, Deserialize)]
struct Index {
    features: Vec<Feature>,
}

#[derive(Debug, Deserialize)]
struct Feature {
    properties: Properties,
}

#[derive(Debug, Deserialize)]
struct Properties {
    id: String,
    name: String,
    parent: Option<String>,
    urls: Urls,
}

#[derive(Debug, Deserialize)]
struct Urls {
    pbf: Option<String>,
}

/// Parses the index, keeping extracts with a valid id and a `-latest.osm.pbf`
/// download on the same host as `index_url` (so a tampered index cannot send
/// the agent elsewhere). Sorted by id.
pub fn parse_index(json: &[u8], index_url: &str) -> Result<Vec<Extract>, CatalogError> {
    let index: Index =
        serde_json::from_slice(json).map_err(|e| CatalogError::Parse(e.to_string()))?;
    let origin = origin(index_url);
    let mut out: Vec<Extract> = index
        .features
        .into_iter()
        .filter_map(|f| {
            let p = f.properties;
            let pbf = p.urls.pbf?;
            let base = pbf.strip_suffix("-latest.osm.pbf")?;
            if !is_valid_region_id(&p.id) || origin.is_none() || origin != self::origin(&pbf) {
                return None;
            }
            Some(Extract {
                poly_url: format!("{base}.poly"),
                pbf_url: pbf,
                entry: CatalogEntry {
                    // Some names repeat the id (`us/california`); show the last part.
                    name: p
                        .name
                        .rsplit('/')
                        .next()
                        .unwrap_or(&p.name)
                        .chars()
                        .take(100)
                        .collect(),
                    id: p.id,
                    parent: p.parent.filter(|p| is_valid_region_id(p)),
                },
            })
        })
        .collect();
    out.sort_by(|a, b| a.entry.id.cmp(&b.entry.id));
    Ok(out)
}

/// `scheme://host[:port]` of an http(s) URL.
fn origin(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.origin().ascii_serialization())
}

/// Fetches and caches the index.
#[derive(Debug)]
pub struct Catalog {
    http: Arc<dyn HttpClient>,
    clock: Arc<dyn Clock>,
    url: String,
    file: PathBuf,
    cached: Mutex<Option<(SystemTime, Arc<Vec<Extract>>)>>,
    /// Download sizes by id, with when they were asked for. Held across the
    /// `HEAD`s, so concurrent callers never ask about the same extract twice.
    sizes: Mutex<HashMap<String, (Instant, Option<u64>)>>,
}

impl Catalog {
    pub fn new(
        http: Arc<dyn HttpClient>,
        clock: Arc<dyn Clock>,
        url: String,
        file: PathBuf,
    ) -> Self {
        Self {
            http,
            clock,
            url,
            file,
            cached: Mutex::new(None),
            sizes: Mutex::new(HashMap::new()),
        }
    }

    /// Download sizes of the extracts in `ids` that the catalog has (others
    /// are left out). Unknown sizes are `None`. Asks the download server with
    /// `HEAD` for sizes not known from the last day, for up to [`SIZES_BUDGET`].
    pub async fn sizes(
        &self,
        ids: &[String],
    ) -> Result<BTreeMap<String, Option<u64>>, CatalogError> {
        let list = self.extracts().await?;
        let wanted: Vec<&Extract> = ids
            .iter()
            .filter_map(|id| list.iter().find(|x| &x.entry.id == id))
            .collect();
        // Counted from before the lock: a caller queued behind a slow one
        // answers from what that one found instead of starting its own budget.
        let deadline = Instant::now() + SIZES_BUDGET;
        let mut cache = self.sizes.lock().await;
        let fresh = |e: &(Instant, Option<u64>)| {
            let ttl = if e.1.is_some() {
                SIZE_MAX_AGE
            } else {
                SIZE_RETRY_AFTER
            };
            e.0.elapsed() < ttl
        };
        cache.retain(|_, e| fresh(e));
        let mut missing: Vec<(String, String)> = wanted
            .iter()
            .filter(|x| !cache.contains_key(&x.entry.id))
            .map(|x| (x.entry.id.clone(), x.pbf_url.clone()))
            .collect();
        missing.sort();
        missing.dedup();
        // Up to SIZE_CONCURRENCY in flight, a new one started as each ends
        // (if it can end before the deadline), so one slow `HEAD` holds up no other.
        let mut queue = missing.into_iter();
        let mut set = tokio::task::JoinSet::new();
        loop {
            while set.len() < SIZE_CONCURRENCY && Instant::now() + SIZE_TIMEOUT <= deadline {
                let Some((id, url)) = queue.next() else { break };
                let http = Arc::clone(&self.http);
                set.spawn(async move { (id, head_size(&*http, &url).await) });
            }
            let Some(done) = set.join_next().await else {
                break;
            };
            if let Ok((id, size)) = done {
                cache.insert(id, (Instant::now(), size));
            }
        }
        Ok(wanted
            .iter()
            .map(|x| {
                let id = x.entry.id.clone();
                let size = cache.get(&id).and_then(|e| e.1);
                (id, size)
            })
            .collect())
    }

    /// The extracts, refetched when older than a day. A failed refetch falls
    /// back to the copy on disk, however old.
    pub async fn extracts(&self) -> Result<Arc<Vec<Extract>>, CatalogError> {
        // A time in the future is not fresh: the list is fetched again.
        let fresh = |at: SystemTime| {
            self.clock
                .now()
                .duration_since(at)
                .is_ok_and(|age| age < MAX_AGE)
        };
        let mut cached = self.cached.lock().await;
        if let Some((at, list)) = cached.as_ref()
            && fresh(*at)
        {
            return Ok(Arc::clone(list));
        }
        let disk = tokio::fs::read(&self.file).await.ok();
        let disk_age = tokio::fs::metadata(&self.file)
            .await
            .and_then(|m| m.modified())
            .ok();
        if let (Some(bytes), Some(at)) = (&disk, disk_age)
            && fresh(at)
            && let Ok(list) = parse_index(bytes, &self.url)
        {
            let list = Arc::new(list);
            *cached = Some((at, Arc::clone(&list)));
            return Ok(list);
        }
        match self.fetch().await {
            Ok((bytes, list)) => {
                if let Err(e) = crate::write_atomic(&self.file, &bytes).await {
                    tracing::warn!(error = %e, "could not cache the extract list");
                }
                let list = Arc::new(list);
                *cached = Some((self.clock.now(), Arc::clone(&list)));
                Ok(list)
            }
            Err(e) => {
                let stale = disk.and_then(|b| parse_index(&b, &self.url).ok());
                match stale {
                    Some(list) => {
                        tracing::warn!(error = %e, "using an old extract list");
                        let list = Arc::new(list);
                        // Kept as if fetched long ago, so the next fetch is tried in
                        // RETRY_AFTER rather than by every caller meanwhile.
                        let at = self
                            .clock
                            .now()
                            .checked_sub(MAX_AGE.saturating_sub(RETRY_AFTER))
                            .unwrap_or(SystemTime::UNIX_EPOCH);
                        *cached = Some((at, Arc::clone(&list)));
                        Ok(list)
                    }
                    None => Err(e),
                }
            }
        }
    }

    /// The extract with this id.
    pub async fn find(&self, id: &str) -> Result<Option<Extract>, CatalogError> {
        Ok(self
            .extracts()
            .await?
            .iter()
            .find(|x| x.entry.id == id)
            .cloned())
    }

    async fn fetch(&self) -> Result<(Vec<u8>, Vec<Extract>), CatalogError> {
        let req = HttpRequest::get(&self.url, MAX_INDEX_BYTES)
            .timeout(Duration::from_secs(30))
            .skip_error_bodies();
        let resp = self
            .http
            .send(req)
            .await
            .map_err(|e| CatalogError::Fetch(e.to_string()))?;
        let bytes = checked_body(resp)
            .map_err(CatalogError::Fetch)?
            .ok_or_else(|| CatalogError::Fetch("the list is unexpectedly large".into()))?;
        let list = parse_index(&bytes, &self.url)?;
        if list.is_empty() {
            return Err(CatalogError::Parse("it lists no extracts".into()));
        }
        Ok((bytes.to_vec(), list))
    }
}

/// The `Content-Length` of a `HEAD` for `url`, following its redirects (to
/// the dated file). Read from the header, not from the (empty) body.
async fn head_size(http: &dyn HttpClient, url: &str) -> Option<u64> {
    let req = HttpRequest::head(url)
        .timeout(SIZE_TIMEOUT)
        .skip_error_bodies();
    let error = match http.send(req).await {
        Ok(resp) if resp.status.is_client_error() || resp.status.is_server_error() => {
            status_error(resp.status, resp.reason.as_deref())
        }
        Ok(resp) => return resp.content_length(),
        Err(e) => e.to_string(),
    };
    tracing::debug!(error = %error, "extract size unknown");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"{"type":"FeatureCollection","features":[
      {"type":"Feature","properties":{"id":"monaco","parent":"europe","name":"Monaco",
        "urls":{"pbf":"https://download.geofabrik.de/europe/monaco-latest.osm.pbf"}}},
      {"type":"Feature","properties":{"id":"us/california","parent":"north-america","name":"us/california",
        "urls":{"pbf":"https://download.geofabrik.de/north-america/us/california-latest.osm.pbf"}}},
      {"type":"Feature","properties":{"id":"evil","name":"Evil",
        "urls":{"pbf":"https://evil.example/x-latest.osm.pbf"}}},
      {"type":"Feature","properties":{"id":"Bad Id","name":"x",
        "urls":{"pbf":"https://download.geofabrik.de/x-latest.osm.pbf"}}},
      {"type":"Feature","properties":{"id":"nopbf","name":"x","urls":{}}}
    ]}"#;

    #[test]
    fn parses_and_filters() {
        let list = parse_index(
            INDEX.as_bytes(),
            "https://download.geofabrik.de/index-v1-nogeom.json",
        )
        .unwrap();
        let ids: Vec<&str> = list.iter().map(|x| x.entry.id.as_str()).collect();
        assert_eq!(ids, vec!["monaco", "us/california"]);
        assert_eq!(
            list[0].poly_url,
            "https://download.geofabrik.de/europe/monaco.poly"
        );
        assert_eq!(list[1].entry.name, "california");
        assert_eq!(list[0].entry.parent.as_deref(), Some("europe"));
    }

    #[tokio::test]
    async fn sizes_come_from_head_and_are_cached() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let index = format!(
            r#"{{"features":[
              {{"properties":{{"id":"monaco","name":"Monaco","parent":"europe",
                "urls":{{"pbf":"{base}/europe/monaco-latest.osm.pbf"}}}}}},
              {{"properties":{{"id":"gone","name":"Gone",
                "urls":{{"pbf":"{base}/gone-latest.osm.pbf"}}}}}}
            ]}}"#
        );
        let counter = Arc::clone(&hits);
        let app = axum::Router::new()
            .route(
                "/index.json",
                axum::routing::get(move || async move { index }),
            )
            .route(
                "/europe/monaco-latest.osm.pbf",
                axum::routing::get(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { vec![0u8; 1234] }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await });
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::new(
            Arc::new(
                observation_upstream::GuardedHttp::builder(
                    true,
                    observation_upstream::Redirects::SameHost,
                )
                .build()
                .unwrap(),
            ),
            Arc::new(crate::ports::SystemClock),
            format!("{base}/index.json"),
            dir.path().join("catalog.json"),
        );
        let ids = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let sizes = catalog
            .sizes(&ids(&["monaco", "gone", "nowhere", "monaco"]))
            .await
            .unwrap();
        assert_eq!(
            sizes,
            BTreeMap::from([("gone".to_owned(), None), ("monaco".to_owned(), Some(1234))]),
            "unknown ids left out, a failed HEAD is null"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "asked once for a repeated id"
        );
        catalog.sizes(&ids(&["monaco"])).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1, "cached");
    }

    /// The index: a 30 s GET whose error bodies are not read, capped at
    /// 16 MiB, and the sentence for each way it fails.
    #[tokio::test]
    async fn fetch_failures_are_described() {
        use http::StatusCode;
        use observation_upstream::TransportErrorKind;
        use observation_upstream::testing::{FakeHttp, FakeReply};
        const URL: &str = "https://download.geofabrik.de/index-v1-nogeom.json";
        let cases = [
            (
                FakeReply::status(StatusCode::NOT_FOUND),
                "could not fetch the extract list: HTTP status client error (404 Not Found)",
            ),
            (
                FakeReply::status(StatusCode::BAD_GATEWAY).reason("Upstream Down"),
                "could not fetch the extract list: HTTP status server error (502 Upstream Down)",
            ),
            (
                FakeReply::ok(vec![b' '; MAX_INDEX_BYTES + 1]),
                "could not fetch the extract list: the list is unexpectedly large",
            ),
            (
                FakeReply::error(TransportErrorKind::Timeout),
                "could not fetch the extract list: FakeHttp: scripted Timeout",
            ),
            (
                FakeReply::ok("<html>"),
                "the extract list could not be read: expected value at line 1 column 1",
            ),
            (
                FakeReply::ok(r#"{"features":[]}"#),
                "the extract list could not be read: it lists no extracts",
            ),
        ];
        for (reply, want) in cases {
            let fake = FakeHttp::replying([reply]);
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::new(
                Arc::new(fake.clone()),
                Arc::new(crate::ports::SystemClock),
                URL.to_owned(),
                dir.path().join("catalog.json"),
            );
            let err = catalog.extracts().await.unwrap_err();
            assert_eq!(err.to_string(), want);
            let req = &fake.requests()[0];
            assert_eq!((&req.method, req.url.as_str()), (&http::Method::GET, URL));
            assert_eq!(req.timeout, Some(Duration::from_secs(30)));
            assert_eq!(req.max_bytes, 16 * 1024 * 1024);
            assert!(req.skip_error_bodies);
            fake.assert_drained();
        }
    }

    /// Sizes: a 5 s `HEAD` per extract whose error bodies are not read. The
    /// size is the `Content-Length` header of a 2xx or 3xx reply, and unknown
    /// after a 4xx or 5xx (whatever its header says) or a failed request.
    #[tokio::test]
    async fn sizes_are_read_from_head_replies() {
        use http::header::CONTENT_LENGTH;
        use http::{Method, StatusCode};
        use observation_upstream::TransportErrorKind;
        use observation_upstream::testing::{FakeHttp, FakeReply};
        const URL: &str = "https://download.geofabrik.de/index-v1-nogeom.json";
        const MONACO: &str = "https://download.geofabrik.de/europe/monaco-latest.osm.pbf";
        const CALIFORNIA: &str =
            "https://download.geofabrik.de/north-america/us/california-latest.osm.pbf";
        let ids = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let rounds = [
            (StatusCode::OK, Some(1234)),
            (StatusCode::FOUND, Some(1234)),
            (StatusCode::NOT_FOUND, None),
            (StatusCode::SERVICE_UNAVAILABLE, None),
        ];
        for (status, want) in rounds {
            let fake = FakeHttp::new();
            fake.on(Method::GET, URL, FakeReply::ok(INDEX));
            let reply = FakeReply::status(status).header(CONTENT_LENGTH, "1234");
            fake.on(Method::HEAD, MONACO, reply);
            let failed = FakeReply::error(TransportErrorKind::Connect);
            fake.on(Method::HEAD, CALIFORNIA, failed);
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::new(
                Arc::new(fake.clone()),
                Arc::new(crate::ports::SystemClock),
                URL.to_owned(),
                dir.path().join("catalog.json"),
            );
            let sizes = catalog
                .sizes(&ids(&["monaco", "us/california"]))
                .await
                .unwrap();
            assert_eq!(
                sizes,
                BTreeMap::from([
                    ("monaco".to_owned(), want),
                    ("us/california".to_owned(), None)
                ]),
                "{status}"
            );
            for req in fake.requests().iter().filter(|r| r.method == Method::HEAD) {
                assert_eq!(req.timeout, Some(Duration::from_secs(5)));
                assert!(req.skip_error_bodies);
            }
            fake.assert_drained();
        }
    }

    /// The list's age is read from the clock. It is kept for a day and
    /// fetched again after; a copy whose time lies in the future (the disk
    /// copy's real mtime, here) is not fresh. After a failed fetch the old
    /// list is used, and the next fetch is tried after `RETRY_AFTER`.
    #[tokio::test]
    async fn the_list_ages_on_the_clock() {
        use observation_upstream::testing::{FakeHttp, FakeReply};
        const URL: &str = "https://download.geofabrik.de/index-v1-nogeom.json";
        let fake = FakeHttp::replying([
            FakeReply::ok(INDEX),
            FakeReply::ok(INDEX),
            FakeReply::status(http::StatusCode::SERVICE_UNAVAILABLE),
        ]);
        let clock = Arc::new(crate::ports::ManualClock::at_unix(1_700_000_000));
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::new(
            Arc::new(fake.clone()),
            clock.clone(),
            URL.to_owned(),
            dir.path().join("catalog.json"),
        );
        let fetches = |n: usize| {
            let requests = fake.requests();
            assert_eq!(requests.len(), n, "{requests:?}");
        };
        let ids =
            |list: Arc<Vec<Extract>>| list.iter().map(|x| x.entry.id.clone()).collect::<Vec<_>>();

        assert_eq!(
            ids(catalog.extracts().await.unwrap()),
            ["monaco", "us/california"]
        );
        fetches(1);
        let req = &fake.requests()[0];
        assert_eq!((&req.method, req.url.as_str()), (&http::Method::GET, URL));
        assert_eq!(req.timeout, Some(Duration::from_secs(30)));
        assert_eq!(req.max_bytes, 16 * 1024 * 1024);
        assert!(req.skip_error_bodies);

        clock.advance(Duration::from_secs(24 * 3600 - 1));
        catalog.extracts().await.unwrap();
        fetches(1);
        clock.advance(Duration::from_secs(1));
        catalog.extracts().await.unwrap();
        fetches(2);

        // The fetch fails: the copy on disk is used, as if fetched long ago.
        clock.advance(Duration::from_secs(10 * 24 * 3600));
        assert_eq!(catalog.extracts().await.unwrap().len(), 2);
        fetches(3);
        clock.advance(Duration::from_secs(599));
        catalog.extracts().await.unwrap();
        fetches(3);
        fake.push(FakeReply::ok(INDEX));
        clock.advance(Duration::from_secs(1));
        catalog.extracts().await.unwrap();
        fetches(4);
        fake.assert_drained();
    }

    /// Known sizes are kept for a day and unknown ones for ten minutes, on
    /// Tokio's clock.
    #[tokio::test(start_paused = true)]
    async fn sizes_are_cached_on_tokio_time() {
        use http::header::CONTENT_LENGTH;
        use http::{Method, StatusCode};
        use observation_upstream::TransportErrorKind;
        use observation_upstream::testing::{FakeHttp, FakeReply};
        const URL: &str = "https://download.geofabrik.de/index-v1-nogeom.json";
        const MONACO: &str = "https://download.geofabrik.de/europe/monaco-latest.osm.pbf";
        const CALIFORNIA: &str =
            "https://download.geofabrik.de/north-america/us/california-latest.osm.pbf";
        let sized = |n: &str| FakeReply::status(StatusCode::OK).header(CONTENT_LENGTH, n);
        let fake = FakeHttp::new();
        fake.on(Method::GET, URL, FakeReply::ok(INDEX));
        fake.on(Method::HEAD, MONACO, sized("1234"));
        let failed = FakeReply::error(TransportErrorKind::Connect);
        fake.on(Method::HEAD, CALIFORNIA, failed);
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::new(
            Arc::new(fake.clone()),
            Arc::new(crate::ports::SystemClock),
            URL.to_owned(),
            dir.path().join("catalog.json"),
        );
        let ids = ["monaco".to_owned(), "us/california".to_owned()];
        let heads = || {
            fake.requests()
                .iter()
                .filter(|r| r.method == Method::HEAD)
                .count()
        };
        let sizes = |m: Option<u64>, c: Option<u64>| {
            BTreeMap::from([("monaco".to_owned(), m), ("us/california".to_owned(), c)])
        };

        assert_eq!(catalog.sizes(&ids).await.unwrap(), sizes(Some(1234), None));
        assert_eq!(heads(), 2);
        tokio::time::advance(Duration::from_secs(599)).await;
        assert_eq!(catalog.sizes(&ids).await.unwrap(), sizes(Some(1234), None));
        assert_eq!(heads(), 2, "an unknown size is kept for ten minutes");
        tokio::time::advance(Duration::from_secs(1)).await;
        fake.on(Method::HEAD, CALIFORNIA, sized("99"));
        assert_eq!(
            catalog.sizes(&ids).await.unwrap(),
            sizes(Some(1234), Some(99))
        );
        assert_eq!(heads(), 3);
        tokio::time::advance(Duration::from_secs(24 * 3600 - 600 - 1)).await;
        catalog.sizes(&ids).await.unwrap();
        assert_eq!(heads(), 3, "a known size is kept for a day");
        tokio::time::advance(Duration::from_secs(1)).await;
        fake.on(Method::HEAD, MONACO, sized("1235"));
        assert_eq!(
            catalog.sizes(&ids).await.unwrap(),
            sizes(Some(1235), Some(99))
        );
        assert_eq!(heads(), 4);
        fake.assert_drained();
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_index(b"<html>", "https://download.geofabrik.de/").is_err());
    }
}
