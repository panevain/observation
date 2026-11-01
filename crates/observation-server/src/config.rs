//! Runtime configuration, read from `OBS_*` environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use observation_core::tiles::TileId;
use observation_upstream::{UrlProblem, check_url, redact};
use thiserror::Error;

use crate::ratelimit::{Cidr, Rate};

/// Default tile URL template (OpenStreetMap standard tiles).
pub const DEFAULT_TILE_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
/// Default elevation tile URL template (Terrarium tiles from the AWS Open Data
/// "Terrain Tiles" set).
pub const DEFAULT_DEM_URL: &str =
    "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png";
/// Default credit for the elevation data, embedded in bundles. The client
/// links it to the sources' full credits.
pub const DEFAULT_DEM_ATTRIBUTION: &str = observation_core::terrain::TERRAIN_TILES_ATTRIBUTION;
/// `OBS_DEM_URL` value that turns elevation off (flat ground).
pub const DEM_OFF: &str = "off";
/// Default Overpass interpreter endpoint.
pub const DEFAULT_OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";
/// Most Overpass endpoints accepted in `OBS_OVERPASS_URL`.
pub const MAX_OVERPASS_URLS: usize = 4;
/// Default bundle cache cap (`OBS_BUNDLE_CACHE_MB`), in MiB.
pub const DEFAULT_BUNDLE_CACHE_MB: u64 = 1024;
/// Largest `OBS_BUNDLE_CACHE_MB` accepted (1 PiB), so the byte count cannot overflow.
pub const MAX_BUNDLE_CACHE_MB: u64 = 1 << 30;
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
    #[error(
        "OBS_BUNDLE_CACHE_MB `{0}` is not a whole number of MiB from 1 to {MAX_BUNDLE_CACHE_MB}"
    )]
    BundleCacheMb(String),
    #[error("OBS_OVERPASS_URL lists more than {MAX_OVERPASS_URLS} endpoints")]
    OverpassUrls,
    #[error("OBS_OVERPASS_URL entry `{0}` is not an http(s) URL")]
    OverpassUrl(String),
    #[error("OBS_TILE_URL `{0}` is not an http(s) URL")]
    TileUrl(String),
    #[error(
        "OBS_TILE_URL `{0}` uses {{x}}, {{y}} or {{z}} in the host name; only {{s}} may appear there"
    )]
    TileUrlHost(String),
    #[error("OBS_DEM_URL `{0}` is not an http(s) URL or `off`")]
    DemUrl(String),
    #[error(
        "OBS_DEM_URL `{0}` uses {{x}}, {{y}} or {{z}} in the host name; only {{s}} may appear there"
    )]
    DemUrlHost(String),
    #[error(
        "{var} `{url}` points at a loopback, private or link-local address; \
         set OBS_ALLOW_PRIVATE_UPSTREAMS=1 if that is intended"
    )]
    PrivateUpstream { var: &'static str, url: String },
    #[error("OBS_ALLOW_PRIVATE_UPSTREAMS `{0}` is not 1, true, yes, on, 0, false, no or off")]
    AllowPrivate(String),
    #[error("OBS_METRICS_BIND `{0}` is not a socket address like 127.0.0.1:9090")]
    MetricsBind(String),
    #[error("OBS_METRICS_BIND `{0}` must differ from OBS_BIND")]
    MetricsBindSame(String),
    #[error("OBS_LOG_FORMAT `{0}` is not `text` or `json`")]
    LogFormat(String),
    #[error("OBS_REGIONS_URL `{0}` is not an http(s) URL")]
    RegionsUrl(String),
    #[error("OBS_TRUSTED_PROXIES entry `{0}` is not an IP address or a block like 10.0.0.0/8")]
    TrustedProxy(String),
    #[error("{var} `{value}` is not a whole number from {min} to {MAX_RATE}")]
    Rate {
        var: &'static str,
        value: String,
        min: u32,
    },
}

