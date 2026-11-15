//! Settings for the HTTP transport, read from the environment.
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `OBS_MCP_LISTEN` | Address to serve on, e.g. `127.0.0.1:8090`; setting it (or `--http`) selects HTTP |
//! | `OBS_MCP_TOKENS` | Comma-separated `name:secret` pairs |
//! | `OBS_MCP_TOKENS_FILE` | A file of `name:secret` pairs, on lines or separated by commas (`#` comment lines and blank lines ignored) |
//! | `OBS_MCP_INSECURE_NO_AUTH` | `1` or `true` serves without tokens |
//! | `OBS_MCP_ALLOWED_HOSTS` | Comma-separated `Host` values to accept (default loopback names) |
//! | `OBS_MCP_ALLOWED_ORIGINS` | Comma-separated browser origins to accept and answer CORS for (default none) |

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;
use thiserror::Error;

/// The shortest secret accepted: 32 characters of the bearer-token
/// alphabet, about 190 bits of entropy when generated at random.
pub const MIN_SECRET_LEN: usize = 32;

/// `Host` values accepted when `OBS_MCP_ALLOWED_HOSTS` is unset (any port).
pub const DEFAULT_ALLOWED_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// A client's bearer token. The name identifies the client in logs; the
/// secret is never logged or printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Token {
    pub name: String,
    pub secret: String,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("name", &self.name)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// How requests are authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// A request must carry `Authorization: Bearer <secret>` for one of these.
    Tokens(Vec<Token>),
    /// No authentication (`OBS_MCP_INSECURE_NO_AUTH`).
    Disabled,
}

/// The HTTP transport's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConfig {
    pub listen: SocketAddr,
    pub auth: Auth,
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("listen address `{0}` is not an IP address and port, e.g. 127.0.0.1:8090")]
    Listen(String),
    #[error(
        "HTTP needs bearer tokens: set OBS_MCP_TOKENS or OBS_MCP_TOKENS_FILE, \
         or OBS_MCP_INSECURE_NO_AUTH=1 to serve without authentication"
    )]
    NoTokens,
    #[error("set only one of OBS_MCP_TOKENS and OBS_MCP_TOKENS_FILE")]
    BothTokenSources,
    #[error("OBS_MCP_INSECURE_NO_AUTH is set together with tokens; unset one of them")]
    InsecureWithTokens,
    #[error("OBS_MCP_INSECURE_NO_AUTH must be 1, true, 0 or false, not `{0}`")]
    InsecureFlag(String),
    #[error("cannot read OBS_MCP_TOKENS_FILE `{path}`: {reason}")]
    TokensFile { path: String, reason: String },
    #[error("{source_name}: entry {entry} is not `name:secret`")]
    TokenFormat { source_name: String, entry: usize },
    #[error(
        "{source_name}: token name `{name}` must be 1-64 characters of letters, digits, `-`, `_` or `.`"
    )]
    TokenName { source_name: String, name: String },
    #[error(
        "{source_name}: the secret of token `{name}` must be at least {MIN_SECRET_LEN} characters of \
         letters, digits and `-._~+/`, optionally ending in `=` (try `openssl rand -base64 32`)"
    )]
    TokenSecret { source_name: String, name: String },
    #[error("{source_name}: token name `{0}` appears more than once", source_name = .1)]
    DuplicateName(String, String),
    #[error("{source_name}: tokens `{0}` and `{1}` have the same secret", source_name = .2)]
    DuplicateSecret(String, String, String),
    #[error("OBS_MCP_ALLOWED_HOSTS has an empty entry")]
    EmptyHost,
    #[error(
        "OBS_MCP_ALLOWED_ORIGINS entry `{0}` must be `null` or http(s)://host:port, with `:*` for any port"
    )]
    Origin(String),
}

impl HttpConfig {
    /// Reads the settings from the process environment and the file
    /// system. `listen` (from `--http`) takes precedence over `OBS_MCP_LISTEN`.
    /// `None` when neither selects HTTP.
    pub fn from_env(listen: Option<String>) -> Result<Option<Self>, ConfigError> {
        Self::from_lookup(
            listen,
            |k| std::env::var(k).ok().filter(|v| !v.is_empty()),
            |p| std::fs::read_to_string(p).map_err(|e| e.to_string()),
        )
    }

