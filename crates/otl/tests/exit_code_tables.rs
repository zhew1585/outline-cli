//! Every published exit-code table must agree with `docs/exit-codes.md`.
//!
//! The exit-code table is a public API (project-context.md, CLI contract),
//! and it is published in three places. `docs/exit-codes.md` carries the
//! full detail and is the single source of truth; the other two are
//! renderings of it, each shaped for who reads it:
//!
//! - `README.md` - Code and Meaning, because that is where users look first
//!   and the Examples column would swamp the page;
//! - `crates/otl/skill/SKILL.md` - Code and `Meaning: Agent summary`,
//!   because an agent reading only "Generic failure" cannot act on it,
//!   while the Examples column is far too long to put in front of a model
//!   on every failure.
//!
//! Three copies of a public API drift, and the third one drifted first: the
//! skill's table was hand-maintained and nothing compared it to anything.
//! So this test derives both blocks from the doc and fails when either
//! disagrees.
//!
//! When a story adds a new error class it registers the code in
//! `docs/exit-codes.md`; this test then fails until both renderings are
//! updated, which is the point. Regenerate rather than hand-edit:
//!
//! ```sh
//! UPDATE_EXIT_CODE_TABLES=1 cargo test -p outline-cli --test exit_code_tables
//! ```
//!
//! Regenerating the SKILL.md block changes a document that ships inside the
//! binary, so bump its `version:` frontmatter in the same commit - that is
//! what `otl doctor` compares an installed copy against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

/// Markers delimiting a derived block in a rendering.
const BEGIN_MARKER: &str = "<!-- BEGIN GENERATED EXIT CODES";
const END_MARKER: &str = "<!-- END GENERATED EXIT CODES -->";

/// Env var that switches the test from checking to rewriting.
const UPDATE_ENV: &str = "UPDATE_EXIT_CODE_TABLES";

/// A stand-in for an escaped `\|` while a row is split on its separators.
///
/// A cell may legitimately contain a pipe - the skill's row for code 0
/// names `otl ... | head` - and markdown spells that `\|`. Splitting on the
/// raw character would cut such a cell in half, so the escape is folded out
/// first and restored after. The sentinel is a private-use character, which
/// cannot appear in the document (`check_skill` in build.rs refuses control
/// characters, and nothing authors this range).
const PIPE_SENTINEL: char = '\u{e000}';

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
    /// The few words that change what an agent does next. May be empty.
    agent_summary: String,
}

impl ExitCodeRow {
    /// The cell `README.md` publishes.
    fn readme_cell(&self) -> String {
        self.meaning.clone()
    }

    /// The cell `SKILL.md` publishes.
    fn skill_cell(&self) -> String {
        match self.agent_summary.is_empty() {
            true => self.meaning.clone(),
            false => format!("{}: {}", self.meaning, self.agent_summary),
        }
    }
}

/// Parses the leading `| Code | Meaning | Agent summary | ... |` table out of
/// `docs/exit-codes.md`, ignoring the wide "Examples" column.
fn parse_doc_table(doc: &str) -> Vec<ExitCodeRow> {
    let mut rows = Vec::new();
    for line in doc.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells = split_cells(line);
        if cells.len() < 3 {
            continue;
        }
        // Skips the header row and the `|---|---|` separator: only a cell
        // that parses as a number is a code.
        let Ok(code) = cells[0].parse::<u8>() else {
            continue;
        };
        rows.push(ExitCodeRow {
            code,
            meaning: cells[1].clone(),
            agent_summary: cells[2].clone(),
        });
    }
    rows
}

/// Split one markdown table row into trimmed cells, honouring `\|`.
fn split_cells(line: &str) -> Vec<String> {
    line.replace("\\|", &PIPE_SENTINEL.to_string())
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().replace(PIPE_SENTINEL, "\\|"))
        .collect()
}

/// Renders a derived block (marker lines excluded) for the given rows.
fn render_block(
    rows: &[ExitCodeRow],
    separator: &str,
    cell: impl Fn(&ExitCodeRow) -> String,
) -> String {
    let mut out = format!("\n| Code | Meaning |\n{separator}\n");
    for row in rows {
        out.push_str(&format!("| {} | {} |\n", row.code, cell(row)));
    }
    out.push('\n');
    out
}

