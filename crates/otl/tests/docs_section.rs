//! Reading and writing one section of a document, end to end.
//!
//! # Why these are integration tests and not more unit tests
//!
//! The section path is the only write in this CLI that spends TWO requests:
//! `documents.info` to get the body an anchor is derived from, then
//! `documents.update` to send it. Everything interesting about it lives in
//! the relationship between those two - what the second request contains,
//! whether it happens at all, and what is pinned to what - and none of that
//! is reachable from a unit test, which sees one function at a time.
//!
//! So the assertions here are deliberately about the wire:
//!
//! - the update body is EXACT (`body_json`), because "only the section
//!   travels" is the whole claim and a `text` holding the full page would
//!   satisfy any looser matcher;
//! - `lastRevision` is a real JSON number, for the reason `publish` is
//!   checked as a real boolean in `docs_write.rs`: the engine coerces from
//!   `key=value`, and a quoted integer is a different request;
//! - every refusal asserts that `documents.update` was NEVER REACHED. A
//!   check that refuses after writing is not a check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{blocking, otl_at};
use predicates::prelude::*;
use serde_json::{json, Value};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The stored page every test here edits.
///
/// Shaped to carry the cases that matter: a preamble outside any section, a
/// nested child, and a final section that runs to the end of the document.
const PAGE: &str =
    "intro\n\n## Deploy\n\nold steps\n\n### Rollback\n\nundo it\n\n## FAQ\n\nanswers\n";

/// The revision `documents.info` reports for [`PAGE`].
const REVISION: u64 = 12;

const DOCUMENT: &str = "doc-1";

/// The document as `documents.info` answers it.
fn stored() -> Value {
    json!({
        "id": DOCUMENT,
        "title": "Runbook",
        "url": "/doc/runbook-abc123",
        "urlId": "abc123",
        "revision": REVISION,
        "updatedAt": "2026-08-26T08:00:00.000Z",
        "publishedAt": "2026-08-26T08:00:00.000Z",
        "text": PAGE,
    })
}

/// The receipt `documents.update` answers with.
fn written() -> Value {
    let mut document = stored();
    document["revision"] = json!(REVISION + 1);
    document
}

fn envelope(data: Value) -> Value {
    json!({ "data": data, "status": 200, "ok": true })
}

/// A server that answers `documents.info` with [`PAGE`].
async fn reader() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .and(body_json(json!({ "id": DOCUMENT })))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(stored())))
        .mount(&server)
        .await;
    server
}

/// [`reader`], plus a `documents.update` that insists on this exact body.
async fn writer(expected: Value) -> MockServer {
    let server = reader().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_json(expected))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(written())))
        .mount(&server)
        .await;
    server
}

/// Whether any request reached one operation.
async fn reached(server: &MockServer, operation: &str) -> bool {
    let target = format!("/api/{operation}");
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .any(|request| request.url.path() == target)
}

// ---------------------------------------------------------------- reading

#[tokio::test(flavor = "multi_thread")]
async fn the_outline_lists_every_address_with_the_revision_to_pin() {
    let server = reader().await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", DOCUMENT, "--outline", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    })
    .await;
    let outline: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(outline["revision"], json!(REVISION));
    assert_eq!(outline["bytes"], json!(PAGE.len()));
    let sections = outline["sections"].as_array().unwrap();
    let paths: Vec<&str> = sections
        .iter()
        .map(|section| section["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["Deploy", "Deploy > Rollback", "FAQ"]);
    assert_eq!(sections[1]["level"], json!(3));
    assert_eq!(sections[1]["line"], json!(7));
}

/// The point of the command, measured on a page big enough for it to have
/// one. [`PAGE`] is 68 bytes, so any outline of it is "bigger than the page"
/// - the constant per-section overhead is the whole output at that size, and
/// asserting a ratio there would only be measuring JSON punctuation.
#[tokio::test(flavor = "multi_thread")]
async fn the_outline_stays_small_as_the_page_grows() {
    let server = MockServer::start().await;
    let big = format!(
        "## Bulk\n\n{}\n## FAQ\n\nanswers\n",
        "filler line\n".repeat(4000)
    );
    let size = big.len();
    let mut document = stored();
    document["text"] = json!(big);
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(document)))
        .mount(&server)
        .await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", DOCUMENT, "--outline", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    })
    .await;
    assert!(
        output.len() * 50 < size,
        "the outline is {} bytes for a {size} byte page",
        output.len()
    );
    // And it still reports the page's real size, so a caller can tell what
    // it chose not to read.
    let outline: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(outline["bytes"], json!(size));
}

