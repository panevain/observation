//! Agent configuration, read from `OBS_REGIONS_*` environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use observation_upstream::{UrlProblem, redact};
use thiserror::Error;

/// Geofabrik's extract list without geometry.
pub const DEFAULT_CATALOG_URL: &str = "https://download.geofabrik.de/index-v1-nogeom.json";
/// Contact URL placed in the `User-Agent`.
pub const DEFAULT_CONTACT: &str = "https://github.com/panevain/observation";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("OBS_REGIONS_BIND `{0}` is not a socket address like 0.0.0.0:8090")]
    Bind(String),
    #[error("{0} `{1}` is not a non-negative whole number")]
    Number(&'static str, String),
    #[error("OBS_REGIONS_CATALOG_URL `{0}` is not an http(s) URL")]
    CatalogUrl(String),
    #[error(
        "OBS_REGIONS_CATALOG_URL `{0}` points at a private or loopback address; set OBS_ALLOW_PRIVATE_UPSTREAMS=1 if that is intended"
    )]
    PrivateCatalogUrl(String),
    #[error("OBS_ALLOW_PRIVATE_UPSTREAMS `{0}` is not one of 1/true/yes/on or 0/false/no/off")]
    AllowPrivate(String),
    #[error("OBS_LOG_FORMAT `{0}` is not `text` or `json`")]
    LogFormat(String),
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Listen address (`OBS_REGIONS_BIND`). Keep it on a private network.
    pub bind: SocketAddr,
    /// Region databases, downloads and state (`OBS_REGIONS_DATA_DIR`).
    pub data_dir: PathBuf,
    /// Directory with Overpass's `osm3s_query` and `update_database` (`OBS_REGIONS_OVERPASS_BIN`).
    pub overpass_bin: PathBuf,
    /// The `osmium` executable (`OBS_REGIONS_OSMIUM`).
    pub osmium: PathBuf,
    /// Extract list URL (`OBS_REGIONS_CATALOG_URL`).
    pub catalog_url: String,
    /// Let upstreams resolve to loopback, private or link-local addresses
    /// (`OBS_ALLOW_PRIVATE_UPSTREAMS`).
    pub allow_private_upstreams: bool,
    /// Largest extract that will be downloaded (`OBS_REGIONS_MAX_DOWNLOAD_MB`).
    pub max_download_bytes: u64,
    /// Re-import ready regions after this many days; 0 = never (`OBS_REGIONS_REFRESH_DAYS`).
    pub refresh_days: u32,
    /// Bearer token every request but `/healthz` must carry (`OBS_REGIONS_TOKEN`).
    pub token: Option<String>,
    /// Queries run at once (`OBS_REGIONS_QUERY_CONCURRENCY`).
    pub query_concurrency: usize,
    /// Longest a query may run.
    pub query_timeout: Duration,
    /// Contact URL for the `User-Agent` (`OBS_CONTACT`).
    pub contact: String,
    /// Log output format (`OBS_LOG_FORMAT`).
    pub json_logs: bool,
}

