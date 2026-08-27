//! `otl collections list`.
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

use std::collections::{HashMap, HashSet};

use anyhow::anyhow;
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::config::Overrides;
use crate::exit::CliError;
use crate::fields::{self, Column, COMPUTED};
use crate::render::{self, OutputMode};
use crate::session::{self, Session};
use crate::stdio;

/// Operation that lists collections (auto-paginated).
const LIST_OPERATION: &str = "collections.list";
/// Operation that returns one collection's document structure.
const DOCUMENTS_OPERATION: &str = "collections.documents";

/// Placeholder for a count that could not be determined.
const UNKNOWN_COUNT: &str = "?";
/// Suffix marking a count that stopped at [`MAX_COUNTED_NODES`].
const CAPPED_MARKER: &str = "+";

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
#[command(after_long_help = "API contracts:
  Results come from collections.list. Unless --no-counts is used, document
  counts also use collections.documents once per collection.

  Inspect them with:
    otl api describe collections.list --json
    otl api describe collections.documents --json")]
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
pub fn run(
    args: &CollectionsArgs,
    mode: OutputMode,
    overrides: &Overrides,
) -> Result<(), CliError> {
    match &args.command {
        CollectionsCommand::List(args) => list(args, mode, overrides),
    }
}

/// Run `otl collections list`.
fn list(cmd: &ListArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let session = Session::open(overrides)?;
    let collections = session.call_rows(LIST_OPERATION, &[], cmd.limit)?;
    if mode == OutputMode::Json {
        // Raw server rows: no synthetic count field, so a script never sees
        // a value the API cannot confirm.
        let payload = Value::Array(collections.items.clone());
        let rendered = render::render_json(&payload)
            .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
        stdio::write_data_line(&rendered)?;
    } else {
        let counts = if cmd.no_counts {
            Counts::default()
        } else {
            document_counts(&session, &collections.items)
        };
        stdio::write_data_line(&table(&collections.items, &counts, cmd.no_counts))?;
    }
    // "Auto-paginated to the end" is this command's whole promise (story
    // 3.5). When the CLI's own page cap cut the listing instead, the rows
    // still go to stdout but the exit code must not read as "here is every
    // collection".
    match collections.incomplete() {
        Some(truncation) => Err(session::incomplete_error(
            "the collection listing",
            truncation,
        )),
        None => Ok(()),
    }
}

/// Document counts per collection id, plus which ones hit the node cap.
#[derive(Debug, Default)]
struct Counts {
    /// Counted nodes per collection id.
    counted: HashMap<String, usize>,
    /// Collection ids whose walk stopped at [`MAX_COUNTED_NODES`], so the
    /// number is a floor and not the count.
    capped: HashSet<String>,
}

impl Counts {
    /// Record one collection's count, and whether it hit the walk cap.
    fn record(&mut self, id: &str, count: &NodeCount) {
        self.counted.insert(id.to_string(), count.nodes);
        if count.capped {
            self.capped.insert(id.to_string());
        }
    }
}

/// Build the human-readable table.
fn table(collections: &[Value], counts: &Counts, no_counts: bool) -> String {
    let mut rows = fields::rows(collections, COLUMNS);
    for (row, collection) in rows.iter_mut().zip(collections.iter()) {
        if let Some(cell) = row.get_mut(COUNT_COLUMN) {
            *cell = count_label(collection, counts, no_counts);
        }
    }
    render::render_columns(&fields::headers(COLUMNS), &rows)
}

