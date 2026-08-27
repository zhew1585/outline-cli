//! End-to-end coverage for the stable MCP-parity workflows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use serde_json::{json, Value};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COLLECTION_ID: &str = "11111111-1111-4111-8111-111111111111";
const COMMENT_ID: &str = "22222222-2222-4222-8222-222222222222";
const ATTACHMENT_ID: &str = "33333333-3333-4333-8333-333333333333";

#[tokio::test(flavor = "multi_thread")]
async fn fetch_collection_accepts_a_url_and_combines_metadata_with_the_tree() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/collections.info"))
        .and(body_json(json!({ "id": COLLECTION_ID })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": COLLECTION_ID, "name": "Engineering" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/collections.documents"))
        .and(body_json(json!({ "id": COLLECTION_ID })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "id": "doc-1", "title": "Runbook", "children": [] }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let resource_url = format!("{uri}/collection/{COLLECTION_ID}");
    let output = common::blocking(move || {
        common::otl_at(&uri)
            .args(["fetch", "collection", &resource_url, "--json"])
            .output()
            .unwrap()
    })
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["collection"]["name"], "Engineering");
    assert_eq!(value["documents"][0]["title"], "Runbook");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_attachment_returns_the_signed_location_without_following_it() {
    let server = MockServer::start().await;
    let signed = "https://storage.example/private/file?signature=secret";
    Mock::given(method("POST"))
        .and(path("/api/attachments.redirect"))
        .and(body_json(json!({ "id": ATTACHMENT_ID })))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", signed))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = common::blocking(move || {
        common::otl_at(&uri)
            .args(["fetch", "attachment", ATTACHMENT_ID, "--json"])
            .output()
            .unwrap()
    })
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["signedUrl"], signed);
}

/// `fetch attachment` echoes the id it was given beside the signed URL, so
/// that object is AUTHORED here rather than forwarded from the server. Two
/// defences stand behind it, and this pins the outer one: the id is a
/// `format: uuid` parameter, every curated command validates strictly, and
/// `fetch` exposes no `--no-validate`, so text that could reorder a
/// terminal never even reaches the request.
///
/// The inner defence - `output::emit_authored` scrubbing what it prints - is
/// unit-tested in `commands/output.rs`, because nothing that survives this
/// check can carry a hazard into the output.
#[tokio::test(flavor = "multi_thread")]
async fn fetch_attachment_refuses_an_id_that_could_reorder_a_terminal() {
    // A right-to-left override, a zero-width joiner and a soft hyphen: none
    // is a control character, so JSON encoding would pass all three through.
    let hostile = "att\u{202e}id\u{200f}\u{00ad}";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/attachments.redirect"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "https://storage/x"))
        // Nothing may be sent: the argument is refused locally.
        .expect(0)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = common::blocking(move || {
        common::otl_at(&uri)
            .args(["fetch", "attachment", hostile, "--json"])
            .output()
            .unwrap()
    })
    .await;
    assert_eq!(
        output.status.code(),
        Some(2),
        "a hostile id must be refused"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for hazard in ['\u{202e}', '\u{200f}', '\u{00ad}'] {
        assert!(
            !stderr.contains(hazard),
            "{hazard:?} reached stderr: {stderr}"
        );
    }
    assert!(output.stdout.is_empty(), "a refused command printed data");
}

#[tokio::test(flavor = "multi_thread")]
async fn collection_archive_uses_the_real_application_api_route() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/collections.archive"))
        .and(body_json(json!({ "id": COLLECTION_ID })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": COLLECTION_ID, "archivedAt": "2026-08-27T00:00:00Z" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = common::blocking(move || {
        common::otl_at(&uri)
            .args([
                "collections",
                "delete",
                COLLECTION_ID,
                "--archive",
                "--json",
            ])
            .assert()
    })
    .await;
    assert.success();
}

#[tokio::test(flavor = "multi_thread")]
async fn comment_listing_filters_thread_and_resolution_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/comments.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "id": "resolved", "parentCommentId": COMMENT_ID,
                  "resolvedAt": "2026-08-27T00:00:00Z" },
                { "id": "open", "parentCommentId": COMMENT_ID, "resolvedAt": null }
            ],
            "pagination": { "offset": 0, "limit": 100 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = common::blocking(move || {
        common::otl_at(&uri)
            .args([
                "comments",
                "list",
                "--document",
                COLLECTION_ID,
                "--parent",
                COMMENT_ID,
                "--status",
                "resolved",
                "--json",
            ])
            .output()
            .unwrap()
    })
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value.as_array().map(Vec::len), Some(1));
    assert_eq!(value[0]["id"], "resolved");
}

#[tokio::test(flavor = "multi_thread")]
async fn resolving_a_comment_uses_the_missing_openapi_overlay_operation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/comments.resolve"))
        .and(body_json(json!({ "id": COMMENT_ID })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": COMMENT_ID, "resolvedAt": "2026-08-27T00:00:00Z" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let assert = common::blocking(move || {
        common::otl_at(&uri)
            .args(["comments", "update", COMMENT_ID, "--resolve", "--json"])
            .assert()
    })
    .await;
    assert.success();
}
