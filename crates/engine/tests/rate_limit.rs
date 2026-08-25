//! Story 1.7: 429 backoff inside the single request channel.
//!
//! Real waits are kept tiny: tests inject a `RetryPolicy` with millisecond
//! backoff, and the only real `Retry-After` wait is a single second.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::time::{Duration, Instant, SystemTime};

use engine::{BodyMode, Client, EngineError, OpSpec, RetryPolicy, Throttle, ValidationMode};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn list_op() -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("things.info"),
        path: Cow::Borrowed("/api/things.info"),
        summary: Cow::Borrowed("Retrieve a thing"),
        content_type: Cow::Borrowed("application/json"),
        body_mode: BodyMode::KeyValue,
        params: Cow::Borrowed(&[]),
    }
}

/// A throttle wide enough not to pace these tests (pacing is covered by
/// its own tests; sharing the process-wide handle would couple them).
fn test_throttle() -> Throttle {
    Throttle::new(10_000.0, 10_000.0)
}

/// A retry policy with waits small enough for fast tests.
fn fast_policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 3,
        backoff_base: Duration::from_millis(10),
        max_wait: Duration::from_secs(2),
    }
}

/// Mount a mock that answers 429 `n` times, then let `then` answer.
async fn mount_429_then_ok(server: &MockServer, n: u64, retry_after: Option<&str>) {
    let mut template = ResponseTemplate::new(429);
    if let Some(value) = retry_after {
        template = template.insert_header("retry-after", value);
    }
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(template)
        .up_to_n_times(n)
        .expect(n)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_429_after_retry_after_seconds_and_waits() {
    let server = MockServer::start().await;
    mount_429_then_ok(&server, 1, Some("1")).await;

    let base_url = server.uri();
    let (result, elapsed) = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?
            .with_retry_policy(fast_policy())
            .with_throttle(test_throttle());
        let started = Instant::now();
        let value = client.execute(&list_op(), &[], ValidationMode::Strict);
        Ok::<_, EngineError>((value, started.elapsed()))
    })
    .await
    .unwrap()
    .unwrap();

    assert_eq!(result.unwrap()["data"]["ok"], true);
    assert!(
        elapsed >= Duration::from_millis(900),
        "did not honor Retry-After: waited only {elapsed:?}"
    );
    // Generous upper bound: this only guards against waiting a multiple of
    // Retry-After, so it stays meaningful without being load-flaky.
    assert!(
        elapsed < Duration::from_secs(20),
        "waited far longer than Retry-After: {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_429_with_http_date_retry_after() {
    // An HTTP-date in the past must parse and clamp to a zero wait.
    let past = SystemTime::now() - Duration::from_secs(30);
    let server = MockServer::start().await;
    mount_429_then_ok(&server, 1, Some(&httpdate::fmt_http_date(past))).await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?
            .with_retry_policy(fast_policy())
            .with_throttle(test_throttle());
        client.execute(&list_op(), &[], ValidationMode::Strict)
    })
    .await
    .unwrap();

    assert_eq!(result.unwrap()["data"]["ok"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_429_without_header_using_backoff() {
    let server = MockServer::start().await;
    mount_429_then_ok(&server, 2, None).await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?
            .with_retry_policy(fast_policy())
            .with_throttle(test_throttle());
        client.execute(&list_op(), &[], ValidationMode::Strict)
    })
    .await
    .unwrap();

    assert_eq!(result.unwrap()["data"]["ok"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn exhausted_retries_yield_dedicated_error() {
    let server = MockServer::start().await;
    // Always 429: retries must run out.
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(429))
        // 1 initial attempt + max_retries retries.
        .expect(4)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let origin = base_url.clone();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?
            .with_retry_policy(fast_policy())
            .with_throttle(test_throttle());
        client.execute(&list_op(), &[], ValidationMode::Strict)
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::RateLimited { retries, .. }) => {
            assert_eq!(retries, 3);
        }
        other => panic!("expected RateLimited error, got {other:?}"),
    }
    // The Display must be readable and credential-free (origin only).
    let error = format!(
        "{}",
        EngineError::RateLimited {
            origin: origin.clone(),
            retries: 3,
        }
    );
    assert!(error.contains("429"), "unreadable error: {error}");
    assert!(error.contains(&origin), "origin missing: {error}");
}

#[test]
fn retry_after_parses_seconds_and_http_date() {
    let now = SystemTime::now();
    assert_eq!(
        engine::retry::parse_retry_after("7", now),
        Some(Duration::from_secs(7))
    );
    let date = httpdate::fmt_http_date(now + Duration::from_secs(10));
    let parsed = engine::retry::parse_retry_after(&date, now).unwrap();
    assert!(
        parsed >= Duration::from_secs(9) && parsed <= Duration::from_secs(11),
        "bad date parse: {parsed:?}"
    );
    // Past dates clamp to zero; garbage is None (caller falls back to backoff).
    let past = httpdate::fmt_http_date(now - Duration::from_secs(10));
    assert_eq!(
        engine::retry::parse_retry_after(&past, now),
        Some(Duration::ZERO)
    );
    assert_eq!(engine::retry::parse_retry_after("soon", now), None);
}

