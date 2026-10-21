//! Guards for outbound HTTP to operator-configured upstreams.
//!
//! The upstreams (tile server, Overpass, extract downloads) are trusted to be
//! what the operator configured, not to stay that way: a compromised upstream
//! can redirect to, or a hostname can resolve to, an internal address such as a
//! cloud metadata endpoint. Clients built with [`client_builder`] resolve names
//! through [`FilteringResolver`], which drops loopback, private, link-local and
//! similar addresses, and do not follow redirects off the original host.
//! Literal IP hosts never reach the resolver, so [`check_url`] screens those in
//! configuration and the redirect policy screens them in `Location` headers.
//!
//! `allow_private` turns the address filter off for operators whose upstream
//! lives on a private network.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;

/// Redirects followed by a client built with [`Redirects::SameHost`].
const MAX_REDIRECTS: usize = 5;

/// Whether `ip` is an address an upstream should never resolve to: loopback,
/// private, link-local, unique-local, shared (CGNAT), benchmarking, multicast,
/// reserved, "this network", or an IPv6 form that embeds one of those
/// (IPv4-mapped, IPv4-compatible, NAT64, 6to4).
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => blocked_v4(v4),
        IpAddr::V6(v6) => blocked_v6(v6),
    }
}

fn blocked_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    a == 0 // 0.0.0.0/8, "this network"
        || a == 10
        || (a == 100 && b & 0xc0 == 64) // 100.64.0.0/10, CGNAT
        || a == 127
        || (a == 169 && b == 254) // link-local, cloud metadata
        || (a == 172 && b & 0xf0 == 16)
        || (a == 192 && b == 0 && c == 0) // 192.0.0.0/24, incl. 192.0.0.192
        || (a == 192 && b == 168)
        || (a == 198 && b & 0xfe == 18) // 198.18.0.0/15, benchmarking
        || a >= 224 // multicast, reserved, broadcast
}

fn blocked_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let embedded = |hi: u16, lo: u16| {
        let [a, b] = hi.to_be_bytes();
        let [c, d] = lo.to_be_bytes();
        blocked_v4(Ipv4Addr::new(a, b, c, d))
    };
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return blocked_v4(v4);
    }
    // ::a.b.c.d (IPv4-compatible, deprecated).
    if s[..6].iter().all(|&x| x == 0) {
        return embedded(s[6], s[7]);
    }
    // 64:ff9b::/96 (NAT64) maps onto IPv4; 64:ff9b:1::/48 is local-use.
    if s[0] == 0x64 && s[1] == 0xff9b {
        return s[2] != 0 || s[3..6].iter().any(|&x| x != 0) || embedded(s[6], s[7]);
    }
    // ::ffff:0:a.b.c.d (IPv4-translated, SIIT) maps onto IPv4 too.
    if s[..4].iter().all(|&x| x == 0) && s[4] == 0xffff && s[5] == 0 {
        return embedded(s[6], s[7]);
    }
    // 100::/64, discard-only.
    if s[0] == 0x100 && s[1..4].iter().all(|&x| x == 0) {
        return true;
    }
    // 2002::/16 (6to4) carries an IPv4 address in bits 16..48.
    if s[0] == 0x2002 {
        return embedded(s[1], s[2]);
    }
    s[0] & 0xfe00 == 0xfc00 // fc00::/7, unique-local
        || s[0] & 0xffc0 == 0xfe80 // fe80::/10, link-local
        || s[0] & 0xffc0 == 0xfec0 // fec0::/10, site-local (deprecated)
        || s[0] & 0xff00 == 0xff00 // multicast
        || (s[0] == 0x2001 && s[1] == 0) // 2001::/32, Teredo
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
}

/// Why [`check_url`] refused a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlProblem {
    /// Not an absolute http(s) URL with a host.
    Malformed,
    /// The host is a literal address (or `localhost`) that [`is_blocked_ip`] refuses.
    Private,
}

/// Checks an upstream URL given in configuration: http(s) with a host and, unless
/// `allow_private`, not a literal blocked address or `localhost`. Names that
/// merely resolve to blocked addresses are caught at connect time by
/// [`FilteringResolver`] instead.
pub fn check_url(raw: &str, allow_private: bool) -> Result<(), UrlProblem> {
    let url = reqwest::Url::parse(raw)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or(UrlProblem::Malformed)?;
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or(UrlProblem::Malformed)?;
    if !allow_private && host_is_private(host) {
        return Err(UrlProblem::Private);
    }
    Ok(())
}

