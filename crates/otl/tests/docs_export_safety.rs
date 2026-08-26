//! Story 3.6: `otl docs export` - robustness, hostile input, and the
//! guarantees about what reaches the filesystem.
//!
//! The ordinary "did it export the right documents" cases live in
//! `docs_export.rs`; the two files share their fixtures through
//! `common::export`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::export::{row, server_with, tree, COLLECTION};
use common::{blocking, otl_at};
use predicates::prelude::*;
use serde_json::{json, Value};
use wiremock::matchers::{method, path as request_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_can_never_leave_an_empty_document_file() {
    // The property the placeholder broke, watched on the path that HAD the
    // placeholder: the default, no-`--overwrite` export into a fresh
    // directory. (An earlier version of this test ran `--overwrite`, which
    // never used a placeholder at all - it could not have failed.)
    //
    // The structural guarantee is that the destination name is only ever
    // created by linking a finished temp file; this watcher is the
    // observable half of it.
    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");

    let watched = out.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher_stop = std::sync::Arc::clone(&stop);
    let watcher = std::thread::spawn(move || {
        let mut sightings = 0_usize;
        let mut empty = 0_usize;
        while !watcher_stop.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok(content) = std::fs::read(watched.join("Alpha.md")) {
                sightings += 1;
                if content.is_empty() {
                    empty += 1;
                }
            }
        }
        (sightings, empty)
    });

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
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let (sightings, empty) = watcher.join().unwrap();

    assert_eq!(
        empty, 0,
        "the destination existed with no content at some point"
    );
    // The watcher has to have actually seen the file, or "never empty"
    // would be true of a test that looked at nothing.
    assert!(sightings > 0, "the watcher never observed the file at all");
    assert!(std::fs::read_to_string(out.join("Alpha.md"))
        .unwrap()
        .contains("body of a"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hostile_document_id_cannot_rewrite_the_terminal() {
    // The id of a document that FAILED goes into the failure summary. Like
    // a title it is server text; unlike a title it also has to stay
    // verbatim in the JSON so the user can retry with it. So the JSON keeps
    // it raw and the terminal summary quotes it.
    const HOSTILE: &str = "evil\u{1b}]52;c;cGF5bG9hZA==\u{7}\n  forged: failure";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": HOSTILE,
                "title": "Hostile",
                "updatedAt": "2026-08-01T00:00:00.000Z",
            }],
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains('\u{1b}'), "ESC reached stderr: {stderr:?}");
    assert!(!stderr.contains('\u{7}'), "BEL reached stderr: {stderr:?}");
    let forged = stderr
        .lines()
        .filter(|line| line.trim_start().starts_with("forged:"))
        .count();
    assert_eq!(forged, 0, "an id forged a failure line: {stderr:?}");

    // The JSON keeps the id exactly as the server sent it: that is what a
    // retry needs, and JSON encoding makes it safe to carry.
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["failed"][0]["id"], json!(HOSTILE));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_successful_export_reports_itself_as_durable() {
    // `durable` exists so an automated backup can tell "written" from
    // "written and flushed". On Unix, where a directory CAN be flushed, the
    // happy path must report true - otherwise the field is noise.
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
    assert_eq!(parsed["stray"], json!([]));
}