/// Splits a rendering into the byte range its derived block occupies:
/// everything between the end of the BEGIN marker line and the start of the
/// END marker line.
fn block_range(rendering: &str, path: &Path) -> (usize, usize) {
    let begin = rendering
        .find(BEGIN_MARKER)
        .unwrap_or_else(|| panic!("{} is missing the BEGIN marker", path.display()));
    let begin_line_end = rendering[begin..]
        .find('\n')
        .map(|offset| begin + offset + 1)
        .expect("BEGIN marker line is not newline-terminated");
    let end = rendering[begin_line_end..]
        .find(END_MARKER)
        .map(|offset| begin_line_end + offset)
        .unwrap_or_else(|| panic!("{} is missing the END marker", path.display()));
    (begin_line_end, end)
}

/// Check (or, with the env var set, rewrite) one rendering's derived block.
fn check_rendering(path: &Path, expected: &str) -> Result<(), String> {
    let rendering = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    let (body_start, body_end) = block_range(&rendering, path);
    let actual = &rendering[body_start..body_end];
    if actual == expected {
        return Ok(());
    }
    if std::env::var_os(UPDATE_ENV).is_some() {
        let mut updated = String::with_capacity(rendering.len() + expected.len());
        updated.push_str(&rendering[..body_start]);
        updated.push_str(expected);
        updated.push_str(&rendering[body_end..]);
        fs::write(path, updated)
            .unwrap_or_else(|err| panic!("cannot write {}: {err}", path.display()));
        return Ok(());
    }
    Err(format!(
        "{}'s exit-code table no longer matches docs/exit-codes.md.\n\
         Regenerate with:\n\
         \n    {UPDATE_ENV}=1 cargo test -p outline-cli --test exit_code_tables\n\
         \nexpected block:\n{expected}\nfound block:\n{actual}",
        path.display()
    ))
}

/// The doc's rows, with the invariants a published register has to hold.
fn doc_rows() -> Vec<ExitCodeRow> {
    let doc_path = repo_root().join("docs/exit-codes.md");
    let doc = fs::read_to_string(&doc_path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", doc_path.display()));
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
    rows
}

#[test]
fn readme_exit_code_table_matches_docs() {
    let rows = doc_rows();
    let expected = render_block(&rows, "|------|---------|", ExitCodeRow::readme_cell);
    if let Err(problem) = check_rendering(&repo_root().join("README.md"), &expected) {
        panic!("{problem}\nA new exit code registered in the doc must also appear in the README.");
    }
}

#[test]
fn skill_exit_code_table_matches_docs() {
    let rows = doc_rows();
    let expected = render_block(&rows, "|---|---|", ExitCodeRow::skill_cell);
    let path = repo_root().join("crates/otl/skill/SKILL.md");
    if let Err(problem) = check_rendering(&path, &expected) {
        panic!(
            "{problem}\nThis table ships inside the binary, so bump the skill's \
             `version:` frontmatter in the same commit."
        );
    }
}

/// Every code the doc registers must have an agent summary.
///
/// An empty one renders as the bare Meaning, which is the state the skill's
/// table was in before this column existed: "Generic failure" tells an agent
/// a request failed and nothing about what to do next.
#[test]
fn every_code_carries_an_agent_summary() {
    let missing: Vec<u8> = doc_rows()
        .iter()
        .filter(|row| row.agent_summary.is_empty())
        .map(|row| row.code)
        .collect();
    assert!(
        missing.is_empty(),
        "docs/exit-codes.md leaves the Agent summary column empty for exit \
         code(s) {missing:?}. The skill's table would then say only what the \
         README says, which is the gap that column exists to close."
    );
}

#[test]
fn a_cell_containing_an_escaped_pipe_survives_the_round_trip() {
    let cells = split_cells(r"| 0 | Success | a pipe \| like this | example |");
    assert_eq!(cells[1], "Success");
    assert_eq!(cells[2], r"a pipe \| like this");
    assert_eq!(cells[3], "example");
}
