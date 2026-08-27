//! `otl docs export` - what reaches the TERMINAL.
//!
//! Document titles, document ids, file names and paths all end up in
//! diagnostics, and all of them are written by someone other than the
//! person reading them. A terminal executes some of those bytes rather than
//! printing them, so every one of these paths has to be neutralized.
//!
//! Two layers are under test here, and they are tested separately on
//! purpose: `text::quote` at the call sites that must keep a value on one
//! line, and the scrub inside `stdio::write_diagnostic_line` that catches
//! whatever a future message forgets. The last test in this file is the one
//! that fails if the sink layer is removed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::export::COLLECTION;
use common::{blocking, otl_at};
use serde_json::{json, Value};
use wiremock::matchers::{method, path as request_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
async fn a_hostile_leftover_filename_cannot_rewrite_the_terminal() {
    // A filename may contain any byte but NUL and `/`, so the message that
    // names a leftover is another place server-or-filesystem text reaches a
    // terminal. Two layers have to hold: `stdio` scrubs the control classes
    // on the way out of every diagnostic, and this value additionally goes
    // through `text::quote` so it cannot introduce a line break.
    use std::os::unix::ffi::OsStrExt;

    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    let hostile = std::ffi::OsStr::from_bytes(b".otl-export-\x1b]52;c;cGF5bG9hZA==\x07\nforged.md")
        .to_owned();
    std::fs::write(out.join(&hostile), "leftover").unwrap();

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

    // Not one of ours (the hex part is not hex), so it counts as ordinary
    // content - but either message would carry the name, and neither may
    // carry the bytes.
    assert_eq!(output.status.code(), Some(2));
    assert!(
        !output.stderr.contains(&0x1b),
        "ESC reached stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stderr.contains(&0x07),
        "BEL reached stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_hostile_leftover_name_that_is_ours_is_also_scrubbed() {
    // The same, on the path that classifies it as a leftover: a name that
    // IS the exact shape we produce, with the hostile bytes after it.
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    // Exactly our shape, so `is_our_leftover` accepts it...
    std::fs::write(out.join(".otl-export-0123456789abcdef.md"), "leftover").unwrap();
    // ...next to a control-bearing sibling, so both messages are exercised
    // across the two runs of this file.
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
    assert!(stderr.contains("leftover temporary file"), "{stderr}");
    assert!(
        !stderr.contains("--overwrite to export over them"),
        "--overwrite does not replace leftovers, so it must not be offered \
         as if it did: {stderr}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_path_in_a_diagnostic_cannot_carry_control_characters() {
    // The message that reports a non-empty output directory interpolates
    // that directory's PATH, and does so without quoting it - a path is
    // authored by whoever ran the command, not by a server, so quoting it
    // at the call site would be noise.
    //
    // Which makes this the test that the sink scrub is load-bearing:
    // nothing on this path calls `text::quote`, so if
    // `write_diagnostic_line` stopped scrubbing, the bytes would arrive.
    use std::os::unix::ffi::OsStrExt;

    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let hostile = dir.path().join(std::ffi::OsStr::from_bytes(
        b"out\x1b]52;c;cGF5bG9hZA==\x07dir",
    ));
    std::fs::create_dir(&hostile).unwrap();
    std::fs::write(hostile.join("mine.md"), "mine").unwrap();

    let uri = server.uri();
    let target = hostile.clone();
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
    assert!(stderr.contains("is not empty"), "{stderr:?}");
    assert!(
        !output.stderr.contains(&0x1b),
        "ESC reached stderr through an unquoted path: {stderr:?}"
    );
    assert!(
        !output.stderr.contains(&0x07),
        "BEL reached stderr through an unquoted path: {stderr:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_diagnostic_ever_carries_a_terminal_control_sequence() {
    // A property over the whole command rather than one message: whatever
    // the server sends and whatever it is called, nothing that reaches
    // stderr may contain a byte a terminal would execute. This is the
    // backstop for the next message someone adds without thinking about it.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "title": "no id \u{1b}]52;c;x\u{7}\nforged" },
                { "id": "id\u{1b}[31m\nforged", "title": "hostile\u{1b}]8;;http://evil\u{7}" },
            ],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "ok": false,
            "error": "internal\u{1b}[2Jerror",
            "message": "boom\u{1b}]52;c;y\u{7}",
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    let executable: Vec<char> = stderr
        .chars()
        .filter(|c| c.is_control() && *c != '\n')
        .collect();
    assert!(
        executable.is_empty(),
        "stderr carries {} control character(s) a terminal would act on: {stderr:?}",
        executable.len()
    );
}
