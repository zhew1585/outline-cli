//! `otl docs export` - what reaches the FILESYSTEM.
//!
//! File names that must not escape the output directory, symlinks, atomic
//! placement, durability, and validation of `--out`. The cases about
//! hostile text reaching a TERMINAL live in `docs_export_terminal.rs`, and
//! the ordinary "did it export the right documents" cases in
//! `docs_export.rs`; all three share fixtures through `common::export`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::export::{row, server_with, tree, COLLECTION};
use common::{blocking, otl_at};
use predicates::prelude::*;
// `json!` is only reached by the Unix-gated tests below, so on another
// target it is genuinely unused. Silenced per target rather than dropped:
// CI runs clippy with `-D warnings` on Windows too, and an unused import
// there fails that leg while looking perfectly fine here.
#[cfg_attr(not(unix), allow(unused_imports))]
use serde_json::{json, Value};
use wiremock::MockServer;

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
    // existing entry is not emptied first (a truncate would destroy a
    // previous backup before discovering the failure), and no partial file
    // is left behind.
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
    // With `--overwrite`, writing a fresh temp file and renaming it over
    // the destination closes the check-then-open race structurally:
    // `rename` replaces the link itself, so the file it pointed at is
    // never touched.
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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_can_never_leave_an_empty_document_file() {
    // The property the placeholder broke, watched on the path that HAD the
    // placeholder: the default, no-`--overwrite` export into a fresh
    // directory. (An earlier version ran `--overwrite`, which never used a
    // placeholder at all - it could not have failed.)
    //
    // The structural guarantee is that a destination name is only ever
    // created by linking a finished temp file, and the unit tests in
    // `target` assert that directly. This is the observable half, and it is
    // made deterministic rather than left to luck: five documents through a
    // paced request channel take hundreds of milliseconds, and the watched
    // file is the LAST one written, so the poller gets many samples before
    // it appears and the file is certainly there once the export returns.
    let server = server_with(vec![
        row("a", "Alpha", None),
        row("b", "Bravo", None),
        row("c", "Charlie", None),
        row("d", "Delta", None),
        row("e", "Echo", None),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");

    // Siblings are written in (title, id) order, so `Echo.md` is last.
    let watched = out.join("Echo.md");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher_stop = std::sync::Arc::clone(&stop);
    let watcher = std::thread::spawn(move || {
        let mut sightings = 0_usize;
        let mut empty = 0_usize;
        loop {
            if let Ok(content) = std::fs::read(&watched) {
                sightings += 1;
                if content.is_empty() {
                    empty += 1;
                }
            }
            // One more pass after the export has finished, so "did the
            // watcher look at all" cannot fail for want of scheduling.
            if watcher_stop.load(std::sync::atomic::Ordering::Relaxed) && sightings > 0 {
                break;
            }
            std::thread::yield_now();
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
    // Not vacuous: the file was really looked at.
    assert!(sightings > 0, "the watcher never observed the file at all");
    assert!(std::fs::read_to_string(out.join("Echo.md"))
        .unwrap()
        .contains("body of e"));
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

#[tokio::test(flavor = "multi_thread")]
async fn an_output_path_that_steps_out_of_a_new_directory_is_refused() {
    // `--out a/../b` would leave an empty `a/` behind: the components
    // are created one at a time, so `a` gets made and then stepped out of.
    // Collapsing `..` lexically instead would be wrong when a component is
    // a symlink, so this is refused rather than rewritten.
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let uri = server.uri();
    let cwd = dir.path().to_path_buf();
    let output = blocking(move || {
        otl_at(&uri)
            .current_dir(&cwd)
            .args([
                "docs",
                "export",
                "--collection",
                COLLECTION,
                "--out",
                "a/../b",
            ])
            .output()
            .unwrap()
    })
    .await;

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("steps back out"), "{stderr}");
    // Nothing was created, and no request was made.
    assert!(
        !dir.path().join("a").exists(),
        "an empty directory was left"
    );
    assert!(!dir.path().join("b").exists());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parent_segment_after_an_existing_directory_still_works() {
    // The counterpart: `..` is only refused when it steps out of something
    // this run would have to create.
    let server = server_with(vec![row("a", "Alpha", None)]).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("here")).unwrap();
    let uri = server.uri();
    let cwd = dir.path().to_path_buf();
    let output = blocking(move || {
        otl_at(&uri)
            .current_dir(&cwd)
            .args([
                "docs",
                "export",
                "--collection",
                COLLECTION,
                "--out",
                "here/../backup",
            ])
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
    assert_eq!(
        tree(&dir.path().join("backup")),
        BTreeSet::from(["Alpha.md".to_string()])
    );
}
