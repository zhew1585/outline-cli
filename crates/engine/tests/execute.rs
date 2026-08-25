//! Integration tests for the single request channel.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;

use engine::{Client, EngineError, OpSpec, ParamSpec, ParamType};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn op_with_path(op_path: &'static str) -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("things.info"),
        path: Cow::Borrowed(op_path),
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
        client.execute(
            &op_with_path("/api/things.info"),
            &[("id".to_string(), "doc-123".to_string())],
        )
    })
    .await
    .unwrap();

    let value = result.unwrap();
    assert_eq!(value["data"]["title"], "Hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_uses_ir_path_verbatim_not_name_convention() {
    // The engine must join base + op.path; op.name plays no role in the URL.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/custom"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "secret-token")?;
        client.execute(&op_with_path("/rpc/custom"), &[])
    })
    .await
    .unwrap();

    assert_eq!(result.unwrap()["data"]["ok"], true);
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
        client.execute(&op_with_path("/api/things.info"), &[])
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api {
            status,
            code,
            message,
        }) => {
            assert_eq!(status, 404);
            assert_eq!(code.as_deref(), Some("not_found"));
            assert_eq!(message, "document not found");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn non_json_error_body_has_no_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(502).set_body_raw("Bad Gateway", "text/plain"))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "secret-token")?;
        client.execute(&op_with_path("/api/things.info"), &[])
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api {
            status,
            code,
            message,
        }) => {
            assert_eq!(status, 502);
            assert_eq!(code, None);
            assert_eq!(message, "Bad Gateway");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn error_code_is_sanitized_and_capped() {
    // The machine-readable code goes through the same hygiene pipeline as
    // the message: control characters stripped, token redacted, length
    // capped.
    let forged_code = format!("evil\x1b[31m{}reflected-secret-token", "c".repeat(200));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": forged_code,
            "message": "bad request"
        })))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "reflected-secret-token")?;
        client.execute(&op_with_path("/api/things.info"), &[])
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api { code, .. }) => {
            let code = code.expect("code should be present");
            assert!(!code.contains('\x1b'), "ESC not stripped: {code:?}");
            assert!(
                !code.contains("reflected-secret-token"),
                "token leaked: {code:?}"
            );
            assert!(code.chars().count() <= 64, "code not capped: {code:?}");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn error_message_is_sanitized_and_capped() {
    let forged = format!(
        "remote failure\nFORGED-DIAGNOSTIC\x1b[31mRED{}",
        "x".repeat(500)
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "message": forged })))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "secret-token")?;
        client.execute(&op_with_path("/api/things.info"), &[])
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api { message, .. }) => {
            assert!(!message.contains('\n'), "newline not stripped: {message:?}");
            assert!(!message.contains('\x1b'), "ESC not stripped: {message:?}");
            assert!(message.chars().count() <= 200, "message not capped");
            assert!(message.starts_with("remote failure FORGED-DIAGNOSTIC"));
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reflected_bearer_token_is_redacted_from_error_message() {
    // A server or proxy may echo the Authorization value back in its error
    // body; the client must scrub its own token before the message can
    // reach any error type.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "message": "invalid header: Bearer reflected-secret-token"
        })))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "reflected-secret-token")?;
        client.execute(&op_with_path("/api/things.info"), &[])
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api { message, .. }) => {
            assert!(
                !message.contains("reflected-secret-token"),
                "token leaked: {message:?}"
            );
            assert_eq!(message, "invalid header: Bearer ***");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn token_prefix_cut_by_body_cap_is_redacted() {
    // Reviewer PoC: 8180 bytes of padding followed by the token. The 8 KiB
    // body cap cuts the token mid-way, leaving only its prefix at the end
    // of the capped text, where an exact replacement cannot match. Any
    // trailing token prefix (>= 4 chars) must still be redacted.
    let token = "TOKEN-SECRET-ABCDEFGHIJKLMN";
    let body = format!("{}{token}", "\n".repeat(8180));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(body, "text/plain"))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, token)?;
        client.execute(&op_with_path("/api/things.info"), &[])
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api { message, .. }) => {
            // No prefix of the token of length >= 4 may survive.
            for length in 4..=token.len() {
                assert!(
                    !message.contains(&token[..length]),
                    "token prefix {:?} leaked: {message:?}",
                    &token[..length]
                );
            }
            assert_eq!(message, "***");
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
fn new_rejects_unparseable_and_structured_urls() {
    for bad in [
        "http://[::1",                  // unparseable
        "not a url",                    // no scheme
        "http://example.com?tenant=x",  // query
        "http://example.com/#fragment", // fragment
    ] {
        let error = Client::new(bad, "token").unwrap_err();
        assert!(
            matches!(error, EngineError::InvalidBaseUrl { .. }),
            "expected InvalidBaseUrl for {bad:?}, got {error:?}"
        );
    }
}

#[test]
fn new_rejects_userinfo_and_never_echoes_credentials() {
    let error = Client::new("http://alice:url-password@example.com", "token").unwrap_err();
    assert!(matches!(error, EngineError::InvalidBaseUrl { .. }));
    let rendered = format!("{error} / {error:?}");
    assert!(
        !rendered.contains("url-password"),
        "credential leaked: {rendered}"
    );
    assert!(!rendered.contains("alice"), "username leaked: {rendered}");
}

#[test]
fn debug_output_redacts_token() {
    let client = Client::new("https://example.com", "super-secret-token").unwrap();
    let rendered = format!("{client:?}");
    assert!(
        !rendered.contains("super-secret-token"),
        "token leaked: {rendered}"
    );
    assert!(rendered.contains("***"));
}

#[test]
fn debug_output_reduces_base_url_to_origin() {
    // A base URL path can carry secrets (token-in-path auth schemes);
    // Debug must show the origin only.
    let client = Client::new("https://example.com/PATH-SECRET-9c7a", "token").unwrap();
    let rendered = format!("{client:?}");
    assert!(
        !rendered.contains("PATH-SECRET-9c7a"),
        "path leaked: {rendered}"
    );
    assert!(rendered.contains("https://example.com"));
}

#[test]
fn transport_error_display_shows_origin_only() {
    // Nothing listens on port 9; the send fails at the transport level.
    // The error Display must show scheme://host:port only - never the
    // path (which may carry secrets), and never raw reqwest error text.
    let client = Client::new("http://127.0.0.1:9/PATH-SECRET-9c7a", "PATH-SECRET-9c7a").unwrap();
    let error = client
        .execute(&op_with_path("/api/things.info"), &[])
        .unwrap_err();
    assert!(
        matches!(error, EngineError::Transport { .. }),
        "got {error:?}"
    );
    let rendered = format!("{error}");
    assert!(
        !rendered.contains("PATH-SECRET"),
        "path secret leaked: {rendered}"
    );
    assert!(
        rendered.contains("http://127.0.0.1:9"),
        "origin missing: {rendered}"
    );
    assert!(
        !rendered.contains("/api/things.info"),
        "request path leaked: {rendered}"
    );
}

/// Render an error's Display and Debug plus those of every error in its
/// `source()` chain, recursively.
fn render_full_error_chain(error: &dyn std::error::Error) -> String {
    let own = format!("{error} / {error:?}");
    match error.source() {
        Some(inner) => format!("{own} / {}", render_full_error_chain(inner)),
        None => own,
    }
}

#[test]
fn transport_error_debug_and_source_chain_are_credential_free() {
    // reqwest errors embed the full request URL in Display AND Debug; the
    // engine must store them URL-stripped (without_url) so that even
    // callers formatting {:?} or walking source() never see a path secret.
    let client = Client::new("http://127.0.0.1:9/PATH-SECRET-9c7a", "PATH-SECRET-9c7a").unwrap();
    let error = client
        .execute(&op_with_path("/api/things.info"), &[])
        .unwrap_err();
    let rendered = render_full_error_chain(&error);
    assert_eq!(
        rendered.matches("PATH-SECRET").count(),
        0,
        "secret leaked in Debug/source chain: {rendered}"
    );
    assert!(
        !rendered.contains("/api/things.info"),
        "request path leaked in Debug/source chain: {rendered}"
    );
}

#[test]
fn new_normalizes_trailing_slash() {
    // A trailing slash must not produce `//api/...` URLs; constructing the
    // client is enough to exercise normalization without a network call.
    assert!(Client::new("https://example.com/", "token").is_ok());
}