    /// As [`from_env`](Self::from_env), with the environment and file reads
    /// supplied by the caller.
    pub fn from_lookup(
        listen: Option<String>,
        get: impl Fn(&str) -> Option<String>,
        read: impl Fn(&Path) -> Result<String, String>,
    ) -> Result<Option<Self>, ConfigError> {
        let Some(listen_raw) = listen.or_else(|| get("OBS_MCP_LISTEN")) else {
            return Ok(None);
        };
        let listen = listen_raw
            .trim()
            .parse()
            .map_err(|_| ConfigError::Listen(listen_raw.clone()))?;

        let insecure = match get("OBS_MCP_INSECURE_NO_AUTH").as_deref().map(str::trim) {
            None | Some("0" | "false") => false,
            Some("1" | "true") => true,
            Some(other) => return Err(ConfigError::InsecureFlag(other.to_owned())),
        };
        let tokens = match (get("OBS_MCP_TOKENS"), get("OBS_MCP_TOKENS_FILE")) {
            (Some(_), Some(_)) => return Err(ConfigError::BothTokenSources),
            (Some(list), None) => {
                let entries = list.split(',').filter(|e| !e.trim().is_empty());
                Some(parse_tokens("OBS_MCP_TOKENS", entries)?)
            }
            (None, Some(path)) => {
                let text = read(Path::new(&path)).map_err(|reason| ConfigError::TokensFile {
                    path: path.clone(),
                    reason,
                })?;
                // Pairs on lines or separated by commas (as in OBS_MCP_TOKENS, which a
                // Compose secret may hold as it is).
                let entries = text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .flat_map(|l| l.split(','))
                    .filter(|e| !e.trim().is_empty());
                Some(parse_tokens("OBS_MCP_TOKENS_FILE", entries)?)
            }
            (None, None) => None,
        };
        let auth = match (tokens, insecure) {
            (Some(_), true) => return Err(ConfigError::InsecureWithTokens),
            (Some(tokens), false) => Auth::Tokens(tokens),
            (None, true) => Auth::Disabled,
            (None, false) => return Err(ConfigError::NoTokens),
        };

        let allowed_hosts = match get("OBS_MCP_ALLOWED_HOSTS") {
            None => DEFAULT_ALLOWED_HOSTS
                .iter()
                .map(|&h| h.to_owned())
                .collect(),
            Some(list) => list
                .split(',')
                .map(|h| {
                    let h = h.trim();
                    if h.is_empty() {
                        Err(ConfigError::EmptyHost)
                    } else {
                        Ok(h.to_owned())
                    }
                })
                .collect::<Result<_, _>>()?,
        };
        let allowed_origins = get("OBS_MCP_ALLOWED_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .map(|o| {
                if valid_origin(o) {
                    Ok(o.to_owned())
                } else {
                    Err(ConfigError::Origin(o.to_owned()))
                }
            })
            .collect::<Result<_, _>>()?;

        Ok(Some(Self {
            listen,
            auth,
            allowed_hosts,
            allowed_origins,
        }))
    }
}

fn parse_tokens<'a>(
    source_name: &str,
    entries: impl Iterator<Item = &'a str>,
) -> Result<Vec<Token>, ConfigError> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut names = HashSet::new();
    for (i, entry) in entries.enumerate() {
        let entry = entry.trim();
        let Some((name, secret)) = entry.split_once(':') else {
            return Err(ConfigError::TokenFormat {
                source_name: source_name.to_owned(),
                entry: i + 1,
            });
        };
        let (name, secret) = (name.trim(), secret.trim());
        if !valid_name(name) {
            return Err(ConfigError::TokenName {
                source_name: source_name.to_owned(),
                name: name.to_owned(),
            });
        }
        if !valid_secret(secret) {
            return Err(ConfigError::TokenSecret {
                source_name: source_name.to_owned(),
                name: name.to_owned(),
            });
        }
        if !names.insert(name.to_owned()) {
            return Err(ConfigError::DuplicateName(
                name.to_owned(),
                source_name.to_owned(),
            ));
        }
        if let Some(other) = tokens.iter().find(|t| t.secret == secret) {
            return Err(ConfigError::DuplicateSecret(
                other.name.clone(),
                name.to_owned(),
                source_name.to_owned(),
            ));
        }
        tokens.push(Token {
            name: name.to_owned(),
            secret: secret.to_owned(),
        });
    }
    if tokens.is_empty() {
        return Err(ConfigError::NoTokens);
    }
    Ok(tokens)
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// RFC 6750's `b64token`, at least [`MIN_SECRET_LEN`] characters long.
fn valid_secret(secret: &str) -> bool {
    let body = secret.trim_end_matches('=');
    secret.len() >= MIN_SECRET_LEN
        && !body.is_empty()
        && body.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
        })
}

