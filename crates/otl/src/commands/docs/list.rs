//! Unified recent-document listing and full-text search.

use clap::Args;
use serde_json::Value;

use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;
use crate::session::{self, Session};

use crate::commands::output;

const SEARCH_OPERATION: &str = "documents.search";
const LIST_OPERATION: &str = "documents.list";

#[derive(Debug, Args)]
#[command(after_long_help = "API contracts:
  Without a query, results come from documents.list sorted by updatedAt.
  With one, they come from documents.search.

  Inspect them with:
    otl api describe documents.list --json
    otl api describe documents.search --json

JSON shape:
  A JSON array of the rows the operation returned, verbatim. THE TWO
  OPERATIONS RETURN DIFFERENT ROWS, so this command has two shapes:

    otl docs list --json          -> [ <document>, ... ]
                                     .[0].id, .[0].title
    otl docs list QUERY --json    -> [ { context, ranking, document }, ... ]
                                     .[0].document.id, .[0].document.title

  A `jq` path written against one silently yields null against the other.
  `otl docs search QUERY --json` is the same command as the second form and
  always has the search shape, so prefer it when a query is involved and
  keep this one for the query-less listing.")]
pub struct ListArgs {
    /// Optional full-text query. Without it, recently updated documents are
    /// returned.
    query: Option<String>,

    /// Restrict results to one collection.
    #[arg(long, value_name = "ID")]
    collection: Option<String>,

    /// Stop after N documents.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    limit: Option<u64>,
}

pub fn run(args: &ListArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let session = Session::open(overrides)?;
    let mut request = Vec::new();
    if let Some(collection) = &args.collection {
        request.push(("collectionId".to_string(), collection.clone()));
    }
    let operation = if let Some(query) = &args.query {
        request.push(("query".to_string(), query.clone()));
        SEARCH_OPERATION
    } else {
        request.push(("sort".to_string(), "updatedAt".to_string()));
        request.push(("direction".to_string(), "DESC".to_string()));
        LIST_OPERATION
    };
    let rows = session.call_rows(operation, &request, args.limit)?;
    let incomplete = rows.incomplete().copied();
    output::emit_server(&Value::Array(rows.items), mode)?;
    match incomplete {
        Some(truncation) => Err(session::incomplete_error(
            "the document listing",
            &truncation,
        )),
        None => Ok(()),
    }
}
