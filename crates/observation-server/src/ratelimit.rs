//! Per-client token buckets for requests that go upstream, and the client
//! address they are keyed by.
//!
//! Only cache misses spend tokens (see [`crate::bundles`] and [`crate::tiles`]),
//! so a visitor panning over areas that are already cached is never limited.
//! Clients are told apart by their IP address; IPv6 addresses are grouped by
//! their /64, which one host usually holds whole. Behind a reverse proxy listed
//! in `OBS_TRUSTED_PROXIES`, the address comes from `X-Forwarded-For` instead.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;

use crate::AppState;
use crate::error::ApiError;

/// Most clients tracked per limiter. Past it, idle clients are forgotten, and
/// if every one is still active, new clients share a single bucket.
pub const MAX_CLIENTS: usize = 100_000;
/// Least time between two sweeps of idle clients when the table is full.
const SWEEP_EVERY: Duration = Duration::from_secs(1);

/// A token-bucket rate: `burst` requests at once, refilled at `per_minute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub per_minute: u32,
    pub burst: u32,
}

/// An address block in `OBS_TRUSTED_PROXIES`, such as `10.0.0.0/8` or `::1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parses `addr` or `addr/prefix`. Host bits below the prefix are ignored.
    pub fn parse(s: &str) -> Option<Self> {
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = addr.trim().parse().ok()?;
        let max: u8 = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            // `u8::from_str` would accept a leading `+`.
            Some(p) if p.trim().bytes().all(|b| b.is_ascii_digit()) => p.trim().parse().ok()?,
            Some(_) => return None,
            None => max,
        };
        (prefix <= max).then_some(Self {
            addr: addr.to_canonical(),
            // A v4-mapped v6 block is a v4 block.
            prefix: if addr.is_ipv6() && addr.to_canonical().is_ipv4() {
                prefix.checked_sub(96)?
            } else {
                prefix
            },
        })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

/// Who a request is charged to: a normalised client address, or `None` when
/// the connection's peer is unknown (all such requests share one bucket).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientKey(Option<IpAddr>);

impl ClientKey {
    /// The key for `ip`: IPv4 (also when v4-mapped) as is, IPv6 cut to its /64.
    pub fn from_ip(ip: IpAddr) -> Self {
        Self(Some(match ip.to_canonical() {
            IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !(u64::MAX as u128))),
            v4 => v4,
        }))
    }

    /// The client behind `peer`. When `peer` is a trusted proxy, `X-Forwarded-For`
    /// is read from the right (the end the proxies append to), and the first
    /// address that is not itself a trusted proxy is the client. An entry that
    /// is not an address stops the walk at the last good hop, so a garbled
    /// header is charged to the proxy that passed it on rather than ignored.
    pub fn resolve(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[Cidr]) -> Self {
        let Some(mut client) = peer else {
            return Self(None);
        };
        let is_trusted = |ip: IpAddr| trusted.iter().any(|c| c.contains(ip));
        if is_trusted(client) {
            let hops = headers
                .get_all("x-forwarded-for")
                .iter()
                .rev()
                .flat_map(|v| v.to_str().unwrap_or("?").rsplit(','));
            for hop in hops {
                let Some(ip) = parse_hop(hop) else { break };
                client = ip;
                if !is_trusted(ip) {
                    break;
                }
            }
        }
        Self::from_ip(client)
    }
}

/// One `X-Forwarded-For` entry: a bare address, or one in brackets or with a
/// port (`[2001:db8::1]`, `1.2.3.4:5678`, `[2001:db8::1]:5678`), as some proxies write it.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    let hop = hop.trim();
    hop.parse()
        .ok()
        .or_else(|| hop.parse::<std::net::SocketAddr>().ok().map(|s| s.ip()))
        .or_else(|| {
            let inner = hop.strip_prefix('[')?.strip_suffix(']')?;
            inner.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
        })
}

