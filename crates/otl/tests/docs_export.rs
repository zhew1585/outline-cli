//! Story 3.6: `otl docs export` - what ends up in the exported tree, and
//! how the run accounts for it.
//!
//! Robustness and hostile-input cases live in `docs_export_safety.rs`; the
//! two files share their fixtures through `common::export`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::export::{row, server_with, tree, COLLECTION};
use common::{blocking, otl_at};
use predicates::prelude::*;
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path as request_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread")]
async fn a_hierarchy_becomes_nested_directories() {
    // Alpha has a child, so it becomes `Alpha/` holding its own `Alpha.md`
    // and the child; Solo is a leaf and stays one file.
    let server = server_with(vec![
        row("a", "Alpha", None),
        row("b", "Beta", Some("a")),
        row("s", "Solo", None),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");

    let uri = server.uri();
    let target = out.clone();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .assert()
    })
    .await;
    assert.success();

    assert_eq!(
        tree(&out),
        BTreeSet::from([
            "Alpha/Alpha.md".to_string(),
            "Alpha/Beta.md".to_string(),
            "Solo.md".to_string(),
        ])
    );
    let alpha = std::fs::read_to_string(out.join("Alpha/Alpha.md")).unwrap();
    // The real title survives as a heading even when the file name was
    // sanitized away from it.
    assert!(alpha.starts_with("# Alpha\n\n"), "{alpha:?}");
    assert!(alpha.contains("body of a"), "{alpha:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_page_of_the_collection_is_exported() {
    let server = MockServer::start().await;
    let full: Vec<Value> = (0..100)
        .map(|index| row(&format!("d{index}"), &format!("Doc {index}"), None))
        .collect();
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .and(body_partial_json(json!({ "offset": 0 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": full,
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .and(body_partial_json(json!({ "offset": 100 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [row("d100", "Doc 100", None)],
            "pagination": { "offset": 100, "limit": 100 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "x", "title": "T", "text": "body\n" },
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .assert()
    })
    .await;
    assert.success();

    // 101 documents across two pages: nothing silently dropped at the page
    // boundary.
    assert_eq!(tree(&out).len(), 101);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_failing_document_exits_9_and_the_rest_are_written() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [row("ok", "Good", None), row("bad", "Broken", None)],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .and(body_partial_json(json!({ "id": "ok" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "ok", "title": "Good", "text": "kept\n" },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .and(body_partial_json(json!({ "id": "bad" })))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error": "not_found",
            "message": "Document not found",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(9), "expected partial failure");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("bad"), "failure not reported: {stderr}");
    assert!(
        stderr.contains("could not be exported"),
        "no summary: {stderr}"
    );
    // The good document is on disk and the failed one is not.
    assert_eq!(tree(&out), BTreeSet::from(["Good.md".to_string()]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_enumeration_reports_its_own_exit_code_and_writes_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error": "not_found",
            "message": "Collection not found",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .assert()
    })
    .await;

    // Not 9: nothing was exported, so this is a plain not-found.
    assert.failure().code(5);
    assert!(tree(&out).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_empty_output_directory_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("keep.md"), "mine").unwrap();

    let uri = server.uri();
    let target = out.clone();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .assert()
    })
    .await;

    assert
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--overwrite"));
    // No request was made: the check is local and comes first.
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(out.join("keep.md")).unwrap(),
        "mine"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn overwrite_allows_exporting_into_an_existing_directory() {
    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("Alpha.md"), "stale").unwrap();

    let uri = server.uri();
    let target = out.clone();
    let assert = blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "export",
                "--collection",
                COLLECTION,
                "--overwrite",
                "--out",
            ])
            .arg(&target)
            .assert()
    })
    .await;
    assert.success();

    let written = std::fs::read_to_string(out.join("Alpha.md")).unwrap();
    assert!(written.contains("body of a"), "{written:?}");
    assert!(!written.contains("stale"), "{written:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_written_paths_go_to_stdout_and_progress_to_stderr() {
    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    // In JSON mode (non-terminal stdout) stdout is the machine summary and
    // every human word is on stderr.
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["exported"], json!(["Alpha.md"]));
    assert_eq!(parsed["failed"], json!([]));
    assert!(String::from_utf8_lossy(&output.stderr).contains("exported 1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_collection_exports_nothing_and_says_so() {
    let server = server_with(vec![]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    assert!(output.status.success());
    assert!(tree(&out).is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no documents"));
}

/// Pages the CLI is willing to fetch before its own safety cap stops it
/// (`paging::MAX_PAGES`). Kept in sync by the assertion in the test below:
/// if the cap changes, the test stops proving anything and says so.
const MAX_PAGES: usize = 100;

/// Mount a `documents.list` that never runs out of pages.
///
/// Each page reports an applied page size of 1 and returns exactly one row,
/// so the engine always sees a full page and keeps asking - until its own
/// page cap stops it. That is the shape of a collection larger than the CLI
/// can enumerate, without needing 10,001 fixtures.
async fn server_that_never_runs_out(row_id: &str) -> MockServer {
    let server = MockServer::start().await;
    for page in 0..MAX_PAGES + 1 {
        Mock::given(method("POST"))
            .and(request_path("/api/documents.list"))
            .and(body_partial_json(json!({ "offset": page })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [row(row_id, "Endless", None)],
                "pagination": { "offset": page, "limit": 1 },
            })))
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": row_id, "title": "Endless", "text": "body\n" },
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn an_enumeration_stopped_by_the_page_cap_is_not_a_successful_backup() {
    // The regression this exists for: the pagination cap used to produce a
    // stderr warning and exit 0, so an automated backup of a collection
    // larger than the cap was recorded as complete. The files that WERE
    // written are kept - they are real - but the exit code and the JSON
    // summary both have to say the copy is partial.
    let server = server_that_never_runs_out("d1").await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");

    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(
        output.status.code(),
        Some(9),
        "an incomplete export must not exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("NOT a complete copy"),
        "the summary must say the export is partial: {stderr}"
    );

    // The machine-readable summary must carry the same verdict, because a
    // backup script reads stdout and not stderr.
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["complete"], json!(false));
    assert_eq!(parsed["enumeration_truncated"], json!(true));

    // The page cap really was reached (and not, say, a mock mismatch that
    // ended the fetch early), so this test is measuring what it claims to.
    let calls = server.received_requests().await.unwrap();
    let list_calls = calls
        .iter()
        .filter(|call| call.url.path() == "/api/documents.list")
        .count();
    assert_eq!(
        list_calls, MAX_PAGES,
        "the fetch did not reach the page cap; MAX_PAGES may have changed"
    );

    // Every page returned the same document id: it is exported once, not a
    // hundred times.
    assert_eq!(tree(&out), BTreeSet::from(["Endless.md".to_string()]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_with_no_markdown_body_is_a_failure_not_an_empty_file() {
    // `documents.info` answering without `text` used to produce a file
    // holding only the title heading, counted as exported, exit 0.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [row("ok", "Good", None), row("nobody", "Bodyless", None)],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .and(body_partial_json(json!({ "id": "ok" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "ok", "title": "Good", "text": "kept\n" },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .and(body_partial_json(json!({ "id": "nobody" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            // Null body, and a second variant (an absent field) is covered
            // by the same code path.
            "data": { "id": "nobody", "title": "Bodyless", "text": null },
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(9));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("nobody"), "not reported: {stderr}");
    assert!(stderr.contains("no markdown body"), "{stderr}");
    // No file for the bodyless document: an empty-looking .md would read as
    // a successfully backed-up empty document.
    assert_eq!(tree(&out), BTreeSet::from(["Good.md".to_string()]));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_document_body_is_still_exported() {
    // The counterpart to the test above: an empty STRING is a real document
    // body, and must not be mistaken for a missing one.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [row("blank", "Blank", None)],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "blank", "title": "Blank", "text": "" },
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .assert()
    })
    .await;
    assert.success();
    assert_eq!(tree(&out), BTreeSet::from(["Blank.md".to_string()]));
    assert_eq!(
        std::fs::read_to_string(out.join("Blank.md")).unwrap(),
        "# Blank\n\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_requested_limit_is_not_reported_as_an_incomplete_export() {
    // The counterpart to `an_enumeration_stopped_by_the_page_cap_...`, and
    // the boundary docs/exit-codes.md publishes: `--limit N` stopping at N
    // documents is the requested outcome, so exit 0. Reporting 9 here would
    // contradict the documented contract and make a deliberate partial
    // export indistinguishable from a broken one.
    let server = server_with(vec![
        row("a", "Alpha", None),
        row("b", "Beta", None),
        row("c", "Gamma", None),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");

    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args([
                "docs",
                "export",
                "--collection",
                COLLECTION,
                "--limit",
                "1",
                "--out",
            ])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(
        output.status.code(),
        Some(0),
        "--limit must not be reported as a failure; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["complete"], json!(true));
    assert_eq!(parsed["enumeration_truncated"], json!(false));
    // But the fact that the export covers only part of the collection is
    // still visible, for a script asking "is this the whole collection?".
    assert_eq!(parsed["limit_reached"], json!(true));
    assert_eq!(tree(&out).len(), 1);
    // And the ordinary truncation warning still went to stderr.
    assert!(String::from_utf8_lossy(&output.stderr).contains("truncated"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_listing_row_without_an_id_is_counted_as_a_failure() {
    // The server listed a document; nothing could be fetched for it because
    // the row carried no id. Dropping it with only a warning let the export
    // claim `complete: true` while missing a document the server named.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "title": "Unidentified" },
                { "id": "", "title": "Empty id" },
                { "id": 42, "title": "Numeric id" },
                row("ok", "Good", None),
            ],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "ok", "title": "Good", "text": "kept\n" },
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(9));
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["complete"], json!(false));
    // One entry per unusable row, each locatable in the listing.
    let failed = parsed["failed"].as_array().expect("a failed array");
    assert_eq!(failed.len(), 3, "{parsed}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("listing row 1 (Unidentified)"), "{stderr}");
    assert!(stderr.contains("no usable id"), "{stderr}");
    // The document that could be fetched is still exported.
    assert_eq!(tree(&out), BTreeSet::from(["Good.md".to_string()]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_duplicate_row_is_not_counted_as_a_failure() {
    // The counterpart: a repeated id is not a missing document, so it must
    // not turn a healthy export into a partial one.
    let server = server_with(vec![row("a", "Alpha", None), row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(0));
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["complete"], json!(true));
    assert_eq!(tree(&out), BTreeSet::from(["Alpha.md".to_string()]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_collection_of_only_unusable_rows_does_not_claim_to_be_empty() {
    // Two contradictory diagnostics used to appear together: "this
    // collection has no documents to export" and a list of documents that
    // could not be exported.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "title": "Unidentified" }],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(9));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("no documents to export"),
        "claimed the collection was empty while reporting a failure: {stderr}"
    );
    assert!(stderr.contains("listing row 1"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unusable_row_has_no_id_to_retry_with() {
    // `failed[].id` is documented as what a script feeds back to the API.
    // A listing row that never had an id has nothing to feed back, so the
    // field is null rather than a human-readable stand-in - handing
    // `documents.info` the string "listing row 1 (Unidentified)" would
    // produce a local validation error instead of the real explanation.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "title": "Unidentified" },
                row("ok", "Good", None),
            ],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error": "not_found",
            "message": "Document not found",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&target)
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(9));
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    let failed = parsed["failed"].as_array().expect("a failed array");
    assert_eq!(failed.len(), 2, "{parsed}");

    // The unusable row: no id, a label that locates it in the listing.
    let unusable = failed
        .iter()
        .find(|entry| entry["id"].is_null())
        .expect("the unusable row must report a null id");
    assert_eq!(unusable["label"], json!("listing row 1 (Unidentified)"));

    // The document that had an id keeps it, verbatim, for a retry.
    let retryable = failed
        .iter()
        .find(|entry| !entry["id"].is_null())
        .expect("the document with an id");
    assert_eq!(retryable["id"], json!("ok"));
    assert_eq!(retryable["label"], json!("ok"));
}
