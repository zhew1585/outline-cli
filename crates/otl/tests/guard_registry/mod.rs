//! What the startup guard has REVIEWED, as data.
//!
//! Split out of `startup_guard.rs` because it is a different kind of thing:
//! that file holds the checks, this one holds the registry they check
//! against - the patterns that may not appear in runtime sources, and the
//! individual call sites that were reviewed and allowed.
//!
//! The split is also what keeps either half readable. The registry grows
//! every time a story legitimately opens a file (the `--body` argument, the
//! export directory, the credential file), and it grew past the point where
//! a reader looking for the CHECK had to scroll through a hundred lines of
//! justifications to find it.
//!
//! `startup_guard.rs` owns the tests that keep this registry honest:
//! `the_allowlist_does_not_exempt_a_whole_file` (a `context` must name a
//! call site, not a file) and the count check inside `scan` (a second
//! occurrence at an allowlisted site still fails until its count is
//! updated).

#![allow(dead_code)]

/// File name of the vendored spec; a runtime read has to name it.
///
/// Registry data, not a test fixture: it is one of the patterns below.
pub const SPEC_FILE_NAME: &str = "spec3.json";

/// Constructs a runtime spec read would need, with why each is forbidden in
/// runtime sources. Plain substring matches, comments included: a mention is
/// as good as a use for review purposes, and genuine exceptions go through
/// `SOURCE_SCAN_ALLOWLIST` so they stay visible.
pub const FORBIDDEN_SOURCE_PATTERNS: &[(&str, &str)] = &[
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
/// lines that also contain `context`, and only `count` times.
///
/// The two halves catch different things, and both came from a real miss:
///
/// - the CONTEXT keeps an exception from becoming a file-wide hole. A
///   file-wide exemption would let a later `fs::read_to_string("spec3.json")`
///   into the same file unnoticed - the very thing this guard exists to
///   prevent.
/// - the COUNT catches a second occurrence that happens to match the same
///   context anyway. Removing one fails too, so a stale exception cannot
///   linger while appearing to constrain something.
///
/// The release-binary assertions above remain the hard proof that no spec is
/// embedded or opened at runtime; this scan is the early warning.
pub struct Exception {
    pub file: &'static str,
    pub pattern: &'static str,
    pub context: &'static str,
    pub count: usize,
}

/// Reviewed exceptions. Add one only with a comment saying why, and keep
/// the context as specific as the reviewed line allows.
pub const SOURCE_SCAN_ALLOWLIST: &[Exception] = &[
    // The upstream spec URL ends in the same file name. It is a
    // URL constant for `otl spec sync` (a network fetch on one explicit
    // command), not a path to the vendored file - nothing here reads a spec
    // from disk. The context pins it to the URL line: any other mention of
    // the file name in this file is still a violation.
    Exception {
        file: "crates/otl/src/spec/mod.rs",
        pattern: SPEC_FILE_NAME,
        context: "https://raw.githubusercontent.com/outline/openapi",
        count: 1,
    },
    // The name of the `--spec <PATH>` flag of `otl spec sync`,
    // which compiles a document the USER points at (the documented
    // development override). It is a clap flag name, not a directory this
    // process goes looking in.
    Exception {
        file: "crates/otl/src/commands/spec.rs",
        pattern: "\"spec\"",
        context: "#[arg(long = ",
        count: 1,
    },
    // `otl docs export` refuses to write into a directory that
    // already has contents unless `--overwrite` is given, and has to tell
    // leftovers of its own from content the user put there - both of which
    // mean enumerating the user-supplied output directory. Nothing to do
    // with the vendored spec.
    Exception {
        file: "commands/docs/outdir.rs",
        pattern: "read_dir",
        context: "let entries = std::fs::read_dir(dir)",
        count: 1,
    },
    // One `#[cfg(test)]` helper that lists a temporary directory the test
    // just created, so the write tests can assert exactly which entries a
    // write left behind - which is how "no temporary file survived" is
    // checked.
    Exception {
        file: "commands/docs/target.rs",
        pattern: "read_dir",
        context: "let mut found: Vec<String> = std::fs::read_dir(dir)",
        count: 1,
    },
    // Golden-file assertions inside `#[cfg(test)]` modules: the curated
    // commands' human-readable output is compared byte-for-byte against
    // `tests/golden/*.txt`. Test fixtures, compiled only into the test
    // harness, never into the shipped binary.
    Exception {
        file: "commands/collections.rs",
        pattern: "include_str!",
        context: "tests/golden/collections_list_table.txt",
        count: 1,
    },
    Exception {
        file: "commands/docs/detail.rs",
        pattern: "include_str!",
        context: "tests/golden/docs_detail_pairs.txt",
        count: 1,
    },
    Exception {
        file: "commands/docs/search.rs",
        pattern: "include_str!",
        context: "tests/golden/docs_search_table.txt",
        count: 1,
    },
];

/// Ways to get at a file's contents. Every one of them must appear only
/// at a registered call site ([`FILE_READ_ALLOWLIST`]).
///
/// This is the rule that makes the literal-string rules above hard to
/// dodge. A path can always be assembled out of pieces
/// (`["spec/spec3", ".json"].concat()`), so no amount of substring matching
/// on path literals is conclusive - but a read still has to go through one
/// of these, `.open(` included, which covers the aliases
/// (`File::options()`, a renamed `OpenOptions`) that name-based matching
/// would miss.
///
/// What this does NOT do: recognise a read performed by a subprocess, a
/// dependency, or an API added to std after this list was written. It is a
/// registry of the ways this codebase opens files, and it makes adding an
/// unregistered one fail - which is the review it stands in for, not a
/// proof.
pub const FORBIDDEN_FILE_READS: &[(&str, &str)] = &[
    ("read_to_string", "reading a file by name"),
    ("File::open", "opening a file by name"),
    ("File::options", "opening a file by name"),
    ("OpenOptions", "opening a file by name"),
    (".open(", "opening a file, whatever the type is called"),
    ("fs::read", "reading a file by name"),
    // `read_to_end`/`read` on an already-open handle are deliberately NOT
    // here: they are how a response body is read, and getting a FILE handle
    // to use them on requires one of the patterns above.
];

/// The registered file-opening call sites.
///
/// Per CALL SITE, not per file: a second read added to one of these files
/// fails the guard, because its line will not match the registered context
/// and its count will not match either.
pub const FILE_READ_ALLOWLIST: &[Exception] = &[
    // User-selected command input files (`--body @file.json` and comment
    // ProseMirror data). One reviewed, bounded reader owns both call sites.
    Exception {
        file: "crates/otl/src/commands/input.rs",
        pattern: "File::open",
        context: "let file = File::open(path)",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/commands/input.rs",
        pattern: "read_to_string",
        context: ".read_to_string(&mut text)",
        count: 1,
    },
    // The `--spec <PATH>` document, likewise named by the user.
    Exception {
        file: "crates/otl/src/commands/spec.rs",
        pattern: "read_to_string",
        context: ".read_to_string(&mut raw)",
        count: 1,
    },
    // The one place the runtime opens a path: a watchdogged open used by
    // both the `--spec` path and the cache.
    Exception {
        file: "crates/otl/src/spec/openfile.rs",
        pattern: "File::open",
        context: "sender.send(File::open(owned))",
        count: 1,
    },
    // `otl docs export` writes into a user-named directory.
    // Opening the directory itself is how it is fsynced (a write is not
    // durable until the directory entry is), and `read_dir` is how it tells
    // its own leftovers from content the user put there.
    //
    // Two occurrences, both the user's own output directory and neither the
    // vendored spec: `flush_directory` opens it to fsync it, and
    // `open_directory` holds it open for the lifetime of the pin so the
    // inode cannot be freed and its number reused under us - which is what
    // makes the `(dev, ino)` pin mean anything on Linux.
    Exception {
        file: "crates/otl/src/commands/docs/dir.rs",
        pattern: "File::open",
        context: "std::fs::File::open(path)",
        count: 2,
    },
    Exception {
        file: "crates/otl/src/commands/docs/outdir.rs",
        pattern: "fs::read",
        context: "let entries = std::fs::read_dir(dir)",
        count: 1,
    },
    // The exported file itself, created in the user's output directory.
    Exception {
        file: "crates/otl/src/commands/docs/target.rs",
        pattern: "OpenOptions",
        context: "let file = std::fs::OpenOptions::new()",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/commands/docs/target.rs",
        pattern: ".open(",
        context: ".open(&path)?",
        count: 1,
    },
    // The credential file. `auth::secret_file` is the ONLY module
    // that opens it, and every one of these call sites exists because a
    // credential file cannot be opened the ordinary way:
    //
    // - the read is `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC` and then fstats the
    //   descriptor it actually got, so a symlink or a fifo swapped in between
    //   the check and the open is refused rather than followed;
    // - the write is `create_new` with mode 0600 passed to `open(2)` itself,
    //   never created-then-chmod'd;
    // - the directory is opened only to fsync it, because a rename is not
    //   durable until the directory entry is.
    //
    // Registered per call site like everything else here: a new open in this
    // module is exactly the change that must not pass unreviewed.
    Exception {
        file: "crates/otl/src/auth/secret_file.rs",
        pattern: "read_to_string",
        context: ".read_to_string(&mut text)",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/auth/secret_file.rs",
        pattern: "File::open",
        context: "match File::open(path) {",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/auth/secret_file.rs",
        pattern: "File::open",
        context: "let handle = File::open(dir)",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/auth/secret_file.rs",
        pattern: "OpenOptions",
        context: "use std::fs::{self, File, OpenOptions};",
        count: 1,
    },
    Exception {
        // The `OpenOptionsExt` import, whose `mode()` is what passes 0600 to
        // `open(2)`. Registered by the trait name rather than by the whole
        // `use` line, because spelling the platform module path in a data
        // literal here trips `portability.rs` - which cannot tell a string
        // from an import, and should not have to.
        file: "crates/otl/src/auth/secret_file.rs",
        pattern: "OpenOptions",
        context: "OpenOptionsExt",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/auth/secret_file.rs",
        pattern: "OpenOptions",
        context: "let mut options = OpenOptions::new();",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/auth/secret_file.rs",
        pattern: "OpenOptions",
        context: "OpenOptions::new()",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/auth/secret_file.rs",
        pattern: ".open(",
        context: "options.open(path)",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/auth/secret_file.rs",
        pattern: ".open(",
        context: ".open(path)",
        count: 1,
    },
    // The credential DIRECTORY, opened to fsync it after a rename.
    Exception {
        file: "crates/otl/src/auth/file_guard.rs",
        pattern: "File::open",
        context: "let handle = File::open(dir)",
        count: 1,
    },
    // `otl auth set-key` reads the key from piped stdin. A
    // handle, not a path - but the pattern list matches the method name, and
    // a blanket file-wide exemption is exactly what this guard refuses.
    Exception {
        file: "crates/otl/src/commands/auth/mod.rs",
        pattern: "read_to_string",
        context: ".read_to_string(&mut raw)",
        count: 1,
    },
    // `otl docs create --file <PATH>`, the document body, named
    // by the user on the command line.
    Exception {
        file: "crates/otl/src/commands/docs/content.rs",
        pattern: "File::open",
        context: "let file = File::open(path).map_err(io_error)?",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/commands/docs/content.rs",
        pattern: "read_to_string",
        context: ".read_to_string(&mut text)",
        count: 1,
    },
    // The user config file, at a path that comes from
    // `OUTLINE_CONFIG` or from `directories` - never from the build, so it
    // cannot reach the vendored spec.
    Exception {
        file: "crates/otl/src/config/file.rs",
        pattern: "File::open",
        context: "let file = File::open(path)",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/config/file.rs",
        pattern: "read_to_string",
        context: ".read_to_string(&mut raw)",
        count: 1,
    },
    // The installed copy of the agent skill, so `otl skill install` and
    // `otl doctor` can say whether it is the current one. The path is
    // `<skills dir>/<skill name>/SKILL.md`, and both halves of it are
    // constrained: the directory comes from `--dir`, `OUTLINE_SKILL_DIR` or
    // `directories`, and the rest is compiled in - so it cannot name the
    // vendored spec. The file type is checked on `symlink_metadata` before
    // the read, and a non-regular file is refused rather than followed.
    //
    // One read, on one line, which both patterns in `FORBIDDEN_FILE_READS`
    // match ("read_to_string" and "fs::read"); each is registered with its
    // own count so a second read added here still fails.
    Exception {
        file: "crates/otl/src/commands/skill/targets.rs",
        pattern: "read_to_string",
        context: "match std::fs::read_to_string(path)",
        count: 1,
    },
    Exception {
        file: "crates/otl/src/commands/skill/targets.rs",
        pattern: "fs::read",
        context: "match std::fs::read_to_string(path)",
        count: 1,
    },
];
