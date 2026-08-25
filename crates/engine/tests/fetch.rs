//! The plain-document fetch channel (`engine::fetch`).
//!
//! Every case runs against a local wiremock server: no test in this file
//! touches the real network. Real waits are kept tiny by injecting a
//! `RetryPolicy` with millisecond backoff, exactly as the RPC channel's
//! rate-limit tests do - the two channels must behave the same way on a
//! 429, and these tests are what says so.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use engine::fetch::{DocumentFetch, FetchError, MAX_DOCUMENT_BYTES};
use engine::{RetryPolicy, Throttle, TransportKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A throttle wide enough not to pace the tests that are not about pacing.
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

/// Run a blocking fetch off the async test runtime, with fast policies.
async fn fetch_text(url: String, max_bytes: u64) -> Result<String, FetchError> {
    fetch_with(url, max_bytes, fast_policy(), test_throttle()).await
}

async fn fetch_with(
    url: String,
    max_bytes: u64,
    retry: RetryPolicy,
    throttle: Throttle,
) -> Result<String, FetchError> {
    tokio::task::spawn_blocking(move || {
        DocumentFetch::new(Duration::from_secs(5))?
            .with_max_bytes(max_bytes)
            .with_retry_policy(retry)
            .with_throttle(throttle)
            .get_text(&url)
    })
    .await
    .unwrap()
}

async fn serve(body: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/spec.json"))
        .respond_with(body)
        .mount(&server)
        .await;
    server
}

/// Mount a mock that answers 429 `n` times, then a 200.
async fn mount_429_then_ok(server: &MockServer, n: u64, retry_after: Option<&str>) {
    let mut template = ResponseTemplate::new(429);
    if let Some(value) = retry_after {
        template = template.insert_header("retry-after", value);
    }
    Mock::given(method("GET"))
        .and(path("/spec.json"))
        .respond_with(template)
        .up_to_n_times(n)
        .expect(n)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/spec.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"ok\":true}"))
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fetches_a_document_body() {
    let server = serve(ResponseTemplate::new(200).set_body_string("{\"ok\":true}")).await;
    let fetched = fetch_text(format!("{}/spec.json", server.uri()), MAX_DOCUMENT_BYTES)
        .await
        .expect("fetch succeeds");
    assert_eq!(fetched, "{\"ok\":true}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_no_authorization_header() {
    // A spec host is a third party: credentials must never go there.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/spec.json"))
        .and(|request: &wiremock::Request| !request.headers.contains_key("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    fetch_text(format!("{}/spec.json", server.uri()), MAX_DOCUMENT_BYTES)
        .await
        .expect("no authorization header was sent");
}

/// A 429 is retried, exactly as it is on the RPC channel: the fetch does
/// not surface the first rate-limit response to the caller.
#[tokio::test(flavor = "multi_thread")]
async fn retries_a_429_and_then_succeeds() {
    let server = MockServer::start().await;
    mount_429_then_ok(&server, 2, None).await;
    let fetched = fetch_text(format!("{}/spec.json", server.uri()), MAX_DOCUMENT_BYTES)
        .await
        .expect("retried past the 429s");
    assert_eq!(fetched, "{\"ok\":true}");
    // `expect(2)` on the 429 mock is verified when the server drops.
    drop(server);
}

/// `Retry-After` is honoured, not merely counted.
#[tokio::test(flavor = "multi_thread")]
async fn honours_retry_after_before_retrying() {
    let server = MockServer::start().await;
    mount_429_then_ok(&server, 1, Some("1")).await;
    let started = Instant::now();
    let fetched = fetch_text(format!("{}/spec.json", server.uri()), MAX_DOCUMENT_BYTES)
        .await
        .expect("retried after waiting");
    let elapsed = started.elapsed();
    assert_eq!(fetched, "{\"ok\":true}");
    assert!(
        elapsed >= Duration::from_millis(900),
        "did not honour Retry-After: waited only {elapsed:?}"
    );
    // Generous upper bound: guards against waiting a multiple of the
    // header without being load-flaky.
    assert!(elapsed < Duration::from_secs(20), "waited {elapsed:?}");
}

/// A host that never stops rate limiting exhausts the budget and produces
/// the dedicated error (which the CLI maps to exit code 8), NOT a generic
/// HTTP failure. This is the case that used to be a single unretried
/// request.
#[tokio::test(flavor = "multi_thread")]
async fn exhausted_429_retries_are_their_own_error() {
    let server = MockServer::start().await;
    let policy = RetryPolicy {
        max_retries: 2,
        backoff_base: Duration::from_millis(5),
        max_wait: Duration::from_millis(50),
    };
    Mock::given(method("GET"))
        .and(path("/spec.json"))
        .respond_with(ResponseTemplate::new(429))
        // One initial attempt plus max_retries retries, and no more.
        .expect(3)
        .mount(&server)
        .await;

    let error = fetch_with(
        format!("{}/spec.json", server.uri()),
        MAX_DOCUMENT_BYTES,
        policy,
        test_throttle(),
    )
    .await
    .expect_err("must give up");
    match error {
        FetchError::RateLimited { retries, .. } => assert_eq!(retries, 2),
        other => panic!("unexpected error: {other:?}"),
    }
    // Request count verified on drop.
    drop(server);
}

/// Every attempt draws from the throttle, so a fetch cannot burst past the
/// rate the rest of the process paces itself to.
#[tokio::test(flavor = "multi_thread")]
async fn attempts_are_paced_by_the_throttle() {
    let server = MockServer::start().await;
    mount_429_then_ok(&server, 2, None).await;
    // 100 requests/second, no burst: the 2nd and 3rd attempts each wait
    // about 10ms, so three attempts cannot finish in under 20ms.
    let throttle = Throttle::new(100.0, 1.0);
    let started = Instant::now();
    fetch_with(
        format!("{}/spec.json", server.uri()),
        MAX_DOCUMENT_BYTES,
        RetryPolicy {
            max_retries: 3,
            backoff_base: Duration::from_millis(1),
            max_wait: Duration::from_millis(5),
        },
        throttle,
    )
    .await
    .expect("succeeds after the 429s");
    assert!(
        started.elapsed() >= Duration::from_millis(20),
        "attempts were not paced: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_document_over_the_size_cap() {
    let big = "x".repeat(4096);
    let server = serve(ResponseTemplate::new(200).set_body_string(big)).await;
    let error = fetch_text(format!("{}/spec.json", server.uri()), 1024)
        .await
        .expect_err("must be rejected");
    assert!(
        matches!(error, FetchError::Unusable { .. }),
        "unexpected error: {error:?}"
    );
    // The limit is stated, the content is not.
    assert!(error.to_string().contains("1024"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_document_that_is_not_utf8() {
    let server = serve(ResponseTemplate::new(200).set_body_bytes(vec![0xff, 0xfe, 0x00])).await;
    let error = fetch_text(format!("{}/spec.json", server.uri()), MAX_DOCUMENT_BYTES)
        .await
        .expect_err("must be rejected");
    assert!(
        matches!(error, FetchError::Unusable { .. }),
        "unexpected error: {error:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_status_never_echoes_the_response_body() {
    let server = serve(ResponseTemplate::new(404).set_body_string("<html>gone</html>")).await;
    let error = fetch_text(format!("{}/spec.json", server.uri()), MAX_DOCUMENT_BYTES)
        .await
        .expect_err("must fail");
    match &error {
        FetchError::Status { status, .. } => assert_eq!(*status, 404),
        other => panic!("unexpected error: {other:?}"),
    }
    // A document host is not an API: its error page is not a diagnostic.
    let text = format!("{error} {error:?}");
    assert!(!text.contains("gone"), "body echoed: {text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_connection_failure_as_transport() {
    // Port 1 on loopback: reachable stack, nothing listening. (A dropped
    // MockServer's port would be racy - another test may rebind it.)
    let error = fetch_text(
        "http://127.0.0.1:1/spec.json".to_string(),
        MAX_DOCUMENT_BYTES,
    )
    .await
    .expect_err("must fail");
    assert!(
        matches!(
            error,
            FetchError::Transport {
                kind: TransportKind::Connect,
                ..
            }
        ),
        "unexpected error: {error:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_unusable_urls_before_any_request() {
    for url in [
        "not a url",
        "ftp://example.com/spec.json",
        "file:///etc/passwd",
        "https://user:secret@example.com/spec.json",
        // Note: `https:///x` is NOT here - the URL parser reads the first
        // path segment as the host, so it is a valid (if useless) URL.
        "https://",
    ] {
        let error = fetch_text(url.to_string(), MAX_DOCUMENT_BYTES)
            .await
            .expect_err("must be rejected");
        assert!(
            matches!(error, FetchError::InvalidUrl { .. }),
            "{url}: unexpected error: {error:?}"
        );
        // Never echo the URL itself: it may carry credentials.
        assert!(
            !error.to_string().contains("secret"),
            "credential leaked: {error}"
        );
    }
}

/// The fetch error type is deliberately NOT `EngineError`: nothing about a
/// document host may be reported in terms of the API the user is
/// authenticated against.
#[tokio::test(flavor = "multi_thread")]
async fn fetch_errors_are_not_api_errors() {
    let server = serve(ResponseTemplate::new(401)).await;
    let error = fetch_text(format!("{}/spec.json", server.uri()), MAX_DOCUMENT_BYTES)
        .await
        .expect_err("must fail");
    let text = format!("{error} {error:?}");
    for forbidden in ["OUTLINE_API_KEY", "OUTLINE_URL", "API key"] {
        assert!(
            !text.contains(forbidden),
            "{forbidden:?} in a document-fetch error: {text}"
        );
    }
    assert!(text.contains("document source"), "{text}");
}
