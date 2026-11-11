//! Screening the configured upstream URLs and tile URL templates.

use observation_core::tiles::TileId;
use observation_upstream::{UrlProblem, check_url, redact};

use super::ConfigError;

/// Checks one configured upstream URL, naming `var` and the redacted URL on failure.
pub(super) fn check_upstream(
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
pub(super) fn check_template(
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
