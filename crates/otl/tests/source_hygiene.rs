//! Architectural constraints that no functional test can catch.
//!
//! `project-context.md` states the file-size limit as a hard rule, not a
//! style preference. A module that grows past it keeps compiling and keeps
//! passing its own tests, so nothing notices until a reviewer counts lines
//! by hand - which is exactly how it went unnoticed once already.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// Hard limit from `project-context.md` ("文件 <800 行").
const MAX_SOURCE_LINES: usize = 800;

/// Size at which a file should be split before it becomes a problem.
/// Reported, not enforced: the rule is the limit above.
const ADVISORY_SOURCE_LINES: usize = 700;

#[test]
fn no_runtime_source_file_exceeds_the_size_limit() {
    let mut violations = Vec::new();
    let mut approaching = Vec::new();
    for file in runtime_source_files() {
        let lines = std::fs::read_to_string(&file)
            .unwrap_or_default()
            .lines()
            .count();
        if lines > MAX_SOURCE_LINES {
            violations.push(format!("  {}: {lines} lines", file.display()));
        } else if lines > ADVISORY_SOURCE_LINES {
            approaching.push(format!("  {}: {lines} lines", file.display()));
        }
    }
    if !approaching.is_empty() {
        // Not a failure: a nudge, visible with `--nocapture`.
        eprintln!(
            "note: approaching the {MAX_SOURCE_LINES}-line limit:\n{}",
            approaching.join("\n")
        );
    }
    assert!(
        violations.is_empty(),
        "these files exceed the {MAX_SOURCE_LINES}-line limit from \
         project-context.md; split them by responsibility rather than \
         raising the limit:\n{}",
        violations.join("\n")
    );
}

/// Collect `crates/*/src/**/*.rs`.
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

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/<name> sits two levels below the workspace root")
        .to_path_buf()
}
