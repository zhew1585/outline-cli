//! CLI parameter validation, type coercion, and `--body` (Story 1.3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{body_json, body_string, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::isolate;

/// Nothing listens here; validation must fail before any network attempt.
const CLOSED_PORT_URL: &str = "http://127.0.0.1:9";

/// `otl` with valid-looking config pointing at a closed port.
///
/// `common::isolate` shuts off the credential file, the user config file, the
/// selected profile, the plaintext-key notice and the spec cache - validation
/// is asserted against the facets of the spec compiled into the binary.
fn otl_offline() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    isolate(&mut cmd)
        .env("OUTLINE_URL", CLOSED_PORT_URL)
        .env("OUTLINE_API_KEY", "test-key");
    cmd
}

/// `otl` pointed at a wiremock server.
fn otl_online(uri: &str) -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    isolate(&mut cmd)
        .env("OUTLINE_URL", uri)
        .env("OUTLINE_API_KEY", "test-key");
    cmd
}

/// A temp file with the given content, kept alive by the returned guard.
fn temp_json(content: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
    file.write_all(content.as_bytes()).unwrap();
    file.flush().unwrap();
    file
}

#[tokio::test(flavor = "multi_thread")]
async fn kv_args_become_native_json_types_in_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        // integer and boolean must arrive as native JSON types, not strings
        .and(body_json(json!({
            "id": "doc-1",
            "lastRevision": 7,
            "publish": true,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl_online(&uri)
            .args([
                "api",
                "documents.update",
                "id=doc-1",
                "lastRevision=7",
                "publish=true",
            ])
            .assert()
    })
    .await
    .unwrap();

    assert.success();
}

#[test]
fn missing_required_param_exits_2_before_network() {
    // A network attempt against the closed port would yield exit 1 with a
    // transport message; the local validation error must win instead.
    otl_offline()
        .args(["api", "documents.update", "publish=true"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("\"id\""))
        .stderr(predicate::str::contains("string"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn missing_required_complex_param_hints_at_body_flag() {
    // users.invite requires `invites` (an array): the error must point the
    // user at --body.
    otl_offline()
        .args(["api", "users.invite"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("invites"))
        .stderr(predicate::str::contains("--body"));
}

#[test]
fn unknown_param_exits_2_listing_valid_params() {
    otl_offline()
        .args(["api", "documents.info", "bogus=1"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("bogus"))
        .stderr(predicate::str::contains("id"));
}

#[test]
fn complex_param_as_kv_exits_2_and_suggests_body() {
    otl_offline()
        .args(["api", "documents.update", "id=x", "preferences={}"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("preferences"))
        .stderr(predicate::str::contains("--body"));
}

#[test]
fn bad_integer_value_exits_2() {
    otl_offline()
        .args(["api", "documents.update", "id=x", "lastRevision=abc"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("lastRevision"))
        .stderr(predicate::str::contains("integer"));
}

#[test]
fn bad_boolean_value_exits_2() {
    otl_offline()
        .args(["api", "documents.update", "id=x", "publish=yes"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("publish"))
        .stderr(predicate::str::contains("boolean"));
}

#[tokio::test(flavor = "multi_thread")]
async fn body_file_is_passed_through_verbatim_skipping_kv_validation() {
    // The file deliberately omits the required `id` and uses an unknown
    // field: --body bypasses k=v assembly and validation entirely, and the
    // bytes arrive unmodified (key order preserved).
    let raw = r#"{"zeta": 1, "alpha": {"nested": [true, null]}}"#;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_string(raw.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        let file = temp_json(raw);
        let body_arg = format!("@{}", file.path().display());
        otl_online(&uri)
            .args(["api", "documents.update", "--body", &body_arg])
            .assert()
    })
    .await
    .unwrap();

    assert.success();
}

#[test]
fn body_combined_with_kv_args_exits_2() {
    let file = temp_json(r#"{"id": "x"}"#);
    let body_arg = format!("@{}", file.path().display());
    otl_offline()
        .args(["api", "documents.update", "id=x", "--body", &body_arg])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--body"))
        .stderr(predicate::str::contains("key=value"));
}

#[test]
fn body_without_at_prefix_exits_2() {
    otl_offline()
        .args(["api", "documents.update", "--body", "file.json"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("@"));
}

#[test]
fn body_with_missing_file_exits_2() {
    otl_offline()
        .args([
            "api",
            "documents.update",
            "--body",
            "@/nonexistent/otl-body.json",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("otl-body.json"));
}

#[test]
fn body_with_invalid_json_exits_2_naming_the_file() {
    let file = temp_json("{not json");
    let body_arg = format!("@{}", file.path().display());
    otl_offline()
        .args(["api", "documents.update", "--body", &body_arg])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("not valid JSON"));
}

// --- Finding 1: operations whose body is not application/json ------------

#[tokio::test(flavor = "multi_thread")]
async fn multipart_operation_exits_2_without_making_a_request() {
    // A catch-all mock that must never be hit: wiremock verifies the
    // expectation when the server is dropped at the end of the test.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {} })))
        .expect(0)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl_online(&uri).args(["api", "documents.import"]).assert()
    })
    .await
    .unwrap();

    assert
        .failure()
        .code(2)
        .stderr(predicate::str::contains("multipart/form-data"))
        .stderr(predicate::str::contains("dedicated command"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn multipart_operation_rejects_a_raw_body_too() {
    let file = temp_json(r#"{"file": "x"}"#);
    let body_arg = format!("@{}", file.path().display());
    otl_offline()
        .args(["api", "documents.import", "--body", &body_arg])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("multipart/form-data"));
}

// --- Finding 2: root-level oneOf/anyOf operations ------------------------

#[test]
fn union_body_operation_refuses_key_value_args() {
    // shares.create requires exactly one of documentId/collectionId; both
    // the empty call and a two-key call must fail locally, before network.
    for extra in [vec![], vec!["documentId=a", "collectionId=b"]] {
        let mut cmd = otl_offline();
        cmd.args(["api", "shares.create"]).args(&extra);
        cmd.assert()
            .failure()
            .code(2)
            .stderr(predicate::str::contains("--body"))
            .stdout(predicate::str::is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn union_body_operation_accepts_a_raw_body() {
    let raw = r#"{"documentId":"doc-1"}"#;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/shares.create"))
        .and(body_string(raw.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        let file = temp_json(raw);
        let body_arg = format!("@{}", file.path().display());
        otl_online(&uri)
            .args(["api", "shares.create", "--body", &body_arg])
            .assert()
    })
    .await
    .unwrap();

    assert.success();
}

// --- Finding 3: enum, nullable and numeric bounds ------------------------

#[test]
fn unknown_enum_value_exits_2_listing_allowed_values() {
    otl_offline()
        .args([
            "api",
            "accessRequests.approve",
            "id=d8f7a1b2-3c4d-5e6f-7081-92a3b4c5d6e7",
            "permission=definitely-not-enum",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("read_write"))
        .stderr(predicate::str::contains("admin"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn out_of_range_number_exits_2() {
    otl_offline()
        .args([
            "api",
            "attachments.create",
            "name=x",
            "contentType=text/plain",
            "size=-1",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("size"))
        .stdout(predicate::str::is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn nullable_param_sends_json_null() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_json(json!({ "id": "doc-1", "collectionId": null })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl_online(&uri)
            .args(["api", "documents.update", "id=doc-1", "collectionId=null"])
            .assert()
    })
    .await
    .unwrap();

    assert.success();
}

// --- Finding 4: server error text on raw-body requests ------------------

/// A wiremock server that rejects everything, echoing `message` back.
async fn rejecting_server(message: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "ok": false,
            "error": "validation_error",
            "message": message,
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn body_request_withholds_the_server_message() {
    // A short secret defeats any length-based redaction, so the rule is
    // categorical: free-form server text is not echoed for --body calls.
    let secret = "s3cr3t!";
    let server = rejecting_server(&format!("rejected {{\"password\":\"{secret}\"}}")).await;

    let uri = server.uri();
    let output = tokio::task::spawn_blocking(move || {
        let file = temp_json(&format!(r#"{{"password":"{secret}"}}"#));
        let body_arg = format!("@{}", file.path().display());
        otl_online(&uri)
            .args(["api", "documents.update", "--body", &body_arg])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // A rejected 4xx is exit code 3 in the published table (docs/exit-codes.md);
    // withholding the message changes what is printed, never the class.
    assert_eq!(output.status.code(), Some(3), "stderr: {stderr}");
    assert_eq!(stderr.matches(secret).count(), 0, "secret leaked: {stderr}");
    assert!(!stdout.contains(secret), "stdout leaked: {stdout}");
    // The user still sees the status, the structured code, and the way in.
    assert!(stderr.contains("400"), "no status: {stderr}");
    assert!(stderr.contains("validation_error"), "no code: {stderr}");
    assert!(
        stderr.contains("--show-server-message"),
        "no opt-in hint: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn body_request_withholds_escaped_and_overlapping_secrets() {
    // Both PoCs that defeated substring redaction: a secret whose JSON
    // encoding differs from its decoded form, and overlapping values.
    let cases = [
        (
            r#"{"password":"LONG\"SECRET-123"}"#,
            "rejected LONG\\\"SECRET-123",
            "SECRET-123",
        ),
        (
            r#"["password","passwordSUPERSECRET"]"#,
            "rejected passwordSUPERSECRET",
            "SUPERSECRET",
        ),
    ];
    for (body, message, secret) in cases {
        let server = rejecting_server(message).await;
        let uri = server.uri();
        let output = tokio::task::spawn_blocking(move || {
            let file = temp_json(body);
            let body_arg = format!("@{}", file.path().display());
            otl_online(&uri)
                .args(["api", "documents.update", "--body", &body_arg])
                .output()
                .unwrap()
        })
        .await
        .unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(3), "stderr: {stderr}");
        assert_eq!(
            stderr.matches(secret).count(),
            0,
            "secret {secret} leaked: {stderr}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn show_server_message_restores_the_server_text() {
    let server = rejecting_server("document doc-1 is archived").await;
    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        let file = temp_json(r#"{"id":"doc-1"}"#);
        let body_arg = format!("@{}", file.path().display());
        otl_online(&uri)
            .args([
                "api",
                "documents.update",
                "--body",
                &body_arg,
                "--show-server-message",
            ])
            .assert()
    })
    .await
    .unwrap();

    assert
        .failure()
        .code(3)
        .stderr(predicate::str::contains("document doc-1 is archived"));
}

#[tokio::test(flavor = "multi_thread")]
async fn key_value_request_still_shows_the_server_message() {
    let server = rejecting_server("invalid permission").await;
    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl_online(&uri)
            .args(["api", "documents.update", "id=doc-1"])
            .assert()
    })
    .await
    .unwrap();

    assert
        .failure()
        .code(3)
        .stderr(predicate::str::contains("invalid permission"))
        .stderr(predicate::str::contains("withheld").not());
}

// --- format validation and the --no-validate escape hatch ---------------

#[test]
fn invalid_uuid_format_exits_2_naming_the_format() {
    otl_offline()
        .args(["api", "accessRequests.approve", "id=not-a-uuid"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("uuid"))
        .stderr(predicate::str::contains("id"))
        .stdout(predicate::str::is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn pattern_constrained_value_is_still_sent() {
    // Documented deferral: `templates.update` color declares a regex
    // pattern that is deliberately not compiled into the IR, so `red`
    // reaches the server instead of being rejected locally.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/templates.update"))
        .and(body_json(json!({ "id": "doc-1", "color": "red" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl_online(&uri)
            .args(["api", "templates.update", "id=doc-1", "color=red"])
            .assert()
    })
    .await
    .unwrap();

    assert.success();
}

#[tokio::test(flavor = "multi_thread")]
async fn no_validate_bypasses_facet_checks() {
    // The spec-drift escape hatch: an enum value and a format the vendored
    // spec rejects are sent anyway.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accessRequests.approve"))
        .and(body_json(
            json!({ "id": "not-a-uuid", "permission": "future_role" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl_online(&uri)
            .args([
                "api",
                "accessRequests.approve",
                "id=not-a-uuid",
                "permission=future_role",
                "--no-validate",
            ])
            .assert()
    })
    .await
    .unwrap();

    assert.success();
}

#[test]
fn no_validate_still_rejects_unknown_and_missing_params() {
    for extra in [vec!["bogus=1"], vec![]] {
        let mut cmd = otl_offline();
        cmd.args(["api", "documents.update", "--no-validate"])
            .args(&extra);
        cmd.assert().failure().code(2);
    }
}

// --- Finding 5: malformed key=value must not echo the argument -----------

#[test]
fn malformed_argument_never_echoes_its_text() {
    for bad in ["=LEAK-ME-82dd", "MALFORMED-SECRET-7c2e"] {
        let output = otl_offline()
            .args(["api", "documents.info", bad])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
        for secret in ["LEAK-ME-82dd", "MALFORMED-SECRET-7c2e"] {
            assert_eq!(
                stderr.matches(secret).count(),
                0,
                "argument text leaked: {stderr}"
            );
            assert!(!stdout.contains(secret), "stdout leaked: {stdout}");
        }
        // The user still learns which argument and what shape is expected.
        assert!(
            stderr.contains("argument #1") && stderr.contains("key=value"),
            "unhelpful message: {stderr}"
        );
    }
}

#[test]
fn unknown_param_error_never_echoes_the_value() {
    let output = otl_offline()
        .args(["api", "documents.info", "bogus=VALUE-SECRET-4a1b"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("bogus"), "no key named: {stderr}");
    assert_eq!(
        stderr.matches("VALUE-SECRET").count(),
        0,
        "value leaked: {stderr}"
    );
}

// --- Finding 6: exact JSON numbers --------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn large_integer_is_sent_without_precision_loss() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.search"))
        .and(body_string(r#"{"limit":9007199254740993}"#.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl_online(&uri)
            .args(["api", "documents.search", "limit=9007199254740993"])
            .assert()
    })
    .await
    .unwrap();

    assert.success();
}

#[test]
fn inexact_number_exits_2() {
    otl_offline()
        .args(["api", "documents.search", "limit=9007199254740993.0"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--body"));
}

// --- Finding 7: --body size cap -----------------------------------------

#[test]
fn oversize_body_file_exits_2_before_reading_it_all() {
    // Build a valid JSON array just over the cap in a temp dir (never in
    // the repo), then assert a clean usage error rather than an OOM.
    let cap = otl::commands::api::MAX_BODY_FILE_BYTES;
    let filler = "\"aaaaaaaa\",";
    let count = (cap as usize / filler.len()) + 2;
    let mut content = String::with_capacity(count * filler.len() + 8);
    content.push('[');
    for _ in 0..count {
        content.push_str(filler);
    }
    content.push_str("\"end\"]");
    assert!(content.len() as u64 > cap, "test fixture below the cap");

    let file = temp_json(&content);
    let body_arg = format!("@{}", file.path().display());
    otl_offline()
        .args(["api", "documents.update", "--body", &body_arg])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("too large"))
        .stdout(predicate::str::is_empty());
}
