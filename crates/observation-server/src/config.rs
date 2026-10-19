//! Runtime configuration, read from `OBS_*` environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// Default tile URL template (OpenStreetMap standard tiles).
pub const DEFAULT_TILE_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
/// Default Overpass interpreter endpoint.
pub const DEFAULT_OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";
/// Most Overpass endpoints accepted in `OBS_OVERPASS_URL`.
pub const MAX_OVERPASS_URLS: usize = 4;
/// Default attribution string shown by the client and embedded in bundles.
pub const DEFAULT_ATTRIBUTION: &str = "© OpenStreetMap contributors";
/// Default contact URL placed in the `User-Agent`.
pub const DEFAULT_CONTACT: &str = "https://github.com/panevain/observation";

/// An environment variable had an unusable value.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("OBS_BIND `{0}` is not a socket address like 0.0.0.0:8080")]
    Bind(String),
    #[error("OBS_BUNDLE_TTL_DAYS `{0}` is not a non-negative number")]
    Ttl(String),
    #[error("OBS_OVERPASS_URL lists more than {MAX_OVERPASS_URLS} endpoints")]
    OverpassUrls,
    #[error("OBS_OVERPASS_URL entry `{0}` is not an http(s) URL")]
    OverpassUrl(String),
}

/// Server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Listen address (`OBS_BIND`).
    pub bind: SocketAddr,
    /// Directory with the built client (`OBS_STATIC_DIR`).
    pub static_dir: PathBuf,
    /// Root of the on-disk bundle and tile cache (`OBS_CACHE_DIR`).
    pub cache_dir: PathBuf,
    /// Contact URL for the `User-Agent` (`OBS_CONTACT`).
    pub contact: String,
    /// Overpass interpreter URLs, tried in order (`OBS_OVERPASS_URL`, comma-separated).
    pub overpass_urls: Vec<String>,
    /// Upstream tile URL template (`OBS_TILE_URL`).
    pub tile_url: String,
    /// Attribution text (`OBS_TILE_ATTRIBUTION`).
    pub attribution: String,
    /// How long a cached bundle is considered fresh (`OBS_BUNDLE_TTL_DAYS`).
    pub bundle_ttl: Duration,
    /// Lower bound for tile freshness, required by the OSM tile usage policy.
    pub tile_min_ttl: Duration,
    /// Tiles are no longer written once the tile cache exceeds this size.
    pub tile_cache_cap_bytes: u64,
    /// Pause before retrying Overpass after a busy or failed attempt.
    pub overpass_retry_wait: Duration,
    /// Time allowed for one Overpass attempt (the query itself asks for 25 s).
    pub overpass_timeout: Duration,
    /// How long new Overpass fetches fail fast after we were rate limited.
    pub overpass_cooldown: Duration,
    /// Per-request timeout of the whole HTTP stack.
    pub request_timeout: Duration,
}

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Reads the configuration through `get`; empty values count as unset.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let or = |k: &str, d: &str| get(k).unwrap_or_else(|| d.to_owned());

        let bind_raw = or("OBS_BIND", "0.0.0.0:8080");
        let bind = bind_raw.parse().map_err(|_| ConfigError::Bind(bind_raw))?;
        let ttl_days = match get("OBS_BUNDLE_TTL_DAYS") {
            None => 7.0,
            Some(raw) => raw
                .parse::<f64>()
                .ok()
                .filter(|d| d.is_finite() && *d >= 0.0)
                .ok_or(ConfigError::Ttl(raw))?,
        };
        let mut overpass_urls: Vec<String> = or("OBS_OVERPASS_URL", DEFAULT_OVERPASS_URL)
            .split(',')
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_owned)
            .collect();
        if overpass_urls.len() > MAX_OVERPASS_URLS {
            return Err(ConfigError::OverpassUrls);
        }
        if overpass_urls.is_empty() {
            overpass_urls.push(DEFAULT_OVERPASS_URL.to_owned());
        }
        if let Some(bad) = overpass_urls.iter().find(|u| {
            !reqwest::Url::parse(u)
                .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
        }) {
            return Err(ConfigError::OverpassUrl(bad.clone()));
        }
        Ok(Self {
            bind,
            static_dir: or("OBS_STATIC_DIR", "dist").into(),
            cache_dir: or("OBS_CACHE_DIR", "cache").into(),
            contact: or("OBS_CONTACT", DEFAULT_CONTACT),
            overpass_urls,
            tile_url: or("OBS_TILE_URL", DEFAULT_TILE_URL),
            attribution: or("OBS_TILE_ATTRIBUTION", DEFAULT_ATTRIBUTION),
            bundle_ttl: Duration::from_secs_f64(ttl_days * 86_400.0),
            ..Self::defaults()
        })
    }

    /// Fixed (non-environment) settings shared by production and tests.
    fn defaults() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            static_dir: "dist".into(),
            cache_dir: "cache".into(),
            contact: DEFAULT_CONTACT.into(),
            overpass_urls: vec![DEFAULT_OVERPASS_URL.into()],
            tile_url: DEFAULT_TILE_URL.into(),
            attribution: DEFAULT_ATTRIBUTION.into(),
            bundle_ttl: Duration::from_secs(7 * 86_400),
            tile_min_ttl: Duration::from_secs(7 * 86_400),
            tile_cache_cap_bytes: 512 * 1024 * 1024,
            overpass_retry_wait: Duration::from_secs(2),
            overpass_timeout: Duration::from_secs(35),
            overpass_cooldown: Duration::from_secs(30),
            // Two Overpass attempts and the pause between them.
            request_timeout: Duration::from_secs(80),
        }
    }

    /// Configuration for tests: loopback upstreams, no minimum tile TTL and a
    /// short retry pause.
    pub fn for_tests(
        cache_dir: impl Into<PathBuf>,
        static_dir: impl Into<PathBuf>,
        overpass_url: impl Into<String>,
        tile_url: impl Into<String>,
    ) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            static_dir: static_dir.into(),
            overpass_urls: vec![overpass_url.into()],
            tile_url: tile_url.into(),
            tile_min_ttl: Duration::ZERO,
            overpass_retry_wait: Duration::from_millis(50),
            overpass_timeout: Duration::from_secs(5),
            overpass_cooldown: Duration::from_secs(30),
            request_timeout: Duration::from_secs(20),
            ..Self::defaults()
        }
    }

    /// `User-Agent` sent to every upstream, e.g. `observation/0.1.0 (+https://…)`.
    pub fn user_agent(&self) -> String {
        format!(
            "observation/{} (+{})",
            env!("CARGO_PKG_VERSION"),
            self.contact
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn defaults_and_overrides() {
        let c = Config::from_lookup(lookup(&[])).unwrap();
        assert_eq!(c.bind.port(), 8080);
        assert_eq!(c.static_dir, PathBuf::from("dist"));
        assert_eq!(c.bundle_ttl, Duration::from_secs(7 * 86_400));
        assert!(c.user_agent().starts_with("observation/"));

        let c = Config::from_lookup(lookup(&[
            ("OBS_BIND", "127.0.0.1:9"),
            ("OBS_BUNDLE_TTL_DAYS", "1"),
            ("OBS_CACHE_DIR", " "),
        ]))
        .unwrap();
        assert_eq!(c.bind.port(), 9);
        assert_eq!(c.bundle_ttl, Duration::from_secs(86_400));
        assert_eq!(c.cache_dir, PathBuf::from("cache"));
        assert_eq!(c.overpass_urls, vec![DEFAULT_OVERPASS_URL.to_owned()]);

        let c = Config::from_lookup(lookup(&[(
            "OBS_OVERPASS_URL",
            "https://a.example/api/interpreter, ,https://b.example/api/interpreter",
        )]))
        .unwrap();
        assert_eq!(
            c.overpass_urls,
            vec![
                "https://a.example/api/interpreter".to_owned(),
                "https://b.example/api/interpreter".to_owned()
            ]
        );
    }

    #[test]
    fn rejects_bad_values() {
        assert!(matches!(
            Config::from_lookup(lookup(&[("OBS_BIND", "nope")])),
            Err(ConfigError::Bind(_))
        ));
        assert!(matches!(
            Config::from_lookup(lookup(&[("OBS_BUNDLE_TTL_DAYS", "-1")])),
            Err(ConfigError::Ttl(_))
        ));
        assert!(matches!(
            Config::from_lookup(lookup(&[("OBS_OVERPASS_URL", "a,b,c,d,e")])),
            Err(ConfigError::OverpassUrls)
        ));
        // A missing scheme is caught at startup, not as a failed request later.
        assert!(matches!(
            Config::from_lookup(lookup(&[(
                "OBS_OVERPASS_URL",
                "overpass.example/api/interpreter,https://overpass-api.de/api/interpreter"
            )])),
            Err(ConfigError::OverpassUrl(u)) if u == "overpass.example/api/interpreter"
        ));
    }
}