#[test]
fn backoff_grows_exponentially_with_bounded_jitter() {
    let policy = fast_policy();
    for attempt in 0..3 {
        let base = policy.backoff_base * 2u32.pow(attempt);
        let wait = policy.backoff_delay(attempt);
        assert!(wait >= base, "attempt {attempt}: {wait:?} < {base:?}");
        assert!(
            wait <= base + base / 2,
            "attempt {attempt}: jitter out of range: {wait:?}"
        );
    }
}

#[test]
fn backoff_is_capped_at_max_wait() {
    let policy = RetryPolicy {
        max_retries: 30,
        backoff_base: Duration::from_secs(1),
        max_wait: Duration::from_secs(3),
    };
    assert_eq!(policy.backoff_delay(20), Duration::from_secs(3));
}

#[test]
fn huge_retry_after_header_is_capped_by_the_policy() {
    // The wait actually used by the request channel (not just the backoff
    // fallback) must respect max_wait, in both header forms.
    let policy = fast_policy();
    assert_eq!(
        policy.retry_wait(Some("999999999999"), 0),
        policy.max_wait,
        "delta-seconds header not capped"
    );
    let far_future = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(86_400));
    assert_eq!(
        policy.retry_wait(Some(&far_future), 0),
        policy.max_wait,
        "HTTP-date header not capped"
    );
    // An unusable header falls back to backoff, which is far shorter.
    assert!(policy.retry_wait(Some("soon"), 0) < policy.max_wait);
}

#[test]
fn token_bucket_paces_after_burst() {
    use engine::TokenBucket;

    let start = Instant::now();
    let mut bucket = TokenBucket::started_at(10.0, 2.0, start);
    // Burst capacity: first two are free.
    assert_eq!(bucket.acquire_delay_at(start), Duration::ZERO);
    assert_eq!(bucket.acquire_delay_at(start), Duration::ZERO);
    // Third must wait ~1/rate.
    let delay = bucket.acquire_delay_at(start);
    assert!(
        delay > Duration::from_millis(50) && delay <= Duration::from_millis(150),
        "unexpected pacing delay: {delay:?}"
    );
    // After enough time passes, tokens refill (capped at burst).
    let later = start + Duration::from_secs(10);
    assert_eq!(bucket.acquire_delay_at(later), Duration::ZERO);
    assert_eq!(bucket.acquire_delay_at(later), Duration::ZERO);
    assert!(bucket.acquire_delay_at(later) > Duration::ZERO);
}

#[test]
fn token_bucket_never_rewinds_its_refill_clock() {
    use engine::TokenBucket;

    // Finding 7: two threads may read the clock and then enter the lock in
    // the opposite order. Accounting a stale `now` must not roll the
    // refill timestamp back, or tokens already granted get re-credited and
    // the bucket exceeds its configured rate.
    let start = Instant::now();
    let later = start + Duration::from_secs(1);
    // 1 token per second, burst 1: every grant must be a second apart.
    let mut bucket = TokenBucket::started_at(1.0, 1.0, start);

    assert_eq!(
        bucket.acquire_delay_at(start),
        Duration::ZERO,
        "burst token"
    );
    assert_eq!(
        bucket.acquire_delay_at(later),
        Duration::ZERO,
        "one second of refill"
    );
    // A stale reading enters the lock last; it gets no free token...
    assert_eq!(
        bucket.acquire_delay_at(start),
        Duration::from_secs(1),
        "stale acquire was granted a token that was not refilled yet"
    );
    // ...and must not have rewound the clock. Exact value matters: if
    // `refreshed` had gone back to `start`, this acquire would re-credit
    // that same second and ask for only 1s instead of 2s.
    assert_eq!(
        bucket.acquire_delay_at(later),
        Duration::from_secs(2),
        "refill clock rewound: one second of tokens was credited twice"
    );
}

#[test]
fn clients_sharing_a_throttle_cannot_exceed_the_rate() {
    // Finding 7: the throttle is an injectable shared handle, and several
    // clients sharing it must be paced against one another.
    use std::thread;

    const RATE: f64 = 20.0;
    const BURST: f64 = 2.0;
    const THREADS: usize = 4;
    const PER_THREAD: usize = 5;

    let throttle = Throttle::new(RATE, BURST);
    let started = Instant::now();
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let shared = throttle.clone();
            thread::spawn(move || {
                // One Client per thread, all sharing one budget.
                let client = Client::new("https://example.invalid", "token")
                    .unwrap()
                    .with_throttle(shared.clone());
                let mut total = Duration::ZERO;
                for _ in 0..PER_THREAD {
                    // Exercise the same handle the client holds; no
                    // request is sent, only the pacing decision.
                    total += shared.acquire_delay();
                }
                drop(client);
                total
            })
        })
        .collect();
    let waited: Duration = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .sum();

    // 20 acquisitions, 2 free: the rest must be spread over >= 18/20 s.
    let total = (THREADS * PER_THREAD) as f64;
    let expected = Duration::from_secs_f64((total - BURST) / RATE);
    let accounted = started.elapsed() + waited;
    assert!(
        accounted >= expected,
        "shared throttle exceeded its rate: accounted {accounted:?} < {expected:?}"
    );
}
