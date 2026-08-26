//! `otl docs search <query>` (story 3.1).
//!
//! Human output is a four-column table - title, collection, last change,
//! matching snippet - built by naming fields, not by writing a renderer.
//! `--json` (and any non-terminal stdout) prints the raw result rows, which
//! carry the document `id` a script needs to feed the other commands.

use std::collections::HashMap;

use clap::Args;
use serde_json::Value;

use crate::config::Overrides;
use crate::exit::CliError;
use crate::fields::{self, Column, COMPUTED};
use crate::render::{self, OutputMode};
use crate::session::{self, Session};
use crate::stdio;

/// The compiled operation this command drives.
const OPERATION: &str = "documents.search";
/// Operation used to turn collection ids into names.
const COLLECTIONS_OPERATION: &str = "collections.list";

/// Pointer to the collection id of one search hit.
const COLLECTION_ID_POINTER: &str = "/document/collectionId";

/// The curated columns, in display order.
///
/// The collection column is [`COMPUTED`]: it is filled from the id at
/// [`COLLECTION_ID_POINTER`], resolved to a name where possible.
const COLUMNS: &[Column] = &[
    Column::plain("TITLE", "/document/title"),
    Column::plain("COLLECTION", COMPUTED),
    Column::timestamp("UPDATED", "/document/updatedAt"),
    Column::snippet("MATCH", "/context"),
];

/// Index of the collection column in [`COLUMNS`].
const COLLECTION_COLUMN: usize = 1;

/// Arguments for `otl docs search`.
#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Full-text search query.
    pub query: String,

    /// Restrict the search to one collection (its id).
    #[arg(long, value_name = "ID")]
    pub collection: Option<String>,

    /// Stop after N results (a warning says so on stderr).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub limit: Option<u64>,
}

/// Run `otl docs search`.
pub fn run(cmd: &SearchArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let session = Session::open(overrides)?;
    let mut args = vec![("query".to_string(), cmd.query.clone())];
    if let Some(collection) = &cmd.collection {
        // The vendored spec marks `collectionId` deprecated in favour of
        // the structured `filters` array, which cannot be expressed as a
        // `key=value` argument at all. Until the CLI grows a filter
        // builder, this is the only scalar way to scope a search; `otl api
        // documents.search --body @filter.json` covers the rest.
        args.push(("collectionId".to_string(), collection.clone()));
    }
    let hits = session.call_rows(OPERATION, &args, cmd.limit)?;
    match mode {
        OutputMode::Json => print_json(&hits.items)?,
        OutputMode::Table => {
            let names = collection_names(&session, &hits.items);
            stdio::write_data_line(&table(&hits.items, &names))?;
        }
    }
    // The hits are already on stdout: they are real results and worth
    // having. But the CLI stopped before the server ran out of them for a
    // reason the caller never asked for, so the exit code has to say the
    // result set is short - a stderr warning alone is invisible to
    // `otl docs search ... --json | jq`.
    match hits.incomplete() {
        Some(truncation) => Err(session::incomplete_error("the search result", truncation)),
        None => Ok(()),
    }
}

/// Print the raw result rows, exactly as the server sent them.
fn print_json(hits: &[Value]) -> Result<(), CliError> {
    let payload = Value::Array(hits.to_vec());
    let rendered = render::render_json(&payload).map_err(|error| {
        CliError::failure(anyhow::anyhow!("failed to render response: {error}"))
    })?;
    stdio::write_data_line(&rendered)
}

/// Build the human-readable table.
fn table(hits: &[Value], names: &HashMap<String, String>) -> String {
    let mut rows = fields::rows(hits, COLUMNS);
    for (row, hit) in rows.iter_mut().zip(hits.iter()) {
        if let Some(cell) = row.get_mut(COLLECTION_COLUMN) {
            *cell = collection_label(hit, names);
        }
    }
    render::render_columns(&fields::headers(COLUMNS), &rows)
}

