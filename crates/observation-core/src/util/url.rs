//! URL checks.

/// Longest `source` URL accepted.
const MAX_SOURCE_LEN: usize = 2048;

/// An absolute `https://` URL with a non-empty host, no user info (so
/// `https://trusted.example@evil.example` cannot pose as another site) and no
/// whitespace or control characters.
pub fn is_https_url(s: &str) -> bool {
    if s.len() > MAX_SOURCE_LEN || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((scheme, rest)) = s.split_at_checked(8) else {
        return false;
    };
    // Browsers end the authority at `/`, `\`, `?` or `#`.
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or_default();
    scheme.eq_ignore_ascii_case("https://")
        && !authority.is_empty()
        && !authority.starts_with(':')
        && !authority.contains('@')
}
