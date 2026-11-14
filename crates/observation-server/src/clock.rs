//! The wall clock, as a port so tests can set the time.
//!
//! Only "what time is it now" goes through [`Clock`]: freshness still comes
//! from file modification times and upstream headers, compared with `now`.
//! Elapsed time (retry pauses, cool-downs, rate-limit refill) uses
//! `tokio::time::Instant` instead, which paused-time tests drive.

use std::fmt::Debug;
use std::time::{SystemTime, UNIX_EPOCH};

/// The current wall-clock time.
pub trait Clock: Send + Sync + Debug {
    fn now(&self) -> SystemTime;
}

/// The system's clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Whole seconds since the Unix epoch; 0 for a time before it.
pub fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A clock that stands still until it is set or advanced.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug)]
pub struct ManualClock(std::sync::Mutex<SystemTime>);

#[cfg(any(test, feature = "testing"))]
impl ManualClock {
    pub fn new(at: SystemTime) -> Self {
        Self(std::sync::Mutex::new(at))
    }

    /// A clock at `secs` after the Unix epoch.
    pub fn at_unix(secs: u64) -> Self {
        Self::new(UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }

    pub fn set(&self, at: SystemTime) {
        *self.lock() = at;
    }

    pub fn advance(&self, by: std::time::Duration) {
        *self.lock() += by;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SystemTime> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(any(test, feature = "testing"))]
impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        *self.lock()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let c = ManualClock::at_unix(100);
        assert_eq!(unix_secs(c.now()), 100);
        c.advance(Duration::from_secs(5));
        assert_eq!(unix_secs(c.now()), 105);
        c.set(UNIX_EPOCH);
        assert_eq!(unix_secs(c.now()), 0);
        assert_eq!(unix_secs(UNIX_EPOCH - Duration::from_secs(1)), 0);
    }
}
