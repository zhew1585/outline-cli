//! Story 3.6: `otl docs export --collection <id> --out <dir>`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::{blocking, otl_at};
use predicates::prelude::*;
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path as request_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A syntactically valid collection id. The vendored spec declares
/// `collectionId` as `format: uuid`, so the engine rejects anything else
/// locally - before any request, which is the intended behaviour.
const COLLECTION: &str = "11111111-1111-4111-8111-111111111111";

/// One row of `documents.list`.
fn row(id: &str, title: &str, parent: Option<&str>) -> Value {
    let mut row = json!({ "id": id, "title": title, "updatedAt": "2026-08-01T00:00:00.000Z" });
    if let Some(parent) = parent {
        row["parentDocumentId"] = json!(parent);
    }
    row
}

/// Mount `documents.list` with one page of rows and `documents.info` for
/// each of them.
async fn server_with(rows: Vec<Value>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": rows.clone(),
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    for row in rows {
        let id = row["id"].as_str().unwrap().to_string();
        let title = row["title"].as_str().unwrap().to_string();
        Mock::given(method("POST"))
            .and(request_path("/api/documents.info"))
            .and(body_partial_json(json!({ "id": id })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "id": id, "title": title, "text": format!("body of {id}\n") },
            })))
            .mount(&server)
            .await;
    }
    server
}

