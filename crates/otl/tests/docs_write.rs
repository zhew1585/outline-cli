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

/// A created/updated document as the API answers it - body included, which
/// is what the write receipt has to drop.
fn document() -> Value {
    json!({
        "id": "doc-new",
        "title": "Notes",
        "url": "/doc/notes-xyz789",
        "urlId": "xyz789",
        "updatedAt": "2026-08-26T08:00:00.000Z",
        "revision": 1,
        "publishedAt": "2026-08-26T08:00:00.000Z",
        "text": BODY_THE_SERVER_ECHOES,
        "data": { "type": "doc", "content": [] },
        "updatedBy": { "id": "user-1", "name": "Ada" },
    })
}

/// The stored body, as Outline echoes it back on every write.
const BODY_THE_SERVER_ECHOES: &str = "# Notes\n\nthe whole stored page\n";

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

#[tokio::test(flavor = "multi_thread")]
async fn the_write_receipt_does_not_echo_the_document_body() {
    // An agent appending one line to a large page used to get the whole
    // page back, and had to hold it in context to read the revision.
    let server = server_for("documents.update").await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "doc-1", "--mode", "append", "--json"])
            .write_stdin("one more line\n")
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        !stdout.contains("the whole stored page"),
        "the receipt echoed the body back:\n{stdout}"
    );
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    // Identity survives; the body and the nested actor do not.
    assert_eq!(parsed["id"], json!("doc-new"));
    assert_eq!(parsed["revision"], json!(1));
    assert_eq!(parsed["urlId"], json!("xyz789"));
    for dropped in ["text", "data", "updatedBy"] {
        assert!(
            parsed.get(dropped).is_none(),
            "{dropped} survived: {stdout}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_verbatim_response_is_still_one_command_away() {
    // The receipt is a projection, so the escape hatch is part of the
    // contract: `otl api` forwards the same operation unfiltered.
    let server = server_for("documents.update").await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args([
                "api",
                "documents.update",
                "id=doc-1",
                "title=Renamed",
                "--json",
            ])
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("the whole stored page"),
        "otl api must still round-trip the response:\n{stdout}"
    );
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

/// A file exactly as `otl docs export` writes one.
fn exported_file(revision: u64) -> String {
    format!(
        "---\n\
         outline_id: \"doc-new\"\n\
         outline_url_id: \"xyz789\"\n\
         title: \"Notes\"\n\
         revision: {revision}\n\
         updated_at: \"2026-08-26T08:00:00.000Z\"\n\
         ---\n\
         \n\
         {NOTES}"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exported_file_is_written_back_without_its_block_and_without_an_id() {
    // The round trip this feature exists for: no ID argument, the metadata
    // block must not reach the server as document text, and the block's
    // revision becomes the pin - a real JSON number, not "1".
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_json(json!({
            "id": "doc-new",
            "text": NOTES,
            "lastRevision": 1,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": document(), "status": 200, "ok": true,
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Notes.md");
    std::fs::write(&file, exported_file(1)).unwrap();

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "--file"])
            .arg(&file)
            .write_stdin("")
            .assert()
    })
    .await;
    // A mismatched body would never match the mock, so success IS the
    // assertion that the block was stripped.
    assert.success();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_edited_since_the_export_is_refused_rather_than_overwritten() {
    // The pin travels as `lastRevision`, so the server is the one that
    // refuses - which is what makes it atomic rather than a read followed
    // by a hopeful write.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_partial_json(json!({ "lastRevision": 1 })))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "ok": false,
            "error": "revision_conflict",
            "message": "Document has been updated since the given revision",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Notes.md");
    std::fs::write(&file, exported_file(1)).unwrap();

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "--file"])
            .arg(&file)
            .write_stdin("")
            .assert()
    })
    .await;
    assert
        .failure()
        .code(3)
        .stderr(predicate::str::contains("pinned to revision 1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn force_overwrites_the_newer_version() {
    // --force drops the block's revision, so no pin travels at all and the
    // write lands on whatever the document is now.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_json(json!({ "id": "doc-new", "text": NOTES })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": document(), "status": 200, "ok": true,
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Notes.md");
    std::fs::write(&file, exported_file(1)).unwrap();

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "--force", "--file"])
            .arg(&file)
            .write_stdin("")
            .assert()
    })
    .await;
    assert.success();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_id_that_contradicts_the_file_is_refused_before_any_request() {
    // No mocks at all: the refusal must be local, so a mistyped id never
    // gets the chance to overwrite an unrelated document.
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Notes.md");
    std::fs::write(&file, exported_file(1)).unwrap();

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "some-other-doc", "--file"])
            .arg(&file)
            .write_stdin("")
            .assert()
    })
    .await;
    assert
        .failure()
        .code(2)
        .stderr(predicate::str::contains("doc-new"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_short_id_from_a_url_is_not_a_contradiction() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_partial_json(json!({ "id": "xyz789" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": document(), "status": 200, "ok": true,
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Notes.md");
    std::fs::write(&file, exported_file(1)).unwrap();

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "xyz789", "--file"])
            .arg(&file)
            .write_stdin("")
            .assert()
    })
    .await;
    assert.success();
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_from_an_exported_file_strips_the_block_and_says_what_it_did() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.create"))
        .and(body_json(json!({
            "text": NOTES,
            "collectionId": COLLECTION,
            "publish": true,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": document(), "status": 200, "ok": true,
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Notes.md");
    std::fs::write(&file, exported_file(1)).unwrap();

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "create", "--collection", COLLECTION, "--file"])
            .arg(&file)
            .write_stdin("")
            .assert()
    })
    .await;
    assert
        .success()
        .stderr(predicate::str::contains("creating a NEW document"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_piped_body_still_costs_exactly_one_request() {
    // No block, so nothing to pin to: the request must carry no
    // lastRevision, and no documents.info may be spent looking for one.
    let server = server_for("documents.update").await;
    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", "doc-new"])
            .write_stdin(NOTES)
            .assert()
    })
    .await;
    assert.success();
}
