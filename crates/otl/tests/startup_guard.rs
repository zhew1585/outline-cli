//! Startup guards (Story 1.8): the binary must start without any spec file
//! present at runtime - the vendored OpenAPI spec is compiled to a static IR
//! table by `build.rs`, so runtime never parses OpenAPI/YAML.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

/// A fresh empty directory far away from the repo (and its vendored spec).
fn empty_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("otl-startup-guard-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `otl` command with Outline env scrubbed, running from `dir`.
fn otl_in(dir: &PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.current_dir(dir)
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY");
    cmd
}

#[test]
fn help_works_with_no_spec_file_at_runtime() {
    let dir = empty_dir("help");
    otl_in(&dir)
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("otl"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn version_works_with_no_spec_file_at_runtime() {
    let dir = empty_dir("version");
    otl_in(&dir)
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("otl"));
    std::fs::remove_dir_all(&dir).ok();
}
