//! Settings of the optional place-name search (`OBS_GEOCODER_*`).
//!
//! The search is off unless the operator names a Nominatim-compatible
//! endpoint, because every search then goes to that third party.

use observation_upstream::redact;

use super::upstreams::check_upstream;
use super::{ConfigError, MAX_RATE};
use crate::ratelimit::Rate;

/// `OBS_GEOCODER_URL` value that keeps the search off (as unset does).
pub const GEOCODER_OFF: &str = "off";
/// Default credit shown with search results. Nominatim's results are
/// OpenStreetMap data, which must be credited wherever they are shown.
pub const DEFAULT_GEOCODER_ATTRIBUTION: &str =
    "Search by Nominatim · Data © OpenStreetMap contributors, ODbL";
/// Longest `OBS_GEOCODER_CONTACT` accepted, in characters.
pub const MAX_CONTACT_CHARS: usize = 200;
/// Default per-client limit on searches that go upstream: 5 at once, then
/// one every 6 s. A person types a handful of searches, never a stream.
pub const DEFAULT_GEOCODER_RATE: Rate = Rate {
    per_minute: 10,
    burst: 5,
};

/// The place-name search settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeocoderConfig {
    /// The Nominatim-compatible `/search` endpoint (`OBS_GEOCODER_URL`);
    /// `None` (unset or `off`) turns the search off.
    pub url: Option<String>,
    /// The operator's contact, appended to the `User-Agent` and, when it is
    /// an email address, sent as Nominatim's `email` parameter
    /// (`OBS_GEOCODER_CONTACT`).
    pub contact: Option<String>,
    /// Credit shown with the results (`OBS_GEOCODER_ATTRIBUTION`).
    pub attribution: String,
    /// Per-client limit on searches that go upstream
    /// (`OBS_GEOCODER_RATE_PER_MIN`, `OBS_GEOCODER_BURST`).
    pub rate: Rate,
}

impl Default for GeocoderConfig {
    fn default() -> Self {
        Self {
            url: None,
            contact: None,
            attribution: DEFAULT_GEOCODER_ATTRIBUTION.to_owned(),
            rate: DEFAULT_GEOCODER_RATE,
        }
    }
}

impl GeocoderConfig {
    /// Reads the settings through `get`, which already trims values and
    /// treats empty ones as unset.
    pub(super) fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let url = get("OBS_GEOCODER_URL").filter(|u| !u.eq_ignore_ascii_case(GEOCODER_OFF));
        let number = |var: &'static str, default: u32, min: u32| match get(var) {
            None => Ok(default),
            Some(value) => value
                .parse::<u32>()
                .ok()
                .filter(|n| (min..=MAX_RATE).contains(n))
                .ok_or(ConfigError::Rate { var, value, min }),
        };
        Ok(Self {
            url,
            contact: get("OBS_GEOCODER_CONTACT"),
            attribution: get("OBS_GEOCODER_ATTRIBUTION")
                .unwrap_or_else(|| DEFAULT_GEOCODER_ATTRIBUTION.to_owned()),
            rate: Rate {
                per_minute: number(
                    "OBS_GEOCODER_RATE_PER_MIN",
                    DEFAULT_GEOCODER_RATE.per_minute,
                    0,
                )?,
                burst: number("OBS_GEOCODER_BURST", DEFAULT_GEOCODER_RATE.burst, 1)?,
            },
        })
    }

    /// Checks the endpoint like the other upstreams, the contact and the
    /// per-client limit.
    pub(super) fn validate(&self, allow_private: bool) -> Result<(), ConfigError> {
        for (var, value, min) in [
            ("OBS_GEOCODER_RATE_PER_MIN", self.rate.per_minute, 0),
            ("OBS_GEOCODER_BURST", self.rate.burst, 1),
        ] {
            if !(min..=MAX_RATE).contains(&value) {
                return Err(ConfigError::Rate {
                    var,
                    value: value.to_string(),
                    min,
                });
            }
        }
        if let Some(contact) = &self.contact
            && !contact_is_usable(contact)
        {
            return Err(ConfigError::GeocoderContact);
        }
        match &self.url {
            Some(url) => check_upstream("OBS_GEOCODER_URL", url, allow_private, || {
                ConfigError::GeocoderUrl(redact(url))
            }),
            None => Ok(()),
        }
    }

    /// The `User-Agent` sent to the geocoder: the app's name, version and
    /// repository, then the operator's contact if one is set, as
    /// Nominatim's usage policy asks. It carries nothing about visitors.
    pub fn user_agent(&self) -> String {
        let base = format!(
            "observation/{} (+{}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_REPOSITORY")
        );
        match &self.contact {
            Some(contact) => format!("{base}; {contact})"),
            None => format!("{base})"),
        }
    }

    /// The contact when it is an email address, for Nominatim's `email`
    /// parameter.
    pub fn email(&self) -> Option<&str> {
        self.contact.as_deref().filter(|c| {
            !c.contains(['/', ' '])
                && c.split_once('@')
                    .is_some_and(|(a, b)| !a.is_empty() && b.contains('.'))
        })
    }
}

