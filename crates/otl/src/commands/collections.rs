//! `otl collections list` (story 3.5).
//!
//! The list itself is auto-paginated, so a workspace with more collections
//! than one page still comes back whole.
//!
//! The document count needs a word of explanation: Outline's collection
//! object does NOT carry one (see the vendored spec's `Collection` schema),
//! so it is derived by asking each collection for its document structure -
//! one extra request per collection. That is worth it for the column the
//! command exists to show, but it is also why `--no-counts` exists, and why
//! `--json` prints the server's own rows untouched instead of inventing a
//! field the API does not have.

use std::collections::HashMap;

use anyhow::anyhow;
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::exit::CliError;
use crate::fields::{self, Column, COMPUTED};
use crate::render::{self, OutputMode};
use crate::session::Session;
use crate::stdio;

/// Operation that lists collections (auto-paginated).
const LIST_OPERATION: &str = "collections.list";
/// Operation that returns one collection's document structure.
const DOCUMENTS_OPERATION: &str = "collections.documents";

/// Placeholder for a count that could not be determined.
const UNKNOWN_COUNT: &str = "?";

/// Upper bound on the nodes counted for one collection.
///
/// The structure is server data walked as a tree; a bound keeps a
/// pathological response from turning a list command into an endless walk.
const MAX_COUNTED_NODES: usize = 100_000;

/// The curated columns, in display order. The count is filled in
/// separately (the API has no such field).
const COLUMNS: &[Column] = &[
    Column::plain("NAME", "/name"),
    Column::plain("ID", "/id"),
    Column::plain("DOCUMENTS", COMPUTED),
];

/// Index of the document-count column in [`COLUMNS`].
const COUNT_COLUMN: usize = 2;

/// Arguments for `otl collections`.
#[derive(Debug, Args)]
pub struct CollectionsArgs {
    #[command(subcommand)]
    command: CollectionsCommand,
}

/// The curated collection subcommands.
#[derive(Debug, Subcommand)]
enum CollectionsCommand {
    /// List every collection with its id and document count.
    List(ListArgs),
}

/// Arguments for `otl collections list`.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Stop after N collections (a warning says so on stderr).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub limit: Option<u64>,

    /// Skip the document counts, and the one request per collection they
    /// cost.
    #[arg(long)]
    pub no_counts: bool,
}

/// Run the requested `otl collections` subcommand.
pub fn run(args: &CollectionsArgs, mode: OutputMode) -> Result<(), CliError> {
    match &args.command {
        CollectionsCommand::List(args) => list(args, mode),
    }
}

/// Run `otl collections list`.
fn list(cmd: &ListArgs, mode: OutputMode) -> Result<(), CliError> {
    let session = Session::open()?;
    let collections = session.call_rows(LIST_OPERATION, &[], cmd.limit)?;
    if mode == OutputMode::Json {
        // Raw server rows: no synthetic count field, so a script never sees
        // a value the API cannot confirm.
        let payload = Value::Array(collections);
        let rendered = render::render(&payload, OutputMode::Json)
            .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
        return stdio::write_data_line(&rendered);
    }
    let counts = if cmd.no_counts {
        HashMap::new()
    } else {
        document_counts(&session, &collections)
    };
    stdio::write_data_line(&table(&collections, &counts, cmd.no_counts))
}

/// Build the human-readable table.
fn table(collections: &[Value], counts: &HashMap<String, usize>, no_counts: bool) -> String {
    let mut rows = fields::rows(collections, COLUMNS);
    for (row, collection) in rows.iter_mut().zip(collections.iter()) {
        if let Some(cell) = row.get_mut(COUNT_COLUMN) {
            *cell = count_label(collection, counts, no_counts);
        }
    }
    render::render_columns(&fields::headers(COLUMNS), &rows)
}

/// The document-count cell for one collection.
fn count_label(collection: &Value, counts: &HashMap<String, usize>, no_counts: bool) -> String {
    if no_counts {
        return String::new();
    }
    let Some(id) = fields::string_at(collection, "/id") else {
        return UNKNOWN_COUNT.to_string();
    };
    match counts.get(id) {
        Some(count) => count.to_string(),
        None => UNKNOWN_COUNT.to_string(),
    }
}

