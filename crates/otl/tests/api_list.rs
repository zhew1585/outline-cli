//! `otl api list` end-to-end tests (Story 1.2, dual state from Story 4.6).
//!
//! The listing is purely local, so every case here runs without an instance
//! URL, without a credential and without a network.
//!
//! # Why these assert JSON
//!
//! `assert_cmd` captures stdout through a pipe, so these runs are exactly
//! the non-TTY state the CLI contract describes: `--json` is the default
//! whenever stdout is not a terminal. Until Story 4.6 this command printed
//! its terminal form there instead - including when `--json` was passed
//! explicitly - which is the bug this file now pins shut. The terminal form
//! is unit-tested in `commands::api::list`, because no integration test can
//! give the child process a TTY.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

mod common;
use common::isolate;

/// `otl` with every machine-dependent input shut off.
///
/// `common::isolate` covers the credential file, the user config file, the
/// selected profile, the plaintext-key notice and the spec cache; this suite
/// adds only the instance and credential variables it sets per test.
/// These assertions are about the spec compiled into the binary.
fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    isolate(&mut cmd)
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY");
    cmd
}

/// Number of POST operations in the vendored spec (source of truth).
fn spec_op_count() -> usize {
    let spec: Value = serde_json::from_str(include_str!("../spec/spec3.json")).unwrap();
    spec["paths"]
        .as_object()
        .unwrap()
        .values()
        .filter(|item| item.get("post").is_some())
        .count()
}

/// Run `otl api list ...` and parse stdout as the JSON array it must be.
fn listing(args: &[&str]) -> Vec<Value> {
    let output = otl().args(["api", "list"]).args(args).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    serde_json::from_str::<Value>(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {stdout}"))
        .as_array()
        .expect("the listing is a JSON array")
        .clone()
}

#[test]
fn api_list_prints_one_object_per_spec_operation_without_config() {
    let rows = listing(&[]);
    assert_eq!(rows.len(), spec_op_count(), "row count != operation count");
    for row in &rows {
        let name = row["name"].as_str().expect("every row names an operation");
        assert!(name.contains('.'), "malformed op name: {name:?}");
        assert!(
            row["summary"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            "empty summary for {name}"
        );
        assert!(
            row["path"].as_str().is_some_and(|p| p.starts_with('/')),
            "missing path for {name}"
        );
    }
}

/// The whole point of the fix: the explicit flag and the pipe default agree,
/// and neither of them prints the terminal form.
#[test]
fn the_explicit_json_flag_and_the_pipe_default_agree() {
    let default = otl().args(["api", "list"]).output().unwrap().stdout;
    let explicit = otl()
        .args(["api", "list", "--json"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(default, explicit, "--json changed a non-TTY listing");
    let text = String::from_utf8_lossy(&default);
    assert!(
        !text.lines().next().unwrap_or_default().contains('\t'),
        "a pipe still got the tab-separated terminal form: {text:.120}"
    );
}

#[test]
fn api_list_includes_known_operations_with_their_summary() {
    let rows = listing(&[]);
    let info = rows
        .iter()
        .find(|row| row["name"] == "documents.info")
        .expect("documents.info missing");
    assert_eq!(info["summary"], "Retrieve a document");
    assert_eq!(info["path"], "/api/documents.info");
    assert!(rows.iter().any(|row| row["name"] == "collections.list"));
}

#[test]
fn api_list_flags_operations_that_are_not_callable() {
    // documents.import needs multipart/form-data: it is still listed, but
    // flagged so nobody scripts against it expecting a JSON call.
    let rows = listing(&[]);
    let import = rows
        .iter()
        .find(|row| row["name"] == "documents.import")
        .expect("documents.import missing from listing");
    assert_eq!(import["callable"], false, "{import}");
    assert_eq!(import["body_mode"], "unsupported", "{import}");
    // Ordinary operations carry no flag.
    let info = rows
        .iter()
        .find(|row| row["name"] == "documents.info")
        .expect("documents.info missing");
    assert_eq!(info["callable"], true, "{info}");
    assert_eq!(info["body_mode"], "key_value", "{info}");
}

#[test]
fn api_list_rejects_extra_arguments() {
    otl()
        .args(["api", "list", "id=x"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::is_empty());
}

/// Flags that shape a request describe something that will not happen here.
#[test]
fn api_list_rejects_request_flags() {
    for flag in ["--no-validate", "--show-server-message"] {
        otl()
            .args(["api", "list", flag])
            .assert()
            .failure()
            .code(2)
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains("sends no request"));
    }
    otl()
        .args(["api", "list", "--limit", "5"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::is_empty());
}