fn host_is_private(host: &str) -> bool {
    match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(ip) => is_blocked_ip(ip),
        Err(_) => {
            let h = host.trim_end_matches('.').to_ascii_lowercase();
            h == "localhost" || h.ends_with(".localhost")
        }
    }
}

/// `raw` with any user name and password removed, for error messages and logs.
/// An unparseable value that contains `@` is replaced outright, since it may
/// still hold credentials.
pub fn redact(raw: &str) -> String {
    match reqwest::Url::parse(raw) {
        Ok(mut u) if u.has_host() => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        _ if raw.contains('@') => "<invalid URL>".to_owned(),
        _ => raw.to_owned(),
    }
}

/// A DNS resolver that refuses to hand out blocked addresses.
#[derive(Debug, Clone, Copy)]
pub struct FilteringResolver {
    allow_private: bool,
}

impl FilteringResolver {
    pub fn new(allow_private: bool) -> Self {
        Self { allow_private }
    }
}

impl Resolve for FilteringResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let total = found.len();
            let allowed: Vec<SocketAddr> = found
                .into_iter()
                .filter(|a| allow_private || !is_blocked_ip(a.ip()))
                .collect();
            if allowed.is_empty() {
                if total > 0 {
                    tracing::warn!(
                        host,
                        "upstream resolves only to private addresses; set OBS_ALLOW_PRIVATE_UPSTREAMS=1 if that is intended"
                    );
                }
                let why = if total > 0 {
                    "resolves only to private addresses"
                } else {
                    "did not resolve"
                };
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, why).into());
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

/// What a client does with a 3xx response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redirects {
    /// Return the 3xx as the response.
    None,
    /// Follow up to five redirects that stay on the original host,
    /// never downgrade https to http, and never target a blocked literal address.
    SameHost,
}

