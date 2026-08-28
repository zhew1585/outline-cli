//! `otl docs update [ID]`.
//!
//! Either the title, the body, or both. The body arrives the same way as
//! for `create` (a pipe or `--file`); with neither the command refuses
//! rather than sending an empty update.
//!
//! # Writing back an exported file
//!
//! A file `otl docs export` wrote opens with a block naming the document
//! (see [`super::frontmatter`]), so `otl docs update --file backup/Doc.md`
//! needs no ID: the block supplies it. The block also records the revision
//! the copy was taken from, and this command refuses to send an update when
//! the document has moved past it - the whole point of exporting, editing
//! and writing back is that nothing between those steps is lost, and a
//! `--mode replace` against a document somebody else has since edited would
//! lose exactly that.
//!
//! Both behaviours are opt-out by being opt-in: they happen only when the
//! body actually carried a block. A piped body, or a file without one,
//! behaves exactly as it did before.

use anyhow::anyhow;
use clap::{Args, ValueEnum};
use std::path::PathBuf;

use crate::config::Overrides;
use crate::exit::CliError;
use crate::fields;
use crate::render::OutputMode;
use crate::session::Session;
use crate::stdio;

use super::content::{self, Body};
use super::detail;
use super::frontmatter::FrontMatter;

/// The compiled operation this command drives.
const OPERATION: &str = "documents.update";

/// Operation read to check the document has not moved past the local copy.
const INFO_OPERATION: &str = "documents.info";

/// How an incoming body changes existing document content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EditMode {
    Replace,
    Append,
    Prepend,
    Patch,
}

impl EditMode {
    fn api_value(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Append => "append",
            Self::Prepend => "prepend",
            Self::Patch => "patch",
        }
    }
}

/// Arguments for `otl docs update`.
#[derive(Debug, Args)]
#[command(after_long_help = "API contract:
  This command uses documents.update.

  Inspect it with:
    otl api describe documents.update --json

JSON shape:
  A write receipt, not the stored document - the same shape `otl docs
  create --json` returns, with `.revision` bumped:

    { id, collectionId, parentDocumentId, title, url, urlId,
      revision, createdAt, updatedAt, publishedAt }

  Fields the server did not send are absent. The body is deliberately NOT
  echoed back: an append to a large page would otherwise return the whole
  page. For the full response use `otl docs view ID --json` (the document,
  read back) or `otl api documents.update id=ID ...` (the raw operation).

  A terminal instead gets the labelled essentials, which is a summary of
  the same receipt and not a separate contract.

Writing back an exported file:
  A file from `otl docs export` opens with a YAML block naming the
  document, so ID may be omitted:

    otl docs update --file backup/Design.md

  The block is stripped before sending, so it never becomes document text.
  When the block records a revision, this command reads the document first
  and refuses if it has been edited since the export; --force skips that
  check and overwrites. Passing an ID that disagrees with the block is a
  usage error, not a silent choice between them.")]
pub struct UpdateArgs {
    /// Document id (UUID or the short urlId from its URL). Optional when
    /// the body comes from a file carrying an `outline_id` block.
    pub id: Option<String>,

    /// New title.
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,

    /// New emoji or named Outline icon.
    #[arg(long, conflicts_with = "clear_icon")]
    pub icon: Option<String>,

    /// Remove the document icon.
    #[arg(long, conflicts_with = "icon")]
    pub clear_icon: bool,

    /// Read the new body from this file instead of standard input (takes
    /// precedence: standard input is not read at all when this is given).
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Body edit mode; omitted means replace.
    #[arg(long, value_enum, value_name = "MODE")]
    pub mode: Option<EditMode>,

    /// Existing text to replace when --mode patch is used.
    #[arg(long, value_name = "TEXT")]
    pub find_text: Option<String>,

    /// Publish the document if it is still a draft.
    #[arg(long)]
    pub publish: bool,

    /// Send the update even when the document has changed since the local
    /// copy was exported (skips the revision check).
    #[arg(long)]
    pub force: bool,
}

