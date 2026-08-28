//! `otl docs update <id>`.
//!
//! Either the title, the body, or both. The body arrives the same way as
//! for `create` (a pipe or `--file`); with neither the command refuses
//! rather than sending an empty update.
//!
//! # Editing part of a body
//!
//! `--section` and `--delete-section` name one section of the page instead
//! of replacing all of it, and `--mode patch` names a string. All three
//! become the same underlying write, and everything they share - deriving
//! or verifying the anchor, and pinning the write to the revision it was
//! computed from - lives in [`super::anchor`], which is also where the
//! reasoning for it is written down.
//!
//! What stays here is the command: which flag combinations are refusable
//! before a request, and which of those paths each one takes. Two of them
//! need the current body and so spend one `documents.info` first;
//! `--mode append` and `--mode prepend` have no anchor to verify and stay a
//! single request.

use anyhow::anyhow;
use clap::{Args, ValueEnum};
use std::path::PathBuf;

use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;
use crate::session::Session;

use super::anchor::{self, Plan};
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

Editing one section:
  --section replaces the section a heading names, heading line included, so
  the new text is the same shape `otl docs view ID --section` prints. The
  section runs to the next heading of the same or a higher level, so
  replacing a chapter replaces what is nested under it.

    otl docs view ID --outline --json                 # addresses, and .revision
    otl docs view ID --section 'Deploy' --raw > s.md
    otl docs update ID --section 'Deploy' --file s.md --if-revision 12
    otl docs update ID --delete-section 'Deploy > Rollback'

  Only the changed part is sent: this CLI reads the body, splices it, and
  derives a findText that occurs exactly once, so nothing has to guess
  which copy of a repeated heading was meant. When no anchor short of the
  whole page is unique it sends the whole body instead - same write, more
  bytes, and it says so on stderr.

  An address is a heading title, optionally with its parents
  ('Deploy > Rollback') and optionally with its level pinned ('## Deploy').
  An address matching several headings is refused, with each one listed.

Not overwriting someone else's edit:
  --if-revision N refuses the write unless the document is still at
  revision N - the number `docs view --outline --json` or a previous write
  receipt reported. Pass it whenever the new text was written against a
  body you read earlier: without it, an edit computed from a stale copy can
  still apply cleanly on top of someone else's change.

  Every path that reads the body also pins the write to the revision it
  just read, so the window between this command's own read and its write is
  closed with or without the flag.")]
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

    /// Existing text to replace when --mode patch is used. Refused unless
    /// it occurs exactly once in the document.
    #[arg(long, value_name = "TEXT")]
    pub find_text: Option<String>,

    /// Replace only the section this heading names, heading line included
    /// (`docs view ID --outline` lists every address).
    #[arg(
        long,
        value_name = "HEADING",
        conflicts_with_all = ["mode", "find_text", "delete_section"]
    )]
    pub section: Option<String>,

    /// Remove the section this heading names, heading line and all nested
    /// sections included. Takes no new body.
    #[arg(
        long,
        value_name = "HEADING",
        conflicts_with_all = ["mode", "find_text", "section", "file"]
    )]
    pub delete_section: Option<String>,

    /// Refuse the write unless the document is still at this revision.
    #[arg(long, value_name = "N")]
    pub if_revision: Option<u64>,

    /// Publish the document if it is still a draft.
    #[arg(long)]
    pub publish: bool,
}

/// Run `otl docs update`.
pub fn run(cmd: &UpdateArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let body = content::read(cmd.file.as_deref())?;
    validate(cmd, body.as_ref())?;
    let session = Session::open(overrides)?;
    let plan = body_args(&session, cmd, body)?;
    let mut args = metadata_args(cmd);
    args.extend(plan.args);
    if let Some(revision) = plan.pinned {
        args.push(("lastRevision".to_string(), revision.to_string()));
    }
    let document = session
        .call_data(OPERATION, &args)
        .map_err(|error| anchor::explain_conflict(&cmd.id, plan.pinned, error))?;
    detail::report(&session, &document, mode)
}