/// A client builder with the guards applied. Unless `allow_private`, proxy
/// environment variables (`HTTP_PROXY` and friends) are ignored. The caller adds timeouts and the
/// user agent.
pub fn client_builder(allow_private: bool, redirects: Redirects) -> reqwest::ClientBuilder {
    let policy = match redirects {
        Redirects::None => redirect::Policy::none(),
        Redirects::SameHost => redirect::Policy::custom(move |attempt| {
            let next = attempt.url();
            let first = attempt.previous().first();
            let ok = attempt.previous().len() <= MAX_REDIRECTS
                && first.is_some_and(|f| {
                    f.host_str() == next.host_str()
                        && (f.scheme() == next.scheme() || next.scheme() == "https")
                })
                && (allow_private || next.host_str().is_none_or(|h| !host_is_private(h)));
            if ok { attempt.follow() } else { attempt.stop() }
        }),
    };
    let builder = reqwest::Client::builder();
    // A proxy resolves the upstream's name itself, past the filter, so proxy
    // environment variables apply only when the filter is off anyway.
    let builder = if allow_private {
        builder
    } else {
        builder.no_proxy()
    };
    builder
        .dns_resolver(FilteringResolver::new(allow_private))
        .redirect(policy)
        .connect_timeout(Duration::from_secs(10))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(s: &str) -> bool {
        is_blocked_ip(s.parse().unwrap())
    }

    #[test]
    fn blocks_private_and_special_addresses() {
        for ip in [
            "0.1.2.3",
            "10.1.2.3",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.192",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "::169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::1",
            "2002:a9fe:a9fe::",
            "2002:7f00:1::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001::1",
        ] {
            assert!(blocked(ip), "{ip} should be blocked");
        }
    }

    #[test]
    fn allows_public_addresses() {
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.0.1",
            "172.32.0.1",
            "192.0.2.1",
            "192.169.0.1",
            "198.17.0.1",
            "198.20.0.1",
            "223.255.255.255",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::",
        ] {
            assert!(!blocked(ip), "{ip} should be allowed");
        }
    }

    #[test]
    fn checks_configured_urls() {
        assert_eq!(
            check_url("https://tile.openstreetmap.org/0/0/0.png", false),
            Ok(())
        );
        assert_eq!(
            check_url("ftp://x.example/", false),
            Err(UrlProblem::Malformed)
        );
        assert_eq!(
            check_url("overpass.example/api", false),
            Err(UrlProblem::Malformed)
        );
        for u in [
            "http://169.254.169.254/latest",
            "http://[::1]:8080/",
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://localhost/x",
            "http://LOCALHOST./x",
            "http://a.localhost/x",
        ] {
            assert_eq!(check_url(u, false), Err(UrlProblem::Private), "{u}");
            assert_eq!(check_url(u, true), Ok(()), "{u}");
        }
        // Credentials are not this function's business.
        assert_eq!(check_url("https://u:p@overpass.example/o", false), Ok(()));
    }

    #[test]
    fn redacts_credentials() {
        assert_eq!(
            redact("https://u:secret@o.example/x"),
            "https://o.example/x"
        );
        assert_eq!(redact("https://o.example/x"), "https://o.example/x");
        assert_eq!(redact("u:secret@o.example/x"), "<invalid URL>");
        assert_eq!(redact("o.example/x"), "o.example/x");
    }

    /// A one-shot HTTP server on loopback; returns its port.
    async fn serve_once(response: &'static str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(response.as_bytes()).await;
            }
        });
        port
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

    #[tokio::test]
    async fn resolver_refuses_names_that_resolve_privately() {
        // `localhost` resolves to loopback, so only the allowing client may connect.
        let port = serve_once(OK).await;
        let url = format!("http://localhost:{port}/");
        let strict = client_builder(false, Redirects::None).build().unwrap();
        assert!(strict.get(&url).send().await.unwrap_err().is_connect());
        let lax = client_builder(true, Redirects::None).build().unwrap();
        assert_eq!(lax.get(&url).send().await.unwrap().status(), 200);
    }

    #[tokio::test]
    async fn same_host_redirects_are_followed_and_others_are_not() {
        let target = serve_once(OK).await;
        let to_target: &'static str = Box::leak(
            format!("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .into_boxed_str(),
        );
        let port = serve_once(to_target).await;
        // Even with private hosts allowed, a redirect to another host is not followed.
        let c = client_builder(true, Redirects::SameHost).build().unwrap();
        let resp = c
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 302);
        let c = client_builder(true, Redirects::None).build().unwrap();
        let resp = c
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 302);
        // Same host (the port may differ): followed.
        let c = client_builder(true, Redirects::SameHost).build().unwrap();
        let resp = c
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        // A blocked literal target is refused unless private hosts are allowed.
        let c = client_builder(false, Redirects::SameHost).build().unwrap();
        let resp = c
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 302);
    }

    /// A server whose `/n` redirects to `/n+1` until `/{last}`, which answers 200.
    async fn serve_chain(last: u32) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                let at: u32 = head
                    .split_whitespace()
                    .nth(1)
                    .and_then(|p| p.trim_start_matches('/').parse().ok())
                    .unwrap_or(0);
                let reply = if at >= last {
                    OK.to_owned()
                } else {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: /{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        at + 1
                    )
                };
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn five_redirects_are_followed_and_a_sixth_is_not() {
        let c = client_builder(true, Redirects::SameHost).build().unwrap();
        let five = serve_chain(5).await;
        let resp = c
            .get(format!("http://127.0.0.1:{five}/0"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let six = serve_chain(6).await;
        let resp = c
            .get(format!("http://127.0.0.1:{six}/0"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 302);
    }

    /// Runs in a child process that has `HTTP_PROXY` set (the workspace forbids
    /// `unsafe`, so the test cannot set it in-process); a no-op otherwise.
    #[tokio::test]
    async fn proxy_probe_child() {
        // The host the proxy would be asked for, handed over by the parent test.
        let Some(host) = std::env::var_os("OBSERVATION_PROXY_PROBE") else {
            return;
        };
        let target = format!("http://{}/", host.to_string_lossy());
        let strict = client_builder(false, Redirects::None).build().unwrap();
        let err = strict.get(&target).send().await.unwrap_err();
        assert!(err.is_connect(), "{err}");
        let lax = client_builder(true, Redirects::None).build().unwrap();
        assert_eq!(
            lax.get(&target).send().await.unwrap().status(),
            200,
            "the proxy variable should apply once the filter is off"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proxy_variables_cannot_bypass_the_filter() {
        // A proxy that answers 200 to anything: a strict client must not use it.
        let proxy = serve_once(OK).await;
        let out = tokio::task::spawn_blocking(move || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::proxy_probe_child", "--nocapture"])
                .env("OBSERVATION_PROXY_PROBE", "metadata.invalid")
                .env("HTTP_PROXY", format!("http://127.0.0.1:{proxy}"))
                .env("http_proxy", format!("http://127.0.0.1:{proxy}"))
                .env_remove("NO_PROXY")
                .env_remove("no_proxy")
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{text}");
        assert!(text.contains("1 passed"), "{text}");
    }
}
