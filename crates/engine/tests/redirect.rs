//! Signed redirect handling stays credential-safe and never follows targets.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;

use engine::{Client, EngineError, OpSpec, ParamSpec, ParamType, ValidationMode};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn redirect_op() -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("attachments.redirect"),
        path: Cow::Borrowed("/api/attachments.redirect"),
        summary: Cow::Borrowed("Redirect to an attachment"),
        content_type: Cow::Borrowed("application/json"),
        body_mode: engine::BodyMode::KeyValue,
        response_fields: Cow::Borrowed(&[]),
        params: Cow::Borrowed(&[ParamSpec {
            name: Cow::Borrowed("id"),
            ty: ParamType::String,
            required: false,
            nullable: false,
            enum_values: Cow::Borrowed(&[]),
            format: Cow::Borrowed(""),
            minimum: None,
            maximum: None,
            description: Cow::Borrowed(""),
        }]),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn location_is_returned_without_following_it() {
    let target = MockServer::start().await;
    let server = MockServer::start().await;
    let signed = format!("{}/private/file?signature=secret", target.uri());
    Mock::given(method("POST"))
        .and(path("/api/attachments.redirect"))
        .and(header("authorization", "Bearer secret-token"))
        .and(body_json(json!({ "id": "attachment-id" })))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", signed.as_str()))
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let expected = signed.clone();
    let result = tokio::task::spawn_blocking(move || {
        Client::new(&base_url, "secret-token")?.execute_redirect_location(
            &redirect_op(),
            &[("id".to_string(), "attachment-id".to_string())],
            ValidationMode::Strict,
        )
    })
    .await
    .unwrap();

    assert_eq!(result.unwrap(), expected);
    assert!(target.received_requests().await.unwrap().is_empty());
}

/// The Location comes back exactly as the server sent it.
///
/// It is signed, so re-serializing it is not harmless: `Url::to_string`
/// lowercases the host, drops a default port and normalizes
/// percent-encoding, any of which can invalidate the signature the storage
/// host will check. Validation must not rewrite what it validates.
#[tokio::test(flavor = "multi_thread")]
async fn the_location_is_returned_byte_for_byte() {
    let server = MockServer::start().await;
    // Non-canonical on purpose: default port spelled out, mixed-case host,
    // and an already-encoded path and query.
    let signed = "https://Storage.Example:443/private/f%2Fo%20o?sig=a%2Bb&x=1";
    Mock::given(method("POST"))
        .and(path("/api/attachments.redirect"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", signed))
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        Client::new(&base_url, "secret-token")?.execute_redirect_location(
            &redirect_op(),
            &[("id".to_string(), "attachment-id".to_string())],
            ValidationMode::Strict,
        )
    })
    .await
    .unwrap();

    assert_eq!(
        result.unwrap(),
        signed,
        "the signed Location was rewritten on the way out"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn location_must_be_an_absolute_http_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/attachments.redirect"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/relative/secret"))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        Client::new(&base_url, "secret-token")?.execute_redirect_location(
            &redirect_op(),
            &[],
            ValidationMode::Strict,
        )
    })
    .await
    .unwrap();

    let error = result.unwrap_err();
    assert!(matches!(error, EngineError::UnexpectedResponse { .. }));
    assert!(!error.to_string().contains("relative/secret"));
}
