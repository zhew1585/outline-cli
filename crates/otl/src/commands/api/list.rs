//! `otl api list` - which operations this binary can dispatch.
//!
//! Purely local: it reads the effective IR table ([`crate::ops`]) and
//! touches neither configuration, credential nor network.
//!
//! # Dual state, and what the JSON form carries
//!
//! Both states describe the same table. The terminal gets one
//! `name<TAB>summary` line per operation, which is what it has always
//! printed; a pipe or `--json` gets an array of objects, which is what the
//! CLI contract said it would get all along and did not.
//!
//! The JSON form carries `path`, `content_type`, `body_mode`, `callable`
//! and `curated_command` beyond the two columns, and deliberately stops
//! there:
//!
//! - `curated_command` names the semver-stable command that covers this
//!   operation, or is null. It belongs in a triage list for the same reason
//!   `callable` does: without it the list answers "how do I reach this
//!   operation" with the unstable path, because the stable one is invisible
//!   from here. See [`super::curated`].
//!
//! - `callable` has to be there, or the JSON would say LESS than the text:
//!   the text flags operations the generic client cannot call, and a
//!   consumer reading the structured form must not be the one that misses
//!   it. The text also names the content type that makes an operation
//!   uncallable, so `content_type` and `body_mode` come along for the same
//!   reason - "not less than the text" is a rule about every fact in it,
//!   not just the flag.
//! - `path` is what an operation IS on the wire, and it is one short string.
//! - parameters and response fields do NOT come along. Not for size alone
//!   (the full contract of all 113 operations is roughly a hundred times
//!   this payload) but because a PARTIAL contract is the more dangerous
//!   object: a list of parameter names with no types, no facets and no
//!   response shape reads like the answer while being a fragment of it.
//!   Two steps - list to triage, `otl api describe` for the one operation
//!   picked - keep every published contract complete.

use serde_json::{json, Value};

use engine::{BodyMode, OpSpec};

use super::{curated, describe};
use crate::exit::CliError;
use crate::ops;
use crate::render::OutputMode;
use crate::stdio;

/// Marker appended to operations the generic client cannot call.
const NOT_CALLABLE_MARKER: &str = "[not callable via api";

/// Marker appended to operations a curated command already covers.
const CURATED_MARKER: &str = "[stable command: ";

/// Print the effective operation table in the resolved output state.
pub(super) fn run(mode: OutputMode) -> Result<(), CliError> {
    let table = ops::table();
    match mode {
        OutputMode::Json => stdio::write_data_line(&describe::to_json_text(&as_json(table))?),
        // Never `print!` on the data path: a consumer that closes the pipe
        // early (`otl api list | head -1`) must not turn into a panic.
        OutputMode::Table => stdio::write_data(&as_text(table)),
    }
}

/// One `name<TAB>summary` line per operation, newline-terminated.
///
/// Every string that came out of the document is scrubbed on the way out;
/// see [`describe::safe`] for why that is still needed after the compiler
/// already validated it.
fn as_text(table: &[OpSpec]) -> String {
    let mut out = String::new();
    for op in table {
        out.push_str(&describe::safe(&op.name));
        out.push('\t');
        out.push_str(&describe::safe(&op.summary));
        if op.body_mode == BodyMode::Unsupported {
            out.push(' ');
            out.push_str(NOT_CALLABLE_MARKER);
            out.push_str(": requires ");
            out.push_str(&describe::safe(&op.content_type));
            out.push(']');
        }
        // The curated command is a literal from this binary, not spec text,
        // so it needs no scrubbing - but it does need to be here, because
        // the JSON carries it and neither state may say less than the other.
        if let Some(command) = curated::curated_command(&op.name) {
            out.push(' ');
            out.push_str(CURATED_MARKER);
            out.push_str(command);
            out.push(']');
        }
        out.push('\n');
    }
    out
}

