//! `otl api list` end-to-end tests (Story 1.2).

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

#[test]
fn api_list_prints_one_line_per_spec_operation_without_config() {
    // Listing is a purely local operation: no OUTLINE_URL / OUTLINE_API_KEY
    // needed, no network touched.
    let output = otl().args(["api", "list"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        spec_op_count(),
        "line count != spec operation count"
    );
    // Every line is `name<TAB>summary` with a non-empty summary.
    for line in &lines {
        let (name, summary) = line.split_once('\t').unwrap_or_else(|| {
            panic!("line without tab separator: {line:?}");
        });
        assert!(name.contains('.'), "malformed op name: {name:?}");
        assert!(!summary.trim().is_empty(), "empty summary for {name}");
    }
}

#[test]
fn api_list_includes_known_operation_with_summary() {
    otl()
        .args(["api", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("documents.info"))
        .stdout(predicate::str::contains("Retrieve a document"))
        .stdout(predicate::str::contains("collections.list"));
}

#[test]
fn api_list_flags_operations_that_are_not_callable() {
    // documents.import needs multipart/form-data: it is still listed, but
    // flagged so nobody scripts against it expecting a JSON call.
    let output = otl().args(["api", "list"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("documents.import\t"))
        .expect("documents.import missing from listing");
    assert!(
        line.contains("multipart/form-data"),
        "unflagged line: {line}"
    );
    assert!(line.contains("not callable"), "unflagged line: {line}");
    // Ordinary operations carry no flag.
    let info = stdout
        .lines()
        .find(|line| line.starts_with("documents.info\t"))
        .expect("documents.info missing");
    assert!(!info.contains("not callable"), "false flag: {info}");
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
