//! Guard: platform-specific code must be behind a `cfg`.
//!
//! `std::os::unix` does not exist when compiling for Windows, and
//! `std::os::windows` does not exist on Unix. Using either without a `cfg`
//! guard compiles perfectly on the author's machine and fails the build on
//! the other platform - including in TEST modules, where it takes the whole
//! test harness down with it, so `cargo test --workspace` on that platform
//! never runs a single test.
//!
//! That is exactly what happened once: splitting a file moved a test into a
//! new module and left its `#[cfg(unix)]` behind. Every local gate stayed
//! green because every local gate runs on Unix.
//!
//! CI does build on Windows and would catch it, but only after a push. This
//! test moves the same check into the local gate, where the mistake is made.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// Platform modules that only exist on some targets.
const PLATFORM_MODULES: &[&str] = &["std::os::unix", "std::os::windows", "std::os::wasi"];

/// This file, which NAMES the platform modules as data - in its own
/// documentation and in the fixtures of the negative test below - rather
/// than importing them. Scanning itself would only ever find those.
const SELF: &str = "tests/portability.rs";

/// How far above a use the guarding attribute may sit.
///
/// Generous on purpose: an item's `#[cfg]` can be separated from the line
/// that needs it by a doc comment and a signature. The indentation rule
/// below is what keeps the window from matching an unrelated attribute in a
/// neighbouring item.
const GUARD_WINDOW: usize = 40;

#[test]
fn platform_specific_code_is_behind_a_cfg() {
    let mut violations = Vec::new();
    let mut scanned = 0_usize;
    for file in workspace_rust_files() {
        if file.to_string_lossy().replace('\\', "/").ends_with(SELF) {
            continue;
        }
        let source = std::fs::read_to_string(&file).unwrap_or_default();
        let lines: Vec<&str> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            // Prose that mentions a platform module is not a use of one.
            if line.trim_start().starts_with("//") {
                continue;
            }
            let Some(module) = PLATFORM_MODULES
                .iter()
                .find(|module| line.contains(**module))
            else {
                continue;
            };
            scanned += 1;
            if is_guarded(&lines, index, module) {
                continue;
            }
            violations.push(format!(
                "  {}:{}: `{module}` is not behind a #[cfg] guard",
                file.display(),
                index + 1
            ));
        }
    }
    // The scan has to have found something, or a rename of the platform
    // modules would turn this test into one that passes by looking at
    // nothing.
    assert!(
        scanned > 0,
        "no platform-specific code found at all: the guard is not looking \
         where the code is"
    );
    assert!(
        violations.is_empty(),
        "platform-specific code must be behind a #[cfg] attribute, or the \
         build breaks on every other target - test modules included, where \
         it takes the whole harness with it:\n{}\n\
         Add `#[cfg(unix)]` (or the matching guard) to the enclosing item, \
         or wrap the use in a `#[cfg(...)] {{ ... }}` block.",
        violations.join("\n")
    );
}

/// Whether the platform module used on `index` is covered by a `cfg`.
///
/// Looks upwards for a `#[cfg(...)]` mentioning the platform, at an
/// indentation no deeper than the use itself - an attribute indented
/// further belongs to something nested, not to the item in hand.
fn is_guarded(lines: &[&str], index: usize, module: &str) -> bool {
    let platform = module.rsplit("::").next().unwrap_or(module);
    let depth = indentation(lines[index]);
    let start = index.saturating_sub(GUARD_WINDOW);
    lines[start..=index].iter().rev().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("#[cfg") && trimmed.contains(platform) && indentation(line) <= depth
    })
}

/// Leading whitespace width of a line.
fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Every `.rs` file in the workspace's crates, sources and tests alike.
fn workspace_rust_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates = workspace_root().join("crates");
    let entries = std::fs::read_dir(&crates).expect("crates directory");
    for entry in entries {
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

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn the_guard_recognizes_guarded_and_unguarded_uses() {
    // Negative test for the checker itself: without it, a scan that never
    // matched anything would pass just as quietly as a clean tree.
    let guarded = vec![
        "#[cfg(unix)]",
        "#[test]",
        "fn t() {",
        "    use std::os::unix::fs::symlink;",
        "}",
    ];
    assert!(is_guarded(&guarded, 3, "std::os::unix"));

    let unguarded = vec![
        "#[test]",
        "fn t() {",
        "    use std::os::unix::fs::symlink;",
        "}",
    ];
    assert!(
        !is_guarded(&unguarded, 2, "std::os::unix"),
        "the exact shape that broke the Windows build was accepted"
    );

    // An inner block guard counts.
    let inner = vec![
        "fn t() {",
        "    #[cfg(unix)]",
        "    {",
        "        use std::os::unix::fs::MetadataExt;",
        "    }",
        "}",
    ];
    assert!(is_guarded(&inner, 3, "std::os::unix"));

    // A guard for the OTHER platform does not count.
    let wrong = vec![
        "#[cfg(windows)]",
        "fn t() {",
        "    use std::os::unix::fs::symlink;",
        "}",
    ];
    assert!(!is_guarded(&wrong, 2, "std::os::unix"));

    // An attribute belonging to a more deeply nested item does not count.
    let nested = vec![
        "fn outer() {",
        "        #[cfg(unix)]",
        "        fn inner() {}",
        "    use std::os::unix::fs::symlink;",
        "}",
    ];
    assert!(!is_guarded(&nested, 3, "std::os::unix"));
}