/// The collection cell for one hit: its name when known, else its raw id.
fn collection_label(hit: &Value, names: &HashMap<String, String>) -> String {
    let Some(id) = fields::string_at(hit, COLLECTION_ID_POINTER) else {
        return String::new();
    };
    names.get(id).cloned().unwrap_or_else(|| id.to_string())
}

/// Resolve collection ids to names, best effort.
///
/// A search result names its collection by id only, which is unreadable in
/// a terminal, so one extra (auto-paginated) list call maps ids to names.
/// It is skipped entirely when no hit has a collection, and a failure is a
/// warning rather than an error: the ids are still shown, and a search must
/// not fail because the collection index could not be read.
fn collection_names(session: &Session, hits: &[Value]) -> HashMap<String, String> {
    let wanted = hits
        .iter()
        .any(|hit| fields::string_at(hit, COLLECTION_ID_POINTER).is_some());
    if !wanted {
        return HashMap::new();
    }
    match session.call_rows(COLLECTIONS_OPERATION, &[], None) {
        // A truncated collection index only costs readability here (some
        // cells fall back to raw ids), so it is not escalated: the names are
        // decoration on top of the real result, and `call_rows` has already
        // warned on stderr.
        Ok(collections) => collections
            .items
            .iter()
            .filter_map(|collection| {
                let id = fields::string_at(collection, "/id")?;
                let name = fields::string_at(collection, "/name")?;
                Some((id.to_string(), name.to_string()))
            })
            .collect(),
        Err(error) => {
            stdio::write_diagnostic_line(&format!(
                "warning: could not read the collection list, showing collection ids \
                 instead ({error})"
            ));
            HashMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn hit(title: &str, collection: Option<&str>, updated: &str, context: &str) -> Value {
        let mut document = json!({ "id": "doc-1", "title": title, "updatedAt": updated });
        if let Some(collection) = collection {
            document["collectionId"] = json!(collection);
        }
        json!({ "context": context, "ranking": 1.5, "document": document })
    }

    #[test]
    fn renders_title_collection_updated_and_snippet() {
        let hits = vec![
            hit(
                "Deploy runbook",
                Some("col-1"),
                "2026-08-20T15:30:37.000Z",
                "the deploy step runs on every merge",
            ),
            hit(
                "Onboarding",
                Some("col-2"),
                "2026-07-01T09:05:00.000Z",
                "first deploy of the week",
            ),
        ];
        let names = HashMap::from([("col-1".to_string(), "Engineering".to_string())]);
        // Golden file: byte-for-byte, including column alignment.
        assert_eq!(
            format!("{}\n", table(&hits, &names)),
            include_str!("../../../tests/golden/docs_search_table.txt")
        );
    }

    #[test]
    fn an_unresolved_collection_falls_back_to_its_id() {
        let hits = vec![hit("T", Some("col-9"), "2026-08-20T15:30:37.000Z", "c")];
        let rendered = table(&hits, &HashMap::new());
        assert!(rendered.contains("col-9"), "{rendered}");
    }

    #[test]
    fn a_hit_without_a_collection_leaves_the_cell_empty() {
        let hits = vec![hit("T", None, "2026-08-20T15:30:37.000Z", "c")];
        let rendered = table(&hits, &HashMap::new());
        assert!(!rendered.contains("col-"), "{rendered}");
    }

    #[test]
    fn a_multiline_snippet_stays_on_one_row() {
        let hits = vec![hit(
            "T",
            None,
            "2026-08-20T15:30:37.000Z",
            "one\ntwo\nthree",
        )];
        let rendered = table(&hits, &HashMap::new());
        assert_eq!(rendered.lines().count(), 2, "{rendered}");
        assert!(rendered.contains("one two three"), "{rendered}");
    }

    #[test]
    fn an_empty_result_set_says_so() {
        assert_eq!(table(&[], &HashMap::new()), "(no items)");
    }
}
