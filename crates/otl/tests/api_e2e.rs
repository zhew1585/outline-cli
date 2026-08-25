//! End-to-end CLI tests: config errors via assert_cmd, happy path via
//! assert_cmd against a wiremock server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `otl` command with Outline env scrubbed for deterministic tests.
fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env_remove("OUTLINE_URL").env_remove("OUTLINE_API_KEY");
    cmd
}

#[test]
fn missing_api_key_exits_2_without_network() {
    otl()
        // Point at a closed port: if the CLI ever tried the network the
        // error would differ from the config message asserted below.
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .args(["api", "documents.info", "id=doc-123"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("OUTLINE_API_KEY"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn missing_url_exits_2_with_example() {
    otl()
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "documents.info", "id=doc-123"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("OUTLINE_URL"))
        .stderr(predicate::str::contains("export OUTLINE_URL="));
}

#[test]
fn unknown_operation_exits_2() {
    otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "documents.nope"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("documents.nope"));
}

#[test]
fn base_url_with_credentials_exits_2_and_never_leaks_password() {
    let output = otl()
        .env("OUTLINE_URL", "http://alice:url-password@127.0.0.1:9")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "documents.info", "id=doc-123"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        !stderr.contains("url-password") && !stdout.contains("url-password"),
        "credential leaked: {stderr}"
    );
    assert!(stderr.contains("credentials"), "stderr: {stderr}");
}

#[test]
fn unparseable_base_url_exits_2_not_1() {
    otl()
        .env("OUTLINE_URL", "http://[::1")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "documents.info", "id=doc-123"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("invalid base URL"));
}

#[test]
fn base_url_with_query_exits_2() {
    otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9?tenant=x")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "documents.info", "id=doc-123"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("query"));
}

#[test]
fn malformed_argument_exits_2() {
    otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "documents.info", "id"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("key=value"));
}

#[tokio::test(flavor = "multi_thread")]
async fn success_prints_data_field_pretty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("content-type", "application/json"))
        .and(header("accept", "application/json"))
        .and(body_json(json!({ "id": "doc-123" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "doc-123", "title": "Hello World" },
            "status": 200,
            "ok": true
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_URL", uri)
            .env("OUTLINE_API_KEY", "test-key")
            .args(["api", "documents.info", "id=doc-123"])
            .assert()
    })
    .await
    .unwrap();

    // Golden file: full stdout must match byte-for-byte.
    assert.success().stdout(predicate::eq(include_str!(
        "golden/documents_info_data.txt"
    )));
}

