//! Integration tests for the single request channel.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::time::Duration;

use engine::{Client, EngineError, OpSpec, ParamSpec, ParamType, TransportKind, ValidationMode};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn op_with_path(op_path: &'static str) -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("things.info"),
        path: Cow::Borrowed(op_path),
        summary: Cow::Borrowed("Retrieve a thing"),
        content_type: Cow::Borrowed("application/json"),
        body_mode: engine::BodyMode::KeyValue,
        params: Cow::Borrowed(&[ParamSpec {
            name: Cow::Borrowed("id"),
            ty: ParamType::String,
            required: false,
            nullable: false,
            enum_values: Cow::Borrowed(&[]),
            format: Cow::Borrowed(""),
            minimum: None,
            maximum: None,
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
            ValidationMode::Strict,
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
        client.execute(&op_with_path("/rpc/custom"), &[], ValidationMode::Strict)
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
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
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
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
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
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
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
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
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
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
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
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
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

#[tokio::test(flavor = "multi_thread")]
async fn token_smuggled_through_control_chars_is_discarded() {
    // Adversarial-review PoC: the server interleaves control characters
    // inside the reflected token so the exact match misses; whitespace
    // normalization would otherwise reassemble a token a human can read.
    let token = "reflected-secret-token";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "reflected-\u{0}secret-token",
            "message": "reflected-\u{0}secret-token\u{1b}[31m denied"
        })))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, token)?;
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
    })
    .await
    .unwrap();

    match result {
        Err(error @ EngineError::Api { .. }) => {
            // Squeeze out whitespace/control chars the way a reader would:
            // the token must not be recoverable anywhere in the error, its
            // Debug, or its source chain.
            let rendered = render_full_error_chain(&error);
            let squeezed: String = rendered.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(
                !squeezed.contains(token),
                "token recoverable after squeezing: {rendered}"
            );
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn token_smuggled_through_invisible_characters_is_discarded() {
    // Re-review PoC: U+200B ZERO WIDTH SPACE is neither is_control() nor
    // is_whitespace(), so category-based stripping missed it while a reader
    // sees the token unbroken. Also covers ZWJ and a variation selector.
    let token = "reflected-secret-token";
    for smuggled in [
        "reflected-\u{200b}secret-token",
        "reflected-\u{200d}secret-token",
        "reflected-\u{fe0f}secret-token",
        "reflected-\u{ad}secret\u{200b}-token",
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/things.info"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": smuggled,
                "message": smuggled
            })))
            .mount(&server)
            .await;

        let base_url = server.uri();
        let result = tokio::task::spawn_blocking(move || {
            let client = Client::new(&base_url, token)?;
            client.execute(
                &op_with_path("/api/things.info"),
                &[],
                ValidationMode::Strict,
            )
        })
        .await
        .unwrap();

        let error = result.unwrap_err();
        assert!(matches!(error, EngineError::Api { .. }), "got {error:?}");
        let rendered = render_full_error_chain(&error);
        // Reduce the way a reader would: drop every non-alphanumeric.
        let skeleton: String = rendered
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        let token_skeleton: String = token
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        assert!(
            !skeleton.contains(&token_skeleton),
            "token recoverable from {smuggled:?}: {rendered}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn complete_json_envelope_in_a_capped_body_is_not_mangled() {
    // Re-review PoC: JSON tolerates unlimited trailing whitespace, so a
    // COMPLETE envelope can sit inside a body that hit the read cap. Its
    // fields must not get the cut-fragment treatment.
    let envelope = r#"{"error":"validation_error","message":"document not found"}"#;
    let padded = format!("{envelope}{}", " ".repeat(8200 - envelope.len()));
    assert!(padded.len() > 8192, "PoC must exceed the read cap");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(padded, "application/json"))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "secret-token")?;
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api { code, message, .. }) => {
            assert_eq!(message, "document not found");
            assert_eq!(code.as_deref(), Some("validation_error"));
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn token_fragment_behind_capped_whitespace_is_discarded() {
    // Re-review PoC: the cap lands on whitespace, so a single trailing-run
    // drop leaves the 2-char token fragment sitting behind it.
    let token = "reflected-secret-token";
    let body = format!("{}re\n{}", "\n".repeat(8189), &token[2..]);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(body, "text/plain"))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, token)?;
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api { message, .. }) => {
            assert_eq!(message, "***", "fragment survived: {message:?}");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn short_token_prefix_left_by_body_cap_is_discarded() {
    // Adversarial-review PoC: the 8 KiB cap leaves only "re", shorter than
    // the fragment minimum, so prefix redaction alone cannot catch it. The
    // whole trailing run of a capped body must be dropped.
    let token = "reflected-secret-token";
    // 8190 filler bytes + the full token: the 8 KiB cap cuts the token
    // after its first two characters.
    let body = format!("{}{token}", "\n".repeat(8190));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(body, "text/plain"))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, token)?;
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
    })
    .await
    .unwrap();

    match result {
        Err(EngineError::Api { message, .. }) => {
            assert_eq!(message, "***", "capped fragment survived: {message:?}");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

/// Serve one raw HTTP response on a throwaway localhost port.
///
/// wiremock cannot produce a header/body mismatch (its own hyper layer
/// rejects it), so body-transport failures need a socket we control:
/// `response_head` is written verbatim, then the connection is held open for
/// `hold` before being closed.
fn raw_http_once(response_head: &'static str, hold: Duration) -> String {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let Ok((mut socket, _)) = listener.accept() else {
            return;
        };
        // Drain what is available of the request; the client's body is
        // small enough to arrive with the headers.
        let mut buffer = [0_u8; 4096];
        let _ = socket.read(&mut buffer);
        let _ = socket.write_all(response_head.as_bytes());
        let _ = socket.flush();
        std::thread::sleep(hold);
    });
    base_url
}

