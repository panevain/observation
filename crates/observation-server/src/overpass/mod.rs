//! Overpass API client and `out geom` JSON parsing.
//!
//! The client is deliberately polite: one request in flight at a time, a
//! descriptive `User-Agent`, and a bounded retry. A transient failure (connection
//! error, timeout, 429, 502-504) moves on to the next configured interpreter, or
//! retries a lone interpreter once after a short pause. An interpreter that
//! answers 429 or 504 then rests for a cool-down; while every interpreter rests,
//! new fetches fail fast with a 503.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use observation_core::geo::BBox;
use observation_upstream::{BoundedQueue, QueueError};
use reqwest::StatusCode;

use crate::config::Config;
use crate::error::ApiError;

mod parse;
mod query;

pub use self::parse::{OsmData, ParseError, parse_response, stitch_rings};
pub use self::query::build_query;

/// Largest Overpass response we are willing to buffer.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

// ----- client ----------------------------------------------------------------

/// Why one Overpass attempt failed, when it is worth trying again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transient {
    /// Connection error (including a connect timeout), 502 or 503.
    Failed,
    /// No answer in time, or Overpass reported that the query timed out.
    TimedOut,
    /// 429 or 504: the interpreter is overloaded; it rests for a cool-down.
    Busy,
}

impl Transient {
    fn of(e: &reqwest::Error) -> Self {
        // A connect timeout means the host was never reached, not that it was slow.
        if e.is_timeout() && !e.is_connect() {
            Self::TimedOut
        } else {
            Self::Failed
        }
    }
}

/// How one attempt failed.
#[derive(Debug)]
enum AttemptError {
    Retry(Transient),
    /// Not worth retrying (other statuses, oversized or unusable bodies).
    Fatal(ApiError),
}

/// Rate-limited Overpass client.
#[derive(Debug)]
pub struct OverpassClient {
    client: reqwest::Client,
    /// Interpreters tried in order; transient failures move on to the next one.
    urls: Vec<String>,
    /// One request in flight; the waiters are bounded so distinct boxes can't build a backlog.
    queue: BoundedQueue,
    /// Per interpreter: no requests to it until then, after it answered 429 or 504.
    cooldown_until: Mutex<Vec<Option<Instant>>>,
    retry_wait: Duration,
    cooldown: Duration,
    /// Longest single attempt.
    attempt_timeout: Duration,
    /// Longest whole fetch, queueing included, so that retries stop before the
    /// HTTP request they serve times out.
    budget: Duration,
}

/// Most Overpass requests allowed to queue behind the one in flight.
const MAX_WAITING: usize = 3;

impl OverpassClient {
    /// `client` must already carry the project `User-Agent`.
    pub fn new(client: reqwest::Client, config: &Config) -> Self {
        let mut urls = config.overpass_urls.clone();
        if urls.is_empty() {
            urls.push(crate::config::DEFAULT_OVERPASS_URL.to_owned());
        }
        Self {
            client,
            cooldown_until: Mutex::new(vec![None; urls.len()]),
            urls,
            queue: BoundedQueue::new(1, MAX_WAITING),
            retry_wait: config.overpass_retry_wait,
            cooldown: config.overpass_cooldown,
            attempt_timeout: config.overpass_timeout,
            // Leave a tenth of the request's time to build and send the reply.
            budget: config.request_timeout.mul_f64(0.9),
        }
    }

    /// Fetches and parses buildings and roads for `bbox`.
    pub async fn fetch(&self, bbox: &BBox) -> Result<OsmData, ApiError> {
        self.fetch_since(bbox, Instant::now()).await
    }

