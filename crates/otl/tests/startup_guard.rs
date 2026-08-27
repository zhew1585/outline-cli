//! Startup guards: the vendored OpenAPI spec is compiled to a
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
//! Note: `otl spec sync` DOES parse an OpenAPI document at run
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

/// Place the built `otl` binary in a fresh empty temp dir outside the repo;
/// returns (dir, binary path).
///
/// Linked rather than copied, and the reason is a real failure this suite hit
/// on Linux. These tests run in parallel, and a copy holds a write
/// descriptor on its destination while it runs. `Command::spawn` forks, and
/// the child inherits every descriptor open at that moment - `O_CLOEXEC`
/// closes them at exec, not at fork - so one test's in-flight copy could
/// still be held open by a sibling's forked child when the copying test
/// reached its own exec. The kernel refuses to exec a file anyone has open
/// for writing: `ETXTBSY`. Measured 1-in-4 with four parallel copy-then-exec
/// pairs; macOS does not enforce this, which is why only the Linux leg of CI
/// saw it.
///
/// `hard_link` opens nothing, so the window does not exist. It is also
/// closer to the intent: what these tests need is the binary reachable from
/// a directory with no spec file beside it, not a second copy of its bytes.
/// A link cannot cross filesystems, so the copy path stays as a fallback -
/// and because that path can still lose the race, it retries.
fn isolated_otl(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("otl-startup-guard-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let built = assert_cmd::cargo::cargo_bin("otl");
    let placed = dir.join(built.file_name().unwrap());
    let _ = std::fs::remove_file(&placed);

    if std::fs::hard_link(&built, &placed).is_ok() {
        return (dir, placed);
    }

    // Different filesystem, so the link could not be made: copy instead,
    // and give a lost race a moment to clear.
    for attempt in 0..10 {
        match std::fs::copy(&built, &placed) {
            Ok(_) => {
                flush_to_disk(&placed);
                return (dir, placed);
            }
            Err(error) if attempt < 9 => {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let _ = error;
            }
            Err(error) => panic!("could not place otl in {}: {error}", dir.display()),
        }
    }
    unreachable!("the loop returns or panics")
}

/// Wait for a just-copied binary to actually be on disk before it is exec'd.
///
/// `fs::copy` returning does not mean the write has landed: on a filesystem
/// that acknowledges a close before writeback finishes - a bind-mounted
/// host directory under virtiofs is the case that sent us here, which is
/// exactly the layout `scripts/check-all.sh --linux` uses - the kernel can
/// still hold the file open for writing when we exec it, and exec answers
/// `ETXTBSY` (`Text file busy`). Measured on that layout: 1 of 2 startup
/// tests failed per run, roughly half the runs. With the target directory
/// inside the container instead, 2 of 2 passed. It never reproduces on
/// native macOS, so this is a flake nobody would see locally.
///
/// The descriptor is opened READ-ONLY on purpose. `fsync` flushes the
/// inode's dirty pages whichever way the descriptor was opened, and opening
/// for writing would reintroduce the very thing the `hard_link` above
/// exists to avoid: a sibling test's `Command::spawn` forks, inherits the
/// write descriptor, and holds this binary busy for as long as that child
/// lives.
#[cfg(unix)]
fn flush_to_disk(path: &Path) {
    let file = std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("could not reopen {} to flush: {error}", path.display()));
    file.sync_all()
        .unwrap_or_else(|error| panic!("could not flush {}: {error}", path.display()));
}