/// Run `otl api documents.info id=x` against a mock returning `template`,
/// and hand back the finished assertion.
async fn assert_for_response(template: ResponseTemplate) -> assert_cmd::assert::Assert {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(template)
        .mount(&server)
        .await;

    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_URL", uri)
            .env("OUTLINE_API_KEY", "test-key")
            .args(["api", "documents.info", "id=x"])
            .assert()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn not_found_exits_5_with_server_message() {
    let assert = assert_for_response(ResponseTemplate::new(404).set_body_json(json!({
        "ok": false,
        "error": "not_found",
        "message": "document not found"
    })))
    .await;

    assert
        .failure()
        .code(5)
        .stderr(predicate::str::contains("not found (HTTP 404)"))
        .stderr(predicate::str::contains("document not found"))
        .stdout(predicate::str::is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_error_exits_4_with_key_hint() {
    let assert = assert_for_response(ResponseTemplate::new(401).set_body_json(json!({
        "ok": false,
        "error": "authentication_required",
        "message": "Authentication error"
    })))
    .await;

    assert
        .failure()
        .code(4)
        .stderr(predicate::str::contains("authentication failed (HTTP 401)"))
        .stderr(predicate::str::contains("Authentication error"))
        .stderr(predicate::str::contains("OUTLINE_API_KEY"));
}

#[tokio::test(flavor = "multi_thread")]
async fn forbidden_exits_4() {
    let assert = assert_for_response(ResponseTemplate::new(403).set_body_json(json!({
        "ok": false,
        "error": "authorization_required",
        "message": "Authorization error"
    })))
    .await;

    assert
        .failure()
        .code(4)
        .stderr(predicate::str::contains("permission denied (HTTP 403)"))
        .stderr(predicate::str::contains("Authorization error"));
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_request_exits_3_with_error_code() {
    let assert = assert_for_response(ResponseTemplate::new(400).set_body_json(json!({
        "ok": false,
        "error": "validation_error",
        "message": "id: Invalid uuid"
    })))
    .await;

    assert
        .failure()
        .code(3)
        .stderr(predicate::str::contains("request rejected (HTTP 400)"))
        .stderr(predicate::str::contains("id: Invalid uuid"))
        .stderr(predicate::str::contains("validation_error"));
}

#[tokio::test(flavor = "multi_thread")]
async fn server_5xx_exits_6_with_retry_hint() {
    let assert = assert_for_response(
        ResponseTemplate::new(503).set_body_json(json!({ "message": "Service unavailable" })),
    )
    .await;

    assert
        .failure()
        .code(6)
        .stderr(predicate::str::contains("server error (HTTP 503)"))
        .stderr(predicate::str::contains("Service unavailable"))
        .stderr(predicate::str::contains("retry"));
}

#[test]
fn network_unreachable_exits_7_with_retry_suggestion() {
    // Nothing listens on port 9: connection refused at the transport level.
    otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "documents.info", "id=x"])
        .assert()
        .failure()
        .code(7)
        .stderr(predicate::str::contains("network error"))
        .stderr(predicate::str::contains("retry"))
        .stdout(predicate::str::is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn response_without_data_prints_whole_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_URL", uri)
            .env("OUTLINE_API_KEY", "test-key")
            .args(["api", "documents.info"])
            .assert()
    })
    .await
    .unwrap();

    // Golden file: full stdout must match byte-for-byte.
    assert
        .success()
        .stdout(predicate::eq(include_str!("golden/no_data_envelope.txt")));
}

/// Mount a canned `documents.list` response and return the server.
async fn documents_list_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "id": "doc-1", "title": "Welcome", "updatedAt": "2026-08-01T10:00:00.000Z" },
                { "id": "doc-2", "title": "Roadmap", "updatedAt": "2026-08-20T15:30:00.000Z" }
            ],
            "pagination": { "offset": 0, "limit": 25 },
            "status": 200,
            "ok": true
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn json_flag_prints_raw_json_for_lists() {
    let server = documents_list_server().await;
    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_URL", uri)
            .env("OUTLINE_API_KEY", "test-key")
            .args(["api", "--json", "documents.list"])
            .assert()
    })
    .await
    .unwrap();

    // Golden file: jq-consumable pretty JSON, no color or decoration.
    assert.success().stdout(predicate::eq(include_str!(
        "golden/documents_list_json.txt"
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn non_tty_stdout_defaults_to_raw_json() {
    // assert_cmd pipes stdout (not a TTY), so without --json the output
    // must be byte-identical to the --json output: raw JSON, no ANSI.
    let server = documents_list_server().await;
    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_URL", uri)
            .env("OUTLINE_API_KEY", "test-key")
            .args(["api", "documents.list"])
            .assert()
    })
    .await
    .unwrap();

    assert
        .success()
        .stdout(predicate::eq(include_str!(
            "golden/documents_list_json.txt"
        )))
        .stdout(predicate::str::contains('\u{1b}').not());
}

#[test]
fn base_url_path_secret_never_reaches_stderr() {
    // Reviewer PoC: a secret in the base URL PATH (token-in-path auth) plus
    // the same value as API key. Nothing listens on port 9, so the request
    // fails at the transport level - and neither the Transport Display nor
    // the reqwest source chain may put the secret on stderr.
    let output = otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9/PATH-SECRET-9c7a")
        .env("OUTLINE_API_KEY", "PATH-SECRET-9c7a")
        .args(["api", "documents.info", "id=x"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(7), "stderr: {stderr}");
    assert_eq!(
        stderr.matches("PATH-SECRET").count(),
        0,
        "path secret leaked: {stderr}"
    );
    assert!(!stdout.contains("PATH-SECRET"), "stdout leaked: {stdout}");
    assert!(
        stderr.contains("http://127.0.0.1:9"),
        "origin missing from diagnostics: {stderr}"
    );
}

#[test]
fn cli_error_debug_and_chain_are_credential_free() {
    // The otl-level error type must stay credential-free even under {:?}
    // and when its anyhow chain is walked, with a realistic engine error
    // from the PATH-SECRET PoC as payload.
    use std::borrow::Cow;

    let client =
        engine::Client::new("http://127.0.0.1:9/PATH-SECRET-9c7a", "PATH-SECRET-9c7a").unwrap();
    let op = engine::OpSpec {
        name: Cow::Borrowed("documents.info"),
        path: Cow::Borrowed("/api/documents.info"),
        params: Cow::Borrowed(&[]),
    };
    let engine_error = client.execute(&op, &[]).unwrap_err();
    let cli_error = otl::exit::CliError::failure(engine_error);

    let chain: String = cli_error
        .source
        .chain()
        .map(|err| format!(" / {err} / {err:?}"))
        .collect();
    let rendered = format!("{cli_error} / {cli_error:?}{chain}");
    assert_eq!(
        rendered.matches("PATH-SECRET").count(),
        0,
        "secret leaked: {rendered}"
    );
    assert!(
        !rendered.contains("/api/documents.info"),
        "request path leaked: {rendered}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reflected_api_key_never_reaches_stderr() {
    // A server that echoes the Authorization value in its error body must
    // not cause the API key to be printed to stderr.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "message": "invalid header: Bearer reflected-secret-key"
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_URL", uri)
            .env("OUTLINE_API_KEY", "reflected-secret-key")
            .args(["api", "documents.info", "id=x"])
            .assert()
    })
    .await
    .unwrap();

    assert
        .failure()
        .code(3)
        .stderr(predicate::str::contains("reflected-secret-key").not())
        .stderr(predicate::str::contains("Bearer ***"));
}
