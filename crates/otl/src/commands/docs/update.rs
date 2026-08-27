//! `otl docs update <id>`.
//!
//! Either the title, the body, or both. The body arrives the same way as
//! for `create` (a pipe or `--file`); with neither the command refuses
//! rather than sending an empty update.

use anyhow::anyhow;
use clap::{Args, ValueEnum};
use std::path::PathBuf;

use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;
use crate::session::Session;

use super::content::{self, Body};
use super::detail;

/// The compiled operation this command drives.
const OPERATION: &str = "documents.update";

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
  The documents.update document, verbatim - the same shape `otl docs
  create --json` returns, with `.revision` bumped. A terminal instead gets
  the labelled essentials, which is a summary of the same object and not a
  separate contract.")]
pub struct UpdateArgs {
    /// Document id (UUID or the short urlId from its URL).
    pub id: String,

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
}

/// Run `otl docs update`.
pub fn run(cmd: &UpdateArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let body = content::read(cmd.file.as_deref())?;
    let args = request_args(cmd, body)?;
    let session = Session::open(overrides)?;
    let document = session.call_data(OPERATION, &args)?;
    detail::report(&session, &document, mode)
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
fn request_args(cmd: &UpdateArgs, body: Option<Body>) -> Result<Vec<(String, String)>, CliError> {
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
    let mut args = vec![("id".to_string(), cmd.id.clone())];
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
            id: "doc-1".to_string(),
            title: title.map(str::to_string),
            icon: None,
            clear_icon: false,
            file: None,
            mode: None,
            find_text: None,
            publish,
        }
    }

    fn body(text: &str) -> Option<Body> {
        Some(Body {
            text: text.to_string(),
            origin: Origin::Stdin,
        })
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
}
