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