/// `null`, or `http(s)://host:port` with an explicit port or `:*`. rmcp
/// treats an entry without a port as any port for now but plans to change
/// that, so a port is required here.
fn valid_origin(origin: &str) -> bool {
    origin == "null"
        || split_origin(origin)
            .is_some_and(|(_, _, port)| port == Some("*") || port.and_then(parse_port).is_some())
}

fn parse_port(port: &str) -> Option<u16> {
    port.parse::<u16>().ok().filter(|&p| p > 0)
}

/// An origin's scheme (`http` or `https`, lower case), host (lower case)
/// and port text, if it has one.
fn split_origin(origin: &str) -> Option<(String, String, Option<&str>)> {
    let (scheme, authority) = origin.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // The port follows the last `:` after any IPv6 literal's closing bracket.
    let after_host = authority
        .rfind(']')
        .map_or(authority, |i| &authority[i + 1..]);
    let (host, port) = match after_host.rfind(':') {
        Some(colon) => {
            let host_len = authority.len() - after_host.len() + colon;
            (&authority[..host_len], Some(&after_host[colon + 1..]))
        }
        None => (authority, None),
    };
    if host.is_empty() || host.contains(['/', '@', '?', '#', ' ']) {
        return None;
    }
    Some((scheme, host.to_ascii_lowercase(), port))
}