impl Config {
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
        let num = |k: &'static str, d: u64| -> Result<u64, ConfigError> {
            get(k).map_or(Ok(d), |raw| {
                raw.parse().map_err(|_| ConfigError::Number(k, raw))
            })
        };

        let bind_raw = or("OBS_REGIONS_BIND", "0.0.0.0:8090");
        let bind = bind_raw.parse().map_err(|_| ConfigError::Bind(bind_raw))?;
        let allow_private_upstreams = match get("OBS_ALLOW_PRIVATE_UPSTREAMS") {
            None => false,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "0" | "false" | "no" | "off" => false,
                "1" | "true" | "yes" | "on" => true,
                _ => return Err(ConfigError::AllowPrivate(raw)),
            },
        };
        let catalog_url = or("OBS_REGIONS_CATALOG_URL", DEFAULT_CATALOG_URL);
        match observation_upstream::check_url(&catalog_url, allow_private_upstreams) {
            Ok(()) => {}
            Err(UrlProblem::Malformed) => {
                return Err(ConfigError::CatalogUrl(redact(&catalog_url)));
            }
            Err(UrlProblem::Private) => {
                return Err(ConfigError::PrivateCatalogUrl(redact(&catalog_url)));
            }
        }
        let json_logs = match get("OBS_LOG_FORMAT").as_deref() {
            None | Some("text") => false,
            Some("json") => true,
            Some(other) => return Err(ConfigError::LogFormat(other.to_owned())),
        };
        let refresh_days = num("OBS_REGIONS_REFRESH_DAYS", 7)?;
        Ok(Self {
            bind,
            data_dir: or("OBS_REGIONS_DATA_DIR", "/data").into(),
            overpass_bin: or("OBS_REGIONS_OVERPASS_BIN", "/opt/overpass/bin").into(),
            osmium: or("OBS_REGIONS_OSMIUM", "osmium").into(),
            catalog_url,
            allow_private_upstreams,
            max_download_bytes: num("OBS_REGIONS_MAX_DOWNLOAD_MB", 2048)?.saturating_mul(1 << 20),
            refresh_days: u32::try_from(refresh_days).unwrap_or(u32::MAX),
            token: get("OBS_REGIONS_TOKEN"),
            query_concurrency: num("OBS_REGIONS_QUERY_CONCURRENCY", 4)?.clamp(1, 64) as usize,
            query_timeout: Duration::from_secs(30),
            contact: or("OBS_CONTACT", DEFAULT_CONTACT),
            json_logs,
        })
    }

    /// `User-Agent` for Geofabrik.
    pub fn user_agent(&self) -> String {
        format!(
            "observation-regions/{} (+{})",
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
    fn defaults_and_errors() {
        let c = Config::from_lookup(lookup(&[])).unwrap();
        assert_eq!(c.bind.port(), 8090);
        assert_eq!(c.max_download_bytes, 2048 << 20);
        assert_eq!(c.refresh_days, 7);
        assert_eq!(c.token, None);

        let c = Config::from_lookup(lookup(&[
            ("OBS_REGIONS_MAX_DOWNLOAD_MB", "10"),
            ("OBS_REGIONS_REFRESH_DAYS", "0"),
            ("OBS_REGIONS_TOKEN", " secret "),
        ]))
        .unwrap();
        assert_eq!(c.max_download_bytes, 10 << 20);
        assert_eq!(c.refresh_days, 0);
        assert_eq!(c.token.as_deref(), Some("secret"));

        assert!(matches!(
            Config::from_lookup(lookup(&[("OBS_REGIONS_REFRESH_DAYS", "-1")])),
            Err(ConfigError::Number(..))
        ));
        assert!(matches!(
            Config::from_lookup(lookup(&[("OBS_REGIONS_CATALOG_URL", "ftp://x/")])),
            Err(ConfigError::CatalogUrl(_))
        ));
    }

    #[test]
    fn allow_private_upstreams() {
        let get = |v: &str| {
            Config::from_lookup(lookup(&[("OBS_ALLOW_PRIVATE_UPSTREAMS", v)]))
                .map(|c| c.allow_private_upstreams)
        };
        assert!(
            !Config::from_lookup(lookup(&[]))
                .unwrap()
                .allow_private_upstreams
        );
        for v in ["1", "true", "YES", " On "] {
            assert_eq!(get(v), Ok(true), "{v}");
        }
        for v in ["0", "False", "no", "OFF", ""] {
            assert_eq!(get(v), Ok(false), "{v}");
        }
        assert_eq!(get("maybe"), Err(ConfigError::AllowPrivate("maybe".into())));
    }

    #[test]
    fn catalog_url_checks() {
        let catalog = |url: &str, allow: &str| {
            Config::from_lookup(lookup(&[
                ("OBS_REGIONS_CATALOG_URL", url),
                ("OBS_ALLOW_PRIVATE_UPSTREAMS", allow),
            ]))
            .map(|c| c.catalog_url)
        };
        for url in [
            "http://169.254.169.254/index.json",
            "http://localhost:8080/index.json",
            "http://[::1]/index.json",
        ] {
            assert_eq!(
                catalog(url, ""),
                Err(ConfigError::PrivateCatalogUrl(url.into())),
                "{url}"
            );
            assert_eq!(catalog(url, "1"), Ok(url.to_owned()), "{url}");
        }
        // Credentials stay out of error messages.
        let err = catalog("http://u:secret@10.0.0.1/i.json", "").unwrap_err();
        assert_eq!(
            err,
            ConfigError::PrivateCatalogUrl("http://10.0.0.1/i.json".into())
        );
        assert!(err.to_string().contains("OBS_ALLOW_PRIVATE_UPSTREAMS"));
        assert_eq!(
            catalog("ftp://u:secret@mirror.example/i.json", ""),
            Err(ConfigError::CatalogUrl(
                "ftp://mirror.example/i.json".into()
            ))
        );
        assert_eq!(
            catalog("u:secret@mirror.example/i.json", "1"),
            Err(ConfigError::CatalogUrl("<invalid URL>".into()))
        );
    }
}