/// Everything that can be refused before a request is sent.
///
/// Two of these matter more than the others:
///
/// - an EMPTY body would replace the document with nothing. `content::read`
///   already reports a blank source as "no body", so this is defence in
///   depth for a body that reaches here empty by another route: an
///   accidental wipe is expensive and must be spelled out through `otl api`.
/// - nothing to change at all would be a pointless round trip.
fn validate(cmd: &UpdateArgs, body: Option<&Body>) -> Result<(), CliError> {
    if let Some(body) = body {
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
        && cmd.section.is_none()
        && cmd.delete_section.is_none()
        && !cmd.clear_icon
        && !cmd.publish
    {
        return Err(CliError::usage(anyhow!(
            "nothing to update: pass --title, --icon, --clear-icon, --publish, \
             --delete-section, or a new body on standard input or via --file"
        )));
    }
    validate_mode(cmd, body.is_some())?;
    validate_section(cmd, body.is_some())
}

/// The `--mode` / `--find-text` pair, which only make sense together and
/// only with a body to apply.
fn validate_mode(cmd: &UpdateArgs, has_body: bool) -> Result<(), CliError> {
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

/// The two section flags, which disagree with each other about the body:
/// one needs it and the other refuses it.
fn validate_section(cmd: &UpdateArgs, has_body: bool) -> Result<(), CliError> {
    if cmd.section.is_some() && !has_body {
        return Err(CliError::usage(anyhow!(
            "--section replaces that section, so it needs the new section \
             text on standard input or via --file. To remove the section \
             instead, use --delete-section."
        )));
    }
    // The clap-level conflict cannot see a pipe, so this is where a body
    // arriving alongside a deletion is caught. Guessing which one was meant
    // is exactly the wrong move on a destructive command.
    if cmd.delete_section.is_some() && has_body {
        return Err(CliError::usage(anyhow!(
            "--delete-section removes a section and takes no new body, but a \
             body arrived anyway. Use --section <HEADING> to replace that \
             section with it."
        )));
    }
    Ok(())
}

/// The `key=value` arguments that are not about the body.
fn metadata_args(cmd: &UpdateArgs) -> Vec<(String, String)> {
    let mut args = vec![("id".to_string(), cmd.id.clone())];
    if let Some(title) = &cmd.title {
        args.push(("title".to_string(), title.clone()));
    }
    if let Some(icon) = &cmd.icon {
        args.push(("icon".to_string(), icon.clone()));
    } else if cmd.clear_icon {
        args.push(("icon".to_string(), "null".to_string()));
    }
    if cmd.publish {
        args.push(("publish".to_string(), "true".to_string()));
    }
    args
}

/// Build the body arguments, reading the current body when an anchor needs
/// to be derived or verified.
///
/// The order of the branches is the precedence the flags declare: the two
/// section flags conflict with everything else at the clap level, so by the
/// time a later branch is reached the earlier ones were genuinely absent.
fn body_args(session: &Session, cmd: &UpdateArgs, body: Option<Body>) -> Result<Plan, CliError> {
    if let Some(address) = &cmd.delete_section {
        let (text, pinned) = anchor::current_body(session, &cmd.id, cmd.if_revision)?;
        return anchor::section_plan(&text, pinned, "--delete-section", address, None);
    }
    if let Some(address) = &cmd.section {
        // `validate` has already refused the bodyless case.
        let replacement = body.map(|body| body.text).unwrap_or_default();
        let (text, pinned) = anchor::current_body(session, &cmd.id, cmd.if_revision)?;
        return anchor::section_plan(&text, pinned, "--section", address, Some(&replacement));
    }
    let Some(body) = body else {
        return Ok(Plan {
            args: Vec::new(),
            pinned: cmd.if_revision,
        });
    };
    if cmd.mode == Some(EditMode::Patch) {
        let target = cmd.find_text.clone().unwrap_or_default();
        let (text, pinned) = anchor::current_body(session, &cmd.id, cmd.if_revision)?;
        anchor::verify(&text, &target)?;
        return Ok(anchor::patch_plan(body.text, target, pinned));
    }
    // Replace, append and prepend have no anchor to verify, so they stay a
    // single request and pin only what the caller asserted.
    let mut args = vec![("text".to_string(), body.text)];
    if let Some(mode) = cmd.mode {
        args.push(("editMode".to_string(), mode.api_value().to_string()));
    }
    Ok(Plan {
        args,
        pinned: cmd.if_revision,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::commands::docs::content::Origin;
    use crate::exit::ExitCode;

    fn cmd(title: Option<&str>, publish: bool) -> UpdateArgs {
        UpdateArgs {
            id: "doc-1".to_string(),
            title: title.map(str::to_string),
            icon: None,
            clear_icon: false,
            file: None,
            mode: None,
            find_text: None,
            section: None,
            delete_section: None,
            if_revision: None,
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
    fn the_metadata_half_carries_id_title_and_publish() {
        let mut both = cmd(Some("New"), true);
        both.icon = Some("\u{1f4d3}".to_string());
        validate(&both, body("new text").as_ref()).unwrap();
        let built = metadata_args(&both);
        assert_eq!(value(&built, "id"), Some("doc-1"));
        assert_eq!(value(&built, "title"), Some("New"));
        assert_eq!(value(&built, "publish"), Some("true"));
        assert_eq!(value(&built, "icon"), Some("\u{1f4d3}"));
    }

    #[test]
    fn clearing_an_icon_sends_an_explicit_null() {
        let mut cleared = cmd(None, false);
        cleared.clear_icon = true;
        validate(&cleared, None).unwrap();
        assert_eq!(value(&metadata_args(&cleared), "icon"), Some("null"));
    }

    #[test]
    fn publish_alone_is_enough() {
        validate(&cmd(None, true), None).unwrap();
        assert_eq!(
            value(&metadata_args(&cmd(None, true)), "publish"),
            Some("true")
        );
    }

    #[test]
    fn nothing_to_change_is_a_usage_error() {
        let error = validate(&cmd(None, false), None).unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
    }

    #[test]
    fn an_empty_body_is_refused_rather_than_erasing_the_document() {
        for text in ["", "   ", "\n\n"] {
            let error = validate(&cmd(Some("keep"), false), body(text).as_ref()).unwrap_err();
            assert_eq!(error.code, ExitCode::Usage);
            assert!(error.to_string().contains("erase"), "{error}");
        }
    }

    #[test]
    fn patch_requires_find_text_and_a_body() {
        let mut patch = cmd(None, false);
        patch.mode = Some(EditMode::Patch);
        assert!(validate(&patch, body("new").as_ref()).is_err());
        patch.find_text = Some("old".to_string());
        assert!(validate(&patch, None).is_err());
        assert!(validate(&patch, body("new").as_ref()).is_ok());
    }

    #[test]
    fn find_text_without_patch_mode_is_refused() {
        let mut mismatched = cmd(None, false);
        mismatched.find_text = Some("old".to_string());
        mismatched.mode = Some(EditMode::Append);
        let error = validate(&mismatched, body("new").as_ref()).unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
        assert!(error.to_string().contains("--mode patch"), "{error}");
    }

    #[test]
    fn a_section_needs_a_body_and_a_deletion_refuses_one() {
        let mut replace = cmd(None, false);
        replace.section = Some("Deploy".to_string());
        let error = validate(&replace, None).unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
        assert!(error.to_string().contains("--delete-section"), "{error}");
        assert!(validate(&replace, body("## Deploy\n\nnew").as_ref()).is_ok());

        let mut delete = cmd(None, false);
        delete.delete_section = Some("Deploy".to_string());
        assert!(validate(&delete, None).is_ok());
        // A body arriving on a pipe is ambiguous, and this is a destructive
        // command, so it is refused rather than resolved. clap cannot catch
        // this one: it cannot see a pipe.
        let error = validate(&delete, body("something").as_ref()).unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
        assert!(error.to_string().contains("--section"), "{error}");
    }

    /// A deletion is the one path that needs neither a body nor a title, so
    /// it must not trip the "nothing to update" refusal.
    #[test]
    fn a_deletion_alone_is_something_to_update() {
        let mut delete = cmd(None, false);
        delete.delete_section = Some("Deploy".to_string());
        assert!(validate(&delete, None).is_ok());
    }

    /// The flag combinations clap itself rejects, checked against the real
    /// command tree so that a renamed flag cannot silently drop a conflict.
    #[test]
    fn the_section_flags_conflict_with_the_anchor_flags() {
        use clap::CommandFactory;

        let refused = [
            vec![
                "otl",
                "docs",
                "update",
                "id",
                "--section",
                "A",
                "--mode",
                "append",
            ],
            vec![
                "otl",
                "docs",
                "update",
                "id",
                "--section",
                "A",
                "--find-text",
                "x",
            ],
            vec![
                "otl",
                "docs",
                "update",
                "id",
                "--section",
                "A",
                "--delete-section",
                "B",
            ],
            vec![
                "otl",
                "docs",
                "update",
                "id",
                "--delete-section",
                "A",
                "--file",
                "f.md",
            ],
        ];
        for argv in refused {
            assert!(
                crate::cli::Cli::command()
                    .try_get_matches_from(&argv)
                    .is_err(),
                "{argv:?} was accepted"
            );
        }
    }
}