/// Run `otl docs update`.
///
/// Everything local happens before the session is opened: which document,
/// and whether the request is even coherent. Only then is the revision
/// checked, because that costs a request.
pub fn run(cmd: &UpdateArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let body = content::read(cmd.file.as_deref())?;
    let front = body.as_ref().and_then(|body| body.front.clone());
    let id = resolve_id(cmd.id.as_deref(), front.as_ref())?;
    let args = request_args(cmd, &id, body)?;
    let session = Session::open(overrides)?;
    guard_revision(&session, &id, front.as_ref().and_then(|f| f.revision), cmd)?;
    let document = session.call_data(OPERATION, &args)?;
    detail::report(&session, &document, mode)
}

/// Which document to write to.
///
/// A command line id wins when both are present, because it is the more
/// deliberate of the two - but only after it is checked against the block,
/// and a disagreement is refused rather than resolved. Silently preferring
/// either one would mean a mistyped id quietly overwrites the wrong
/// document with the contents of this file, which is the one outcome this
/// command must never produce. Note that the two spellings of an id are not
/// a disagreement: `outline_url_id` is what a caller copies out of a URL.
fn resolve_id(argument: Option<&str>, front: Option<&FrontMatter>) -> Result<String, CliError> {
    if let Some(argument) = argument {
        if let Some(from_file) = front.filter(|front| !front.names(argument)) {
            let from_file = from_file.document_id().unwrap_or_default();
            return Err(CliError::usage(anyhow!(
                "the ID argument ({argument}) is not the document this file \
                 describes ({from_file}); pass the right one, or drop the ID \
                 argument to use the file's"
            )));
        }
        return Ok(argument.to_string());
    }
    front
        .and_then(FrontMatter::document_id)
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::usage(anyhow!(
                "no document to update: pass its id, or use --file with a \
                 file from `otl docs export` (which names the document in \
                 its leading `outline_id` block)"
            ))
        })
}

/// Refuse to write over a document that has moved past the local copy.
///
/// Only reached when the body carried a revision, which means it came from
/// an export: the caller has a copy taken at a known version, and the whole
/// value of writing it back is that nothing since then is lost. A
/// `--mode replace` against a document edited in the meantime would discard
/// that edit with no trace, so the check is a refusal before the write
/// rather than a warning after it.
///
/// A server that does not report a revision gets a diagnostic and the
/// benefit of the doubt: refusing there would break the command against an
/// instance that simply omits the field, and the caller asked for a write.
fn guard_revision(
    session: &Session,
    id: &str,
    expected: Option<u64>,
    cmd: &UpdateArgs,
) -> Result<(), CliError> {
    let Some(expected) = expected.filter(|_| !cmd.force) else {
        return Ok(());
    };
    let document = session.call_data(INFO_OPERATION, &[("id".to_string(), id.to_string())])?;
    let Some(current) = document.pointer("/revision").and_then(|v| v.as_u64()) else {
        stdio::write_diagnostic_line(
            "warning: the server reported no revision for this document, so \
             whether it changed since this copy was exported could not be \
             checked",
        );
        return Ok(());
    };
    if current == expected {
        return Ok(());
    }
    let title = fields::string_at(&document, "/title").unwrap_or_default();
    Err(CliError::usage(anyhow!(
        "this document has changed since the local copy was exported: the \
         file records revision {expected}, the server is at revision \
         {current}{}. Re-export it and reapply your edits, or pass --force \
         to overwrite the newer version.",
        if title.is_empty() {
            String::new()
        } else {
            format!(" ({})", crate::text::quote(title))
        }
    )))
}

