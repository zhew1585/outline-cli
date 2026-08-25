//! Story 3.1: `otl docs search <query>`.
//!
//! Table rendering is golden-file tested next to the code (the table only
//! appears on a TTY, which a piped test process cannot be). What is tested
//! here is everything observable through the process boundary: the request
//! that goes out, the `--json` payload that comes back, auto-pagination,
//! and the truncation warning.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{blocking, otl_at};
use predicates::prelude::*;
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A syntactically valid collection id (`format: uuid` in the spec).
const COLLECTION: &str = "77777777-7777-4777-8777-777777777777";

/// One search hit as the API shapes it: a context snippet plus the document.
fn hit(id: &str, title: &str, collection: &str) -> Value {
    json!({
        "context": "the deploy step runs on every merge",
        "ranking": 1.18,
        "document": {
            "id": id,
            "title": title,
            "collectionId": collection,
            "updatedAt": "2026-08-20T15:30:37.000Z",
        }
    })
}

/// A one-page search response (fewer rows than the applied page size, so
/// auto-pagination stops after it).
fn one_page(hits: Vec<Value>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "data": hits,
        "pagination": { "offset": 0, "limit": 100 },
        "status": 200,
        "ok": true,
    }))
}

#[tokio::test(flavor = "multi_thread")]
async fn json_output_carries_the_document_id_for_scripts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.search"))
        .and(body_partial_json(json!({ "query": "deploy" })))
        .respond_with(one_page(vec![hit("doc-1", "Deploy runbook", "col-1")]))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "search", "deploy", "--json"])
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success());
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed[0]["document"]["id"], json!("doc-1"));
    assert_eq!(
        parsed[0]["context"],
        json!("the deploy step runs on every merge")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_terminal_stdout_is_json_without_any_escape_sequences() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.search"))
        .respond_with(one_page(vec![hit("doc-1", "Deploy runbook", "col-1")]))
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = blocking(move || otl_at(&uri).args(["docs", "search", "deploy"]).assert()).await;

    assert
        .success()
        .stdout(predicate::str::contains("\"doc-1\""))
        .stdout(predicate::str::contains('\u{1b}').not());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_query_and_collection_filter_reach_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.search"))
        .and(body_partial_json(json!({
            "query": "deploy",
            "collectionId": COLLECTION,
            "offset": 0,
            "limit": 100,
        })))
        .respond_with(one_page(vec![]))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "search",
                "deploy",
                "--collection",
                COLLECTION,
                "--json",
            ])
            .assert()
    })
    .await;
    assert.success();
}

#[tokio::test(flavor = "multi_thread")]
async fn results_beyond_one_page_are_fetched_automatically() {
    // The first page comes back full (100 rows at the applied page size), so
    // the engine must ask for a second one and merge the two.
    let server = MockServer::start().await;
    let full: Vec<Value> = (0..100)
        .map(|index| hit(&format!("doc-{index}"), "Page one", "col-1"))
        .collect();
    Mock::given(method("POST"))
        .and(path("/api/documents.search"))
        .and(body_partial_json(json!({ "offset": 0 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": full,
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/documents.search"))
        .and(body_partial_json(json!({ "offset": 100 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [hit("doc-100", "Page two", "col-1")],
            "pagination": { "offset": 100, "limit": 100 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "search", "deploy", "--json"])
            .output()
            .unwrap()
    })
    .await;

    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed.as_array().map(Vec::len), Some(101));
    assert_eq!(parsed[100]["document"]["id"], json!("doc-100"));
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_truncates_with_an_explicit_stderr_warning() {
    // Truncation is never silent: the rows still go to stdout, and the
    // warning (with the remedy) goes to stderr.
    let server = MockServer::start().await;
    let full: Vec<Value> = (0..100)
        .map(|index| hit(&format!("doc-{index}"), "Hit", "col-1"))
        .collect();
    Mock::given(method("POST"))
        .and(path("/api/documents.search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": full,
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "search", "deploy", "--json", "--limit", "3"])
            .output()
            .unwrap()
    })
    .await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed.as_array().map(Vec::len), Some(3));
    assert!(stderr.contains("truncated"), "no warning: {stderr}");
    assert!(stderr.contains("--limit"), "no remedy: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unauthorized_search_exits_4() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.search"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "ok": false,
            "error": "authentication_required",
            "message": "Authentication error",
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = blocking(move || otl_at(&uri).args(["docs", "search", "deploy"]).assert()).await;

    assert
        .failure()
        .code(4)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("authentication failed"));
}

#[test]
fn a_search_without_configuration_exits_2_before_any_request() {
    common::otl()
        .args(["docs", "search", "deploy"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("OUTLINE_URL"));
}