/// Windows has nothing to wait for here, and no way to ask on these terms.
///
/// `ETXTBSY` is POSIX. Windows refuses a write to a running image with a
/// sharing violation instead, and `fs::copy` has closed its own handle by the
/// time it returns, so there is no in-flight writeback for a later exec to
/// trip over.
///
/// Asking anyway is not free, which is how this was found: `sync_all` is
/// `FlushFileBuffers`, which needs write access, so on the READ-ONLY
/// descriptor the Unix arm deliberately uses it fails with "Access is
/// denied" (os error 5). That took all four `*_with_no_spec_file_reachable`
/// tests down on windows-latest - and only there, because the copy path is
/// the normal path on that runner: the workspace is on `D:` and the temp
/// directory on `C:`, so `hard_link` cannot make the link and the fallback
/// always runs.
#[cfg(not(unix))]
fn flush_to_disk(_path: &Path) {}

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
        .env_remove("OUTLINE_API_KEY")
        // The guard is about the spec inside the binary; a synced cache on
        // the developer's machine must not stand in for it.
        .env(CACHE_DIR_ENV, no_cache_dir())
        .env_remove("OUTLINE_PROFILE")
        // Empty value = read no user config file.
        .env("OUTLINE_CONFIG", "");
    cmd
}

/// How the vendored spec is named when it is READ from disk: the file name
/// alone is not enough evidence, because the upstream URL ends in the same
/// file name and `spec sync` legitimately carries that URL. A
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

mod guard_registry;

use guard_registry::{
    Exception, FILE_READ_ALLOWLIST, FORBIDDEN_FILE_READS, FORBIDDEN_SOURCE_PATTERNS,
    SOURCE_SCAN_ALLOWLIST, SPEC_FILE_NAME,
};

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

/// The source with every `#[cfg(test)]` module removed.
///
/// Brace-counted from the module's opening `{`, so a nested block cannot
/// end the module early. Anything this misses is scanned, not skipped: a
/// module it fails to recognise stays in, which is the safe direction.
fn strip_test_modules(source: &str) -> String {
    let mut kept = String::with_capacity(source.len());
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() != "#[cfg(test)]" {
            kept.push_str(line);
            kept.push('\n');
            continue;
        }
        // Only a MODULE is skipped; a `#[cfg(test)]` on anything else stays.
        match lines.peek() {
            Some(next) if next.trim_start().starts_with("mod ") => {}
            _ => {
                kept.push_str(line);
                kept.push('\n');
                continue;
            }
        }
        let mut depth = 0usize;
        let mut opened = false;
        for body in lines.by_ref() {
            depth += body.matches('{').count();
            depth -= body.matches('}').count().min(depth);
            if body.contains('{') {
                opened = true;
            }
            if opened && depth == 0 {
                break;
            }
        }
    }
    kept
}

/// Normalized, workspace-relative form of a source path.
fn relative(file: &Path) -> String {
    file.to_string_lossy().replace('\\', "/")
}

/// The exceptions registered for one (file, pattern).
fn exceptions_for<'a>(
    allowlist: &'a [Exception],
    file: &Path,
    pattern: &str,
) -> Vec<&'a Exception> {
    let path = relative(file);
    allowlist
        .iter()
        .filter(|entry| entry.pattern == pattern && path.ends_with(entry.file))
        .collect()
}

/// Judge one file against one forbidden pattern, checking BOTH halves of
/// every exception: each occurrence must sit on a reviewed line, and the
/// number of occurrences must be the number that was reviewed.
///
/// `skip_comments` is true for the file-reading family only: a comment
/// cannot open a file, and those modules have to be able to explain
/// themselves. The spec-path patterns deliberately do scan comments,
/// because there a mention is worth reviewing.
///
/// `#[cfg(test)]` modules are excluded from the file-reading family for the
/// same reason: that family asks "what can the RUNTIME open?", and a test
/// module is not part of the shipped binary - it is compiled only into the
/// test harness, which the release-binary assertions above cover. Including
/// them meant registering every temp-file read a write test makes, which is
/// churn that teaches a reader nothing. The spec-path family still scans
/// them, because naming the vendored spec in a test is worth a look.
fn scan(
    allowlist: &[Exception],
    file: &Path,
    source: &str,
    pattern: &str,
    reason: &str,
    skip_comments: bool,
) -> Vec<String> {
    let production = if skip_comments {
        strip_test_modules(source)
    } else {
        source.to_string()
    };
    let lines: Vec<&str> = production
        .lines()
        .filter(|line| !(skip_comments && line.trim_start().starts_with("//")))
        .filter(|line| line.contains(pattern))
        .collect();
    let registered = exceptions_for(allowlist, file, pattern);
    let mut violations = Vec::new();
    for line in &lines {
        if !registered.iter().any(|entry| line.contains(entry.context)) {
            violations.push(format!(
                "  {}: {pattern} - {reason}\n    at: {}",
                file.display(),
                line.trim()
            ));
        }
    }
    let found: usize = lines.iter().map(|line| line.matches(pattern).count()).sum();
    let allowed: usize = registered.iter().map(|entry| entry.count).sum();
    if violations.is_empty() && found != allowed {
        violations.push(format!(
            "  {}: {pattern} appears {found} time(s), but {allowed} reviewed \
             occurrence(s) are allowlisted - review the new one and update \
             its count in the allowlist",
            file.display()
        ));
    }
    violations
}

