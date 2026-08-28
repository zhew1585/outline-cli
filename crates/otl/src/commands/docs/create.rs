//! `otl docs create`.
//!
//! The body comes from a pipe (`cat notes.md | otl docs create ...`) or from
//! `--file`; the two are equivalent, and `--file` wins when both are on the
//! table (see [`super::content`] for why that is precedence rather than an
//! error).
//!
//! A body that carries an `otl docs export` frontmatter block has it
//! removed like everywhere else, but here the block is also a signal: a
//! file that already names a document is almost certainly meant to be
//! written BACK to that document, and this command would instead file a
//! second copy of it. That gets a diagnostic rather than a refusal -
//! creating a new document from an old one is a real thing to want, and the
//! command was asked to create.

use anyhow::anyhow;
use clap::Args;
use std::path::PathBuf;

use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;
use crate::session::Session;
use crate::stdio;

use super::content;
use super::detail;

/// The compiled operation this command drives.
const OPERATION: &str = "documents.create";

/// Arguments for `otl docs create`.
#[derive(Debug, Args)]
#[command(after_long_help = "API contract:
  This command uses documents.create.

  Inspect it with:
    otl api describe documents.create --json

JSON shape:
  A write receipt, not the stored document:

    { id, collectionId, parentDocumentId, title, url, urlId,
      revision, createdAt, updatedAt, publishedAt }

  `.id` is the new document's id and `.url` its path on the instance;
  fields the server did not send are absent. The body is deliberately NOT
  echoed back - you supplied it. For the full response use `otl docs view
  ID --json` or `otl api documents.create ...`.

  A terminal instead gets the labelled essentials (id, title, updated,
  revision, url, status), which is a summary of the same receipt and not a
  separate contract.")]
pub struct CreateArgs {
    /// Document title. Without it Outline derives one from the body.
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,

    /// Emoji or named Outline icon.
    #[arg(long)]
    pub icon: Option<String>,

    /// Collection to create the document in (its id).
    #[arg(long, value_name = "ID")]
    pub collection: Option<String>,

    /// Parent document to nest the new document under (its id).
    #[arg(long, value_name = "ID")]
    pub parent: Option<String>,

    /// Read the body from this file instead of standard input (takes
    /// precedence: standard input is not read at all when this is given).
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Create the document as a draft instead of publishing it.
    #[arg(long)]
    pub draft: bool,
}

/// Run `otl docs create`.
pub fn run(cmd: &CreateArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let Some(body) = content::read(cmd.file.as_deref())? else {
        return Err(CliError::usage(anyhow!(
            "no document body: pipe markdown in (`cat notes.md | otl docs \
             create --title Notes --collection <id>`) or pass --file <path>"
        )));
    };
    if let Some(existing) = body.front.as_ref().and_then(|front| front.document_id()) {
        stdio::write_diagnostic_line(&format!(
            "notice: this body came from an export of document {existing}; \
             creating a NEW document from it (use `otl docs update --file \
             <path>` to write back to that one instead)"
        ));
    }
    let session = Session::open(overrides)?;
    let args = request_args(cmd, body.text);
    let document = session.call_data(OPERATION, &args)?;
    detail::report(&session, &document, mode)
}

/// Build the `key=value` arguments for `documents.create`.
///
/// Publishing is the default whenever a destination is known, because a
/// draft is invisible to everyone else and "one command to file a note"
/// means the note is filed. Without a collection or parent, Outline cannot
/// publish at all, so the document stays a draft and a diagnostic says so.
fn request_args(cmd: &CreateArgs, text: String) -> Vec<(String, String)> {
    let mut args = vec![("text".to_string(), text)];
    if let Some(title) = &cmd.title {
        args.push(("title".to_string(), title.clone()));
    }
    if let Some(icon) = &cmd.icon {
        args.push(("icon".to_string(), icon.clone()));
    }
    if let Some(collection) = &cmd.collection {
        args.push(("collectionId".to_string(), collection.clone()));
    }
    if let Some(parent) = &cmd.parent {
        args.push(("parentDocumentId".to_string(), parent.clone()));
    }
    let placed = cmd.collection.is_some() || cmd.parent.is_some();
    if placed {
        args.push(("publish".to_string(), (!cmd.draft).to_string()));
    } else if !cmd.draft {
        stdio::write_diagnostic_line(
            "notice: creating a draft; Outline needs --collection or --parent \
             to publish a document",
        );
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(collection: Option<&str>, parent: Option<&str>, draft: bool) -> CreateArgs {
        CreateArgs {
            title: Some("Notes".to_string()),
            icon: None,
            collection: collection.map(str::to_string),
            parent: parent.map(str::to_string),
            file: None,
            draft,
        }
    }

    fn value<'a>(args: &'a [(String, String)], key: &str) -> Option<&'a str> {
        args.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn publishes_by_default_when_a_collection_is_given() {
        let built = request_args(&args(Some("col-1"), None, false), "body".to_string());
        assert_eq!(value(&built, "publish"), Some("true"));
        assert_eq!(value(&built, "collectionId"), Some("col-1"));
        assert_eq!(value(&built, "text"), Some("body"));
        assert_eq!(value(&built, "title"), Some("Notes"));
    }

    #[test]
    fn publishes_by_default_when_a_parent_is_given() {
        let built = request_args(&args(None, Some("doc-1"), false), "body".to_string());
        assert_eq!(value(&built, "publish"), Some("true"));
        assert_eq!(value(&built, "parentDocumentId"), Some("doc-1"));
    }

    #[test]
    fn draft_flag_suppresses_publishing() {
        let built = request_args(&args(Some("col-1"), None, true), "body".to_string());
        assert_eq!(value(&built, "publish"), Some("false"));
    }

    #[test]
    fn without_a_destination_publish_is_not_sent_at_all() {
        // Outline rejects publish=true without a collection or parent, so
        // the parameter must be absent rather than false-but-present.
        let built = request_args(&args(None, None, false), "body".to_string());
        assert_eq!(value(&built, "publish"), None);
        assert_eq!(value(&built, "collectionId"), None);
    }

    #[test]
    fn a_missing_title_is_omitted_rather_than_sent_empty() {
        let mut cmd = args(Some("col-1"), None, false);
        cmd.title = None;
        let built = request_args(&cmd, "body".to_string());
        assert_eq!(value(&built, "title"), None);
    }
}