#[cfg(not(unix))]
#[tokio::test(flavor = "multi_thread")]
async fn a_platform_that_cannot_flush_reports_unconfirmed_durability() {
    // The counterpart, and the reason `durable` is a tri-state: a platform
    // with no way to flush a directory must say so rather than claim a
    // guarantee nothing checked.
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

    assert_eq!(output.status.code(), Some(0));
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        parsed["durable"],
        Value::Null,
        "an unflushable platform must not report durable: true"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not be confirmed"));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_created_parent_that_cannot_be_flushed_is_reported() {
    // This is the test that goes RED if the parent-flush loop is deleted.
    //
    // The previous version of it asserted `durable: true` on a nested
    // `--out`, which proved nothing: `durable` is only false when a flush
    // returns an error, and NOT flushing produces no error at all. Zero
    // parents flushed and four parents flushed looked identical.
    //
    // So the fixture makes one created parent impossible to flush: a
    // directory with no read permission can still be traversed and written
    // to, but `File::open` on it - which is how a directory is fsynced -
    // fails with EACCES. If the export flushes its created parents, that
    // failure is reported (exit 9, durable:false); if it does not flush
    // them, the export sails through with exit 0.
    use std::os::unix::fs::PermissionsExt;

    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let unreadable = dir.path().join("unreadable");
    std::fs::create_dir(&unreadable).unwrap();
    // Write + execute, no read: entries can be created, the directory
    // cannot be opened.
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o300)).unwrap();
    if std::fs::File::open(&unreadable).is_ok() {
        // Running as root, where permission bits do not apply.
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }

    let out = unreadable.join("export");
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
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o700)).unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(9),
        "the export did not try to flush the directory it created the \
         output directory in; stderr: {stderr}"
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["durable"], json!(false));
    assert_eq!(parsed["complete"], json!(false));
    // The document itself was still written: this is a durability failure,
    // not a lost document.
    assert_eq!(parsed["exported"], json!(["Alpha.md"]));
    assert_eq!(parsed["failed"], json!([]));
    assert!(
        stderr.contains("unreadable"),
        "the unflushable directory is not named: {stderr}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_relative_output_path_is_not_a_false_alarm() {
    // `--out backup` and `--out ./backup` name the same directory, and used
    // to disagree: the bare form produced an empty flush target, which can
    // only fail, so a clean export reported exit 9 / durable:false with an
    // empty `failed` list.
    for relative in ["backup", "./backup2", "deep/nested/x"] {
        let server = server_with(vec![row("a", "Alpha", None)]).await;
        let dir = tempfile::tempdir().unwrap();
        let uri = server.uri();
        let cwd = dir.path().to_path_buf();
        let out = relative.to_string();
        let output = blocking(move || {
            otl_at(&uri)
                .current_dir(&cwd)
                .args(["docs", "export", "--collection", COLLECTION, "--out", &out])
                .output()
                .unwrap()
        })
        .await;

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(0),
            "--out {relative} reported a failure; stderr: {stderr}"
        );
        let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(parsed["durable"], json!(true), "--out {relative}");
        assert_eq!(parsed["complete"], json!(true), "--out {relative}");
        assert!(
            !stderr.contains("could not flush"),
            "--out {relative}: {stderr}"
        );
        assert_eq!(tree(&dir.path().join(relative)).len(), 1);
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_absolute_output_path_stays_clean_too() {
    // The shape every other test in this file uses, asserted explicitly so
    // the relative cases above have a stated counterpart.
    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("deep").join("nested").join("export");
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
    assert_eq!(parsed["durable"], json!(true));
    assert_eq!(tree(&out), BTreeSet::from(["Alpha.md".to_string()]));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_leftover_temporary_file_explains_itself_on_the_next_run() {
    // A stray temp file is hidden, so `ls` shows an empty directory. The
    // generic "is not empty" told the user something they could not see;
    // this says what is actually there and how to proceed.
    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join(".otl-export-0123456789abcdef.md"), "leftover").unwrap();

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

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("leftover temporary file"),
        "the leftover is not explained: {stderr}"
    );
    assert!(
        stderr.contains(".otl-export-0123456789abcdef.md"),
        "the leftover is not named: {stderr}"
    );
    assert!(
        stderr.contains("hidden"),
        "the reason the directory looks empty is not stated: {stderr}"
    );
    // No request was made: this is still a local, fail-fast check.
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_content_still_gets_the_ordinary_not_empty_message() {
    // The counterpart: content the user put there is not a leftover, and
    // must keep the advice that applies to it.
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("mine.md"), "mine").unwrap();

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
        .stderr(predicate::str::contains("is not empty"))
        .stderr(predicate::str::contains("leftover").not());
}