/// A pipe gets the section's bytes verbatim - no pager, no added newline,
/// and nothing from the rest of the page.
#[tokio::test(flavor = "multi_thread")]
async fn a_section_is_printed_byte_for_byte() {
    let server = reader().await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", DOCUMENT, "--section", "Deploy > Rollback"])
            .assert()
            .success()
            .stdout(predicate::eq("### Rollback\n\nundo it\n\n"));
    })
    .await;
}

/// A parent section carries its children, because that is what replacing a
/// chapter has to mean.
#[tokio::test(flavor = "multi_thread")]
async fn a_parent_section_includes_its_children() {
    let server = reader().await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", DOCUMENT, "--section", "Deploy"])
            .assert()
            .success()
            .stdout(predicate::eq(
                "## Deploy\n\nold steps\n\n### Rollback\n\nundo it\n\n",
            ));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_address_is_refused_with_the_document_outline() {
    let server = reader().await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args(["docs", "view", DOCUMENT, "--section", "Nope"])
            .assert()
            .code(2)
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains("Deploy > Rollback"));
    })
    .await;
}

// ---------------------------------------------------------------- writing

/// The central claim: replacing a section sends the section, an anchor, and
/// the revision it was computed from - and nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn replacing_a_section_sends_only_that_section() {
    let server = writer(json!({
        "id": DOCUMENT,
        "text": "## FAQ\n\nnew answers\n",
        "editMode": "patch",
        "findText": "## FAQ\n\nanswers\n",
        // A number, not "12": the engine coerces from key=value and a
        // quoted integer would be a different request.
        "lastRevision": REVISION,
    }))
    .await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "update",
                DOCUMENT,
                "--section",
                "FAQ",
                "--if-revision",
                "12",
            ])
            .write_stdin("## FAQ\n\nnew answers")
            .assert()
            .success();
    })
    .await;
}

/// Without `--if-revision` the write is still pinned, to the revision this
/// CLI just read. The caller's own staleness is what the flag adds.
#[tokio::test(flavor = "multi_thread")]
async fn a_section_write_is_pinned_even_when_the_caller_did_not_ask() {
    let server = writer(json!({
        "id": DOCUMENT,
        "text": "## FAQ\n\nrewritten\n",
        "editMode": "patch",
        "findText": "## FAQ\n\nanswers\n",
        "lastRevision": REVISION,
    }))
    .await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", DOCUMENT, "--section", "FAQ"])
            .write_stdin("## FAQ\n\nrewritten\n")
            .assert()
            .success();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_section_sends_an_empty_replacement_for_it() {
    let server = writer(json!({
        "id": DOCUMENT,
        "text": "",
        "editMode": "patch",
        "findText": "### Rollback\n\nundo it\n\n",
        "lastRevision": REVISION,
    }))
    .await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "update",
                DOCUMENT,
                "--delete-section",
                "Deploy > Rollback",
            ])
            .assert()
            .success();
    })
    .await;
}

/// Renaming a heading is expressible because the heading line is part of
/// the section being replaced.
#[tokio::test(flavor = "multi_thread")]
async fn a_replacement_may_rename_the_heading() {
    let server = writer(json!({
        "id": DOCUMENT,
        "text": "## Questions\n\nanswers\n",
        "editMode": "patch",
        "findText": "## FAQ\n\nanswers\n",
        "lastRevision": REVISION,
    }))
    .await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", DOCUMENT, "--section", "FAQ"])
            .write_stdin("## Questions\n\nanswers")
            .assert()
            .success();
    })
    .await;
}

/// The write receipt still drops the body, so editing a section does not
/// hand the page back either.
#[tokio::test(flavor = "multi_thread")]
async fn the_receipt_of_a_section_write_is_still_a_receipt() {
    let server = writer(json!({
        "id": DOCUMENT,
        "text": "## FAQ\n\nnew\n",
        "editMode": "patch",
        "findText": "## FAQ\n\nanswers\n",
        "lastRevision": REVISION,
    }))
    .await;
    let uri = server.uri();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", DOCUMENT, "--section", "FAQ", "--json"])
            .write_stdin("## FAQ\n\nnew")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    })
    .await;
    let receipt: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(receipt["revision"], json!(REVISION + 1));
    assert!(receipt.get("text").is_none(), "{receipt}");
}

