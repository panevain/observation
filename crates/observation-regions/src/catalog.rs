//! The list of importable extracts: Geofabrik's `index-v1-nogeom.json`,
//! fetched on demand and kept in memory and on disk for a day, and the
//! extracts' download sizes, asked for with `HEAD` and kept in memory.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use observation_core::regions::{CatalogEntry, is_valid_region_id};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;

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
/// Longest one [`Catalog::sizes`] call spends asking; sizes not asked for by
/// then are answered as unknown (and not cached, so they are asked next time).
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
    client: reqwest::Client,
    url: String,
    file: PathBuf,
    cached: Mutex<Option<(SystemTime, Arc<Vec<Extract>>)>>,
    /// Download sizes by id, with when they were asked for. Held across the
    /// `HEAD`s, so concurrent callers never ask about the same extract twice.
    sizes: Mutex<HashMap<String, (Instant, Option<u64>)>>,
}

impl Catalog {
    pub fn new(client: reqwest::Client, url: String, file: PathBuf) -> Self {
        Self {
            client,
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
        let deadline = Instant::now() + SIZES_BUDGET;
        for chunk in missing.chunks(SIZE_CONCURRENCY) {
            if Instant::now() + SIZE_TIMEOUT > deadline {
                break;
            }
            let mut set = tokio::task::JoinSet::new();
            for (id, url) in chunk.iter().cloned() {
                let client = self.client.clone();
                set.spawn(async move { (id, head_size(&client, &url).await) });
            }
            while let Some(done) = set.join_next().await {
                if let Ok((id, size)) = done {
                    cache.insert(id, (Instant::now(), size));
                }
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
        let mut cached = self.cached.lock().await;
        if let Some((at, list)) = cached.as_ref()
            && at.elapsed().is_ok_and(|age| age < MAX_AGE)
        {
            return Ok(Arc::clone(list));
        }
        let disk = tokio::fs::read(&self.file).await.ok();
        let disk_age = tokio::fs::metadata(&self.file)
            .await
            .and_then(|m| m.modified())
            .ok();
        if let (Some(bytes), Some(at)) = (&disk, disk_age)
            && at.elapsed().is_ok_and(|age| age < MAX_AGE)
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
                *cached = Some((SystemTime::now(), Arc::clone(&list)));
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
                        let at = SystemTime::now()
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
        let resp = self
            .client
            .get(&self.url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| CatalogError::Fetch(e.without_url().to_string()))?;
        let bytes = crate::read_capped(resp, MAX_INDEX_BYTES)
            .await
            .map_err(|e| CatalogError::Fetch(e.without_url().to_string()))?
            .ok_or_else(|| CatalogError::Fetch("the list is unexpectedly large".into()))?;
        let list = parse_index(&bytes, &self.url)?;
        if list.is_empty() {
            return Err(CatalogError::Parse("it lists no extracts".into()));
        }
        Ok((bytes.to_vec(), list))
    }
}

/// The `Content-Length` of a `HEAD` for `url`, following its redirects (to
/// the dated file). Read from the header: for a `HEAD`, reqwest's
/// `content_length()` is the length of the (empty) body.
async fn head_size(client: &reqwest::Client, url: &str) -> Option<u64> {
    let resp = client
        .head(url)
        .timeout(SIZE_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status);
    match resp {
        Ok(resp) => resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok(),
        Err(e) => {
            tracing::debug!(error = %e.without_url(), "extract size unknown");
            None
        }
    }
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
            reqwest::Client::new(),
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

    #[test]
    fn rejects_garbage() {
        assert!(parse_index(b"<html>", "https://download.geofabrik.de/").is_err());
    }
}