/// Build the `key=value` arguments for `documents.update`.
///
/// Two refusals, both before any request:
///
/// - nothing to change at all would be a pointless round trip;
/// - an EMPTY body would replace the document with nothing. `content::read`
///   already reports a blank source as "no body", so this is defence in
///   depth for a body that reaches here empty by another route: an
///   accidental wipe is expensive and must be spelled out through `otl api`.
fn request_args(
    cmd: &UpdateArgs,
    id: &str,
    body: Option<Body>,
) -> Result<Vec<(String, String)>, CliError> {
    if let Some(body) = &body {
        if body.text.trim().is_empty() {
            return Err(CliError::usage(anyhow!(
                "the new document body is empty; refusing to erase the \
                 document's contents (use `otl api documents.update \
                 id=<id> text=` if that is really the intent)"
            )));
        }
    }
    if body.is_none()
        && cmd.title.is_none()
        && cmd.icon.is_none()
        && !cmd.clear_icon
        && !cmd.publish
    {
        return Err(CliError::usage(anyhow!(
            "nothing to update: pass --title, --icon, --clear-icon, --publish, \
             or a new body on standard input or via --file"
        )));
    }
    validate_edit_mode(cmd, body.is_some())?;
    let mut args = vec![("id".to_string(), id.to_string())];
    if let Some(title) = &cmd.title {
        args.push(("title".to_string(), title.clone()));
    }
    if let Some(icon) = &cmd.icon {
        args.push(("icon".to_string(), icon.clone()));
    } else if cmd.clear_icon {
        args.push(("icon".to_string(), "null".to_string()));
    }
    if let Some(body) = body {
        args.push(("text".to_string(), body.text));
        if let Some(mode) = cmd.mode {
            args.push(("editMode".to_string(), mode.api_value().to_string()));
        }
        if let Some(find) = &cmd.find_text {
            args.push(("findText".to_string(), find.clone()));
        }
    }
    if cmd.publish {
        args.push(("publish".to_string(), "true".to_string()));
    }
    Ok(args)
}