/// The document-count cell for one collection.
///
/// Three distinguishable states, because presenting any of them as the
/// others would be a lie: a number, `<n>+` when the walk stopped at the
/// node cap (the count is a floor), and `?` when the structure could not be
/// read at all.
fn count_label(collection: &Value, counts: &Counts, no_counts: bool) -> String {
    if no_counts {
        return String::new();
    }
    let Some(id) = fields::string_at(collection, "/id") else {
        return UNKNOWN_COUNT.to_string();
    };
    match counts.counted.get(id) {
        Some(count) if counts.capped.contains(id) => format!("{count}{CAPPED_MARKER}"),
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
fn document_counts(session: &Session, collections: &[Value]) -> Counts {
    let mut counts = Counts::default();
    let mut failed = 0_usize;
    let mut unrecognized = 0_usize;
    for id in collections
        .iter()
        .filter_map(|collection| fields::string_at(collection, "/id"))
    {
        let args = [("id".to_string(), id.to_string())];
        // An unrecognized shape counts as unreadable, not as zero. Both
        // failure kinds are folded into `Option` here so the loop body stays
        // one level deep; they are counted separately because the diagnostic
        // below distinguishes them.
        let counted = match session.call_data(DOCUMENTS_OPERATION, &args) {
            Ok(structure) => count_nodes(&structure).ok_or(&mut unrecognized),
            Err(_) => Err(&mut failed),
        };
        match counted {
            Ok(count) => counts.record(id, &count),
            Err(tally) => *tally += 1,
        }
    }
    if failed + unrecognized > 0 {
        stdio::write_diagnostic_line(&format!(
            "warning: could not determine the document count of {} \
             collection(s) ({failed} could not be read, {unrecognized} \
             answered with a structure this version does not recognize); \
             their counts show as {UNKNOWN_COUNT} (--no-counts skips this \
             lookup entirely)",
            failed + unrecognized
        ));
    }
    if !counts.capped.is_empty() {
        stdio::write_diagnostic_line(&format!(
            "warning: {} collection(s) have more than {MAX_COUNTED_NODES} \
             documents in their structure; their counts are shown as \
             `{MAX_COUNTED_NODES}{CAPPED_MARKER}` rather than counted to the end",
            counts.capped.len()
        ));
    }
    counts
}

/// The outcome of counting one navigation tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeCount {
    /// Nodes counted.
    nodes: usize,
    /// True when the walk stopped at [`MAX_COUNTED_NODES`], so `nodes` is a
    /// lower bound rather than the answer.
    capped: bool,
}

/// Count the nodes of a navigation tree, or `None` when the payload is not
/// one.
///
/// Walked with an explicit stack, not recursion: the depth of the tree is
/// server-controlled and a recursive walk would be a stack-overflow away
/// from an abort.
///
/// Two things are deliberately NOT reported as a count:
///
/// - a payload that is not an array of nodes (`null`, an object, an entry
///   that is not a node). Such a response does not say the collection is
///   empty, it says the structure could not be recognized - and `0` claims
///   the first. Unrecognized becomes `?`, the same as an unreadable one.
/// - a walk that hit the cap. Showing it as the exact count `100000` would
///   be a wrong fact rather than a rounded one, so it is marked as a floor.
fn count_nodes(structure: &Value) -> Option<NodeCount> {
    let roots = structure.as_array()?;
    let mut stack: Vec<&Value> = roots.iter().collect();
    let mut nodes = 0;
    while let Some(node) = stack.pop() {
        if nodes >= MAX_COUNTED_NODES {
            return Some(NodeCount {
                nodes,
                capped: true,
            });
        }
        // A node that is not an object is not a document: the shape is not
        // what the spec describes, so no count can be claimed from it.
        if !node.is_object() {
            return None;
        }
        nodes += 1;
        if let Some(children) = node.get("children") {
            // A `children` field that is not a list of nodes means the
            // subtree cannot be walked. Treating it as a leaf would report
            // an exact count that silently omits everything under it.
            stack.extend(children.as_array()?.iter());
        }
    }
    Some(NodeCount {
        nodes,
        capped: false,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    #[test]
    fn counts_a_flat_structure() {
        let structure = json!([{ "id": "a" }, { "id": "b" }]);
        let count = count_nodes(&structure).expect("a plain array is countable");
        assert_eq!(count.nodes, 2);
        assert!(!count.capped);
    }

    #[test]
    fn counts_nested_children() {
        let structure = json!([
            { "id": "a", "children": [{ "id": "b", "children": [{ "id": "c" }] }] },
            { "id": "d" }
        ]);
        assert_eq!(count_nodes(&structure).map(|count| count.nodes), Some(4));
    }

    #[test]
    fn a_node_without_a_children_field_is_a_leaf() {
        // Absent is different from malformed: a leaf legitimately has no
        // `children` key at all.
        let structure = json!([{ "id": "a" }, { "id": "b", "children": [] }]);
        assert_eq!(count_nodes(&structure).map(|count| count.nodes), Some(2));
    }

    #[test]
    fn an_empty_array_is_a_genuine_zero() {
        let count = count_nodes(&json!([])).expect("an empty array is countable");
        assert_eq!(count.nodes, 0);
        assert!(!count.capped);
    }

    #[test]
    fn an_unrecognized_structure_is_not_a_count_of_zero() {
        // These responses do not say the collection is empty; they say the
        // shape was not understood. Reporting `0` would claim the first.
        for payload in [
            json!(null),
            json!({}),
            json!({ "documents": [] }),
            json!("nope"),
            json!(7),
            json!([null]),
            json!(["not a node"]),
            json!([{ "id": "a", "children": [null] }]),
            // A `children` field of the wrong type: the subtree under it
            // cannot be walked, so no exact count can be claimed.
            json!([{ "id": "a", "children": { "id": "b" } }]),
            json!([{ "id": "a", "children": "b" }]),
            json!([{ "id": "a", "children": 3 }]),
            json!([{ "id": "a", "children": [{ "id": "b", "children": {} }] }]),
        ] {
            assert_eq!(
                count_nodes(&payload),
                None,
                "{payload} was reported as a count"
            );
        }
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
        let counted = counted.expect("a deep array is countable");
        assert_eq!(counted.nodes, MAX_COUNTED_NODES);
        // The cap must be VISIBLE: a capped walk reported as the exact
        // number 100000 would be a wrong fact rather than a rounded one.
        assert!(counted.capped, "the node cap was not reported");
    }

    #[test]
    fn missing_counts_render_as_unknown() {
        let collection = json!({ "id": "c1", "name": "Eng" });
        let counts = Counts::default();
        assert_eq!(count_label(&collection, &counts, false), UNKNOWN_COUNT);
        assert_eq!(count_label(&collection, &counts, true), "");
    }

    #[test]
    fn known_counts_render_as_numbers() {
        let collection = json!({ "id": "c1", "name": "Eng" });
        let counts = Counts {
            counted: HashMap::from([("c1".to_string(), 7)]),
            capped: HashSet::new(),
        };
        assert_eq!(count_label(&collection, &counts, false), "7");
    }

    #[test]
    fn a_capped_count_is_marked_as_a_floor() {
        let collection = json!({ "id": "c1", "name": "Eng" });
        let counts = Counts {
            counted: HashMap::from([("c1".to_string(), MAX_COUNTED_NODES)]),
            capped: HashSet::from(["c1".to_string()]),
        };
        assert_eq!(
            count_label(&collection, &counts, false),
            format!("{MAX_COUNTED_NODES}+"),
            "a capped walk must not be shown as an exact count"
        );
    }

    #[test]
    fn the_table_shows_name_id_and_count() {
        let collections = vec![
            json!({ "id": "6f1c-eng", "name": "Engineering" }),
            json!({ "id": "8a2d-hr", "name": "Human Resources" }),
            json!({ "id": "9b3e-x", "name": "\u{4e2d}\u{6587}\u{96c6}\u{5408}" }),
        ];
        let counts = Counts {
            counted: HashMap::from([("6f1c-eng".to_string(), 12), ("8a2d-hr".to_string(), 0)]),
            capped: HashSet::new(),
        };
        // Golden file: byte-for-byte, including alignment past a CJK name
        // (whose display width is twice its character count).
        assert_eq!(
            format!("{}\n", table(&collections, &counts, false)),
            include_str!("../../tests/golden/collections_list_table.txt")
        );
    }

    #[test]
    fn an_empty_list_renders_a_placeholder_not_a_bare_header() {
        assert_eq!(table(&[], &Counts::default(), false), "(no items)");
    }
}
