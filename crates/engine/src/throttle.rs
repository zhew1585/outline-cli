//! Token-bucket request throttle.
//!
//! A [`Throttle`] is a shared handle (`Arc`) that callers own and hand to
//! every [`crate::Client`] that should share one budget - there is no
//! mutable global. [`Throttle::process_wide`] returns the conventional
//! one-per-process handle for callers that want exactly that, and tests
//! can inject an isolated throttle instead.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// Sustained request rate (requests per second) of the default throttle.
pub const DEFAULT_RATE_PER_SEC: f64 = 10.0;
/// Burst capacity of the default throttle (requests served immediately).
pub const DEFAULT_BURST: f64 = 10.0;

/// A standard token bucket: tokens refill continuously at `rate_per_sec`
/// up to `burst`. Acquiring may overdraw the bucket; the returned delay is
/// how long the caller must wait to stay within the rate.
///
/// The clock is a parameter so the pacing arithmetic is testable without
/// sleeping; [`Throttle`] reads the real clock while holding the lock.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenBucket {
    rate_per_sec: f64,
    burst: f64,
    tokens: f64,
    refreshed: Instant,
}

impl TokenBucket {
    /// A full bucket refilling at `rate_per_sec` with capacity `burst`.
    ///
    /// Non-positive inputs are clamped to a tiny positive rate/capacity so
    /// the bucket can never divide by zero or deadlock.
    pub fn new(rate_per_sec: f64, burst: f64) -> Self {
        Self::started_at(rate_per_sec, burst, Instant::now())
    }

    /// [`Self::new`] with an explicit start instant (for tests).
    pub fn started_at(rate_per_sec: f64, burst: f64, now: Instant) -> Self {
        let rate = if rate_per_sec > 0.0 {
            rate_per_sec
        } else {
            f64::MIN_POSITIVE
        };
        let capacity = if burst > 0.0 { burst } else { 1.0 };
        Self {
            rate_per_sec: rate,
            burst: capacity,
            tokens: capacity,
            refreshed: now,
        }
    }

    /// Take one token as of `now`; the returned duration is how long the
    /// caller must wait before proceeding (zero when within the rate).
    ///
    /// `refreshed` only ever moves forward: two threads may read the clock
    /// and then enter in the opposite order, and rewinding the refill
    /// timestamp would let the earlier reading re-credit tokens that were
    /// already granted, letting the bucket exceed its configured rate.
    pub fn acquire_delay_at(&mut self, now: Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.refreshed);
        self.tokens = (self.tokens + elapsed.as_secs_f64() * self.rate_per_sec).min(self.burst);
        self.refreshed = self.refreshed.max(now);
        self.tokens -= 1.0;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-self.tokens / self.rate_per_sec)
        }
    }
}

/// A shared throttle handle: clone it to give several clients one budget.
#[derive(Debug, Clone)]
pub struct Throttle {
    bucket: Arc<Mutex<TokenBucket>>,
}

impl Throttle {
    /// A fresh, independent throttle.
    pub fn new(rate_per_sec: f64, burst: f64) -> Self {
        Self {
            bucket: Arc::new(Mutex::new(TokenBucket::new(rate_per_sec, burst))),
        }
    }

    /// The conventional one-per-process throttle at the default rate.
    ///
    /// The `OnceLock` holds an immutable `Arc` handle; all mutable state
    /// lives inside it, so this is shared-via-`Arc`, not a mutable global.
    /// Clients that must not share this budget are given their own handle.
    pub fn process_wide() -> Self {
        static SHARED: OnceLock<Throttle> = OnceLock::new();
        SHARED
            .get_or_init(|| Self::new(DEFAULT_RATE_PER_SEC, DEFAULT_BURST))
            .clone()
    }

    /// Draw one token, returning how long the caller must wait.
    ///
    /// The clock is read while the lock is held, so concurrent callers are
    /// serialized in the same order they are accounted for.
    pub fn acquire_delay(&self) -> Duration {
        self.bucket
            .lock()
            // A poisoned lock only means another thread panicked
            // mid-acquire; the bucket is still a valid rate approximation.
            .unwrap_or_else(PoisonError::into_inner)
            .acquire_delay_at(Instant::now())
    }
}

impl Default for Throttle {
    fn default() -> Self {
        Self::process_wide()
    }
}
