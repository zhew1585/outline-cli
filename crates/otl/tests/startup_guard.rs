//! Startup guards (Story 1.8): the vendored OpenAPI spec is compiled to a
//! static IR table by `build.rs`, so the runtime never reads or parses it.
//!
//! The invariant is enforced in three layers, each closing what the others
//! cannot see:
//!
//! 1. **Source scan** (`runtime_sources_never_reach_for_the_spec`) - no
//!    runtime source file (`crates/*/src/**/*.rs`) names the constructs a
//!    runtime spec read needs: `CARGO_MANIFEST_DIR` (a compile-time path to
//!    the crate, hence to `spec/`), `include_str!`/`include_bytes!`, a
//!    directory walk, or the spec's name. This catches regressions that
//!    embed only the *directory* and discover the file at runtime - which no
//!    binary-content check can see. `build.rs` is deliberately out of scope:
//!    reading the spec at build time is exactly its job.
//! 2. **Binary content** (`spec_path_and_content_absent_from_binary`,
//!    `manifest_dir_absent_from_release_binary`) - the built binary contains
//!    neither the spec file name nor a marker of the spec's content, and the
//!    release artifact does not contain the crate's manifest-directory path
//!    (any compile-time absolute-path regression embeds it). Debug builds
//!    legitimately embed paths, so the manifest-dir check is release-only and
//!    skips with a message when no release artifact is present.
//! 3. **Runtime isolation** (the `*_with_no_spec_file_reachable` cases) - the
//!    binary is copied into a fresh temp directory and run from there, so no
//!    cwd-relative or executable-relative lookup reaches the checkout. The
//!    `api` cases go through operation dispatch, proving the IR lookup itself
//!    works in that environment.
//!
//! Together: the runtime sources cannot ask for the spec, the shipped binary
//! carries no path or copy of it, and the process still works with no spec
//! reachable from where it runs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

/// Repository root: `crates/otl` -> `crates` -> workspace root.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

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
/// A configuration directory that deliberately does not exist, so these
/// tests never read - or write - the developer's real credential file, and
/// so credential resolution depends on the environment alone.
const NO_CREDENTIALS_DIR: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/no-credentials");

/// Point a command at an empty credential store and silence the one-time
/// plaintext-key notice, which is not what these tests are about.
fn isolate(cmd: &mut Command) -> &mut Command {
    cmd.env("OUTLINE_CONFIG_DIR", NO_CREDENTIALS_DIR)
        .env("OUTLINE_NO_KEY_WARNING", "1")
        .env_remove("OUTLINE_PROFILE")
}

fn otl_cmd(dir: &Path, bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    isolate(&mut cmd)
        .current_dir(dir)
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

/// Constructs a runtime spec read would need, with why each is forbidden in
/// runtime sources. Plain substring matches, comments included: a mention is
/// as good as a use for review purposes, and genuine exceptions go through
/// `SOURCE_SCAN_ALLOWLIST` so they stay visible.
const FORBIDDEN_SOURCE_PATTERNS: &[(&str, &str)] = &[
    (
        "CARGO_MANIFEST_DIR",
        "compile-time crate path; it locates the vendored `spec/` directory at runtime",
    ),
    (
        "include_str!",
        "compile-time file embedding; the IR table is the only compiled-in spec artifact",
    ),
    (
        "include_bytes!",
        "compile-time file embedding; the IR table is the only compiled-in spec artifact",
    ),
    (
        "read_dir",
        "directory enumeration; the runtime has no data files to discover",
    ),
    (
        SPEC_FILE_NAME,
        "the vendored spec file name; only build.rs may name it",
    ),
    (
        "\"spec\"",
        "the vendored spec directory name; only build.rs may name it",
    ),
];

/// Reviewed exceptions as (path suffix, pattern) pairs. Empty on purpose:
/// add an entry with a comment only when a runtime source legitimately needs
/// one of the patterns above.
const SOURCE_SCAN_ALLOWLIST: &[(&str, &str)] = &[];

/// Collect `crates/*/src/**/*.rs`. `build.rs` files live outside `src/` and
/// are therefore excluded by construction.
fn runtime_source_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates_dir = workspace_root().join("crates");
    for entry in std::fs::read_dir(&crates_dir).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            collect_rs_files(&src, &mut files);
        }
    }
    assert!(
        !files.is_empty(),
        "found no runtime sources under {}: the guard would pass vacuously",
        crates_dir.display()
    );
    files
}

/// Recursively push every `.rs` file under `dir` into `out`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Is this (file, pattern) combination a reviewed exception?
fn is_allowlisted(file: &Path, pattern: &str) -> bool {
    let path = file.to_string_lossy().replace('\\', "/");
    SOURCE_SCAN_ALLOWLIST
        .iter()
        .any(|(suffix, allowed)| *allowed == pattern && path.ends_with(suffix))
}

#[test]
fn runtime_sources_never_reach_for_the_spec() {
    let mut violations = Vec::new();
    for file in runtime_source_files() {
        let source = std::fs::read_to_string(&file).unwrap();
        for (pattern, reason) in FORBIDDEN_SOURCE_PATTERNS {
            if source.contains(pattern) && !is_allowlisted(&file, pattern) {
                violations.push(format!("  {}: {pattern} - {reason}", file.display()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "runtime sources must never read the OpenAPI spec: build.rs compiles \
         it into the static IR table at build time, and the runtime does zero \
         spec/OpenAPI/YAML parsing (SPEC.md Constraints).\n{}\n\
         If one of these is genuinely needed, add it to SOURCE_SCAN_ALLOWLIST \
         with a justifying comment.",
        violations.join("\n")
    );
}

/// The release artifact must not embed this crate's manifest directory: any
/// compile-time absolute path (`env!("CARGO_MANIFEST_DIR")`, `file!()` based
/// tricks) would leave that string behind. Debug builds embed paths for
/// legitimate reasons, so this is checked against the release binary only.
#[test]
fn manifest_dir_absent_from_release_binary() {
    let release_bin = target_dir()
        .join("release")
        .join(format!("otl{}", std::env::consts::EXE_SUFFIX));
    let Ok(bytes) = std::fs::read(&release_bin) else {
        eprintln!(
            "skipping: no release binary at {} (run `cargo build --release -p outline-cli`; \
             CI's startup-bench job builds it and runs this test)",
            release_bin.display()
        );
        return;
    };

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    assert!(
        !contains_bytes(&bytes, manifest_dir),
        "{} embeds the crate manifest directory {manifest_dir:?}: a \
         compile-time absolute path can reach the vendored spec at runtime",
        release_bin.display()
    );
}

/// Cargo target directory, honouring `CARGO_TARGET_DIR`.
fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("target"))
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
