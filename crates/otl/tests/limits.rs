//! Guard: this repository's file-length limit.
//!
//! > 文件 <800 行（典型 200-400）
//!
//! The rule has no test exemption, and the reason is the same for tests as
//! for sources: nobody reads a 1300-line file, so nobody notices what is
//! already in it. A duplicate test, or one that contradicts another, hides
//! there indefinitely.
//!
//! Until now the rule was enforced by hand, which meant it was enforced on
//! whatever the author happened to be looking at. A refactor undertaken
//! *because* a source file passed 800 lines pushed a test file to 1292 in
//! the same commit, and nothing said a word.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// Maximum lines in one Rust file.
const MAX_LINES: usize = 800;

/// Size at which a file should be split before it becomes a problem.
///
/// Reported, never enforced. The failure mode this guard exists for is a
/// file crossing the limit inside an otherwise unrelated commit, and by then
/// the split is an interruption. Naming the files that are close makes it
/// possible to do the split while it is still cheap.
const ADVISORY_LINES: usize = 700;

/// Files over the limit that predate this rule being enforced.
///
/// Deliberately a list of exact paths with a stated reason, not a pattern:
/// a new file cannot join it by accident, and shrinking one of these below
/// the limit makes this test fail until the entry is removed - so the list
/// can only get shorter.
const GRANDFATHERED: &[(&str, &str)] = &[(
    "crates/engine/tests/validation.rs",
    "arrived over the limit from `develop` (799 -> 802 lines there, and \
         this guard does not exist on that branch, so nothing told anyone). \
         Two lines over, in a crate this branch does not touch: recorded so \
         it is visible and expires by itself, rather than split here as an \
         unrelated change",
)];

#[test]
fn no_rust_file_is_longer_than_the_limit() {
    let mut violations = Vec::new();
    let mut exempt_but_short = Vec::new();
    let mut approaching = Vec::new();
    for file in workspace_rust_files() {
        let lines = std::fs::read_to_string(&file)
            .map(|text| text.lines().count())
            .unwrap_or(0);
        let relative = relative_path(&file);
        let exemption = GRANDFATHERED
            .iter()
            .find(|(path, _)| relative == *path)
            .map(|(_, reason)| *reason);
        match exemption {
            Some(reason) if lines <= MAX_LINES => {
                exempt_but_short.push(format!("  {relative} ({lines} lines): {reason}"));
            }
            Some(_) => {}
            None if lines > MAX_LINES => {
                violations.push(format!("  {relative}: {lines} lines"));
            }
            None if lines > ADVISORY_LINES => {
                approaching.push(format!("  {relative}: {lines} lines"));
            }
            None => {}
        }
    }
    if !approaching.is_empty() {
        eprintln!(
            "note: approaching the {MAX_LINES}-line limit:\n{}",
            approaching.join("\n")
        );
    }
    assert!(
        violations.is_empty(),
        "these files exceed the {MAX_LINES}-line limit, which has no test \
         exemption:\n{}\n\
         Split them by responsibility rather than by line count.",
        violations.join("\n")
    );
    assert!(
        exempt_but_short.is_empty(),
        "these files are listed as grandfathered but are now within the \
         limit; remove their entries so the list keeps shrinking:\n{}",
        exempt_but_short.join("\n")
    );
}

/// Every `.rs` file in the workspace's crates.
fn workspace_rust_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates = workspace_root().join("crates");
    for entry in std::fs::read_dir(&crates).expect("crates directory") {
        let krate = entry.expect("crate entry").path();
        for sub in ["src", "tests"] {
            let dir = krate.join(sub);
            if dir.is_dir() {
                collect_rs(&dir, &mut files);
            }
        }
        let build = krate.join("build.rs");
        if build.is_file() {
            files.push(build);
        }
    }
    assert!(
        !files.is_empty(),
        "found no Rust files under {}: the guard would pass vacuously",
        crates.display()
    );
    files
}

/// Recursively collect `.rs` files.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// A path relative to the workspace root, with forward slashes.
fn relative_path(file: &Path) -> String {
    file.strip_prefix(workspace_root())
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}
