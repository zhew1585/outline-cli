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
//!
//! # What it does NOT catch
//!
//! Only the shape above: a platform module named without a guard. It cannot
//! see the OTHER half of the same mistake, which is what a `cfg` leaves
//! BEHIND - an import, a `mut` binding or a whole function that is used only
//! inside `#[cfg(unix)]` and is therefore dead on Windows. Those are lints,
//! they are cfg-sensitive, and deciding them means running the compiler for
//! the other target. This guard would have to reimplement rustc to guess.
//!
//! `scripts/win-check.sh` runs that compile (`cargo clippy --target
//! x86_64-pc-windows-msvc --workspace --all-targets -- -D warnings`), which
//! is the only thing that does catch them - and it is the local gate that
//! gets forgotten, because it is the one the other four commands do not
//! imply. It was forgotten right after `auth/secret_file.rs` was split into
//! `auth/file_guard.rs`, and Windows CI failed on exactly those two lints in
//! the new file. [`the_windows_cross_check_is_offered_to_developers`] keeps
//! the reminder from being deleted; running it is still a human act.

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
/// Looks upwards for a `#[cfg(...)]` that enables the code on that platform,
/// at an indentation no deeper than the use itself - an attribute indented
/// further belongs to something nested, not to the item in hand.
fn is_guarded(lines: &[&str], index: usize, module: &str) -> bool {
    let platform = module.rsplit("::").next().unwrap_or(module);
    let depth = indentation(lines[index]);
    let start = index.saturating_sub(GUARD_WINDOW);
    lines[start..=index]
        .iter()
        .rev()
        .any(|line| is_cfg_for(line.trim_start(), platform) && indentation(line) <= depth)
}

/// Whether one attribute line is a `cfg` that ENABLES code for `platform`.
///
/// Two ways to get this wrong, both of which produce a guard that reassures
/// without checking anything:
///
/// - `#[cfg_attr(unix, ...)]` is not a compilation guard at all. It applies
///   another attribute conditionally; the item itself still compiles
///   everywhere, so a `std::os::unix` use under it still breaks Windows.
/// - `#[cfg(not(unix))]` MENTIONS the platform while meaning the opposite.
///   On Windows `not(unix)` is true and `std::os::unix` does not exist, so
///   accepting it is exactly the failure this test exists to catch. A plain
///   substring search accepts it, which is how it slipped through: the
///   negation is stripped before looking.
fn is_cfg_for(attribute: &str, platform: &str) -> bool {
    if !attribute.starts_with("#[cfg(") {
        return false;
    }
    mentions_word(&strip_negations(attribute), platform)
}

/// The attribute with every `not( ... )` group removed.
///
/// Balanced removal rather than a text match, so `not(any(unix, windows))`
/// goes as a whole and a nested `all(unix, not(test))` keeps its `unix`.
fn strip_negations(attribute: &str) -> String {
    let mut out = String::with_capacity(attribute.len());
    let mut rest = attribute;
    while let Some(at) = rest.find("not(") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "not(".len()..];
        let mut depth = 1_usize;
        let mut end = after.len();
        for (offset, byte) in after.bytes().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = after.get(end + 1..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// Whether `haystack` contains `needle` as a whole identifier.
///
/// So a `feature = "unixsocket"` predicate does not read as a `unix` guard.
fn mentions_word(haystack: &str, needle: &str) -> bool {
    let boundary = |byte: Option<u8>| match byte {
        Some(byte) => !byte.is_ascii_alphanumeric() && byte != b'_',
        None => true,
    };
    let bytes = haystack.as_bytes();
    haystack.match_indices(needle).any(|(at, _)| {
        boundary(at.checked_sub(1).map(|before| bytes[before]))
            && boundary(bytes.get(at + needle.len()).copied())
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

    // A NEGATED guard mentions the platform while meaning the opposite. On
    // Windows `not(unix)` is true and `std::os::unix` does not exist, so
    // this is precisely the break this test exists to catch - and a plain
    // substring search accepts it, which is how it went unnoticed.
    let negated = vec![
        "#[cfg(not(unix))]",
        "fn t() {",
        "    use std::os::unix::fs::symlink;",
        "}",
    ];
    assert!(
        !is_guarded(&negated, 2, "std::os::unix"),
        "a negated cfg was accepted as a guard for the thing it excludes"
    );

    // `not(windows)` is true on wasi too, where `std::os::unix` is also
    // absent, so it is not a guard for it either.
    let not_windows = vec![
        "#[cfg(not(windows))]",
        "fn t() {",
        "    use std::os::unix::fs::symlink;",
        "}",
    ];
    assert!(!is_guarded(&not_windows, 2, "std::os::unix"));

    // Compound predicates that DO enable the code still count.
    for guard in [
        "#[cfg(all(unix, target_pointer_width = \"64\"))]",
        "#[cfg(any(unix, windows))]",
        "#[cfg(all(unix, not(test)))]",
    ] {
        let compound = vec![
            guard,
            "fn t() {",
            "    use std::os::unix::fs::symlink;",
            "}",
        ];
        assert!(
            is_guarded(&compound, 2, "std::os::unix"),
            "{guard} was not accepted"
        );
    }

    // `cfg_attr` is not a compilation guard: it applies another attribute
    // conditionally and the item compiles everywhere regardless.
    let cfg_attr = vec![
        "#[cfg_attr(unix, allow(dead_code))]",
        "fn t() {",
        "    use std::os::unix::fs::symlink;",
        "}",
    ];
    assert!(!is_guarded(&cfg_attr, 2, "std::os::unix"));

    // A predicate that merely contains the platform name inside a longer
    // identifier is not a guard.
    let lookalike = vec![
        "#[cfg(feature = \"unixsocket\")]",
        "fn t() {",
        "    use std::os::unix::fs::symlink;",
        "}",
    ];
    assert!(!is_guarded(&lookalike, 2, "std::os::unix"));

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

/// The cross-compile lint, which is the only thing that catches what this
/// file cannot.
const WIN_CHECK: &str = "scripts/win-check.sh";

#[test]
fn the_windows_cross_check_is_offered_to_developers() {
    // Not "was it run" - nothing local can know that. What this pins is that
    // the script still exists, is still runnable, and is still named in the
    // one place a contributor looks for the list of local gates. The failure
    // it guards against is the quiet one: the script rots or the line in the
    // README is tidied away, and the next `cfg`-heavy split breaks Windows
    // with nothing having asked for the check.
    let root = workspace_root();
    let script = root.join(WIN_CHECK);
    assert!(
        script.is_file(),
        "{WIN_CHECK} is gone: the lint that catches cfg-only dead code on \
         Windows now runs nowhere before a push"
    );
    let source = std::fs::read_to_string(&script).unwrap();
    assert!(
        source.contains("--all-targets"),
        "{WIN_CHECK} no longer passes --all-targets, so it would miss the \
         same mistake in test code - which is where it takes the whole \
         harness down"
    );
    assert!(
        source.contains("-D warnings"),
        "{WIN_CHECK} no longer denies warnings, so it would not fail on the \
         unused import and unused mut that Windows CI fails on"
    );
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    let development = readme
        .split_once("## Development")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    assert!(
        development.contains(WIN_CHECK),
        "{WIN_CHECK} is not listed under README's Development section, which \
         is the checklist it has to be on to be remembered"
    );
}
