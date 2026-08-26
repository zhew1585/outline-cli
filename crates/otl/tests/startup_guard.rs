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
//!
//! Story 4.2 note: `otl spec sync` DOES parse an OpenAPI document at run
//! time - that is its whole job, and SPEC.md carves it out explicitly. It
//! never touches the vendored file though: the document arrives from the
//! network (or from a path the user typed), is compiled once, and is stored
//! as a framed IR cache that later commands only deserialize. Every guard
//! below therefore still holds, unchanged: no runtime source locates the
//! vendored spec, and startup parses no document.
//!
//! # What the source scan is, and what it is not
//!
//! Layers 1 and 3 are REGRESSION GUARDS over source text, not proofs.
//! A string scan does not parse Rust, and a path can always be assembled
//! from pieces - so what the scan actually guarantees is narrower and more
//! useful than "the spec cannot be read": every way this codebase opens a
//! file is registered here, at the CALL SITE, and adding an unregistered
//! one fails. A read performed by a subprocess, by a dependency, or
//! through an API nobody listed would pass the scan - and would still have
//! to get past layer 2 (the shipped binary carries neither the spec path
//! nor its content) and layer 3 (the process works with no spec file
//! anywhere near it).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

mod common;
use common::{no_cache_dir, CACHE_DIR_ENV};

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
fn otl_cmd(dir: &Path, bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.current_dir(dir)
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        // The guard is about the spec inside the binary; a synced cache on
        // the developer's machine must not stand in for it.
        .env(CACHE_DIR_ENV, no_cache_dir())
        .env_remove("OUTLINE_PROFILE")
        // Empty value = read no user config file (Story 4.1).
        .env("OUTLINE_CONFIG", "");
    cmd
}

/// File name of the vendored spec; a runtime read has to name it.
const SPEC_FILE_NAME: &str = "spec3.json";
/// How the vendored spec is named when it is READ from disk: the file name
/// alone is not enough evidence, because the upstream URL ends in the same
/// file name and `spec sync` legitimately carries that URL (Story 4.2). A
/// path fragment cannot appear in that URL.
const SPEC_PATH_MARKER: &str = "spec/spec3.json";
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
        !contains_bytes(&bytes, SPEC_PATH_MARKER),
        "{} references {SPEC_PATH_MARKER}: the runtime must not open the \
         vendored spec (build.rs compiles it to a static IR table)",
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

/// A reviewed exception: `pattern` is tolerated in `file`, but ONLY on
/// lines that also contain `context`.
///
/// The context is what keeps an exception from becoming a hole. A
/// file-wide exemption would let a later `fs::read_to_string("spec3.json")`
/// into the same file unnoticed - the very thing this guard exists to
/// prevent - so each entry pins the one construct that was reviewed, and
/// every OTHER occurrence in that file is still a violation.
struct Exception {
    file: &'static str,
    pattern: &'static str,
    context: &'static str,
}

/// Reviewed exceptions. Add one only with a comment saying why, and keep
/// the context as specific as the reviewed line allows.
const SOURCE_SCAN_ALLOWLIST: &[Exception] = &[
    // Story 4.2: the upstream spec URL ends in the same file name. It is a
    // URL constant for `otl spec sync` (a network fetch on one explicit
    // command), not a path to the vendored file - nothing here reads a spec
    // from disk. The context pins it to the URL line: any other mention of
    // the file name in this file is still a violation.
    Exception {
        file: "crates/otl/src/spec/mod.rs",
        pattern: SPEC_FILE_NAME,
        context: "https://raw.githubusercontent.com/outline/openapi",
    },
    // Story 4.2: the name of the `--spec <PATH>` flag of `otl spec sync`,
    // which compiles a document the USER points at (the documented
    // development override). It is a clap flag name, not a directory this
    // process goes looking in.
    Exception {
        file: "crates/otl/src/commands/spec.rs",
        pattern: "\"spec\"",
        context: "#[arg(long = ",
    },
];

/// Ways to get at a file's contents. Every one of them must appear only
/// at a registered call site ([`FILE_READ_ALLOWLIST`]).
///
/// This is the rule that makes the literal-string rules above hard to
/// dodge. A path can always be assembled out of pieces
/// (`["spec/spec3", ".json"].concat()`), so no amount of substring
/// matching on path literals is conclusive - but a read still has to go
/// through one of these, `.open(` included, which covers the aliases
/// (`File::options()`, a renamed `OpenOptions`) that name-based matching
/// would miss.
///
/// What this does NOT do: recognise a read performed by a subprocess, a
/// dependency, or an API added to std after this list was written. It is a
/// registry of the ways this codebase opens files, and it makes adding an
/// unregistered one fail - which is the review it stands in for, not a
/// proof.
const FORBIDDEN_FILE_READS: &[(&str, &str)] = &[
    ("read_to_string", "reading a file by name"),
    ("File::open", "opening a file by name"),
    ("File::options", "opening a file by name"),
    ("OpenOptions", "opening a file by name"),
    (".open(", "opening a file, whatever the type is called"),
    ("fs::read", "reading a file by name"),
    // `read_to_end`/`read` on an already-open handle are deliberately NOT
    // here: they are how a response body is read, and getting a FILE handle
    // to use them on requires one of the patterns above.
    ("read_dir", "enumerating a directory to find a file"),
    ("include_str!", "embedding a file at compile time"),
    ("include_bytes!", "embedding a file at compile time"),
];

