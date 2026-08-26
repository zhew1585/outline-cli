//! The exit-code table in README.md must agree with `docs/exit-codes.md`.
//!
//! The exit-code table is a public API (project-context.md, CLI contract),
//! and it is published in two places: `docs/exit-codes.md` carries the full
//! detail and is the single source of truth, while README.md carries a
//! condensed code/meaning table because that is where users look first.
//! Two copies of a public API drift, so this test derives the README table
//! from the doc and fails when they disagree.
//!
//! When a story adds a new error class it registers the code in
//! `docs/exit-codes.md`; this test then fails until README.md is updated,
//! which is the point. Regenerate rather than hand-edit:
//!
//! ```sh
//! UPDATE_README_EXIT_CODES=1 cargo test -p outline-cli --test readme_exit_codes
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

/// Markers delimiting the derived block in README.md.
const BEGIN_MARKER: &str = "<!-- BEGIN GENERATED EXIT CODES";
const END_MARKER: &str = "<!-- END GENERATED EXIT CODES -->";

/// Env var that switches the test from checking to rewriting.
const UPDATE_ENV: &str = "UPDATE_README_EXIT_CODES";

/// Repository root, derived from this crate's manifest directory
/// (`<repo>/crates/otl`) so the test is independent of the working
/// directory `cargo test` happens to use.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate manifest dir has a grandparent")
        .to_path_buf()
}

/// One row of the published exit-code table.
#[derive(Debug, PartialEq, Eq)]
struct ExitCodeRow {
    code: u8,
    meaning: String,
}

/// Parses the leading `| Code | Meaning | ... |` table out of
/// `docs/exit-codes.md`, ignoring the wide "Examples" column.
fn parse_doc_table(doc: &str) -> Vec<ExitCodeRow> {
    let mut rows = Vec::new();
    for line in doc.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        if cells.len() < 2 {
            continue;
        }
        // Skips the header row and the `|---|---|` separator: only a cell
        // that parses as a number is a code.
        let Ok(code) = cells[0].parse::<u8>() else {
            continue;
        };
        rows.push(ExitCodeRow {
            code,
            meaning: cells[1].to_owned(),
        });
    }
    rows
}

/// Renders the README block (marker lines excluded) for the given rows.
fn render_readme_table(rows: &[ExitCodeRow]) -> String {
    let mut out = String::from("\n| Code | Meaning |\n|------|---------|\n");
    for row in rows {
        out.push_str(&format!("| {} | {} |\n", row.code, row.meaning));
    }
    out.push('\n');
    out
}

/// Splits README into (before-block, block-body, after-block), where the
/// block body is everything between the end of the BEGIN marker line and
/// the start of the END marker line.
fn split_readme(readme: &str) -> (usize, usize) {
    let begin = readme
        .find(BEGIN_MARKER)
        .expect("README.md is missing the BEGIN GENERATED EXIT CODES marker");
    let begin_line_end = readme[begin..]
        .find('\n')
        .map(|offset| begin + offset + 1)
        .expect("BEGIN marker line is not newline-terminated");
    let end = readme[begin_line_end..]
        .find(END_MARKER)
        .map(|offset| begin_line_end + offset)
        .expect("README.md is missing the END GENERATED EXIT CODES marker");
    (begin_line_end, end)
}

#[test]
fn readme_exit_code_table_matches_docs() {
    let root = repo_root();
    let doc_path = root.join("docs/exit-codes.md");
    let readme_path = root.join("README.md");

    let doc = fs::read_to_string(&doc_path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", doc_path.display()));
    let readme = fs::read_to_string(&readme_path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", readme_path.display()));

    let rows = parse_doc_table(&doc);
    assert!(
        rows.len() >= 3,
        "parsed only {} rows from {}; the table format changed and this test needs updating",
        rows.len(),
        doc_path.display()
    );

    // Exit codes are a public API: no duplicates, and listed in ascending
    // order so the published table reads as a stable register.
    for pair in rows.windows(2) {
        assert!(
            pair[0].code < pair[1].code,
            "{} lists exit code {} before {}: codes must be unique and ascending",
            doc_path.display(),
            pair[0].code,
            pair[1].code
        );
    }

    let expected = render_readme_table(&rows);
    let (body_start, body_end) = split_readme(&readme);
    let actual = &readme[body_start..body_end];

    if actual == expected {
        return;
    }

    if std::env::var_os(UPDATE_ENV).is_some() {
        let mut updated = String::with_capacity(readme.len() + expected.len());
        updated.push_str(&readme[..body_start]);
        updated.push_str(&expected);
        updated.push_str(&readme[body_end..]);
        fs::write(&readme_path, updated)
            .unwrap_or_else(|err| panic!("cannot write {}: {err}", readme_path.display()));
        return;
    }

    panic!(
        "README.md's exit-code table no longer matches {}.\n\
         A new exit code registered in the doc must also appear in the README.\n\
         Regenerate with:\n\
         \n    {UPDATE_ENV}=1 cargo test -p outline-cli --test readme_exit_codes\n\
         \nexpected block:\n{expected}\nfound block:\n{actual}",
        doc_path.display()
    );
}
