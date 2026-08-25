//! Retry policy for HTTP 429 rate-limit responses.
//!
//! The single request channel ([`crate::Client::execute`]) waits per the
//! server's `Retry-After` header when present (both delta-seconds and
//! HTTP-date forms), and falls back to exponential backoff with jitter
//! otherwise. All waits are bounded by [`RetryPolicy::max_wait`] and the
//! retry count by [`RetryPolicy::max_retries`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default number of retries after the initial attempt.
pub const DEFAULT_MAX_RETRIES: u32 = 5;
/// Default base delay for exponential backoff (used when the server sends
/// no usable `Retry-After` header).
pub const DEFAULT_BACKOFF_BASE: Duration = Duration::from_millis(500);
/// Default upper bound on any single wait (backoff or `Retry-After`).
pub const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(60);
/// Jitter keeps concurrent clients from retrying in lockstep: each backoff
/// is stretched by a pseudo-random factor in `[1.0, 1.0 + this)`.
const JITTER_MAX_FRACTION: f64 = 0.5;
/// Resolution of the jitter factor.
const JITTER_STEPS: u32 = 1_000;

/// How the request channel reacts to HTTP 429 responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries allowed after the initial attempt before giving up.
    pub max_retries: u32,
    /// Base delay of the exponential backoff (doubles per retry).
    pub backoff_base: Duration,
    /// Cap applied to every computed wait, including server-requested ones.
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            backoff_base: DEFAULT_BACKOFF_BASE,
            max_wait: DEFAULT_MAX_WAIT,
        }
    }
}

impl RetryPolicy {
    /// The wait before retry number `attempt` (0-based): the parsed
    /// `Retry-After` value when usable, exponential backoff otherwise.
    pub fn retry_wait(&self, retry_after: Option<&str>, attempt: u32) -> Duration {
        retry_after
            .and_then(|value| parse_retry_after(value, SystemTime::now()))
            .map_or_else(
                || self.backoff_delay(attempt),
                |wait| wait.min(self.max_wait),
            )
    }

    /// Exponential backoff with jitter for retry number `attempt`
    /// (0-based), capped at [`Self::max_wait`].
    pub fn backoff_delay(&self, attempt: u32) -> Duration {
        let doubled = self
            .backoff_base
            .saturating_mul(2u32.saturating_pow(attempt));
        doubled
            .mul_f64(1.0 + JITTER_MAX_FRACTION * jitter_unit())
            .min(self.max_wait)
    }
}

/// Parse a `Retry-After` header value against the given `now`.
///
/// Supports both RFC 9110 forms: delta-seconds (`"7"`) and HTTP-date
/// (`"Fri, 31 Dec 1999 23:59:59 GMT"`). Dates in the past clamp to zero.
/// Returns `None` for anything unparseable so callers fall back to backoff.
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let trimmed = value.trim();
    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(trimmed).ok()?;
    Some(date.duration_since(now).unwrap_or(Duration::ZERO))
}

/// A cheap pseudo-random value in `[0.0, 1.0)` derived from the clock.
///
/// Jitter only needs to de-synchronize independent processes; it has no
/// security role, so this avoids pulling in an RNG dependency.
fn jitter_unit() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos % JITTER_STEPS) / f64::from(JITTER_STEPS)
}