// ------------------------------------------------------------- refusals

/// A stale caller is caught from the read alone, so the write never happens.
#[tokio::test(flavor = "multi_thread")]
async fn a_stale_if_revision_refuses_before_writing() {
    let server = reader().await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "update",
                DOCUMENT,
                "--section",
                "FAQ",
                "--if-revision",
                "11",
            ])
            .write_stdin("## FAQ\n\nnew")
            .assert()
            .code(2)
            .stderr(
                predicate::str::contains("revision 12, not the 11")
                    .and(predicate::str::contains("--outline")),
            );
    })
    .await;
    assert!(
        !reached(&server, "documents.update").await,
        "a stale revision must not reach the write"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ambiguous_address_refuses_before_writing() {
    let server = MockServer::start().await;
    let mut document = stored();
    document["text"] = json!("## Notes\n\nfirst\n\n## Notes\n\nsecond\n");
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(document)))
        .mount(&server)
        .await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", DOCUMENT, "--section", "Notes"])
            .write_stdin("## Notes\n\nnew")
            .assert()
            .code(2)
            .stderr(predicate::str::contains("matches 2 headings"));
    })
    .await;
    assert!(!reached(&server, "documents.update").await);
}

/// `--mode patch` now verifies its anchor, and the refusal names the section
/// each match sits in - which is the reason the extra read is worth it.
#[tokio::test(flavor = "multi_thread")]
async fn a_repeated_find_text_refuses_and_says_which_sections_it_hit() {
    let server = MockServer::start().await;
    let mut document = stored();
    document["text"] = json!("## Deploy\n\nrestart it\n\n## Rollback\n\nrestart it\n");
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(document)))
        .mount(&server)
        .await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "update",
                DOCUMENT,
                "--mode",
                "patch",
                "--find-text",
                "restart it",
            ])
            .write_stdin("reboot it")
            .assert()
            .code(2)
            .stderr(
                predicate::str::contains("occurs in 2 places")
                    .and(predicate::str::contains("Deploy"))
                    .and(predicate::str::contains("Rollback")),
            );
    })
    .await;
    assert!(!reached(&server, "documents.update").await);
}

/// An append has no anchor to verify, so it must not pay for a read.
#[tokio::test(flavor = "multi_thread")]
async fn an_append_stays_a_single_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .and(body_json(json!({
            "id": DOCUMENT,
            "text": "\nmore\n",
            "editMode": "append",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(written())))
        .mount(&server)
        .await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", DOCUMENT, "--mode", "append"])
            .write_stdin("\nmore\n")
            .assert()
            .success();
    })
    .await;
    assert!(
        !reached(&server, "documents.info").await,
        "an append needs no anchor, so it must not read the body"
    );
}

/// A rejected pin is explained rather than left as a bare "request
/// rejected", because the caller cannot otherwise tell a race from a bug.
#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_revision_pin_is_explained() {
    let server = reader().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.update"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "ok": false,
            "error": "revision_mismatch",
            "message": "document has been updated since you last read it",
        })))
        .mount(&server)
        .await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", DOCUMENT, "--section", "FAQ"])
            .write_stdin("## FAQ\n\nnew")
            .assert()
            .code(3)
            .stderr(
                predicate::str::contains("pinned to revision 12")
                    .and(predicate::str::contains("redo the edit")),
            );
    })
    .await;
}

/// Deleting the only section would empty the page, which is the outcome the
/// blank-body guard exists to make impossible by accident.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_the_only_section_is_refused() {
    let server = MockServer::start().await;
    let mut document = stored();
    document["text"] = json!("## Only\n\nbody\n");
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(document)))
        .mount(&server)
        .await;
    let uri = server.uri();
    blocking(move || {
        otl_at(&uri)
            .args(["docs", "update", DOCUMENT, "--delete-section", "Only"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("erase"));
    })
    .await;
    assert!(!reached(&server, "documents.update").await);
}