/// Count the documents in each collection.
///
/// A collection whose structure cannot be read is left out of the map (its
/// cell shows `?`); one unreadable collection must not fail the listing.
/// Failures are summarized once rather than per collection, so a workspace
/// with many private collections does not bury its own output.
fn document_counts(session: &Session, collections: &[Value]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    let mut failed = 0_usize;
    for id in collections
        .iter()
        .filter_map(|collection| fields::string_at(collection, "/id"))
    {
        let args = [("id".to_string(), id.to_string())];
        match session.call_data(DOCUMENTS_OPERATION, &args) {
            Ok(structure) => {
                counts.insert(id.to_string(), count_nodes(&structure));
            }
            Err(_) => failed += 1,
        }
    }
    if failed > 0 {
        stdio::write_diagnostic_line(&format!(
            "warning: could not read the document structure of {failed} \
             collection(s); their counts show as {UNKNOWN_COUNT} \
             (--no-counts skips this lookup entirely)"
        ));
    }
    counts
}

/// Count the nodes of a navigation tree.
///
/// Walked with an explicit stack, not recursion: the depth of the tree is
/// server-controlled and a recursive walk would be a stack-overflow away
/// from an abort.
fn count_nodes(structure: &Value) -> usize {
    let Some(roots) = structure.as_array() else {
        return 0;
    };
    let mut stack: Vec<&Value> = roots.iter().collect();
    let mut count = 0;
    while let Some(node) = stack.pop() {
        if count >= MAX_COUNTED_NODES {
            break;
        }
        count += 1;
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            stack.extend(children.iter());
        }
    }
    count
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    #[test]
    fn counts_a_flat_structure() {
        let structure = json!([{ "id": "a" }, { "id": "b" }]);
        assert_eq!(count_nodes(&structure), 2);
    }

    #[test]
    fn counts_nested_children() {
        let structure = json!([
            { "id": "a", "children": [{ "id": "b", "children": [{ "id": "c" }] }] },
            { "id": "d" }
        ]);
        assert_eq!(count_nodes(&structure), 4);
    }

    #[test]
    fn counts_nothing_for_a_non_array_payload() {
        assert_eq!(count_nodes(&json!({})), 0);
        assert_eq!(count_nodes(&json!(null)), 0);
        assert_eq!(count_nodes(&json!([])), 0);
    }

    #[test]
    fn a_deep_structure_does_not_overflow_the_stack() {
        // A recursive counter would need one frame per level and abort here.
        //
        // The whole fixture lives on a thread with a big stack because
        // serde_json's own `Drop` for a deeply nested `Value` is recursive:
        // that is the TEST's problem, not the counter's, and the point of
        // the test is that `count_nodes` needs no stack depth at all.
        const DEPTH: usize = 100_000;
        const STACK: usize = 128 * 1024 * 1024;

        let counted = std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(|| {
                // Built by MOVING the child into its parent. `json!` on a
                // non-literal expression goes through `to_value`, which
                // deep-copies - that would make this loop quadratic.
                let mut node = json!({ "id": "leaf" });
                for _ in 0..DEPTH {
                    let mut level = serde_json::Map::new();
                    level.insert("id".to_string(), Value::String("n".to_string()));
                    level.insert("children".to_string(), Value::Array(vec![node]));
                    node = Value::Object(level);
                }
                count_nodes(&Value::Array(vec![node]))
            })
            .expect("spawn")
            .join()
            .expect("join");
        assert_eq!(counted, MAX_COUNTED_NODES);
    }

    #[test]
    fn missing_counts_render_as_unknown() {
        let collection = json!({ "id": "c1", "name": "Eng" });
        let counts = HashMap::new();
        assert_eq!(count_label(&collection, &counts, false), UNKNOWN_COUNT);
        assert_eq!(count_label(&collection, &counts, true), "");
    }

    #[test]
    fn known_counts_render_as_numbers() {
        let collection = json!({ "id": "c1", "name": "Eng" });
        let counts = HashMap::from([("c1".to_string(), 7)]);
        assert_eq!(count_label(&collection, &counts, false), "7");
    }

    #[test]
    fn the_table_shows_name_id_and_count() {
        let collections = vec![
            json!({ "id": "6f1c-eng", "name": "Engineering" }),
            json!({ "id": "8a2d-hr", "name": "Human Resources" }),
            json!({ "id": "9b3e-x", "name": "\u{4e2d}\u{6587}\u{96c6}\u{5408}" }),
        ];
        let counts = HashMap::from([("6f1c-eng".to_string(), 12), ("8a2d-hr".to_string(), 0)]);
        // Golden file: byte-for-byte, including alignment past a CJK name
        // (whose display width is twice its character count).
        assert_eq!(
            format!("{}\n", table(&collections, &counts, false)),
            include_str!("../../tests/golden/collections_list_table.txt")
        );
    }

    #[test]
    fn an_empty_list_renders_a_placeholder_not_a_bare_header() {
        assert_eq!(table(&[], &HashMap::new(), false), "(no items)");
    }
}