/// Largest per-minute rate or burst accepted for the per-client limits.
pub const MAX_RATE: u32 = 1_000_000;
/// Default `/api/area` limit: 10 boxes at once, then one every 6 s.
pub const DEFAULT_AREA_RATE: Rate = Rate {
    per_minute: 10,
    burst: 10,
};
/// Default `/api/tiles` limit: a screenful of tiles at once (the client keeps
/// up to 400), then 10 a second.
pub const DEFAULT_TILE_RATE: Rate = Rate {
    per_minute: 600,
    burst: 400,
};

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
    /// Upstream Terrarium elevation tile URL template (`OBS_DEM_URL`); `None`
    /// (`off`) builds every area on flat ground.
    pub dem_url: Option<String>,
    /// Credit for the elevation data (`OBS_DEM_ATTRIBUTION`).
    pub dem_attribution: String,
    /// Allow upstreams on loopback, private or link-local addresses
    /// (`OBS_ALLOW_PRIVATE_UPSTREAMS`).
    pub allow_private_upstreams: bool,
    /// Attribution text (`OBS_TILE_ATTRIBUTION`).
    pub attribution: String,
    /// How long a cached bundle is considered fresh (`OBS_BUNDLE_TTL_DAYS`).
    pub bundle_ttl: Duration,
    /// Bundle cache size cap (`OBS_BUNDLE_CACHE_MB`); the oldest bundles are
    /// evicted to stay below it.
    pub bundle_cache_cap_bytes: u64,
    /// Lower bound for tile freshness, required by the OSM tile usage policy.
    pub tile_min_ttl: Duration,
    /// Tile cache size cap; the least recently used tiles are evicted to stay below it.
    pub tile_cache_cap_bytes: u64,
    /// Lower bound for elevation tile freshness: the data rarely changes.
    pub dem_min_ttl: Duration,
    /// Elevation tile cache size cap, like `tile_cache_cap_bytes`.
    pub dem_cache_cap_bytes: u64,
    /// Pause before retrying Overpass after a busy or failed attempt.
    pub overpass_retry_wait: Duration,
    /// Time allowed for one Overpass attempt (the query itself asks for 25 s).
    pub overpass_timeout: Duration,
    /// How long new Overpass fetches fail fast after we were rate limited.
    pub overpass_cooldown: Duration,
    /// Per-request timeout of the whole HTTP stack.
    pub request_timeout: Duration,
    /// Where to serve Prometheus metrics (`OBS_METRICS_BIND`); `None` disables them.
    pub metrics_bind: Option<SocketAddr>,
    /// Log output format (`OBS_LOG_FORMAT`).
    pub log_format: LogFormat,
    /// Base URL of the region agent (`OBS_REGIONS_URL`); `None` disables local regions.
    pub regions_url: Option<String>,
    /// Bearer token for the region agent (`OBS_REGIONS_TOKEN`).
    pub regions_token: Option<String>,
    /// Token that allows importing and deleting regions from the UI
    /// (`OBS_ADMIN_TOKEN`); `None` makes regions read-only.
    pub admin_token: Option<String>,
    /// Reverse proxies whose `X-Forwarded-For` names the client (`OBS_TRUSTED_PROXIES`).
    pub trusted_proxies: Vec<Cidr>,
    /// Per-client limit on bundles that are not cached yet
    /// (`OBS_AREA_RATE_PER_MIN`, `OBS_AREA_BURST`).
    pub area_rate: Rate,
    /// Per-client limit on tiles that go upstream
    /// (`OBS_TILE_RATE_PER_MIN`, `OBS_TILE_BURST`).
    pub tile_rate: Rate,
}

/// Whether two listen addresses would collide: same port and the same IP, or either
/// one a wildcard (`0.0.0.0` / `::`).
fn overlaps(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() == b.port() && (a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified())
}