fn validate_edit_mode(cmd: &UpdateArgs, has_body: bool) -> Result<(), CliError> {
    if (cmd.mode.is_some() || cmd.find_text.is_some()) && !has_body {
        return Err(CliError::usage(anyhow!(
            "--mode and --find-text require a new body on standard input or via --file"
        )));
    }
    if cmd.mode == Some(EditMode::Patch) && cmd.find_text.is_none() {
        return Err(CliError::usage(anyhow!(
            "--mode patch requires --find-text"
        )));
    }
    if cmd.find_text.is_some() && cmd.mode != Some(EditMode::Patch) {
        return Err(CliError::usage(anyhow!(
            "--find-text is only valid with --mode patch"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::commands::docs::content::Origin;

    fn cmd(title: Option<&str>, publish: bool) -> UpdateArgs {
        UpdateArgs {
            id: Some("doc-1".to_string()),
            title: title.map(str::to_string),
            icon: None,
            clear_icon: false,
            file: None,
            mode: None,
            find_text: None,
            publish,
            force: false,
        }
    }

    fn body(text: &str) -> Option<Body> {
        Some(Body {
            text: text.to_string(),
            origin: Origin::Stdin,
            front: None,
        })
    }

    /// `request_args` with the id the real command would have resolved.
    fn request_args(
        cmd: &UpdateArgs,
        body: Option<Body>,
    ) -> Result<Vec<(String, String)>, CliError> {
        super::request_args(cmd, cmd.id.as_deref().unwrap_or("doc-1"), body)
    }

    fn front(id: Option<&str>, url_id: Option<&str>, revision: Option<u64>) -> FrontMatter {
        FrontMatter {
            id: id.map(str::to_string),
            url_id: url_id.map(str::to_string),
            revision,
        }
    }

    fn value<'a>(args: &'a [(String, String)], key: &str) -> Option<&'a str> {
        args.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn sends_title_and_body_together() {
        let built = request_args(&cmd(Some("New"), false), body("new text")).unwrap();
        assert_eq!(value(&built, "id"), Some("doc-1"));
        assert_eq!(value(&built, "title"), Some("New"));
        assert_eq!(value(&built, "text"), Some("new text"));
        assert_eq!(value(&built, "publish"), None);
    }

    #[test]
    fn a_title_alone_is_enough() {
        let built = request_args(&cmd(Some("New"), false), None).unwrap();
        assert_eq!(value(&built, "title"), Some("New"));
        assert_eq!(value(&built, "text"), None);
    }

    #[test]
    fn a_body_alone_is_enough() {
        let built = request_args(&cmd(None, false), body("text")).unwrap();
        assert_eq!(value(&built, "text"), Some("text"));
        assert_eq!(value(&built, "title"), None);
    }

    #[test]
    fn publish_alone_is_enough() {
        let built = request_args(&cmd(None, true), None).unwrap();
        assert_eq!(value(&built, "publish"), Some("true"));
    }

    #[test]
    fn append_and_patch_modes_reach_the_api() {
        let mut append = cmd(None, false);
        append.mode = Some(EditMode::Append);
        let built = request_args(&append, body("more")).unwrap();
        assert_eq!(value(&built, "editMode"), Some("append"));

        let mut patch = cmd(None, false);
        patch.mode = Some(EditMode::Patch);
        patch.find_text = Some("old".to_string());
        let built = request_args(&patch, body("new")).unwrap();
        assert_eq!(value(&built, "editMode"), Some("patch"));
        assert_eq!(value(&built, "findText"), Some("old"));
    }

    #[test]
    fn patch_requires_find_text_and_a_body() {
        let mut patch = cmd(None, false);
        patch.mode = Some(EditMode::Patch);
        assert!(request_args(&patch, body("new")).is_err());
        patch.find_text = Some("old".to_string());
        assert!(request_args(&patch, None).is_err());
    }

    #[test]
    fn nothing_to_change_is_a_usage_error() {
        let error = request_args(&cmd(None, false), None).unwrap_err();
        assert_eq!(error.code, crate::exit::ExitCode::Usage);
    }

    #[test]
    fn an_empty_body_is_refused_rather_than_erasing_the_document() {
        for text in ["", "   ", "\n\n"] {
            let error = request_args(&cmd(Some("keep"), false), body(text)).unwrap_err();
            assert_eq!(error.code, crate::exit::ExitCode::Usage);
            assert!(error.to_string().contains("erase"), "{error}");
        }
    }

    #[test]
    fn an_exported_file_supplies_the_document_id() {
        let front = front(Some("55baa74a"), Some("engKBTOaWe"), Some(15));
        assert_eq!(resolve_id(None, Some(&front)).unwrap(), "55baa74a");
    }

    #[test]
    fn a_command_line_id_wins_when_it_agrees_with_the_file() {
        let front = front(Some("55baa74a"), Some("engKBTOaWe"), None);
        assert_eq!(
            resolve_id(Some("55baa74a"), Some(&front)).unwrap(),
            "55baa74a"
        );
        // The short id out of a URL is the same document, not a conflict.
        assert_eq!(
            resolve_id(Some("engKBTOaWe"), Some(&front)).unwrap(),
            "engKBTOaWe"
        );
    }

    #[test]
    fn an_id_that_contradicts_the_file_is_refused_rather_than_picked() {
        // The failure this refusal exists for: a mistyped id would
        // otherwise overwrite an unrelated document with this file.
        let front = front(Some("55baa74a"), Some("engKBTOaWe"), None);
        let error = resolve_id(Some("other-doc"), Some(&front)).unwrap_err();
        assert_eq!(error.code, crate::exit::ExitCode::Usage);
        assert!(error.to_string().contains("55baa74a"), "{error}");
    }

    #[test]
    fn a_body_with_no_block_still_needs_an_id_argument() {
        let error = resolve_id(None, None).unwrap_err();
        assert_eq!(error.code, crate::exit::ExitCode::Usage);
        assert!(error.to_string().contains("outline_id"), "{error}");
        // A block that named no document is the same case.
        let error = resolve_id(None, Some(&front(None, None, Some(3)))).unwrap_err();
        assert_eq!(error.code, crate::exit::ExitCode::Usage);
    }

    #[test]
    fn a_piped_body_is_unaffected_by_any_of_this() {
        assert_eq!(resolve_id(Some("doc-1"), None).unwrap(), "doc-1");
    }
}
