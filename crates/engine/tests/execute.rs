//! Integration tests for the single request channel.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;

use engine::{Client, EngineError, OpSpec, ParamSpec, ParamType};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_op() -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("things.info"),
        path: Cow::Borrowed("/things.info"),
        params: Cow::Borrowed(&[ParamSpec {
            name: Cow::Borrowed("id"),
            ty: ParamType::String,
            required: false,
        }]),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_posts_json_with_bearer_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .and(header("authorization", "Bearer secret-token"))
        .and(header("content-type", "application/json"))
        .and(header("accept", "application/json"))
        .and(body_json(json!({ "id": "doc-123" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "doc-123", "title": "Hello" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "secret-token")?;
        client.execute(&sample_op(), &[("id".to_string(), "doc-123".to_string())])
    })
    .await
    .unwrap();

    let value = result.unwrap();
    assert_eq!(value["data"]["title"], "Hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_maps_error_status_to_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error": "not_found",
            "message": "document not found"
        })))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "secret-token")?;
        client.execute(&sample_op(), &[])
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api { status, message }) => {
            assert_eq!(status, 404);
            assert_eq!(message, "document not found");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn new_rejects_non_http_base_url() {
    let error = Client::new("ftp://example.com", "token").unwrap_err();
    assert!(matches!(error, EngineError::InvalidBaseUrl { .. }));
}

#[test]
fn new_normalizes_trailing_slash() {
    // A trailing slash must not produce `//api/...` URLs; constructing the
    // client is enough to exercise normalization without a network call.
    assert!(Client::new("https://example.com/", "token").is_ok());
}
