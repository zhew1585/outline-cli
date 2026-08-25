//! Startup guards (Story 1.8): the binary must work with no spec file
//! reachable at runtime - the vendored OpenAPI spec is compiled to a static
//! IR table by `build.rs`, so runtime never parses OpenAPI/YAML.
//!
//! To make "no spec reachable" real, each test copies the built binary into
//! a fresh temp directory and runs that copy from there: neither the cwd nor
//! any executable-relative path leads back to the repo checkout (and its
//! `crates/otl/spec/`). The `api` cases go through operation dispatch, so
//! they prove the IR lookup itself works without any spec file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

/// Copy the built `otl` binary into a fresh empty temp dir outside the repo;
/// returns (dir, copied binary path).
fn isolated_otl(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("otl-startup-guard-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let built = assert_cmd::cargo::cargo_bin("otl");
    let copied = dir.join(built.file_name().unwrap());
    std::fs::copy(&built, &copied).unwrap();
    (dir, copied)
}

/// Command for the isolated binary, running from its temp dir with Outline
/// env scrubbed.
fn otl_cmd(dir: &Path, bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.current_dir(dir)
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY");
    cmd
}

#[test]
fn help_works_with_no_spec_file_reachable() {
    let (dir, bin) = isolated_otl("help");
    otl_cmd(&dir, &bin)
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("otl"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn version_works_with_no_spec_file_reachable() {
    let (dir, bin) = isolated_otl("version");
    otl_cmd(&dir, &bin)
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("otl"));
    std::fs::remove_dir_all(&dir).ok();
}

/// A known operation resolves in the compiled-in IR table (lookup succeeds,
/// then the run fails on missing config, exit code 2) - proving `api`
/// dispatch works with no spec file anywhere near the process.
#[test]
fn api_known_op_resolves_in_ir_with_no_spec_file_reachable() {
    let (dir, bin) = isolated_otl("known-op");
    otl_cmd(&dir, &bin)
        .args(["api", "documents.info", "id=doc-123"])
        .assert()
        .failure()
        .code(2)
        // Config error, not an unknown-operation error: the IR lookup passed.
        .stderr(predicate::str::contains("OUTLINE_URL"))
        .stderr(predicate::str::contains("unknown API operation").not());
    std::fs::remove_dir_all(&dir).ok();
}

/// An unknown operation is rejected by the IR lookup itself (before any
/// config or network work), again with no spec file reachable.
#[test]
fn api_unknown_op_rejected_by_ir_with_no_spec_file_reachable() {
    let (dir, bin) = isolated_otl("unknown-op");
    otl_cmd(&dir, &bin)
        .args(["api", "nonexistent.op"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unknown API operation"))
        .stderr(predicate::str::contains("nonexistent.op"));
    std::fs::remove_dir_all(&dir).ok();
}