/// Extracts the [`ClientKey`] of a request: the socket peer, or the forwarded
/// client when the peer is in `OBS_TRUSTED_PROXIES`.
impl FromRequestParts<AppState> for ClientKey {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip());
        if peer.is_none() {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                tracing::warn!(
                    "requests carry no peer address, so every client shares one rate limit; \
                     serve the app with `into_make_service_with_connect_info::<SocketAddr>()`"
                );
            });
        }
        Ok(Self::resolve(
            peer,
            &parts.headers,
            &state.config().trusted_proxies,
        ))
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

#[derive(Debug)]
struct Buckets {
    clients: HashMap<ClientKey, Bucket>,
    /// Shared by new clients while the table is full of active ones.
    overflow: Option<Bucket>,
    last_sweep: Option<Instant>,
}

/// A keyed token-bucket limiter. A `per_minute` of zero turns it off.
#[derive(Debug)]
pub struct RateLimiter {
    /// `route` label of [`crate::metrics::CLIENT_LIMITED`].
    route: &'static str,
    rate: Rate,
    max_clients: usize,
    buckets: Mutex<Buckets>,
}

impl RateLimiter {
    pub fn new(route: &'static str, rate: Rate) -> Self {
        Self::with_capacity(route, rate, MAX_CLIENTS)
    }

    fn with_capacity(route: &'static str, rate: Rate, max_clients: usize) -> Self {
        Self {
            route,
            rate,
            max_clients,
            buckets: Mutex::new(Buckets {
                clients: HashMap::new(),
                overflow: None,
                last_sweep: None,
            }),
        }
    }

    /// Takes a token for `key`, or refuses with a 429 saying when one is free.
    pub fn check(&self, key: ClientKey) -> Result<(), ApiError> {
        self.check_at(key, Instant::now()).map_err(|wait| {
            metrics::counter!(crate::metrics::CLIENT_LIMITED, "route" => self.route).increment(1);
            ApiError::TooManyRequests {
                retry_after_secs: wait.as_secs_f64().ceil().max(1.0) as u64,
            }
        })
    }

    fn check_at(&self, key: ClientKey, now: Instant) -> Result<(), Duration> {
        if self.rate.per_minute == 0 {
            return Ok(());
        }
        let per_sec = f64::from(self.rate.per_minute) / 60.0;
        let burst = f64::from(self.rate.burst);
        let refilled = |b: &Bucket| {
            (b.tokens + now.saturating_duration_since(b.at).as_secs_f64() * per_sec).min(burst)
        };
        let mut guard = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        let state = &mut *guard;
        if !state.clients.contains_key(&key) && state.clients.len() >= self.max_clients {
            let due = state
                .last_sweep
                .is_none_or(|t| now.saturating_duration_since(t) >= SWEEP_EVERY);
            if due {
                state.last_sweep = Some(now);
                // A full bucket is the same as no bucket.
                state.clients.retain(|_, b| refilled(b) < burst);
            }
        }
        let fresh = Bucket {
            tokens: burst,
            at: now,
        };
        let bucket = if state.clients.len() < self.max_clients || state.clients.contains_key(&key) {
            state.clients.entry(key).or_insert(fresh)
        } else {
            state.overflow.get_or_insert(fresh)
        };
        let tokens = refilled(bucket);
        // `now` is read before the lock, so a racing caller may have stored a later
        // time; going back would count the time in between twice.
        bucket.at = bucket.at.max(now);
        if tokens >= 1.0 {
            bucket.tokens = tokens - 1.0;
            Ok(())
        } else {
            bucket.tokens = tokens;
            Err(Duration::from_secs_f64((1.0 - tokens) / per_sec))
        }
    }

