//! Startup guards (Story 1.8): the vendored OpenAPI spec is compiled to a
//! static IR table by `build.rs`, so the runtime never reads or parses it.
//!
//! What these tests guarantee, precisely:
//!
//! 1. `spec_path_and_content_absent_from_binary` - the shipped binary
//!    contains neither the spec file name nor the vendored spec's content,
//!    so it cannot open the spec through a compile-time absolute path
//!    (`env!("CARGO_MANIFEST_DIR")` and friends) nor carry an embedded copy
//!    (`include_str!`). This is the invariant that closes the "checkout's
//!    `crates/otl/spec/` is still readable" gap; a regression that reads the
//!    spec at runtime has to name it somewhere, and that string would show
//!    up here.
//! 2. The remaining tests copy the binary into a fresh temp directory and
//!    run that copy from there, so no cwd-relative or executable-relative
//!    lookup reaches the checkout. The `api` cases go through operation
//!    dispatch, proving the IR lookup itself works in that environment.
//!
//! Together: no spec path in the binary, and no spec reachable from where it
//! runs.

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

/// File name of the vendored spec; a runtime read has to name it.
const SPEC_FILE_NAME: &str = "spec3.json";
/// Distinctive text from the vendored spec's `info.description`: present if
/// the spec were embedded in the binary, absent from the compiled IR table
/// (which only carries operation names, paths and parameter names).
const SPEC_CONTENT_MARKER: &str = "structured in an RPC style";

/// Byte-level search, so this works on any platform without decoding the
/// binary as UTF-8.
fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn spec_path_and_content_absent_from_binary() {
    let bin = assert_cmd::cargo::cargo_bin("otl");
    let bytes = std::fs::read(&bin).unwrap();

    // Sanity check: the markers really are in the vendored spec, so a
    // negative result below means something.
    let spec = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("spec")
            .join(SPEC_FILE_NAME),
    )
    .unwrap();
    assert!(
        contains_bytes(&spec, SPEC_CONTENT_MARKER),
        "test marker is stale: {SPEC_CONTENT_MARKER:?} no longer appears in {SPEC_FILE_NAME}"
    );

    assert!(
        !contains_bytes(&bytes, SPEC_FILE_NAME),
        "{} references {SPEC_FILE_NAME}: the runtime must not open the spec \
         (build.rs compiles it to a static IR table)",
        bin.display()
    );
    assert!(
        !contains_bytes(&bytes, SPEC_CONTENT_MARKER),
        "{} embeds the vendored spec's content: the runtime must not carry \
         or parse the OpenAPI document",
        bin.display()
    );
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