/// Checks one configured upstream URL, naming `var` and the redacted URL on failure.
fn check_upstream(
    var: &'static str,
    url: &str,
    allow_private: bool,
    malformed: impl FnOnce() -> ConfigError,
) -> Result<(), ConfigError> {
    check_url(url, allow_private).map_err(|p| match p {
        UrlProblem::Malformed => malformed(),
        UrlProblem::Private => ConfigError::PrivateUpstream {
            var,
            url: redact(url),
        },
    })
}

/// Checks a `{z}/{x}/{y}` tile URL template (`var`): an http(s) URL that is
/// not private unless allowed, and whose host only `{s}` may vary.
fn check_template(
    var: &'static str,
    template: &str,
    allow_private: bool,
    malformed: fn(String) -> ConfigError,
    host: impl FnOnce() -> ConfigError,
) -> Result<(), ConfigError> {
    // Only `{s}` (always `a`) may vary the host; `{x}` and friends could steer
    // the request to another address at run time. Parse the way the HTTP client
    // will, with a marker for the placeholders, rather than splitting the text.
    const MARK: &str = "placeholdermark";
    let marked = template
        .replace("{s}", "a")
        .replace("{x}", MARK)
        .replace("{y}", MARK)
        .replace("{z}", MARK);
    let in_host = match reqwest::Url::parse(&marked) {
        Ok(u) => u.host_str().is_some_and(|h| h.contains(MARK)),
        // E.g. `{x}.0.0.1`: a numeric last label makes the host an invalid IPv4
        // address once the marker is in. Fall back to the text before the path.
        Err(_) => {
            let rest = template.split_once(':').map_or("", |(_, r)| r);
            let authority = rest
                .trim_start_matches(['/', '\\'])
                .split(['/', '\\', '?', '#'])
                .next()
                .unwrap_or("");
            authority.replace("{s}", "a").contains('{')
        }
    };
    if in_host {
        return Err(host());
    }
    let probe = TileId::new(0, 0, 0).expect("tile 0/0/0").url(template);
    check_upstream(var, &probe, allow_private, || malformed(redact(template)))
}