#[test]
fn runtime_sources_never_reach_for_the_spec() {
    let mut violations = Vec::new();
    for file in runtime_source_files() {
        let source = std::fs::read_to_string(&file).unwrap();
        for (pattern, reason) in FORBIDDEN_SOURCE_PATTERNS {
            violations.extend(scan(
                SOURCE_SCAN_ALLOWLIST,
                &file,
                &source,
                pattern,
                reason,
                false,
            ));
        }
        for (pattern, reason) in FORBIDDEN_FILE_READS {
            violations.extend(scan(
                FILE_READ_ALLOWLIST,
                &file,
                &source,
                pattern,
                reason,
                true,
            ));
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

/// Negative test for the source scan itself: an allowlist entry must be a
/// review of specific call sites, not a blanket exemption for a file.
///
/// Both halves are exercised, because they fail differently. Without the
/// COUNT, a second `read_dir` added to an allowlisted file - including one
/// enumerating the vendored spec directory - slips through on the strength
/// of the first one's exception. Without the CONTEXT, any `read_dir` in that
/// file passes as long as the total happens to match.
#[test]
fn the_allowlist_does_not_exempt_a_whole_file() {
    let allowlisted = Path::new("crates/otl/src/commands/docs/outdir.rs");
    let pattern = "read_dir";
    let reason = "test reason";
    let scan_file = |source: &str| {
        scan(
            SOURCE_SCAN_ALLOWLIST,
            allowlisted,
            source,
            pattern,
            reason,
            false,
        )
    };
    assert_eq!(
        exceptions_for(SOURCE_SCAN_ALLOWLIST, allowlisted, pattern).len(),
        1,
        "the fixture no longer matches the allowlist"
    );

    // Exactly the reviewed occurrence, on the reviewed line: clean.
    assert!(scan_file("    let entries = std::fs::read_dir(dir).map_err(|e| e)?;").is_empty());

    // One more occurrence than was reviewed: a violation naming both counts.
    let extra = "let entries = std::fs::read_dir(dir);\nlet x = std::fs::read_dir(spec_dir);";
    let violations = scan_file(extra);
    assert!(
        !violations.is_empty(),
        "an unreviewed occurrence must be reported"
    );
    let joined = violations.join("\n");
    assert!(joined.contains("read_dir"), "{joined}");

    // Fewer than reviewed: also a violation, so a stale exception cannot
    // linger after the code it covered is gone.
    assert!(!scan_file("nothing here").is_empty());

    // The reviewed COUNT but a different line: the context catches it.
    let wrong_line = "let sneaky = std::fs::read_dir(spec_dir);";
    let violations = scan_file(wrong_line);
    assert!(
        violations.iter().any(|text| text.contains("sneaky")),
        "an occurrence on an unreviewed line must be reported: {violations:?}"
    );

    // A file with no exception at all is reported on the first occurrence.
    let other = Path::new("crates/otl/src/session.rs");
    assert!(exceptions_for(SOURCE_SCAN_ALLOWLIST, other, pattern).is_empty());
    assert!(!scan(
        SOURCE_SCAN_ALLOWLIST,
        other,
        "std::fs::read_dir(x)",
        pattern,
        reason,
        false
    )
    .is_empty());
}
