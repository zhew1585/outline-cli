//! Stable comment workflows matching Outline MCP's comment tools.

use std::path::PathBuf;

use anyhow::anyhow;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Value};

use crate::commands::output;
use crate::commands::{api, input};
use crate::config::Overrides;
use crate::exit::CliError;
use crate::fields;
use crate::render::OutputMode;
use crate::session::{self, Session};

#[derive(Debug, Args)]
pub struct CommentsArgs {
    #[command(subcommand)]
    command: CommentsCommand,
}

#[derive(Debug, Subcommand)]
enum CommentsCommand {
    /// List comments for a document or collection.
    List(ListArgs),
    /// Create a document comment, inline comment, or reply.
    Create(CreateArgs),
    /// Change comment content and/or resolve its top-level thread.
    Update(UpdateArgs),
    /// Delete a comment.
    Delete(DeleteArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Status {
    Resolved,
    Unresolved,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(long, value_name = "ID", required_unless_present = "collection")]
    document: Option<String>,
    #[arg(long, value_name = "ID", required_unless_present = "document")]
    collection: Option<String>,
    /// Return only replies beneath this parent comment.
    #[arg(long, value_name = "ID")]
    parent: Option<String>,
    /// Filter by thread resolution status.
    #[arg(long, value_enum)]
    status: Option<Status>,
    /// Skip this many matching comments.
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Return at most this many matching comments.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    limit: Option<u64>,
}

#[derive(Debug, Args)]
struct CreateArgs {
    #[arg(long, value_name = "ID")]
    document: String,
    #[arg(long)]
    text: String,
    #[arg(long, value_name = "ID")]
    parent: Option<String>,
    #[arg(long)]
    anchor_text: Option<String>,
    #[arg(long, requires = "anchor_text")]
    anchor_prefix: Option<String>,
    #[arg(long, requires = "anchor_text")]
    anchor_suffix: Option<String>,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    id: String,
    /// Replace the comment with plain text. Markdown punctuation is kept
    /// literally; use --data for rich ProseMirror content.
    #[arg(long, conflicts_with = "data")]
    text: Option<String>,
    /// JSON file containing a ProseMirror document for the comment body.
    #[arg(long, value_name = "FILE", conflicts_with = "text")]
    data: Option<PathBuf>,
    /// Resolve the top-level comment thread.
    #[arg(long, conflicts_with = "unresolve")]
    resolve: bool,
    /// Reopen the top-level comment thread.
    #[arg(long, conflicts_with = "resolve")]
    unresolve: bool,
}

#[derive(Debug, Args)]
struct DeleteArgs {
    id: String,
}

pub fn run(args: &CommentsArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    match &args.command {
        CommentsCommand::List(args) => list(args, mode, overrides),
        CommentsCommand::Create(args) => create(args, mode, overrides),
        CommentsCommand::Update(args) => update(args, mode, overrides),
        CommentsCommand::Delete(args) => delete(args, mode, overrides),
    }
}

fn list(args: &ListArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let session = Session::open(overrides)?;
    let mut request = vec![("includeAnchorText".to_string(), "true".to_string())];
    push(&mut request, "documentId", args.document.as_ref());
    push(&mut request, "collectionId", args.collection.as_ref());
    push(&mut request, "parentCommentId", args.parent.as_ref());
    let rows = session.call_rows("comments.list", &request, None)?;
    let incomplete = rows.incomplete().copied();
    let filtered = rows
        .items
        .into_iter()
        .filter(|comment| matches_parent(comment, args.parent.as_deref()))
        .filter(|comment| matches_status(comment, args.status))
        .skip(args.offset)
        .take(
            args.limit
                .and_then(|limit| usize::try_from(limit).ok())
                .unwrap_or(usize::MAX),
        )
        .collect();
    output::emit(&Value::Array(filtered), mode)?;
    match incomplete {
        Some(truncation) => Err(session::incomplete_error(
            "the comment listing",
            &truncation,
        )),
        None => Ok(()),
    }
}

fn create(args: &CreateArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let mut request = vec![
        ("documentId".to_string(), args.document.clone()),
        ("text".to_string(), args.text.clone()),
    ];
    push(&mut request, "parentCommentId", args.parent.as_ref());
    push(&mut request, "anchorText", args.anchor_text.as_ref());
    push(&mut request, "anchorPrefix", args.anchor_prefix.as_ref());
    push(&mut request, "anchorSuffix", args.anchor_suffix.as_ref());
    let result = Session::open(overrides)?.call_data("comments.create", &request)?;
    output::emit(&result, mode)
}

fn update(args: &UpdateArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    if args.text.is_none() && args.data.is_none() && !args.resolve && !args.unresolve {
        return Err(CliError::usage(anyhow!(
            "nothing to update: pass --text, --data, --resolve, or --unresolve"
        )));
    }
    let session = Session::open(overrides)?;
    let mut content_result = None;
    if let Some(data) = comment_data(args)? {
        content_result = Some(
            session.call_raw_data("comments.update", &json!({ "id": args.id, "data": data }))?,
        );
    }
    let status_result = if args.resolve {
        Some(session.call_data("comments.resolve", &[("id".to_string(), args.id.clone())])?)
    } else if args.unresolve {
        Some(session.call_data("comments.unresolve", &[("id".to_string(), args.id.clone())])?)
    } else {
        None
    };
    let result = match (content_result, status_result) {
        (Some(content), Some(status)) => json!({ "comment": content, "status": status }),
        (Some(content), None) => content,
        (None, Some(status)) => status,
        (None, None) => Value::Null,
    };
    output::emit(&result, mode)
}

fn delete(args: &DeleteArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let result = Session::open(overrides)?
        .call_data("comments.delete", &[("id".to_string(), args.id.clone())])?;
    output::emit(&result, mode)
}

fn comment_data(args: &UpdateArgs) -> Result<Option<Value>, CliError> {
    if let Some(text) = &args.text {
        return Ok(Some(plain_text_document(text)));
    }
    let Some(path) = &args.data else {
        return Ok(None);
    };
    let raw = input::read_utf8(path, "comment data", api::MAX_BODY_FILE_BYTES)?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| CliError::usage(anyhow!("comment data file is not valid JSON: {error}")))
}

fn plain_text_document(text: &str) -> Value {
    let content = text
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                json!({ "type": "paragraph" })
            } else {
                json!({
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": line }]
                })
            }
        })
        .collect::<Vec<_>>();
    json!({ "type": "doc", "content": content })
}

fn matches_parent(comment: &Value, parent: Option<&str>) -> bool {
    parent.is_none_or(|id| fields::string_at(comment, "/parentCommentId") == Some(id))
}

fn matches_status(comment: &Value, status: Option<Status>) -> bool {
    let resolved = comment
        .get("resolvedAt")
        .is_some_and(|value| !value.is_null());
    match status {
        Some(Status::Resolved) => resolved,
        Some(Status::Unresolved) => !resolved,
        None => true,
    }
}

fn push(request: &mut Vec<(String, String)>, name: &str, value: Option<&String>) {
    if let Some(value) = value {
        request.push((name.to_string(), value.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_becomes_valid_prosemirror_paragraphs() {
        let data = plain_text_document("one\ntwo");
        assert_eq!(data["type"], "doc");
        assert_eq!(data["content"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn resolution_filter_uses_resolved_at() {
        assert!(matches_status(
            &json!({ "resolvedAt": "2026-01-01T00:00:00Z" }),
            Some(Status::Resolved)
        ));
        assert!(matches_status(
            &json!({ "resolvedAt": null }),
            Some(Status::Unresolved)
        ));
    }
}
