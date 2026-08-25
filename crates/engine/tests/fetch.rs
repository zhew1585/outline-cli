//! The plain-document fetch channel (`engine::fetch`).
//!
//! Every case runs against a local wiremock server: no test in this file
//! touches the real network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use engine::fetch::{self, MAX_DOCUMENT_BYTES};
use engine::{EngineError, TransportKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Run a blocking fetch off the async test runtime.
async fn fetch_text(url: String, max_bytes: u64) -> Result<String, EngineError> {
    tokio::task::spawn_blocking(move || {
        fetch::fetch_document(&url, max_bytes, Duration::from_secs(5))
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

#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_document_over_the_size_cap() {
    let big = "x".repeat(4096);
    let server = serve(ResponseTemplate::new(200).set_body_string(big)).await;
    let error = fetch_text(format!("{}/spec.json", server.uri()), 1024)
        .await
        .expect_err("must be rejected");
    assert!(
        matches!(error, EngineError::UnusableDocument { .. }),
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
        matches!(error, EngineError::UnusableDocument { .. }),
        "unexpected error: {error:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maps_an_http_error_status_to_an_api_error() {
    let server = serve(ResponseTemplate::new(404).set_body_string("<html>gone</html>")).await;
    let error = fetch_text(format!("{}/spec.json", server.uri()), MAX_DOCUMENT_BYTES)
        .await
        .expect_err("must fail");
    match error {
        EngineError::Api {
            status,
            code,
            message,
        } => {
            assert_eq!(status, 404);
            assert_eq!(code, None);
            // The response body is never echoed: a document host is not
            // an API and its error page is not a diagnostic.
            assert!(!message.contains("gone"), "body echoed: {message}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
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
            EngineError::Transport {
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
            matches!(error, EngineError::InvalidDocumentUrl { .. }),
            "{url}: unexpected error: {error:?}"
        );
        // Never echo the URL itself: it may carry credentials.
        assert!(
            !error.to_string().contains("secret"),
            "credential leaked: {error}"
        );
    }
}
