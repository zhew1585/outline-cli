//! Stories 3.3 and 3.4: `otl docs create` and `otl docs update`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{blocking, otl_at};
use predicates::prelude::*;
use serde_json::{json, Value};
use wiremock::matchers::{body_json, body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A syntactically valid collection id (`format: uuid` in the spec).
const COLLECTION: &str = "11111111-1111-4111-8111-111111111111";

const NOTES: &str = "# Notes\n\nsomething worth keeping\n";

/// A created/updated document as the API answers it.
fn document() -> Value {
    json!({
        "id": "doc-new",
        "title": "Notes",
        "url": "/doc/notes-xyz789",
        "updatedAt": "2026-08-26T08:00:00.000Z",
        "revision": 1,
        "publishedAt": "2026-08-26T08:00:00.000Z",
    })
}

/// Mount one operation answering with [`document`].
async fn server_for(operation: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/{operation}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": document(),
            "status": 200,
            "ok": true,
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn stdin_becomes_the_document_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.create"))
        // Exact body: nothing extra may be invented, and `publish` must be
        // a real boolean rather than the string "true".
        .and(body_json(json!({
            "text": NOTES,
            "title": "Notes",
            "collectionId": COLLECTION,
            "publish": true,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": document() })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "create",
                "--title",
                "Notes",
                "--collection",
                COLLECTION,
                "--json",
            ])
            .write_stdin(NOTES)
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success(), "{:?}", output);
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["id"], json!("doc-new"));
    assert_eq!(parsed["url"], json!("/doc/notes-xyz789"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_is_equivalent_to_stdin() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.create"))
        .and(body_json(json!({
            "text": NOTES,
            "title": "Notes",
            "collectionId": COLLECTION,
            "publish": true,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": document() })))
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, NOTES).unwrap();

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "create",
                "--title",
                "Notes",
                "--collection",
                COLLECTION,
            ])
            .arg("--file")
            .arg(&file)
            .args(["--json"])
            .assert()
    })
    .await;
    assert.success();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_human_summary_shows_the_id_and_the_absolute_url() {
    // Table mode needs a terminal, so the summary itself is golden-tested
    // next to the code. This checks the JSON contract instead: the id and
    // the server's relative URL are both present for a script to use.
    let server = server_for("documents.create").await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "create",
                "--title",
                "Notes",
                "--collection",
                COLLECTION,
            ])
            .write_stdin(NOTES)
            .output()
            .unwrap()
    })
    .await;
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["id"], json!("doc-new"));
    assert!(parsed["url"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn draft_suppresses_publishing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.create"))
        .and(body_partial_json(json!({ "publish": false })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": document() })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "create",
                "--title",
                "N",
                "--collection",
                COLLECTION,
                "--draft",
                "--json",
            ])
            .write_stdin(NOTES)
            .assert()
    })
    .await;
    assert.success();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_destination_the_draft_notice_appears_and_publish_is_absent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.create"))
        .and(body_json(json!({ "text": NOTES, "title": "N" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": document() })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "create", "--title", "N", "--json"])
            .write_stdin(NOTES)
            .assert()
    })
    .await;
    assert.success().stderr(predicate::str::contains("draft"));
}

#[test]
fn creating_without_a_body_is_a_usage_error_before_any_request() {
    // Empty stdin (the script case) is "no body", not "an empty document".
    common::otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "k")
        .args(["docs", "create", "--title", "N", "--collection", COLLECTION])
        .write_stdin("")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("no document body"));
}

#[test]
fn creating_from_a_blank_body_is_a_usage_error() {
    common::otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "k")
        .args(["docs", "create", "--title", "N", "--collection", COLLECTION])
        .write_stdin("   \n\t\n")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn creating_from_a_missing_file_is_a_usage_error() {
    common::otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "k")
        .args([
            "docs",
            "create",
            "--title",
            "N",
            "--file",
            "/nonexistent/notes-9c7a.md",
        ])
        .assert()
        .failure()
        .code(2);
}

#[tokio::test(flavor = "multi_thread")]
async fn update_sends_the_title_alone() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_json(json!({ "id": "doc-1", "title": "Renamed" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": document() })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "doc-1", "--title", "Renamed", "--json"])
            // A script's stdin: present but empty. It must not read as
            // "replace the body with nothing".
            .write_stdin("")
            .assert()
    })
    .await;
    assert.success();
}

#[tokio::test(flavor = "multi_thread")]
async fn update_sends_a_new_body_from_stdin() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_json(json!({ "id": "doc-1", "text": NOTES })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": document() })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "doc-1", "--json"])
            .write_stdin(NOTES)
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success(), "{:?}", output);
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["revision"], json!(1));
}

#[test]
fn updating_nothing_is_a_usage_error_before_any_request() {
    common::otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "k")
        .args(["docs", "update", "doc-1"])
        .write_stdin("")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("nothing to update"));
}

#[tokio::test(flavor = "multi_thread")]
async fn updating_an_unknown_document_exits_5() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error": "not_found",
            "message": "Document not found",
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "nope", "--title", "X"])
            .write_stdin("")
            .assert()
    })
    .await;
    assert.failure().code(5);
}