/// How tracing events are written (to stdout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable lines.
    Text,
    /// One JSON object per event, for log pipelines.
    Json,
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
        let bind: SocketAddr = bind_raw.parse().map_err(|_| ConfigError::Bind(bind_raw))?;
        let ttl_days = match get("OBS_BUNDLE_TTL_DAYS") {
            None => 7.0,
            Some(raw) => raw
                .parse::<f64>()
                .ok()
                .filter(|d| d.is_finite() && *d >= 0.0)
                .ok_or(ConfigError::Ttl(raw))?,
        };
        let bundle_cache_mb = match get("OBS_BUNDLE_CACHE_MB") {
            None => DEFAULT_BUNDLE_CACHE_MB,
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .filter(|mb| (1..=MAX_BUNDLE_CACHE_MB).contains(mb))
                .ok_or(ConfigError::BundleCacheMb(raw))?,
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
        let allow_private_upstreams = match get("OBS_ALLOW_PRIVATE_UPSTREAMS") {
            None => false,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => return Err(ConfigError::AllowPrivate(raw)),
            },
        };
        let tile_url = or("OBS_TILE_URL", DEFAULT_TILE_URL);
        let dem_url = match get("OBS_DEM_URL") {
            Some(u) if u.eq_ignore_ascii_case(DEM_OFF) => None,
            other => Some(other.unwrap_or_else(|| DEFAULT_DEM_URL.to_owned())),
        };
        let metrics_bind = get("OBS_METRICS_BIND")
            .map(|raw| raw.parse().map_err(|_| ConfigError::MetricsBind(raw)))
            .transpose()?;
        if metrics_bind.is_some_and(|m| overlaps(m, bind)) {
            return Err(ConfigError::MetricsBindSame(bind.to_string()));
        }
        let log_format = match get("OBS_LOG_FORMAT").as_deref() {
            None | Some("text") => LogFormat::Text,
            Some("json") => LogFormat::Json,
            Some(other) => return Err(ConfigError::LogFormat(other.to_owned())),
        };
        let regions_url = get("OBS_REGIONS_URL")
            .map(|u| {
                let ok = reqwest::Url::parse(&u)
                    .is_ok_and(|p| matches!(p.scheme(), "http" | "https") && p.host().is_some());
                if ok {
                    Ok(u.trim_end_matches('/').to_owned())
                } else {
                    Err(ConfigError::RegionsUrl(u))
                }
            })
            .transpose()?;
        let trusted_proxies = get("OBS_TRUSTED_PROXIES")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(|c| Cidr::parse(c).ok_or_else(|| ConfigError::TrustedProxy(c.to_owned())))
            .collect::<Result<_, _>>()?;
        let number = |var: &'static str, default: u32, min: u32| match get(var) {
            None => Ok(default),
            Some(value) => value
                .parse::<u32>()
                .ok()
                .filter(|n| (min..=MAX_RATE).contains(n))
                .ok_or(ConfigError::Rate { var, value, min }),
        };
        // A rate of 0 turns that limit off; otherwise a burst below 1 would refuse everything.
        let area_rate = Rate {
            per_minute: number("OBS_AREA_RATE_PER_MIN", DEFAULT_AREA_RATE.per_minute, 0)?,
            burst: number("OBS_AREA_BURST", DEFAULT_AREA_RATE.burst, 1)?,
        };
        let tile_rate = Rate {
            per_minute: number("OBS_TILE_RATE_PER_MIN", DEFAULT_TILE_RATE.per_minute, 0)?,
            burst: number("OBS_TILE_BURST", DEFAULT_TILE_RATE.burst, 1)?,
        };
        let config = Self {
            trusted_proxies,
            area_rate,
            tile_rate,
            bind,
            metrics_bind,
            regions_url,
            regions_token: get("OBS_REGIONS_TOKEN"),
            admin_token: get("OBS_ADMIN_TOKEN"),
            log_format,
            static_dir: or("OBS_STATIC_DIR", "dist").into(),
            cache_dir: or("OBS_CACHE_DIR", "cache").into(),
            contact: or("OBS_CONTACT", DEFAULT_CONTACT),
            overpass_urls,
            tile_url,
            dem_url,
            dem_attribution: or("OBS_DEM_ATTRIBUTION", DEFAULT_DEM_ATTRIBUTION),
            allow_private_upstreams,
            attribution: or("OBS_TILE_ATTRIBUTION", DEFAULT_ATTRIBUTION),
            bundle_ttl: Duration::from_secs_f64(ttl_days * 86_400.0),
            bundle_cache_cap_bytes: bundle_cache_mb * 1024 * 1024,
            ..Self::defaults()
        };
        config.validate()?;
        Ok(config)
    }

    /// Checks the upstream URLs and the per-client limits. Run by
    /// [`Config::from_lookup`] and again by `AppState::new`, so a hand-built
    /// config cannot skip it. Hosts that are names rather than literal addresses
    /// are checked when connecting instead (see `observation_upstream::FilteringResolver`).
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (var, value, min) in [
            ("OBS_AREA_RATE_PER_MIN", self.area_rate.per_minute, 0),
            ("OBS_AREA_BURST", self.area_rate.burst, 1),
            ("OBS_TILE_RATE_PER_MIN", self.tile_rate.per_minute, 0),
            ("OBS_TILE_BURST", self.tile_rate.burst, 1),
        ] {
            if !(min..=MAX_RATE).contains(&value) {
                return Err(ConfigError::Rate {
                    var,
                    value: value.to_string(),
                    min,
                });
            }
        }
        let allow = self.allow_private_upstreams;
        for u in &self.overpass_urls {
            check_upstream("OBS_OVERPASS_URL", u, allow, || {
                ConfigError::OverpassUrl(redact(u))
            })?;
        }
        check_template(
            "OBS_TILE_URL",
            &self.tile_url,
            allow,
            ConfigError::TileUrl,
            || ConfigError::TileUrlHost(redact(&self.tile_url)),
        )?;
        match &self.dem_url {
            Some(dem) => check_template("OBS_DEM_URL", dem, allow, ConfigError::DemUrl, || {
                ConfigError::DemUrlHost(redact(dem))
            }),
            None => Ok(()),
        }
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
            dem_url: Some(DEFAULT_DEM_URL.into()),
            dem_attribution: DEFAULT_DEM_ATTRIBUTION.into(),
            allow_private_upstreams: false,
            attribution: DEFAULT_ATTRIBUTION.into(),
            bundle_ttl: Duration::from_secs(7 * 86_400),
            bundle_cache_cap_bytes: DEFAULT_BUNDLE_CACHE_MB * 1024 * 1024,
            tile_min_ttl: Duration::from_secs(7 * 86_400),
            tile_cache_cap_bytes: 512 * 1024 * 1024,
            dem_min_ttl: Duration::from_secs(30 * 86_400),
            dem_cache_cap_bytes: 256 * 1024 * 1024,
            overpass_retry_wait: Duration::from_secs(2),
            overpass_timeout: Duration::from_secs(35),
            overpass_cooldown: Duration::from_secs(30),
            // Two Overpass attempts and the pause between them.
            request_timeout: Duration::from_secs(80),
            metrics_bind: None,
            log_format: LogFormat::Text,
            regions_url: None,
            regions_token: None,
            admin_token: None,
            trusted_proxies: Vec::new(),
            area_rate: DEFAULT_AREA_RATE,
            tile_rate: DEFAULT_TILE_RATE,
        }
    }

    /// Configuration for tests: loopback upstreams, no elevation data (set
    /// `dem_url` to test it), no minimum tile TTL, a short retry pause and no
    /// per-client limits.
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
            dem_url: None,
            // The mock upstreams listen on loopback.
            allow_private_upstreams: true,
            tile_min_ttl: Duration::ZERO,
            dem_min_ttl: Duration::ZERO,
            overpass_retry_wait: Duration::from_millis(50),
            overpass_timeout: Duration::from_secs(5),
            overpass_cooldown: Duration::from_secs(30),
            request_timeout: Duration::from_secs(20),
            area_rate: Rate {
                per_minute: 0,
                burst: 1,
            },
            tile_rate: Rate {
                per_minute: 0,
                burst: 1,
            },
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
    fn observability_settings() {
        let c = Config::from_lookup(lookup(&[])).unwrap();
        assert_eq!(c.metrics_bind, None);
        assert_eq!(c.log_format, LogFormat::Text);

        let c = Config::from_lookup(lookup(&[
            ("OBS_METRICS_BIND", "127.0.0.1:9090"),
            ("OBS_LOG_FORMAT", "json"),
        ]))
        .unwrap();
        assert_eq!(c.metrics_bind.map(|a| a.port()), Some(9090));
        assert_eq!(c.log_format, LogFormat::Json);

        assert_eq!(
            Config::from_lookup(lookup(&[("OBS_METRICS_BIND", "nope")])).unwrap_err(),
            ConfigError::MetricsBind("nope".into())
        );
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("OBS_BIND", "127.0.0.1:9090"),
                ("OBS_METRICS_BIND", "127.0.0.1:9090"),
            ]))
            .unwrap_err(),
            ConfigError::MetricsBindSame("127.0.0.1:9090".into())
        );
        // A wildcard on the same port collides with a specific address.
        assert!(
            Config::from_lookup(lookup(&[
                ("OBS_BIND", "127.0.0.1:9090"),
                ("OBS_METRICS_BIND", "0.0.0.0:9090"),
            ]))
            .is_err()
        );
        assert!(
            Config::from_lookup(lookup(&[
                ("OBS_BIND", "127.0.0.1:9090"),
                ("OBS_METRICS_BIND", "127.0.0.1:9091"),
            ]))
            .is_ok()
        );
        assert_eq!(
            Config::from_lookup(lookup(&[("OBS_LOG_FORMAT", "xml")])).unwrap_err(),
            ConfigError::LogFormat("xml".into())
        );
    }

    #[test]
    fn defaults_and_overrides() {
        let c = Config::from_lookup(lookup(&[])).unwrap();
        assert_eq!(c.bind.port(), 8080);
        assert_eq!(c.static_dir, PathBuf::from("dist"));
        assert_eq!(c.bundle_ttl, Duration::from_secs(7 * 86_400));
        assert_eq!(c.bundle_cache_cap_bytes, 1024 * 1024 * 1024);
        assert!(c.user_agent().starts_with("observation/"));

        let c = Config::from_lookup(lookup(&[
            ("OBS_BIND", "127.0.0.1:9"),
            ("OBS_BUNDLE_TTL_DAYS", "1"),
            ("OBS_BUNDLE_CACHE_MB", " 64 "),
            ("OBS_CACHE_DIR", " "),
        ]))
        .unwrap();
        assert_eq!(c.bind.port(), 9);
        assert_eq!(c.bundle_ttl, Duration::from_secs(86_400));
        assert_eq!(c.bundle_cache_cap_bytes, 64 * 1024 * 1024);
        assert_eq!(c.cache_dir, PathBuf::from("cache"));
        assert_eq!(c.overpass_urls, vec![DEFAULT_OVERPASS_URL.to_owned()]);
        assert_eq!(c.regions_url, None);
        assert_eq!(c.admin_token, None);

        let c = Config::from_lookup(lookup(&[
            ("OBS_REGIONS_URL", "http://regions:8090/"),
            ("OBS_REGIONS_TOKEN", "agent"),
            ("OBS_ADMIN_TOKEN", "admin"),
        ]))
        .unwrap();
        assert_eq!(c.regions_url.as_deref(), Some("http://regions:8090"));
        assert_eq!(c.regions_token.as_deref(), Some("agent"));
        assert_eq!(c.admin_token.as_deref(), Some("admin"));

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
    fn client_limits_and_trusted_proxies() {
        let c = Config::from_lookup(lookup(&[])).unwrap();
        assert_eq!(c.area_rate, DEFAULT_AREA_RATE);
        assert_eq!(c.tile_rate, DEFAULT_TILE_RATE);
        assert!(c.trusted_proxies.is_empty());

        let c = Config::from_lookup(lookup(&[
            ("OBS_AREA_RATE_PER_MIN", "0"),
            ("OBS_TILE_RATE_PER_MIN", "120"),
            ("OBS_TILE_BURST", "50"),
            ("OBS_TRUSTED_PROXIES", "10.0.0.0/8, ,::1"),
        ]))
        .unwrap();
        assert_eq!(c.area_rate.per_minute, 0);
        assert_eq!(
            c.tile_rate,
            Rate {
                per_minute: 120,
                burst: 50
            }
        );
        assert_eq!(c.trusted_proxies.len(), 2);
        assert!(c.trusted_proxies[0].contains("10.9.8.7".parse().unwrap()));

        for (var, bad) in [
            ("OBS_AREA_RATE_PER_MIN", "-1"),
            ("OBS_AREA_RATE_PER_MIN", "1.5"),
            ("OBS_TILE_RATE_PER_MIN", "1000001"),
            ("OBS_AREA_BURST", "0"),
            ("OBS_TILE_BURST", "lots"),
        ] {
            assert!(
                matches!(
                    Config::from_lookup(lookup(&[(var, bad)])),
                    Err(ConfigError::Rate { var: v, value, .. }) if v == var && value == bad
                ),
                "{var}={bad}"
            );
        }
        assert_eq!(
            Config::from_lookup(lookup(&[("OBS_TRUSTED_PROXIES", "10.0.0.0/8,proxy")]))
                .unwrap_err(),
            ConfigError::TrustedProxy("proxy".into())
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
        for bad in ["0", "-1", "1.5", "lots", "1073741825"] {
            assert_eq!(
                Config::from_lookup(lookup(&[("OBS_BUNDLE_CACHE_MB", bad)])).unwrap_err(),
                ConfigError::BundleCacheMb(bad.into()),
                "{bad}"
            );
        }
        let c = Config::from_lookup(lookup(&[("OBS_BUNDLE_CACHE_MB", "1073741824")])).unwrap();
        assert_eq!(c.bundle_cache_cap_bytes, 1 << 50);
        assert!(matches!(
            Config::from_lookup(lookup(&[("OBS_REGIONS_URL", "regions:8090")])),
            Err(ConfigError::RegionsUrl(_))
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

    #[test]
    fn upstream_urls_are_screened() {
        let bad = |k: &str, v: &str| Config::from_lookup(lookup(&[(k, v)])).unwrap_err();
        for u in [
            "http://169.254.169.254/latest",
            "http://127.0.0.1:9/o",
            "http://10.0.0.5/o",
            "http://localhost/o",
            "http://[::1]/o",
            "http://[fd00::1]/o",
            "http://[::ffff:169.254.169.254]/o",
            "http://[64:ff9b::a9fe:a9fe]/o",
            "http://192.0.0.192/o",
        ] {
            assert!(
                matches!(
                    bad("OBS_OVERPASS_URL", u),
                    ConfigError::PrivateUpstream { .. }
                ),
                "{u}"
            );
            assert!(
                matches!(
                    bad("OBS_TILE_URL", &format!("{u}/{{z}}/{{x}}/{{y}}.png")),
                    ConfigError::PrivateUpstream { .. }
                ),
                "{u}"
            );
        }
        assert!(matches!(
            bad("OBS_TILE_URL", "ftp://tiles.example/{z}/{x}/{y}.png"),
            ConfigError::TileUrl(_)
        ));
    }

    #[test]
    fn tile_templates_cannot_vary_the_host() {
        let bad = |v: &str| Config::from_lookup(lookup(&[("OBS_TILE_URL", v)])).unwrap_err();
        assert!(matches!(
            bad("http://{x}.0.0.1/t.png"),
            ConfigError::TileUrlHost(_)
        ));
        // Forms without `//` still have a host to the URL parser.
        for u in [
            "http:12{z}.0.0.1/{z}/{x}/{y}.png",
            "http:\\\\16{x}.254.169.254/{z}.png",
            "https://{z}.tiles.example/{x}/{y}.png",
        ] {
            assert!(matches!(bad(u), ConfigError::TileUrlHost(_)), "{u}");
        }
        // `{s}` is always `a`, and placeholders in the path are fine.
        assert!(
            Config::from_lookup(lookup(&[(
                "OBS_TILE_URL",
                "https://{s}.tiles.example/{z}/{x}/{y}.png?k={z}"
            )]))
            .is_ok()
        );
    }

    #[test]
    fn elevation_source_settings() {
        let c = Config::from_lookup(lookup(&[])).unwrap();
        assert_eq!(c.dem_url.as_deref(), Some(DEFAULT_DEM_URL));
        assert_eq!(c.dem_attribution, DEFAULT_DEM_ATTRIBUTION);
        let c = Config::from_lookup(lookup(&[("OBS_DEM_URL", " Off ")])).unwrap();
        assert_eq!(c.dem_url, None);
        let c = Config::from_lookup(lookup(&[
            ("OBS_DEM_URL", "https://dem.example/{z}/{x}/{y}.png"),
            ("OBS_DEM_ATTRIBUTION", "Elevation: somebody"),
        ]))
        .unwrap();
        assert_eq!(
            c.dem_url.as_deref(),
            Some("https://dem.example/{z}/{x}/{y}.png")
        );
        assert_eq!(c.dem_attribution, "Elevation: somebody");
        let bad = |v: &str| Config::from_lookup(lookup(&[("OBS_DEM_URL", v)])).unwrap_err();
        assert!(matches!(bad("dem.example/{z}"), ConfigError::DemUrl(_)));
        assert!(matches!(
            bad("https://{x}.dem.example/{z}.png"),
            ConfigError::DemUrlHost(_)
        ));
        assert!(matches!(
            bad("http://10.0.0.5/{z}/{x}/{y}.png"),
            ConfigError::PrivateUpstream {
                var: "OBS_DEM_URL",
                ..
            }
        ));
    }

    #[test]
    fn credentials_are_allowed_but_never_echoed() {
        let ok = Config::from_lookup(lookup(&[(
            "OBS_OVERPASS_URL",
            "https://user:secret@overpass.example/api",
        )]));
        assert!(ok.is_ok());
        let msg = Config::from_lookup(lookup(&[(
            "OBS_OVERPASS_URL",
            "https://user:secret@127.0.0.1/api",
        )]))
        .unwrap_err()
        .to_string();
        assert!(!msg.contains("secret"), "{msg}");
        let msg = Config::from_lookup(lookup(&[(
            "OBS_TILE_URL",
            "ftp://user:secret@tiles.example/{z}/{x}/{y}.png",
        )]))
        .unwrap_err()
        .to_string();
        assert!(!msg.contains("secret"), "{msg}");
        // Without a scheme the value is not a URL, but could still hold a secret.
        let msg = Config::from_lookup(lookup(&[(
            "OBS_OVERPASS_URL",
            "user:secret@overpass.example",
        )]))
        .unwrap_err()
        .to_string();
        assert!(!msg.contains("secret"), "{msg}");
    }

    #[test]
    fn the_private_upstream_flag_is_parsed_strictly() {
        let flag = |v: &str| {
            Config::from_lookup(lookup(&[("OBS_ALLOW_PRIVATE_UPSTREAMS", v)]))
                .map(|c| c.allow_private_upstreams)
        };
        for on in ["1", "true", "TRUE", "Yes", "on"] {
            assert_eq!(flag(on), Ok(true), "{on}");
        }
        for off in ["0", "false", "No", "OFF"] {
            assert_eq!(flag(off), Ok(false), "{off}");
        }
        assert_eq!(
            flag("maybe"),
            Err(ConfigError::AllowPrivate("maybe".into()))
        );
    }

    #[test]
    fn hand_built_configs_are_validated_too() {
        let mut c = Config::for_tests("c", "s", "http://tiles/o", "http://tiles/{z}");
        c.tile_rate = Rate {
            per_minute: 10,
            burst: 0,
        };
        assert!(matches!(
            c.validate(),
            Err(ConfigError::Rate {
                var: "OBS_TILE_BURST",
                ..
            })
        ));

        let mut c = Config::for_tests("c", "s", "http://169.254.169.254/o", "http://tiles/{z}");
        c.allow_private_upstreams = false;
        assert!(matches!(
            c.validate(),
            Err(ConfigError::PrivateUpstream { .. })
        ));
        assert!(matches!(
            crate::AppState::new(c),
            Err(crate::StateError::Config(
                ConfigError::PrivateUpstream { .. }
            ))
        ));
    }

    #[test]
    fn private_upstreams_can_be_allowed() {
        let c = Config::from_lookup(lookup(&[
            ("OBS_ALLOW_PRIVATE_UPSTREAMS", "1"),
            ("OBS_OVERPASS_URL", "http://10.0.0.5/api/interpreter"),
            ("OBS_TILE_URL", "http://localhost:8081/{z}/{x}/{y}.png"),
        ]))
        .unwrap();
        assert!(c.allow_private_upstreams);
    }
}
