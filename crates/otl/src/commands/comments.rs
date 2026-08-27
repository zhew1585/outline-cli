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

/// Arguments for `otl comments list`.
#[derive(Debug, Args)]
#[command(after_long_help = "API contract:
  This command uses comments.list. --offset and --limit are sent to it;
  --status is applied here, on resolvedAt, because the operation's own
  statusFilter is an array that key=value arguments cannot express.

  Inspect it with:
    otl api describe comments.list --json")]
struct ListArgs {
    #[arg(long, value_name = "ID", required_unless_present = "collection")]
    document: Option<String>,
    #[arg(long, value_name = "ID", required_unless_present = "document")]
    collection: Option<String>,
    /// Return only replies beneath this parent comment.
    #[arg(long, value_name = "ID")]
    parent: Option<String>,
    /// Filter by thread resolution status.
    ///
    /// Applied locally, on each comment's `resolvedAt`: the operation's own
    /// `statusFilter` is an array, which `key=value` arguments cannot
    /// express. Combined with --limit that means the server counts rows
    /// before this filter does, so fewer than --limit may come back.
    #[arg(long, value_enum)]
    status: Option<Status>,
    /// Skip this many comments, server-side.
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Stop after this many comments (a warning says so on stderr).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    limit: Option<u64>,
}

/// Arguments for `otl comments create`.
#[derive(Debug, Args)]
#[command(after_long_help = "API contract:
  This command uses comments.create.

  Inspect it with:
    otl api describe comments.create --json")]
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

/// Arguments for `otl comments update`.
#[derive(Debug, Args)]
#[command(after_long_help = "API contracts:
  --text/--data use comments.update; --resolve and --unresolve use
  comments.resolve and comments.unresolve.

  Inspect them with:
    otl api describe comments.update --json
    otl api describe comments.resolve --json
    otl api describe comments.unresolve --json

  comments.resolve and comments.unresolve are absent from the published API
  description, and comments.list is there but without the parentCommentId
  and statusFilter parameters this surface needs (see spec/VENDOR.md), so
  these subcommands always dispatch from the definitions built into this
  binary. `otl api describe` reads the EFFECTIVE table instead, so after an
  `otl spec sync` it can report resolve/unresolve as unknown operations,
  and describe comments.list without those parameters, while these
  subcommands keep working.")]
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

/// Arguments for `otl comments delete`.
#[derive(Debug, Args)]
#[command(after_long_help = "API contract:
  This command uses comments.delete.

  Inspect it with:
    otl api describe comments.delete --json")]
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
    if args.offset > 0 {
        request.push(("offset".to_string(), args.offset.to_string()));
    }
    // `--limit` is handed to the pagination layer, not applied afterwards:
    // that is what makes it bound the number of requests as well as the
    // number of rows, and it is what produces the documented "truncated
    // because you asked" warning on stderr instead of a silent cut.
    let rows = session.call_rows("comments.list", &request, args.limit)?;
    let incomplete = rows.incomplete().copied();
    let filtered = rows
        .items
        .into_iter()
        .filter(|comment| matches_parent(comment, args.parent.as_deref()))
        .filter(|comment| matches_status(comment, args.status))
        .collect();
    output::emit_server(&Value::Array(filtered), mode)?;
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
    output::emit_server(&result, mode)
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
    let status_result = apply_status_change(&session, args, content_result.as_ref(), mode)?;
    let result = match (content_result, status_result) {
        (Some(content), Some(status)) => json!({ "comment": content, "status": status }),
        (Some(content), None) => content,
        (None, Some(status)) => status,
        (None, None) => Value::Null,
    };
    output::emit_server(&result, mode)
}

/// Resolve or reopen the thread, if that was asked for.
///
/// `content` is the result of the content change that has ALREADY been sent,
/// when there was one. That is what makes the failure path a partial result
/// rather than a failure: the new content is committed on the server, so
/// reporting a wholesale failure would both hide a change the user made and
/// invite a retry that applies it twice. The content is printed, and the
/// error carries code 9 - "what you got is real, and some of it is missing".
fn apply_status_change(
    session: &Session,
    args: &UpdateArgs,
    content: Option<&Value>,
    mode: OutputMode,
) -> Result<Option<Value>, CliError> {
    let Some((operation, label)) = status_change(args) else {
        return Ok(None);
    };
    let request = [("id".to_string(), args.id.clone())];
    let error = match session.call_data(operation, &request) {
        Ok(value) => return Ok(Some(value)),
        Err(error) => error,
    };
    let Some(content) = content else {
        return Err(error);
    };
    output::emit_server(content, mode)?;
    Err(CliError::partial(anyhow!(
        "the comment content was updated, but {label} failed: {error}"
    )))
}

/// The status operation this invocation asks for, with the words the report
/// uses for it.
fn status_change(args: &UpdateArgs) -> Option<(&'static str, &'static str)> {
    if args.resolve {
        return Some(("comments.resolve", "resolving the thread"));
    }
    if args.unresolve {
        return Some(("comments.unresolve", "reopening the thread"));
    }
    None
}

fn delete(args: &DeleteArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let result = Session::open(overrides)?
        .call_data("comments.delete", &[("id".to_string(), args.id.clone())])?;
    output::emit_server(&result, mode)
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