/// Whether a request's `Origin` value matches one of `allowed` (entries as
/// [`HttpConfig::allowed_origins`] holds them), by RFC 6454's scheme, host
/// and port, as rmcp checks it: `:*` matches any port, and an origin without
/// a port has its scheme's default.
pub fn origin_allowed(allowed: &[String], origin: &str) -> bool {
    if origin == "null" {
        return allowed.iter().any(|a| a == "null");
    }
    let Some((scheme, host, port)) = split_origin(origin) else {
        return false;
    };
    let port = match port {
        Some(p) => match parse_port(p) {
            Some(p) => p,
            None => return false,
        },
        None if scheme == "https" => 443,
        None => 80,
    };
    allowed.iter().any(|entry| {
        split_origin(entry).is_some_and(|(s, h, p)| {
            s == scheme && h == host && (p == Some("*") || p.and_then(parse_port) == Some(port))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const S1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const S2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb=";

    fn load(
        listen: Option<&str>,
        env: &[(&str, &str)],
        files: &[(&str, &str)],
    ) -> Result<Option<HttpConfig>, ConfigError> {
        let env: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let files: HashMap<String, String> = files
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        HttpConfig::from_lookup(
            listen.map(str::to_owned),
            |k| env.get(k).cloned(),
            |p| {
                files
                    .get(p.to_str().unwrap_or_default())
                    .cloned()
                    .ok_or_else(|| "no such file".to_owned())
            },
        )
    }

    #[test]
    fn stdio_when_no_listen_address() {
        let tokens = format!("a:{S1}");
        assert_eq!(load(None, &[("OBS_MCP_TOKENS", &tokens)], &[]), Ok(None));
    }

    #[test]
    fn tokens_from_env_with_defaults() {
        // Spaces around entries and empty entries (a trailing comma) are ignored.
        let tokens = format!("laptop:{S1}, ci : {S2},");
        let c = load(
            None,
            &[
                ("OBS_MCP_LISTEN", "127.0.0.1:8090"),
                ("OBS_MCP_TOKENS", &tokens),
            ],
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(c.listen, "127.0.0.1:8090".parse().unwrap());
        let Auth::Tokens(t) = &c.auth else {
            panic!("expected tokens")
        };
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].name.as_str(), t[0].secret.as_str()), ("laptop", S1));
        assert_eq!((t[1].name.as_str(), t[1].secret.as_str()), ("ci", S2));
        assert_eq!(c.allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
        assert!(c.allowed_origins.is_empty());
    }

    #[test]
    fn flag_overrides_env_listen() {
        let tokens = format!("a:{S1}");
        let c = load(
            Some("[::1]:9000"),
            &[
                ("OBS_MCP_LISTEN", "127.0.0.1:8090"),
                ("OBS_MCP_TOKENS", &tokens),
            ],
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(c.listen, "[::1]:9000".parse().unwrap());
    }

    #[test]
    fn tokens_from_file_skip_comments_and_blanks() {
        let file = format!("# clients\n\nlaptop:{S1}\n  ci:{S2}  \n");
        let secret = format!("laptop:{S1},ci:{S2}\n");
        let c = load(
            Some("0.0.0.0:8090"),
            &[("OBS_MCP_TOKENS_FILE", "/run/secrets/t")],
            &[("/run/secrets/t", &file)],
        )
        .unwrap()
        .unwrap();
        let Auth::Tokens(t) = c.auth else {
            panic!("expected tokens")
        };
        assert_eq!(
            t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["laptop", "ci"]
        );
        // A Compose secret holding OBS_MCP_TOKENS's value as it is.
        let c = load(
            Some("0.0.0.0:8090"),
            &[("OBS_MCP_TOKENS_FILE", "/run/secrets/t")],
            &[("/run/secrets/t", &secret)],
        )
        .unwrap()
        .unwrap();
        let Auth::Tokens(from_secret) = c.auth else {
            panic!("expected tokens")
        };
        assert_eq!(from_secret, t);
    }

    #[test]
    fn refuses_to_serve_without_tokens() {
        assert_eq!(
            load(Some("127.0.0.1:1"), &[("OBS_MCP_TOKENS", " , ")], &[]),
            Err(ConfigError::NoTokens)
        );
        assert_eq!(
            load(Some("127.0.0.1:1"), &[], &[]),
            Err(ConfigError::NoTokens)
        );
        let err = load(
            Some("127.0.0.1:1"),
            &[("OBS_MCP_TOKENS_FILE", "/t")],
            &[("/t", "# nothing\n")],
        );
        assert_eq!(err, Err(ConfigError::NoTokens));
    }

    #[test]
    fn insecure_flag() {
        let c = load(
            Some("127.0.0.1:1"),
            &[("OBS_MCP_INSECURE_NO_AUTH", "1")],
            &[],
        );
        assert_eq!(c.unwrap().unwrap().auth, Auth::Disabled);
        let c = load(
            Some("127.0.0.1:1"),
            &[("OBS_MCP_INSECURE_NO_AUTH", "false")],
            &[],
        );
        assert_eq!(c, Err(ConfigError::NoTokens));
        let c = load(
            Some("127.0.0.1:1"),
            &[("OBS_MCP_INSECURE_NO_AUTH", "yes")],
            &[],
        );
        assert_eq!(c, Err(ConfigError::InsecureFlag("yes".into())));
        let tokens = format!("a:{S1}");
        let c = load(
            Some("127.0.0.1:1"),
            &[
                ("OBS_MCP_INSECURE_NO_AUTH", "true"),
                ("OBS_MCP_TOKENS", &tokens),
            ],
            &[],
        );
        assert_eq!(c, Err(ConfigError::InsecureWithTokens));
    }

    #[test]
    fn rejects_bad_tokens() {
        let at = |tokens: &str| load(Some("127.0.0.1:1"), &[("OBS_MCP_TOKENS", tokens)], &[]);
        assert!(matches!(
            at(S1),
            Err(ConfigError::TokenFormat { entry: 1, .. })
        ));
        assert!(matches!(
            at(&format!("bad name:{S1}")),
            Err(ConfigError::TokenName { .. })
        ));
        assert!(matches!(
            at(&format!(":{S1}")),
            Err(ConfigError::TokenName { .. })
        ));
        assert!(matches!(
            at("a:short"),
            Err(ConfigError::TokenSecret { .. })
        ));
        let spaced = format!("a:{} {}", &S1[..20], &S1[..20]);
        assert!(matches!(at(&spaced), Err(ConfigError::TokenSecret { .. })));
        let padded_only = "=".repeat(40);
        assert!(matches!(
            at(&format!("a:{padded_only}")),
            Err(ConfigError::TokenSecret { .. })
        ));
        assert!(matches!(
            at(&format!("a:{S1},a:{S2}")),
            Err(ConfigError::DuplicateName(n, _)) if n == "a"
        ));
        assert!(matches!(
            at(&format!("a:{S1},b:{S1}")),
            Err(ConfigError::DuplicateSecret(a, b, _)) if a == "a" && b == "b"
        ));
        let both = load(
            Some("127.0.0.1:1"),
            &[("OBS_MCP_TOKENS", "x"), ("OBS_MCP_TOKENS_FILE", "/t")],
            &[],
        );
        assert_eq!(both, Err(ConfigError::BothTokenSources));
        let missing = load(
            Some("127.0.0.1:1"),
            &[("OBS_MCP_TOKENS_FILE", "/nope")],
            &[],
        );
        assert!(matches!(missing, Err(ConfigError::TokensFile { .. })));
    }

    #[test]
    fn token_debug_hides_the_secret() {
        let t = Token {
            name: "laptop".into(),
            secret: S1.into(),
        };
        let shown = format!("{t:?}");
        assert!(shown.contains("laptop") && !shown.contains(S1));
    }

    #[test]
    fn matches_origins_as_rmcp_does() {
        let allowed: Vec<String> = [
            "http://localhost:8080",
            "https://chat.example.com:*",
            "null",
        ]
        .map(String::from)
        .to_vec();
        for yes in [
            "http://localhost:8080",
            "HTTP://LocalHost:8080",
            "https://chat.example.com",
            "https://chat.example.com:8443",
            "null",
        ] {
            assert!(origin_allowed(&allowed, yes), "{yes}");
        }
        for no in [
            "http://localhost",
            "http://localhost:8081",
            "https://localhost:8080",
            "http://chat.example.com",
            "http://127.0.0.1:8080",
            "http://localhost:0",
            "http://localhost:x",
            "localhost:8080",
            "",
        ] {
            assert!(!origin_allowed(&allowed, no), "{no}");
        }
        let defaults: Vec<String> = ["https://a.example:443", "http://b.example:80"]
            .map(String::from)
            .to_vec();
        assert!(origin_allowed(&defaults, "https://a.example"));
        assert!(origin_allowed(&defaults, "http://b.example"));
        assert!(!origin_allowed(&[], "null"));
    }

    #[test]
    fn listen_must_be_a_socket_address() {
        assert_eq!(
            load(
                Some("localhost:8090"),
                &[("OBS_MCP_INSECURE_NO_AUTH", "1")],
                &[]
            ),
            Err(ConfigError::Listen("localhost:8090".into()))
        );
    }

    #[test]
    fn hosts_and_origins() {
        let c = load(
            Some("0.0.0.0:8090"),
            &[
                ("OBS_MCP_INSECURE_NO_AUTH", "1"),
                ("OBS_MCP_ALLOWED_HOSTS", "mcp.example.com, localhost:8090"),
                (
                    "OBS_MCP_ALLOWED_ORIGINS",
                    "https://app.example.com:443, http://localhost:*, http://[::1]:8080, null",
                ),
            ],
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(c.allowed_hosts, ["mcp.example.com", "localhost:8090"]);
        assert_eq!(
            c.allowed_origins,
            [
                "https://app.example.com:443",
                "http://localhost:*",
                "http://[::1]:8080",
                "null"
            ]
        );
        let host = |h: &str| {
            load(
                Some("0.0.0.0:8090"),
                &[
                    ("OBS_MCP_INSECURE_NO_AUTH", "1"),
                    ("OBS_MCP_ALLOWED_HOSTS", h),
                ],
                &[],
            )
        };
        assert_eq!(host("a,,b"), Err(ConfigError::EmptyHost));
        for bad in [
            "app.example.com",
            "https://app.example.com",
            "ftp://x:21",
            "https://:443",
            "https://a/b:443",
            "https://[::1]",
            "https://x:0",
        ] {
            let c = load(
                Some("0.0.0.0:8090"),
                &[
                    ("OBS_MCP_INSECURE_NO_AUTH", "1"),
                    ("OBS_MCP_ALLOWED_ORIGINS", bad),
                ],
                &[],
            );
            assert_eq!(c, Err(ConfigError::Origin(bad.into())), "{bad}");
        }
    }
}