    /// As [`Self::fetch`], for a request that began at `started` (time already
    /// spent, such as on a local region, counts against the budget).
    pub async fn fetch_since(&self, bbox: &BBox, started: Instant) -> Result<OsmData, ApiError> {
        let result = self.fetch_inner(bbox, started).await;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(ApiError::RateLimited { .. }) => "rate_limited",
            Err(_) => "error",
        };
        metrics::counter!(crate::metrics::OVERPASS_FETCH, "outcome" => outcome).increment(1);
        result
    }

    async fn fetch_inner(&self, bbox: &BBox, started: Instant) -> Result<OsmData, ApiError> {
        let deadline = started + self.budget;
        self.check_cooldown()?;
        let _permit = self.queue.enter().await.map_err(|e| match e {
            QueueError::Full => ApiError::RateLimited {
                retry_after_secs: 10,
            },
            QueueError::Closed(e) => ApiError::Internal(e.to_string()),
        })?;
        // We may have queued behind a request that got us rate limited.
        self.check_cooldown()?;

        let query = build_query(bbox);
        // Every interpreter once, in order; a lone one gets a second try, since a
        // public instance's load balancer often answers a retry from another backend.
        let plan: Vec<usize> = if self.urls.len() == 1 {
            vec![0, 0]
        } else {
            (0..self.urls.len()).collect()
        };
        // Too little time left for an attempt to be worth starting.
        let min_attempt = self.attempt_timeout / 10;
        let mut last = None;
        for (i, &endpoint) in plan.iter().enumerate() {
            if self.cooling(endpoint).is_some() {
                continue;
            }
            if last.is_some() {
                tokio::time::sleep(self.retry_wait).await;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left < min_attempt {
                last = Some(Transient::TimedOut);
                break;
            }
            match self
                .attempt(endpoint, &query, left.min(self.attempt_timeout))
                .await
            {
                Ok(data) => return Ok(data),
                Err(AttemptError::Fatal(e)) => return Err(e),
                Err(AttemptError::Retry(t)) => {
                    // A busy interpreter rests, unless this fetch is about to retry it.
                    if t == Transient::Busy && !plan[i + 1..].contains(&endpoint) {
                        self.start_cooldown(endpoint);
                    }
                    last = Some(t);
                }
            }
        }
        Err(match last {
            Some(Transient::TimedOut) => ApiError::UpstreamTimeout(
                "the map data provider took too long to answer; try again shortly, or draw a smaller area".into(),
            ),
            Some(Transient::Failed) => {
                ApiError::Upstream("could not reach the map data provider; try again shortly".into())
            }
            // Busy, or every interpreter started cooling down while we queued.
            Some(Transient::Busy) | None => {
                self.check_cooldown()
                    .err()
                    .unwrap_or(ApiError::RateLimited {
                        retry_after_secs: self.cooldown.as_secs().max(1),
                    })
            }
        })
    }

    /// One request to one interpreter, logged once if it fails.
    async fn attempt(
        &self,
        endpoint: usize,
        query: &str,
        timeout: Duration,
    ) -> Result<OsmData, AttemptError> {
        let resp = self
            .client
            .post(&self.urls[endpoint])
            .timeout(timeout)
            .form(&[("data", query)])
            .send()
            .await
            .map_err(|e| {
                let t = Transient::of(&e);
                tracing::warn!(endpoint, error = %e.without_url(), "overpass request failed");
                AttemptError::Retry(t)
            })?;
        let status = resp.status();
        if !status.is_success() {
            if status.is_redirection() {
                // Redirects are not followed (see `observation_upstream`); say where to.
                tracing::warn!(
                    endpoint,
                    %status,
                    location = ?resp.headers().get(reqwest::header::LOCATION),
                    "overpass redirected; point OBS_OVERPASS_URL at the final URL"
                );
                return Err(AttemptError::Retry(Transient::Failed));
            }
            tracing::warn!(endpoint, %status, "overpass returned an error status");
            return Err(match status {
                StatusCode::TOO_MANY_REQUESTS | StatusCode::GATEWAY_TIMEOUT => {
                    AttemptError::Retry(Transient::Busy)
                }
                StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE => {
                    AttemptError::Retry(Transient::Failed)
                }
                _ => AttemptError::Fatal(ApiError::Upstream(format!(
                    "the map data provider answered with an error (HTTP {})",
                    status.as_u16()
                ))),
            });
        }
        let body = crate::read_capped(resp, MAX_RESPONSE_BYTES)
            .await
            .map_err(|e| {
                let t = Transient::of(&e);
                tracing::warn!(endpoint, error = %e.without_url(), "overpass body failed");
                AttemptError::Retry(t)
            })?
            .ok_or_else(|| {
                tracing::warn!(
                    endpoint,
                    max = MAX_RESPONSE_BYTES,
                    "overpass body too large"
                );
                AttemptError::Fatal(ApiError::Upstream(
                    "this area has more map data than the server accepts; draw a smaller area"
                        .into(),
                ))
            })?;
        parse_response(&body).map_err(|e| {
            tracing::warn!(endpoint, error = %e, "overpass response unusable");
            match e {
                // The query ran out of time on a loaded server: another may manage.
                ParseError::Remark(r) if r.contains("timed out") => {
                    AttemptError::Retry(Transient::TimedOut)
                }
                // Out of memory and the like: the same query fails the same way.
                ParseError::Remark(_) => AttemptError::Fatal(ApiError::Upstream(
                    "the map data provider could not process this area; try a smaller area".into(),
                )),
                ParseError::Json(_) => AttemptError::Fatal(ApiError::Upstream(
                    "the map data provider sent a reply the server could not read; try again shortly".into(),
                )),
            }
        })
    }

    /// Time left on `endpoint`'s cool-down, if any.
    fn cooling(&self, endpoint: usize) -> Option<Duration> {
        let until = self
            .cooldown_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[endpoint];
        until
            .map(|t| t.saturating_duration_since(Instant::now()))
            .filter(|left| !left.is_zero())
    }

    /// Fails fast while every interpreter is cooling down.
    fn check_cooldown(&self) -> Result<(), ApiError> {
        let left = (0..self.urls.len()).map(|e| self.cooling(e));
        match left.collect::<Option<Vec<Duration>>>() {
            Some(left) => Err(ApiError::RateLimited {
                retry_after_secs: left
                    .iter()
                    .min()
                    .map_or(1.0, Duration::as_secs_f64)
                    .ceil()
                    .max(1.0) as u64,
            }),
            None => Ok(()),
        }
    }

    fn start_cooldown(&self, endpoint: usize) {
        self.cooldown_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[endpoint] =
            Some(Instant::now() + self.cooldown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_interpreter_list_falls_back_to_the_default() {
        let mut config = Config::for_tests("cache", "dist", "http://127.0.0.1:9/", "http://t/");
        config.overpass_urls.clear();
        let client = OverpassClient::new(reqwest::Client::new(), &config);
        assert_eq!(
            client.urls,
            vec![crate::config::DEFAULT_OVERPASS_URL.to_owned()]
        );
        assert_eq!(client.cooldown_until.lock().unwrap().len(), 1);
    }
}
