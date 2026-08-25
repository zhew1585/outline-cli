//! `otl docs view <id>` (story 3.2).
//!
//! The datum of this command is the document's markdown, so - unlike the
//! list commands - a pipe gets the markdown and not JSON. The dual-state
//! rule still holds, it is just spelled with a different default:
//!
//! - `--json` (explicit): the raw document object, for scripts;
//! - `--raw`, or a non-terminal stdout: the markdown, verbatim, no pager;
//! - a terminal stdout: the markdown, through `$PAGER` when it does not fit
//!   on one screen;
//! - `--web`: the document's URL on stdout, and the browser opened at it.

use anyhow::anyhow;
use clap::Args;
use serde_json::Value;

use crate::browser;
use crate::exit::CliError;
use crate::fields;
use crate::pager;
use crate::render::{self, OutputMode};
use crate::session::Session;
use crate::stdio;

/// The compiled operation this command drives.
const OPERATION: &str = "documents.info";

/// Arguments for `otl docs view`.
#[derive(Debug, Args)]
pub struct ViewArgs {
    /// Document id (UUID or the short urlId from its URL).
    pub id: String,

    /// Write the markdown straight to stdout, never through a pager.
    #[arg(long)]
    pub raw: bool,

    /// Open the document in the default browser (honors `$BROWSER`).
    #[arg(long, conflicts_with = "raw")]
    pub web: bool,
}

/// Run `otl docs view`.
pub fn run(cmd: &ViewArgs, mode: OutputMode, json_requested: bool) -> Result<(), CliError> {
    if cmd.raw && json_requested {
        return Err(CliError::usage(anyhow!(
            "--raw prints the document's markdown and --json prints its \
             metadata as JSON; pass one or the other"
        )));
    }
    let session = Session::open()?;
    let document = session.call_data(OPERATION, &[("id".to_string(), cmd.id.clone())])?;
    if cmd.web {
        return open_in_browser(&session, &document, json_requested);
    }
    if json_requested {
        return print_json(&document);
    }
    let text = markdown(&document);
    // Paging is for humans only: a terminal, no --raw, no --json.
    let paginate = mode == OutputMode::Table && !cmd.raw;
    pager::write(&text, paginate)
}

/// The document's markdown body.
///
/// A document whose body was not returned (Outline can answer with a
/// rich-text `data` field instead of `text`) yields empty output plus a
/// diagnostic, rather than a silent blank.
fn markdown(document: &Value) -> String {
    match fields::string_at(document, "/text") {
        Some(text) => text.to_string(),
        None => {
            stdio::write_diagnostic_line(
                "warning: the server returned no markdown body for this document",
            );
            String::new()
        }
    }
}

/// Print the raw document object.
fn print_json(document: &Value) -> Result<(), CliError> {
    let rendered = render::render(document, OutputMode::Json)
        .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
    stdio::write_data_line(&rendered)
}

/// Print the document's absolute URL and open a browser at it.
///
/// The URL goes to stdout FIRST: if no browser can be launched the user
/// still has the link, and a script that only wants the link gets it.
fn open_in_browser(
    session: &Session,
    document: &Value,
    json_requested: bool,
) -> Result<(), CliError> {
    let path = fields::string_at(document, "/url").ok_or_else(|| {
        CliError::failure(anyhow!(
            "the server did not return a URL for this document, so there is \
             nothing to open"
        ))
    })?;
    let url = session.absolute_url(path)?;
    if json_requested {
        let payload = serde_json::json!({
            "id": fields::string_at(document, "/id"),
            "title": fields::string_at(document, "/title"),
            "url": url,
        });
        print_json(&payload)?;
    } else {
        stdio::write_data_line(&url)?;
    }
    browser::open(&url)
}
