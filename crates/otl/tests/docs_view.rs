//! Story 3.2: `otl docs view <id>`.
//!
//! The pager path itself needs a terminal, which a test process piping
//! stdout does not have; that decision is unit-tested in `pager`. What is
//! covered here is the contract a script sees: a pipe gets the raw
//! markdown (never JSON), `--raw` does the same on a terminal, `--json` is
//! the opt-in machine form, and `--web` prints an absolute URL before
//! handing it to an opener.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{blocking, otl_at};
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MARKDOWN: &str = "# Deploy runbook\n\nStep one.\nStep two.\n";

/// A mock `documents.info` answering with one document.
async fn server_with(document: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .and(body_partial_json(json!({ "id": "doc-1" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": document,
            "status": 200,
            "ok": true,
        })))
        .mount(&server)
        .await;
    server
}

/// The canonical document fixture.
fn document() -> serde_json::Value {
    json!({
        "id": "doc-1",
        "title": "Deploy runbook",
        "text": MARKDOWN,
        "url": "/doc/deploy-runbook-abc123",
        "updatedAt": "2026-08-20T15:30:37.000Z",
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_piped_stdout_gets_the_raw_markdown_and_no_pager() {
    let server = server_with(document()).await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            // A pager that would fail loudly if it were ever spawned.
            .env("PAGER", "otl-no-such-pager-9c7a")
            .args(["docs", "view", "doc-1"])
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), MARKDOWN);
    assert!(
        String::from_utf8_lossy(&output.stderr).is_empty(),
        "unexpected diagnostics: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_prints_the_markdown_too() {
    let server = server_with(document()).await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", "doc-1", "--raw"])
            .output()
            .unwrap()
    })
    .await;
    assert_eq!(String::from_utf8_lossy(&output.stdout), MARKDOWN);
}

#[tokio::test(flavor = "multi_thread")]
async fn json_prints_the_document_object() {
    let server = server_with(document()).await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", "doc-1", "--json"])
            .output()
            .unwrap()
    })
    .await;

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["id"], json!("doc-1"));
    assert_eq!(parsed["text"], json!(MARKDOWN));
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_and_json_together_are_a_usage_error() {
    let server = server_with(document()).await;
    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", "doc-1", "--raw", "--json"])
            .assert()
    })
    .await;
    assert.failure().code(2);
}

#[test]
fn raw_and_web_together_are_rejected_by_clap() {
    common::otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "k")
        .args(["docs", "view", "doc-1", "--raw", "--web"])
        .assert()
        .failure()
        .code(2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_without_a_body_warns_instead_of_printing_nothing_silently() {
    let mut document = document();
    document["text"] = json!(null);
    let server = server_with(document).await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", "doc-1"])
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no markdown body"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_document_exits_5() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error": "not_found",
            "message": "Document not found",
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = blocking(move || otl_at(&uri).args(["docs", "view", "nope"]).assert()).await;
    assert.failure().code(5).stdout(predicate::str::is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn web_prints_the_absolute_url_and_launches_the_opener() {
    let server = server_with(document()).await;
    let uri = server.uri();
    let expected = format!("{uri}/doc/deploy-runbook-abc123\n");
    let output = blocking(move || {
        otl_at(&uri)
            // `true` succeeds and opens nothing, standing in for a browser.
            .env("BROWSER", "true")
            .args(["docs", "view", "doc-1", "--web"])
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn web_with_json_prints_the_link_as_an_object() {
    let server = server_with(document()).await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .env("BROWSER", "true")
            .args(["docs", "view", "doc-1", "--web", "--json"])
            .output()
            .unwrap()
    })
    .await;

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["id"], json!("doc-1"));
    assert!(
        parsed["url"]
            .as_str()
            .unwrap()
            .ends_with("/doc/deploy-runbook-abc123"),
        "{parsed}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn web_refuses_a_document_url_pointing_at_another_origin() {
    // A hostile or broken server must not be able to make `--web` open an
    // arbitrary URL. Nothing is printed and no opener runs.
    let mut document = document();
    document["url"] = json!("https://evil.example/doc/x");
    let server = server_with(document).await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .env("BROWSER", "otl-no-such-browser-9c7a")
            .args(["docs", "view", "doc-1", "--web"])
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "printed a rejected URL");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("evil.example"), "echoed the URL: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn web_without_a_url_in_the_response_is_an_error() {
    let mut document = document();
    document["url"] = json!(null);
    let server = server_with(document).await;
    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", "doc-1", "--web"])
            .assert()
    })
    .await;
    assert.failure().code(1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_that_cannot_be_launched_still_leaves_the_url_on_stdout() {
    // The URL is printed BEFORE the opener runs, so a failure to launch is
    // recoverable by hand.
    let server = server_with(document()).await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .env("BROWSER", "otl-no-such-browser-9c7a")
            .args(["docs", "view", "doc-1", "--web"])
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("/doc/deploy-runbook-abc123"),
        "the URL was not printed"
    );
}
