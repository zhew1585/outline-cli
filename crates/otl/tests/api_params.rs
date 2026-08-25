//! CLI parameter validation, type coercion, and `--body` (Story 1.3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{body_json, body_string, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Nothing listens here; validation must fail before any network attempt.
const CLOSED_PORT_URL: &str = "http://127.0.0.1:9";

/// `otl` with valid-looking config pointing at a closed port.
fn otl_offline() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env("OUTLINE_URL", CLOSED_PORT_URL)
        .env("OUTLINE_API_KEY", "test-key");
    cmd
}

/// `otl` pointed at a wiremock server.
fn otl_online(uri: &str) -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env("OUTLINE_URL", uri)
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