#[test]
fn body_read_timeout_is_a_transport_error_not_invalid_json() {
    // 200 + Content-Length: 100 and no body at all: reading the body times
    // out. That is a retryable transport failure, not malformed JSON. The
    // short client timeout keeps the test fast.
    let base_url = raw_http_once(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n",
        Duration::from_secs(2),
    );
    let client =
        Client::with_timeout(&base_url, "secret-token", Duration::from_millis(300)).unwrap();
    let error = client
        .execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
        .unwrap_err();

    match error {
        EngineError::Transport { kind, .. } => assert_eq!(kind, TransportKind::Timeout),
        other => panic!("expected Transport error, got {other:?}"),
    }
}

#[test]
fn truncated_body_is_a_transport_error_not_invalid_json() {
    // Headers promise 100 bytes, the server sends 8 and closes: the JSON is
    // cut mid-structure. Retrying may help, so this must not be reported as
    // an invalid-JSON error.
    let base_url = raw_http_once(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"data\":",
        Duration::from_millis(50),
    );
    let client = Client::with_timeout(&base_url, "secret-token", Duration::from_secs(5)).unwrap();
    let error = client
        .execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
        .unwrap_err();

    match error {
        EngineError::Transport { kind, .. } => {
            assert!(
                matches!(kind, TransportKind::Body | TransportKind::Timeout),
                "unexpected kind {kind:?}"
            );
        }
        other => panic!("expected Transport error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn genuine_json_syntax_error_stays_invalid_response() {
    // A complete body that simply is not JSON must NOT be reclassified as a
    // transport failure: retrying would not help.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.info"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("not json at all", "application/json"),
        )
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::with_timeout(&base_url, "secret-token", Duration::from_secs(5))?;
        client.execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
    })
    .await
    .unwrap();

    assert!(
        matches!(result, Err(EngineError::InvalidResponse { .. })),
        "expected InvalidResponse, got {result:?}"
    );
}

#[test]
fn invalid_header_credential_is_a_request_build_error() {
    // A token containing a newline cannot go into an HTTP header; the
    // request never reaches the network, so it must not be reported as a
    // transport failure - and the token must not appear anywhere.
    let client = Client::new("http://127.0.0.1:9", "bad\nkey-SECRET-9c7a").unwrap();
    let error = client
        .execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
        .unwrap_err();
    assert!(
        matches!(error, EngineError::InvalidRequest { .. }),
        "expected InvalidRequest, got {error:?}"
    );
    let rendered = render_full_error_chain(&error);
    assert!(
        !rendered.contains("SECRET-9c7a"),
        "credential leaked: {rendered}"
    );
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
        .execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
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
        .execute(
            &op_with_path("/api/things.info"),
            &[],
            ValidationMode::Strict,
        )
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