/// Every file path under `root`, relative and slash-separated.
fn tree(root: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path.strip_prefix(root).unwrap();
            found.insert(
                relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
    found
}

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
async fn hostile_titles_cannot_escape_the_output_directory() {
    let server = server_with(vec![
        row("a", "../../etc/passwd", None),
        row("b", "CON", None),
        row("c", "a/b\\c", None),
        row("d", "Notes... ", None),
        row("e", "", None),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("nested").join("export");
    let sentinel = dir.path().join("passwd");

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

    let files = tree(&out);
    assert_eq!(files.len(), 5, "{files:?}");
    for name in &files {
        assert!(!name.contains(".."), "traversal survived: {name}");
        assert!(!name.contains('\\'), "separator survived: {name}");
        assert_eq!(name.matches('/').count(), 0, "extra level: {name}");
    }
    assert!(!sentinel.exists(), "wrote outside the output directory");
    // Nothing at all outside the export directory.
    assert_eq!(tree(dir.path()).len(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn documents_with_the_same_title_get_distinct_files() {
    let server = server_with(vec![
        row("a", "Deploy", None),
        row("b", "deploy", None),
        row("c", "DEPLOY", None),
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

    // Three documents, three files, even on a case-insensitive filesystem.
    assert_eq!(tree(&out).len(), 3);
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
async fn a_subtree_whose_directory_cannot_be_created_is_reported_whole() {
    // A file already sits where `Alpha/` has to go. Every document under it
    // is unreachable, so all three must appear in the summary - none may
    // vanish from both stdout and stderr.
    let server = server_with(vec![
        row("a", "Alpha", None),
        row("b", "Beta", Some("a")),
        row("c", "Gamma", Some("b")),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("Alpha"), "in the way").unwrap();

    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
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
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(9));
    let stderr = String::from_utf8_lossy(&output.stderr);
    for id in ["a", "b", "c"] {
        assert!(
            stderr.contains(&format!("  {id}: ")),
            "{id} missing: {stderr}"
        );
    }
    assert!(stderr.contains("3 of 3 document(s)"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_directory_and_its_own_file_share_a_name() {
    // Two documents named "Deploy"; the second also has a child, so it gets
    // the de-duplicated stem AND the directory named after it.
    let server = server_with(vec![
        row("a", "Deploy", None),
        row("b", "Deploy", None),
        row("c", "Child", Some("b")),
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
            "Deploy.md".to_string(),
            "Deploy-2/Deploy-2.md".to_string(),
            "Deploy-2/Child.md".to_string(),
        ])
    );
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
async fn titles_that_differ_only_by_unicode_normalization_get_separate_files() {
    // NFC `é` and NFD `e`+U+0301 are ONE directory entry on macOS. Without
    // a normalization-insensitive de-duplication key the second document
    // silently replaced the first while both were reported as exported.
    let server = server_with(vec![
        row("a", "Caf\u{e9}", None),
        row("b", "Cafe\u{301}", None),
    ])
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

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    let reported = parsed["exported"].as_array().map(Vec::len);
    assert_eq!(reported, Some(2), "summary: {parsed}");
    // Two documents claimed, two files on disk: the claim in the summary
    // matches reality even on a normalization-insensitive filesystem.
    assert_eq!(tree(&out).len(), 2, "one document overwrote the other");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_temporary_files_are_left_behind() {
    // Every document is written through a temp file in the destination
    // directory; none may survive the run.
    let server = server_with(vec![row("a", "Alpha", None), row("b", "Beta", Some("a"))]).await;
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

    for path in tree(&out) {
        assert!(
            !path.contains("otl-export-"),
            "temporary file survived: {path}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_that_cannot_be_placed_leaves_the_old_content_alone() {
    // A directory sits where a document's file has to go, so the final
    // rename cannot succeed. The point is what does NOT happen: the
    // existing entry is not emptied first (the old `truncate(true)` would
    // have destroyed a previous backup before discovering the failure), and
    // no partial file is left behind.
    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let blocker = out.join("Alpha.md");
    std::fs::create_dir_all(&blocker).unwrap();
    std::fs::write(blocker.join("precious.txt"), "keep me").unwrap();

    let uri = server.uri();
    let target = out.clone();
    let output = blocking(move || {
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
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(9));
    // The pre-existing content survived untouched.
    assert_eq!(
        std::fs::read_to_string(blocker.join("precious.txt")).unwrap(),
        "keep me"
    );
    // And no temp file was left in the destination directory.
    assert_eq!(
        tree(&out),
        BTreeSet::from(["Alpha.md/precious.txt".to_string()])
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_symlink_at_the_destination_is_replaced_not_written_through() {
    // With `--overwrite`, a symlink where a document's file goes used to be
    // a check-then-open race. Writing a fresh temp file and renaming it over
    // the destination closes it structurally: `rename` replaces the link
    // itself, so the file it pointed at is never touched.
    use std::os::unix::fs::symlink;

    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "do not touch").unwrap();
    symlink(&outside, out.join("Alpha.md")).unwrap();

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

    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "do not touch",
        "the export wrote through the symlink"
    );
    let written = out.join("Alpha.md");
    assert!(
        std::fs::symlink_metadata(&written).unwrap().is_file(),
        "the symlink was not replaced by a real file"
    );
    assert!(std::fs::read_to_string(&written)
        .unwrap()
        .contains("body of a"));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_output_directory_that_is_a_symlink_is_refused() {
    use std::os::unix::fs::symlink;

    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = dir.path().join("link");
    symlink(&real, &link).unwrap();

    let uri = server.uri();
    let assert = blocking(move || {
        otl_at(&uri)
            .args(["docs", "export", "--collection", COLLECTION, "--out"])
            .arg(&link)
            .assert()
    })
    .await;

    assert
        .failure()
        .code(2)
        .stderr(predicate::str::contains("symlink"));
    assert!(server.received_requests().await.unwrap().is_empty());
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
async fn a_hostile_title_on_an_unusable_row_cannot_rewrite_the_terminal() {
    // The failure summary names an unusable row by its title, and a title
    // is written by anyone who can edit the document. Untouched, it could
    // set the clipboard with OSC 52 or use a newline to forge an extra
    // failure entry in the summary it appears in.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "title": "evil\u{1b}]52;c;cGF5bG9hZA==\u{7}\nfake-id: forged failure" },
            ],
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
    assert!(!stderr.contains('\u{1b}'), "ESC reached stderr: {stderr:?}");
    assert!(!stderr.contains('\u{7}'), "BEL reached stderr: {stderr:?}");
    // The forged text is still shown - it is the real title - but folded
    // onto the one line that belongs to this row, so it cannot pose as a
    // second failure entry.
    let forged_lines = stderr
        .lines()
        .filter(|line| line.trim_start().starts_with("fake-id:"))
        .count();
    assert_eq!(forged_lines, 0, "a title forged a failure line: {stderr:?}");
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
async fn a_successful_export_reports_itself_as_durable() {
    // The `durable` field exists so an automated backup can tell "written"
    // from "written and flushed". On the happy path it must be true, or the
    // field would be noise.
    let server = server_with(vec![row("a", "Alpha", None), row("b", "Beta", Some("a"))]).await;
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
    assert_eq!(parsed["durable"], json!(true));
    assert_eq!(parsed["complete"], json!(true));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_can_never_leave_an_empty_document_file() {
    // The property the placeholder broke: at no point does the destination
    // exist without its full content. Approximated here by watching the
    // directory while the export runs - every time `Alpha.md` is visible,
    // it must already be complete.
    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    std::fs::create_dir_all(&out).unwrap();

    let watched = out.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher_stop = std::sync::Arc::clone(&stop);
    let watcher = std::thread::spawn(move || {
        let mut empty_sightings = 0_usize;
        while !watcher_stop.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok(content) = std::fs::read(watched.join("Alpha.md")) {
                if content.is_empty() {
                    empty_sightings += 1;
                }
            }
        }
        empty_sightings
    });

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
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let empty_sightings = watcher.join().unwrap();

    assert_eq!(
        empty_sightings, 0,
        "the destination existed with no content at some point"
    );
    assert!(std::fs::read_to_string(out.join("Alpha.md"))
        .unwrap()
        .contains("body of a"));
}