/// Printable ASCII without parentheses (which would end the `User-Agent`
/// comment early), at most [`MAX_CONTACT_CHARS`] long.
fn contact_is_usable(contact: &str) -> bool {
    contact.len() <= MAX_CONTACT_CHARS
        && contact
            .bytes()
            .all(|b| (0x20..0x7f).contains(&b) && b != b'(' && b != b')')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        Config::from_lookup(move |k| m.get(k).cloned())
    }

    #[test]
    fn the_search_is_off_unless_an_endpoint_is_named() {
        let c = config(&[]).unwrap();
        assert_eq!(c.geocoder, GeocoderConfig::default());
        assert_eq!(c.geocoder.url, None);
        for off in ["off", " OFF ", ""] {
            assert_eq!(
                config(&[("OBS_GEOCODER_URL", off)]).unwrap().geocoder.url,
                None,
                "{off:?}"
            );
        }
        let c = config(&[
            ("OBS_GEOCODER_URL", "https://nominatim.example/search"),
            ("OBS_GEOCODER_CONTACT", "ops@example.org"),
            ("OBS_GEOCODER_ATTRIBUTION", "Search: somebody"),
            ("OBS_GEOCODER_RATE_PER_MIN", "4"),
            ("OBS_GEOCODER_BURST", "2"),
        ])
        .unwrap();
        assert_eq!(
            c.geocoder,
            GeocoderConfig {
                url: Some("https://nominatim.example/search".into()),
                contact: Some("ops@example.org".into()),
                attribution: "Search: somebody".into(),
                rate: Rate {
                    per_minute: 4,
                    burst: 2
                },
            }
        );
    }

    #[test]
    fn bad_values_stop_the_server() {
        assert!(matches!(
            config(&[("OBS_GEOCODER_URL", "nominatim.example/search")]),
            Err(ConfigError::GeocoderUrl(_))
        ));
        assert!(matches!(
            config(&[("OBS_GEOCODER_URL", "http://127.0.0.1:8088/search")]),
            Err(ConfigError::PrivateUpstream {
                var: "OBS_GEOCODER_URL",
                ..
            })
        ));
        assert!(
            config(&[
                ("OBS_GEOCODER_URL", "http://127.0.0.1:8088/search"),
                ("OBS_ALLOW_PRIVATE_UPSTREAMS", "1"),
            ])
            .is_ok()
        );
        let long = "x".repeat(MAX_CONTACT_CHARS + 1);
        for bad in ["me (ops)", "caf\u{e9}@example.org", long.as_str()] {
            assert_eq!(
                config(&[("OBS_GEOCODER_CONTACT", bad)]).unwrap_err(),
                ConfigError::GeocoderContact,
                "{bad}"
            );
        }
        for (var, bad) in [
            ("OBS_GEOCODER_RATE_PER_MIN", "-1"),
            ("OBS_GEOCODER_BURST", "0"),
        ] {
            assert!(
                matches!(
                    config(&[(var, bad)]),
                    Err(ConfigError::Rate { var: v, .. }) if v == var
                ),
                "{var}={bad}"
            );
        }
        let msg = config(&[("OBS_GEOCODER_URL", "ftp://user:secret@nominatim.example/")])
            .unwrap_err()
            .to_string();
        assert!(!msg.contains("secret"), "{msg}");
    }

    #[test]
    fn the_user_agent_names_the_app_and_only_the_operators_contact() {
        let mut g = GeocoderConfig::default();
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            g.user_agent(),
            format!("observation/{version} (+https://github.com/panevain/observation)")
        );
        assert_eq!(g.email(), None);
        g.contact = Some("ops@example.org".into());
        assert_eq!(
            g.user_agent(),
            format!(
                "observation/{version} (+https://github.com/panevain/observation; ops@example.org)"
            )
        );
        assert_eq!(g.email(), Some("ops@example.org"));
        g.contact = Some("https://maps.example.org/contact".into());
        assert_eq!(g.email(), None);
    }
}