    /// Gives back the token [`Self::check`] took for `key`, for work that turned
    /// out not to need the upstream after all.
    pub fn refund(&self, key: ClientKey) {
        if self.rate.per_minute == 0 {
            return;
        }
        let burst = f64::from(self.rate.burst);
        let mut guard = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        let state = &mut *guard;
        // A client charged to the overflow bucket has no bucket of its own.
        if let Some(b) = state.clients.get_mut(&key).or(state.overflow.as_mut()) {
            b.tokens = (b.tokens + 1.0).min(burst);
        }
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clients
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn key(s: &str) -> ClientKey {
        ClientKey::from_ip(ip(s))
    }

    #[test]
    fn cidrs_parse_and_match() {
        let c = Cidr::parse("10.1.0.0/16").unwrap();
        assert!(c.contains(ip("10.1.200.3")));
        assert!(!c.contains(ip("10.2.0.1")));
        assert!(
            c.contains(ip("::ffff:10.1.0.9")),
            "v4-mapped peers match v4 blocks"
        );
        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
        assert!(!Cidr::parse("0.0.0.0/0").unwrap().contains(ip("::1")));
        let single = Cidr::parse(" 192.0.2.7 ").unwrap();
        assert!(single.contains(ip("192.0.2.7")) && !single.contains(ip("192.0.2.8")));
        let v6 = Cidr::parse("2001:db8::/32").unwrap();
        assert!(v6.contains(ip("2001:db8:ffff::1")));
        assert!(!v6.contains(ip("2001:db9::1")));
        assert!(Cidr::parse("::/0").unwrap().contains(ip("2001:db8::1")));
        let mapped = Cidr::parse("::ffff:10.0.0.0/104").unwrap();
        assert!(mapped.contains(ip("10.9.9.9")));
        for bad in [
            "",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/",
            "10.0.0.0/+8",
            "10.0.0/8",
            "host/8",
            "::ffff:10.0.0.0/95",
        ] {
            assert_eq!(Cidr::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn keys_group_ipv6_by_64_and_unmap_ipv4() {
        assert_eq!(key("2001:db8:1:2:aaaa::1"), key("2001:db8:1:2:bbbb::2"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("::ffff:192.0.2.1"), key("192.0.2.1"));
        assert_ne!(key("192.0.2.1"), key("192.0.2.2"));
    }

    fn xff(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append("x-forwarded-for", HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn forwarded_addresses_are_used_only_from_trusted_proxies() {
        let trusted = [Cidr::parse("10.0.0.0/8").unwrap()];
        let resolve =
            |peer: &str, h: &[&str]| ClientKey::resolve(Some(ip(peer)), &xff(h), &trusted);
        // An untrusted peer cannot claim another address.
        assert_eq!(resolve("198.51.100.1", &["192.0.2.1"]), key("198.51.100.1"));
        // A trusted proxy's view of its client.
        assert_eq!(resolve("10.0.0.2", &["192.0.2.1"]), key("192.0.2.1"));
        // Entries a client wrote itself sit to the left of what the proxy appended.
        assert_eq!(
            resolve("10.0.0.2", &["203.0.113.9, 192.0.2.1"]),
            key("192.0.2.1")
        );
        // Chains of trusted proxies are skipped, across repeated headers too.
        assert_eq!(
            resolve("10.0.0.2", &["203.0.113.9, 192.0.2.1", "10.0.0.3"]),
            key("192.0.2.1")
        );
        assert_eq!(
            resolve("10.0.0.2", &["[2001:db8::1]:443"]),
            key("2001:db8::1")
        );
        assert_eq!(resolve("10.0.0.2", &["[2001:db8::1]"]), key("2001:db8::1"));
        assert_eq!(resolve("10.0.0.2", &["192.0.2.1:5678"]), key("192.0.2.1"));
        // No header, or nothing but proxies: the nearest address is the client.
        assert_eq!(resolve("10.0.0.2", &[]), key("10.0.0.2"));
        assert_eq!(resolve("10.0.0.2", &["10.0.0.1"]), key("10.0.0.1"));
        // Garbage stops the walk at the last proxy rather than being skipped.
        assert_eq!(
            resolve("10.0.0.2", &["192.0.2.1, unknown, 10.0.0.3"]),
            key("10.0.0.3")
        );
        assert_eq!(
            ClientKey::resolve(None, &xff(&["192.0.2.1"]), &trusted),
            ClientKey(None)
        );
    }

    const RATE: Rate = Rate {
        per_minute: 60,
        burst: 3,
    };

    #[test]
    fn buckets_allow_a_burst_then_refill() {
        let l = RateLimiter::new("test", RATE);
        let t0 = Instant::now();
        let a = key("192.0.2.1");
        for _ in 0..3 {
            assert_eq!(l.check_at(a, t0), Ok(()));
        }
        let wait = l.check_at(a, t0).unwrap_err();
        assert!((wait.as_secs_f64() - 1.0).abs() < 1e-6, "{wait:?}");
        // Another client is unaffected.
        assert_eq!(l.check_at(key("192.0.2.2"), t0), Ok(()));
        // One token per second at 60 a minute.
        let half = l.check_at(a, t0 + Duration::from_millis(500)).unwrap_err();
        assert!((half.as_secs_f64() - 0.5).abs() < 1e-6, "{half:?}");
        assert_eq!(l.check_at(a, t0 + Duration::from_secs(1)), Ok(()));
        assert!(l.check_at(a, t0 + Duration::from_secs(1)).is_err());
        // Refills stop at the burst.
        let later = t0 + Duration::from_secs(3600);
        for _ in 0..3 {
            assert_eq!(l.check_at(a, later), Ok(()));
        }
        assert!(l.check_at(a, later).is_err());
    }

    #[test]
    fn refusals_round_the_wait_up_to_whole_seconds() {
        let l = RateLimiter::new("test", RATE);
        for _ in 0..3 {
            l.check(key("192.0.2.9")).unwrap();
        }
        assert_eq!(
            l.check(key("192.0.2.9")),
            Err(ApiError::TooManyRequests {
                retry_after_secs: 1
            })
        );
    }

    #[test]
    fn refunds_give_a_token_back_up_to_the_burst() {
        let l = RateLimiter::new("test", RATE);
        let t0 = Instant::now();
        let a = key("192.0.2.1");
        for _ in 0..3 {
            l.check_at(a, t0).unwrap();
        }
        l.refund(a);
        assert_eq!(l.check_at(a, t0), Ok(()));
        assert!(l.check_at(a, t0).is_err());
        for _ in 0..10 {
            l.refund(a);
        }
        for _ in 0..3 {
            l.check_at(a, t0).unwrap();
        }
        assert!(l.check_at(a, t0).is_err(), "refunds stop at the burst");
        // A racing caller that read the clock earlier does not move the bucket back.
        assert!(l.check_at(a, t0 - Duration::from_millis(900)).is_err());
        assert!(l.check_at(a, t0 + Duration::from_millis(500)).is_err());
    }

    #[test]
    fn a_zero_rate_is_off() {
        let l = RateLimiter::new(
            "test",
            Rate {
                per_minute: 0,
                burst: 0,
            },
        );
        let t0 = Instant::now();
        for _ in 0..1000 {
            assert_eq!(l.check_at(key("192.0.2.1"), t0), Ok(()));
        }
        assert_eq!(l.tracked(), 0);
    }

    #[test]
    fn a_full_table_forgets_idle_clients_then_shares_one_bucket() {
        let l = RateLimiter::with_capacity("test", RATE, 2);
        let t0 = Instant::now();
        l.check_at(key("192.0.2.1"), t0).unwrap();
        l.check_at(key("192.0.2.2"), t0).unwrap();
        // Both are active, so newcomers share the overflow bucket.
        for _ in 0..3 {
            l.check_at(key("192.0.2.3"), t0).unwrap();
        }
        assert!(l.check_at(key("192.0.2.4"), t0).is_err());
        assert_eq!(l.tracked(), 2);
        // Known clients keep their own buckets meanwhile.
        assert_eq!(l.check_at(key("192.0.2.1"), t0), Ok(()));
        // Once refilled they count as idle and make room.
        let later = t0 + Duration::from_secs(10);
        assert_eq!(l.check_at(key("192.0.2.5"), later), Ok(()));
        assert_eq!(l.tracked(), 1);
    }
}