/// The registered file-opening call sites, as (file, pattern, line
/// context).
///
/// Per CALL SITE, not per file: a second read added to one of these files
/// fails the guard, because its line will not match the registered
/// context. Each entry names what it opens and why that is not the
/// vendored spec.
const FILE_READ_ALLOWLIST: &[Exception] = &[
    // The `--body @file.json` request body, named by the user (Story 1.3).
    Exception {
        file: "crates/otl/src/commands/api.rs",
        pattern: "File::open",
        context: "let file = File::open(path)",
    },
    Exception {
        file: "crates/otl/src/commands/api.rs",
        pattern: "read_to_string",
        context: ".read_to_string(&mut raw)",
    },
    // The `--spec <PATH>` document, likewise named by the user.
    Exception {
        file: "crates/otl/src/commands/spec.rs",
        pattern: "read_to_string",
        context: ".read_to_string(&mut raw)",
    },
    // The one place the runtime opens a path: a watchdogged open used by
    // both the `--spec` path and the cache.
    Exception {
        file: "crates/otl/src/spec/openfile.rs",
        pattern: "File::open",
        context: "sender.send(File::open(owned))",
    },
    // The user config file (Story 4.1), at a path that comes from
    // `OUTLINE_CONFIG` or from `directories` - never from the build, so it
    // cannot reach the vendored spec.
    Exception {
        file: "crates/otl/src/config/file.rs",
        pattern: "File::open",
        context: "let file = File::open(path)",
    },
    Exception {
        file: "crates/otl/src/config/file.rs",
        pattern: "read_to_string",
        context: ".read_to_string(&mut raw)",
    },
    // Test-only, in that module's own tests: opening the write end of a
    // FIFO so the blocked worker thread finishes.
    Exception {
        file: "crates/otl/src/spec/openfile.rs",
        pattern: "OpenOptions",
        context: "let _writer",
    },
    Exception {
        file: "crates/otl/src/spec/openfile.rs",
        pattern: ".open(",
        context: "let _writer",
    },
];

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

/// Normalized, workspace-relative form of a source path.
fn relative(file: &Path) -> String {
    file.to_string_lossy().replace('\\', "/")
}

/// Every line of `source` that contains `pattern` and is NOT covered by a
/// reviewed exception for this file.
fn unexcused_lines<'a>(file: &Path, source: &'a str, pattern: &str) -> Vec<&'a str> {
    let path = relative(file);
    let exceptions: Vec<&Exception> = SOURCE_SCAN_ALLOWLIST
        .iter()
        .filter(|entry| entry.pattern == pattern && path.ends_with(entry.file))
        .collect();
    source
        .lines()
        .filter(|line| line.contains(pattern))
        .filter(|line| !exceptions.iter().any(|entry| line.contains(entry.context)))
        .collect()
}

/// Code lines containing `pattern` that no registered call site covers.
fn unexcused_read_lines<'a>(file: &Path, source: &'a str, pattern: &str) -> Vec<&'a str> {
    let path = relative(file);
    let sites: Vec<&Exception> = FILE_READ_ALLOWLIST
        .iter()
        .filter(|entry| entry.pattern == pattern && path.ends_with(entry.file))
        .collect();
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| line.contains(pattern))
        .filter(|line| !sites.iter().any(|entry| line.contains(entry.context)))
        .collect()
}

#[test]
fn runtime_sources_never_reach_for_the_spec() {
    let mut violations = Vec::new();
    for file in runtime_source_files() {
        let source = std::fs::read_to_string(&file).unwrap();
        for (pattern, reason) in FORBIDDEN_SOURCE_PATTERNS {
            for line in unexcused_lines(&file, &source, pattern) {
                violations.push(format!(
                    "  {}: {pattern} - {reason}\n    at: {}",
                    file.display(),
                    line.trim()
                ));
            }
        }
        for (pattern, reason) in FORBIDDEN_FILE_READS {
            // Comment lines are skipped for THIS family only: a comment
            // cannot open a file, and these modules have to be able to
            // explain themselves. The spec-path patterns above deliberately
            // do scan comments, because there a mention is worth reviewing.
            for line in unexcused_read_lines(&file, &source, pattern) {
                violations.push(format!(
                    "  {}: {pattern} - {reason}\n    at: {}",
                    file.display(),
                    line.trim()
                ));
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
