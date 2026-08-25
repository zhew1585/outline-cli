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