/// The table as an array of objects, in the table's own (name) order.
fn as_json(table: &[OpSpec]) -> Value {
    Value::Array(
        table
            .iter()
            .map(|op| {
                json!({
                    "name": op.name.as_ref(),
                    "summary": describe::optional(&op.summary),
                    "path": op.path.as_ref(),
                    "content_type": describe::optional(&op.content_type),
                    "body_mode": describe::body_mode_name(op.body_mode),
                    "callable": op.body_mode != BodyMode::Unsupported,
                    "curated_command": curated::curated_command(&op.name),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The table used by the assertions below: the one compiled into this
    /// binary, so the tests describe the shipped product.
    fn table() -> &'static [OpSpec] {
        ops::table()
    }

    #[test]
    fn the_text_state_is_one_tab_separated_line_per_operation() {
        let text = as_text(table());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), table().len());
        let info = lines
            .iter()
            .find(|line| line.starts_with("documents.info\t"))
            .expect("documents.info missing");
        assert!(info.contains("Retrieve a document"), "{info}");
    }

    #[test]
    fn the_json_state_is_an_array_of_objects_one_per_operation() {
        let value = as_json(table());
        let rows = value.as_array().expect("array");
        assert_eq!(rows.len(), table().len());
        let info = rows
            .iter()
            .find(|row| row["name"] == "documents.info")
            .expect("documents.info missing");
        assert_eq!(info["path"], "/api/documents.info");
        assert_eq!(info["body_mode"], "key_value");
        assert_eq!(info["callable"], true);
        assert!(info["summary"].is_string(), "{info}");
    }

    /// The structured state must not say less than the text state: the text
    /// flags what cannot be called, so the JSON has to as well.
    #[test]
    fn an_operation_that_cannot_be_called_is_flagged_in_both_states() {
        let text = as_text(table());
        let flagged: Vec<&str> = text
            .lines()
            .filter(|line| line.contains(NOT_CALLABLE_MARKER))
            .map(|line| line.split('\t').next().unwrap_or_default())
            .collect();
        assert!(
            !flagged.is_empty(),
            "no uncallable operation in the table: this assertion would be vacuous"
        );
        let value = as_json(table());
        let rows = value.as_array().expect("array");
        for name in &flagged {
            let row = rows
                .iter()
                .find(|row| row["name"] == *name)
                .expect("flagged operation missing from json");
            assert_eq!(row["callable"], false, "{row}");
            assert_eq!(row["body_mode"], "unsupported", "{row}");
            // The text names the content type that makes it uncallable, so
            // the JSON has to carry it too - otherwise the structured form
            // says "no" without saying why, which is less than the text.
            let content_type = row["content_type"].as_str().unwrap_or_default();
            assert!(!content_type.is_empty(), "{row}");
            let line = text
                .lines()
                .find(|line| line.starts_with(&format!("{name}\t")))
                .unwrap_or_default();
            assert!(line.contains(content_type), "{line} vs {row}");
        }
        let callable_count = rows.iter().filter(|row| row["callable"] == false).count();
        assert_eq!(callable_count, flagged.len(), "the two states disagree");
    }

    /// Same rule as the flag above, applied to the reverse index: whichever
    /// state a caller reads, it learns that a stable command exists.
    #[test]
    fn a_curated_command_is_named_in_both_states() {
        let text = as_text(table());
        let value = as_json(table());
        let rows = value.as_array().expect("array");
        let mut named = 0usize;
        for row in rows {
            let name = row["name"].as_str().expect("name");
            let line = text
                .lines()
                .find(|line| line.starts_with(&format!("{name}\t")))
                .expect("operation missing from the text state");
            match row["curated_command"].as_str() {
                Some(command) => {
                    named += 1;
                    assert!(
                        line.contains(&format!("{CURATED_MARKER}{command}]")),
                        "{line} does not name {command}"
                    );
                }
                None => assert!(!line.contains(CURATED_MARKER), "{line}"),
            }
        }
        assert_eq!(
            named,
            curated::CURATED_COMMANDS.len(),
            "an entry in the curated table names an operation this binary does not have"
        );
    }

    #[test]
    fn documents_search_points_at_the_stable_command() {
        let value = as_json(table());
        let rows = value.as_array().expect("array");
        let search = rows
            .iter()
            .find(|row| row["name"] == "documents.search")
            .expect("documents.search missing");
        assert_eq!(search["curated_command"], "otl docs search");
        let duplicate = rows
            .iter()
            .find(|row| row["name"] == "documents.duplicate")
            .expect("documents.duplicate missing");
        assert!(duplicate["curated_command"].is_null(), "{duplicate}");
    }
}
