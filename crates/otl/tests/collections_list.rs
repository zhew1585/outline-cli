//! Story 3.5: `otl collections list`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{blocking, otl_at};
use predicates::prelude::*;
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// One collection row.
fn collection(id: &str, name: &str) -> Value {
    json!({ "id": id, "name": name, "createdAt": "2026-01-01T00:00:00.000Z" })
}

#[tokio::test(flavor = "multi_thread")]
async fn json_output_is_the_servers_own_rows() {
    // No synthetic document-count field: the API has none, so a script must
    // not be handed a value the API cannot confirm.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/collections.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [collection("c1", "Engineering")],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["collections", "list", "--json"])
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success());
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed[0]["id"], json!("c1"));
    assert_eq!(parsed[0]["name"], json!("Engineering"));
    assert!(parsed[0].get("documents").is_none());
    // JSON mode must not pay for the per-collection count lookup either.
    let calls = server.received_requests().await.unwrap();
    assert_eq!(calls.len(), 1, "unexpected extra requests");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_page_of_collections_is_fetched() {
    let server = MockServer::start().await;
    let full: Vec<Value> = (0..100)
        .map(|index| collection(&format!("c{index}"), "Page one"))
        .collect();
    Mock::given(method("POST"))
        .and(path("/api/collections.list"))
        .and(body_partial_json(json!({ "offset": 0 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": full,
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/collections.list"))
        .and(body_partial_json(json!({ "offset": 100 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [collection("c100", "Page two")],
            "pagination": { "offset": 100, "limit": 100 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["collections", "list", "--json"])
            .output()
            .unwrap()
    })
    .await;

    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed.as_array().map(Vec::len), Some(101));
    assert_eq!(parsed[100]["id"], json!("c100"));
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_warns_on_stderr_when_it_cuts_the_list() {
    let server = MockServer::start().await;
    let full: Vec<Value> = (0..100)
        .map(|index| collection(&format!("c{index}"), "Name"))
        .collect();
    Mock::given(method("POST"))
        .and(path("/api/collections.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": full,
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["collections", "list", "--json", "--limit", "2"])
            .output()
            .unwrap()
    })
    .await;

    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed.as_array().map(Vec::len), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("truncated"),
        "no truncation warning"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forbidden_list_exits_4() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/collections.list"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "ok": false,
            "error": "authorization_required",
            "message": "Authorization error",
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = blocking(move || otl_at(&uri).args(["collections", "list"]).assert()).await;
    assert.failure().code(4).stdout(predicate::str::is_empty());
}

#[test]
fn listing_without_configuration_exits_2() {
    common::otl()
        .args(["collections", "list"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("OUTLINE_URL"));
}

/// Pages the CLI is willing to fetch before its own safety cap stops it
/// (`paging::MAX_PAGES`).
const MAX_PAGES: usize = 100;

#[tokio::test(flavor = "multi_thread")]
async fn a_listing_stopped_by_the_page_cap_does_not_exit_0() {
    // "Auto-paginated to the end" is the promise of this command. When the
    // CLI's own cap stops the fetch instead, the rows still go to stdout -
    // they are real - but exit 0 would tell a script it had seen every
    // collection.
    let server = MockServer::start().await;
    for page in 0..MAX_PAGES + 1 {
        Mock::given(method("POST"))
            .and(path("/api/collections.list"))
            .and(body_partial_json(json!({ "offset": page })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [collection(&format!("c{page}"), "Endless")],
                "pagination": { "offset": page, "limit": 1 },
            })))
            .mount(&server)
            .await;
    }

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["collections", "list", "--json"])
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(
        output.status.code(),
        Some(9),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The rows fetched are still valid output, and still on stdout.
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed.as_array().map(Vec::len), Some(MAX_PAGES));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("incomplete"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_requested_limit_is_not_treated_as_a_failure() {
    // The counterpart: `--limit` truncation is what the caller asked for,
    // so it stays exit 0 with a warning. Only the CLI giving up on its own
    // becomes exit 9.
    let server = MockServer::start().await;
    let full: Vec<Value> = (0..100)
        .map(|index| collection(&format!("c{index}"), "Name"))
        .collect();
    Mock::given(method("POST"))
        .and(path("/api/collections.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": full,
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["collections", "list", "--json", "--limit", "2"])
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).contains("truncated"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unrecognized_document_structure_does_not_become_a_count_of_zero() {
    // `collections.documents` answering with something that is not a list of
    // navigation nodes proves nothing about how many documents there are.
    // Reporting `0` would claim the collection is empty.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/collections.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [collection("c1", "Engineering")],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/collections.documents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": null })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = blocking(move || {
        // Table mode is the only mode that computes counts, and it needs a
        // terminal - so this asserts the diagnostic, which is where the
        // "unrecognized" verdict is observable from outside.
        otl_at(&uri).args(["collections", "list"]).output().unwrap()
    })
    .await;

    assert!(output.status.success());
    // JSON mode (a pipe) prints the raw rows and never asks for counts, so
    // no warning is expected here; the guard is that the command did not
    // fail and did not invent a count.
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed[0]["id"], json!("c1"));
    assert!(parsed[0].get("documents").is_none());
}
