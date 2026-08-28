//! Anchoring a partial write, and pinning it to the revision it was
//! computed from.
//!
//! `otl docs update` has three ways to change part of a body rather than
//! all of it - `--section`, `--delete-section`, and `--mode patch` - and all
//! three become the same underlying write: `documents.update` with
//! `editMode=patch` and a `findText`. This module is everything those three
//! share, which is exactly the two ways that write can go wrong.
//!
//! # The anchor has to be unique
//!
//! `findText` is a plain string match, so an anchor occurring twice leaves
//! the server to choose, and its choice is not part of the published
//! contract. The two paths reach that guarantee differently, and the
//! difference is the point:
//!
//! - a section address supplies an intended POSITION, so
//!   [`super::section`] can widen the anchor outward from it until the body
//!   contains it once. Uniqueness becomes a counted fact.
//! - `--find-text` supplies only a string, so there is nothing to widen
//!   from and [`verify`] can only refuse. What it can do that a local-file
//!   editor cannot is say *where* the matches are and which section each
//!   one sits in, because the heading tree is already parsed. That is what
//!   turns "3 matches" into a next step instead of a retry in the dark.
//!
//! # The body has to still be the body
//!
//! An anchor computed from text that has since changed can still match, in
//! a place that now means something else. So every path here pins its write
//! to the revision it read, by sending `lastRevision`.
//!
//! That asks the server to close the window between this CLI's own read and
//! its write, which is milliseconds - and only asks, because not every
//! instance implements the field (see [`require_revision`]). The window
//! that actually matters is longer and this CLI cannot see it: an agent
//! reads a section on one turn and writes it two turns later, and by then
//! its own copy is stale while every read this module performs is fresh.
//! `--if-revision` is the caller's half of the check for exactly that - it
//! asserts the revision the CALLER last saw, and it is enforced HERE,
//! locally, on every path, before anything is sent.

use anyhow::anyhow;
use serde_json::Value;

use crate::exit::{CliError, ExitCode};
use crate::fields;
use crate::session::Session;
use crate::stdio;
use crate::text;

use super::section::{self, Edit};

/// The operation that supplies the current body when an anchor is needed.
const READ_OPERATION: &str = "documents.info";

/// How many anchor positions a refusal lists before it summarizes.
const MAX_REPORTED: usize = 20;

/// The `editMode` value every path here sends.
const PATCH_MODE: &str = "patch";

/// The body half of the request, and the revision the write is pinned to.
#[derive(Debug)]
pub(super) struct Plan {
    /// `text`, and `editMode`/`findText` when there is an anchor.
    pub args: Vec<(String, String)>,
    /// Sent as `lastRevision`, which asks the server to reject a write
    /// whose anchor was computed against a body that has since moved.
    ///
    /// A request, not a guarantee: not every instance implements it (see
    /// [`require_revision`]), so this rides alongside a local check rather
    /// than standing in for one.
    pub pinned: Option<u64>,
}

/// A revision the caller asserted, and where they asserted it.
///
/// The origin is carried because it is the only difference between the two
/// refusals a mismatch can produce. "Not the 16 given to --if-revision" is
/// wrong - and confusing - for someone who never typed that flag and is
/// writing back an exported file, and the remedy differs too: re-read, or
/// re-export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Pin {
    /// The revision the caller's copy was taken from.
    pub revision: u64,
    /// How that number arrived.
    pub origin: PinSource,
}

/// Where a pinned revision came from.
///
/// Named for the pin rather than just `Origin`, because `content::Origin`
/// already means "where the body came from" in this module tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PinSource {
    /// `--if-revision N`, typed by the caller.
    Flag,
    /// The `revision` recorded in an exported file's frontmatter.
    File,
}

impl PinSource {
    /// How the refusal names this number.
    fn describes(self) -> &'static str {
        match self {
            Self::Flag => "given to --if-revision",
            Self::File => "recorded in the file's `revision`",
        }
    }

    /// What to do about a mismatch.
    fn remedy(self, id: &str) -> String {
        match self {
            Self::Flag => format!(
                "Read it again with `otl docs view {} --outline --json` and \
                 redo the edit.",
                text::quote(id)
            ),
            Self::File => "Export it again, reapply your edits to the fresh \
                           copy, and write that back - or pass --force to \
                           overwrite the newer version with this one."
                .to_string(),
        }
    }
}

/// Read the document's current markdown body and revision.
///
/// This is also where a [`Pin`] is enforced. Checking it locally rather
/// than leaving it to `lastRevision` alone is deliberate, and the reason is
/// stronger than it first looks - see [`require_revision`].
pub(super) fn current_body(
    session: &Session,
    id: &str,
    pin: Option<Pin>,
) -> Result<(String, Option<u64>), CliError> {
    let document = session.call_data(READ_OPERATION, &[("id".to_string(), id.to_string())])?;
    let current = document.pointer("/revision").and_then(Value::as_u64);
    if let Some(pin) = pin {
        check_revision(id, pin, current)?;
    }
    let text = fields::string_at(&document, "/text").ok_or_else(|| {
        CliError::failure(anyhow!(
            "the server returned no markdown body for this document, so there \
             is nothing to locate this edit in. Replace the whole body \
             instead, by piping it without --section or --mode patch."
        ))
    })?;
    Ok((text.to_string(), current))
}

/// Enforce `--if-revision` for a write that does not otherwise read the
/// document.
///
/// # Why this exists rather than trusting `lastRevision`
///
/// The spec declares `lastRevision` and documents it as rejecting a stale
/// write with HTTP 409. Real instances do not all implement it: on one
/// tested Outline, `--if-revision 999` against a document at revision 17
/// was accepted and written. The write that has the most to lose is
/// exactly the one that used to rely on it - a whole-body replace, where
/// the local copy overwrites the page outright - so a promise this CLI
/// makes in its own help cannot be delegated to a field the server may
/// ignore.
///
/// So every `--if-revision` is checked HERE, against a revision this CLI
/// read. `lastRevision` is still sent: on a server that honours it, it
/// closes the window between this read and the write, which nothing local
/// can. Neither mechanism is load-bearing alone; the local one is the one
/// that is always there.
///
/// Costs one `documents.info`, and only when the caller (or an exported
/// file's frontmatter) actually asserted a revision.
pub(super) fn require_revision(
    session: &Session,
    id: &str,
    pin: Option<Pin>,
) -> Result<(), CliError> {
    let Some(pin) = pin else {
        return Ok(());
    };
    let document = session.call_data(READ_OPERATION, &[("id".to_string(), id.to_string())])?;
    check_revision(
        id,
        pin,
        document.pointer("/revision").and_then(Value::as_u64),
    )
}

/// Refuse the write when the caller's revision is not the current one.
///
/// A server that reports no revision at all is also a refusal: the caller
/// asked for a guarantee, and "could not check" is not that guarantee. On a
/// write that replaces a whole page, quietly proceeding would be the worst
/// of the three outcomes.
fn check_revision(id: &str, pin: Pin, current: Option<u64>) -> Result<(), CliError> {
    let Pin { revision, origin } = pin;
    match current {
        Some(found) if found == revision => Ok(()),
        Some(found) => Err(CliError::usage(anyhow!(
            "this document is at revision {found}, not the {revision} {}: it \
             changed since your copy was read, so the edit was written \
             against an older version and nothing was sent. {}",
            origin.describes(),
            origin.remedy(id)
        ))),
        None => Err(CliError::usage(anyhow!(
            "the revision {revision} {} cannot be checked: the server did not \
             report a revision for this document, so nothing was sent.",
            origin.describes()
        ))),
    }
}

/// Turn a section edit into the smallest write that expresses it.
///
/// `replacement` is `None` for a deletion, which is the one case where an
/// empty result is the intent rather than an accident.
pub(super) fn section_plan(
    text: &str,
    pinned: Option<u64>,
    flag: &str,
    address: &str,
    replacement: Option<&str>,
) -> Result<Plan, CliError> {
    let found = section::headings(text);
    let heading = section::resolve(&found, address).map_err(|error| {
        CliError::usage(anyhow!(section::unresolved_message(
            flag, address, &found, &error
        )))
    })?;
    let args = match replacement {
        Some(body) => section::replace(text, heading, body),
        None => section::delete(text, heading),
    };
    Ok(Plan {
        args: edit_args(args, flag, address)?,
        pinned,
    })
}

/// The `key=value` pairs one [`Edit`] becomes.
fn edit_args(edit: Edit, flag: &str, address: &str) -> Result<Vec<(String, String)>, CliError> {
    match edit {
        Edit::Patch {
            find_text,
            replacement,
        } => Ok(vec![
            ("text".to_string(), replacement),
            ("editMode".to_string(), PATCH_MODE.to_string()),
            ("findText".to_string(), find_text),
        ]),
        Edit::Replace { text: whole } => {
            // Removing the only section of a page empties it. That is the
            // one outcome the blank-body guard exists for, and a section
            // address is not an explicit enough way to ask for it.
            if whole.trim().is_empty() {
                return Err(CliError::usage(anyhow!(
                    "{flag} {address:?} covers the whole document, so this \
                     would leave it empty; refusing to erase its contents \
                     (use `otl api documents.update id=<id> text=` if that is \
                     really the intent)"
                )));
            }
            stdio::write_diagnostic_line(&format!(
                "warning: no anchor shorter than the whole document was unique, \
                 so the full body ({} bytes) was sent instead of a patch",
                whole.len()
            ));
            Ok(vec![("text".to_string(), whole)])
        }
    }
}

/// The arguments for a verified `--mode patch`.
pub(super) fn patch_plan(body: String, anchor: String, pinned: Option<u64>) -> Plan {
    Plan {
        args: vec![
            ("text".to_string(), body),
            ("editMode".to_string(), PATCH_MODE.to_string()),
            ("findText".to_string(), anchor),
        ],
        pinned,
    }
}

/// Refuse a `--find-text` anchor that does not occur exactly once.
pub(super) fn verify(text: &str, anchor: &str) -> Result<(), CliError> {
    let found = section::headings(text);
    let located = section::locate(text, anchor, &found, MAX_REPORTED + 1);
    match located.len() {
        1 => Ok(()),
        0 => Err(CliError::usage(anyhow!(
            "--find-text does not occur in this document, so the patch would \
             change nothing and nothing was sent. Copy the anchor from the \
             current text (`otl docs view <id> --raw`, or one section with \
             `--section <HEADING>`)."
        ))),
        _ => {
            let count = if located.len() > MAX_REPORTED {
                format!("more than {MAX_REPORTED}")
            } else {
                located.len().to_string()
            };
            Err(CliError::usage(anyhow!(
                "--find-text occurs in {count} places in this document, and the \
                 server would replace one of them without saying which, so \
                 nothing was sent:\n{}\n\
                 Extend the anchor until it is unique, or name the section \
                 instead - `otl docs update <id> --section '<heading>'` derives \
                 a unique anchor for you.",
                positions(&located)
            )))
        }
    }
}

/// One line per anchor position: where it is, and the section it is in.
fn positions(located: &[section::Location]) -> String {
    located
        .iter()
        .take(MAX_REPORTED)
        .map(|at| {
            stdio::scrub_to_one_line(&format!(
                "  L{:<5} {}",
                at.line,
                at.section
                    .as_deref()
                    .unwrap_or("(before the first heading)")
            ))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Explain a rejection that the revision pin is the likely cause of.
///
/// Outline answers a `lastRevision` mismatch with HTTP 409, which the shared
/// mapping classifies as "request rejected" (exit 3) - correct, and not
/// self-explanatory. The generic mapping is deliberately not changed: 409
/// means other things on other operations, and only this call site knows it
/// pinned a revision.
pub(super) fn explain_conflict(id: &str, pinned: Option<u64>, error: CliError) -> CliError {
    let Some(revision) = pinned else {
        return error;
    };
    if error.code != ExitCode::ApiRequest || !error.to_string().contains("409") {
        return error;
    }
    CliError::new(
        error.code,
        anyhow!(
            "{error}\nThis write was pinned to revision {revision} and the \
             document has moved past it, so nothing was changed. Read it again \
             with `otl docs view {} --outline --json` and redo the edit against \
             the new revision.",
            text::quote(id)
        ),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const BODY: &str = "intro\n\n## Deploy\n\nsteps\n\n### Rollback\n\nundo\n\n## FAQ\n\nanswers\n";

    fn value<'a>(args: &'a [(String, String)], key: &str) -> Option<&'a str> {
        args.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn plan(address: &str, replacement: Option<&str>) -> Result<Plan, CliError> {
        section_plan(BODY, Some(12), "--section", address, replacement)
    }

    #[test]
    fn a_section_replacement_becomes_a_pinned_patch() {
        let planned = plan("FAQ", Some("## FAQ\n\nnew answers")).unwrap();
        assert_eq!(value(&planned.args, "editMode"), Some("patch"));
        assert_eq!(
            value(&planned.args, "findText"),
            Some("## FAQ\n\nanswers\n")
        );
        assert_eq!(
            value(&planned.args, "text"),
            Some("## FAQ\n\nnew answers\n")
        );
        assert_eq!(planned.pinned, Some(12));
    }

    /// The size claim the whole feature rests on: what goes on the wire is
    /// the section, not the page.
    ///
    /// The bulk sits in a section that is NOT the one being edited, which is
    /// the case that matters - a section runs to the next heading of its own
    /// level or higher, so the LAST section of a page legitimately contains
    /// everything after it.
    #[test]
    fn a_section_patch_does_not_carry_the_rest_of_the_page() {
        let big = format!(
            "## Bulk\n\n{}\n## FAQ\n\nanswers\n",
            "filler line\n".repeat(4000)
        );
        let planned =
            section_plan(&big, Some(1), "--section", "FAQ", Some("## FAQ\n\nnew")).unwrap();
        let sent: usize = planned.args.iter().map(|(_, value)| value.len()).sum();
        assert!(
            sent * 20 < big.len(),
            "sent {sent} bytes for a {} byte document",
            big.len()
        );
    }

    #[test]
    fn a_section_deletion_sends_an_empty_replacement() {
        let planned = plan("Deploy > Rollback", None).unwrap();
        assert_eq!(value(&planned.args, "editMode"), Some("patch"));
        assert_eq!(value(&planned.args, "text"), Some(""));
        assert_eq!(
            value(&planned.args, "findText"),
            Some("### Rollback\n\nundo\n\n")
        );
    }

    /// A section spanning the whole page cannot be anchored inside it, so it
    /// is sent as a plain body - one `text`, no `editMode`, still pinned.
    #[test]
    fn a_section_that_is_the_whole_page_is_sent_whole_rather_than_doubled() {
        let planned = section_plan(
            "## Only\n\nbody\n",
            Some(3),
            "--section",
            "Only",
            Some("## Only\n\nnew"),
        )
        .unwrap();
        assert_eq!(value(&planned.args, "text"), Some("## Only\n\nnew\n"));
        assert_eq!(value(&planned.args, "editMode"), None);
        assert_eq!(value(&planned.args, "findText"), None);
        assert_eq!(planned.pinned, Some(3));
    }

    #[test]
    fn an_address_that_names_no_section_lists_the_ones_that_exist() {
        let error = plan("Nonexistent", Some("x")).unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
        let message = error.to_string();
        for address in ["Deploy", "Deploy > Rollback", "FAQ"] {
            assert!(
                message.contains(address),
                "{address} missing from:\n{message}"
            );
        }
    }

    #[test]
    fn an_ambiguous_address_is_refused_with_every_match() {
        let text = "## Notes\n\nfirst\n\n## Other\n\nx\n\n## Notes\n\nsecond\n";
        let error = section_plan(text, None, "--section", "Notes", Some("x")).unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
        let message = error.to_string();
        assert!(message.contains("matches 2 headings"), "{message}");
        assert!(
            message.contains("L1") && message.contains("L9"),
            "{message}"
        );
    }

    /// Deleting the only section empties the page, which is what the
    /// blank-body guard is for.
    #[test]
    fn deleting_the_whole_document_is_refused() {
        let error =
            section_plan("## Only\n\nbody\n", None, "--delete-section", "Only", None).unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
        assert!(error.to_string().contains("erase"), "{error}");
    }

    #[test]
    fn a_unique_anchor_passes_verification_and_a_missing_one_does_not() {
        assert!(verify(BODY, "## FAQ").is_ok());
        assert!(verify(BODY, "steps").is_ok());

        let error = verify(BODY, "not in here").unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
        assert!(error.to_string().contains("does not occur"), "{error}");
    }

    /// The refusal has to say WHERE, and name the section each hit is in -
    /// that is the whole reason verifying is worth a request.
    #[test]
    fn a_repeated_anchor_is_reported_with_its_enclosing_sections() {
        let text = "## Deploy\n\nrestart it\n\n## Rollback\n\nrestart it\n";
        let error = verify(text, "restart it").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("occurs in 2 places"), "{message}");
        assert!(message.contains("Deploy"), "{message}");
        assert!(message.contains("Rollback"), "{message}");
        assert!(message.contains("--section"), "{message}");
    }

    #[test]
    fn a_match_outside_any_section_is_still_located() {
        let error = verify("intro\n\nintro\n\n## A\n\nb\n", "intro").unwrap_err();
        assert!(
            error.to_string().contains("before the first heading"),
            "{error}"
        );
    }

    #[test]
    fn a_hostile_heading_cannot_forge_a_line_in_an_anchor_refusal() {
        let text = "## evil\u{1b}[31m\n\nsame\n\n## other\n\nsame\n";
        let error = verify(text, "same").unwrap_err();
        assert!(!error.to_string().contains('\u{1b}'), "{error}");
    }

    fn pin(revision: u64, origin: PinSource) -> Pin {
        Pin { revision, origin }
    }

    #[test]
    fn a_stale_caller_revision_is_refused_before_anything_is_sent() {
        let error = check_revision("doc-1", pin(11, PinSource::Flag), Some(12)).unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
        let message = error.to_string();
        assert!(message.contains("revision 12, not the 11"), "{message}");
        assert!(message.contains("--outline"), "{message}");

        assert!(check_revision("doc-1", pin(12, PinSource::Flag), Some(12)).is_ok());

        // A server that reports no revision cannot answer the question, and
        // guessing "it is probably fine" is the wrong direction.
        let error = check_revision("doc-1", pin(12, PinSource::Flag), None).unwrap_err();
        assert_eq!(error.code, ExitCode::Usage);
        assert!(error.to_string().contains("cannot be checked"), "{error}");
    }

    #[test]
    fn the_refusal_names_where_the_revision_came_from() {
        // A caller writing back an exported file never typed
        // --if-revision, so naming that flag would send them looking for
        // an argument they did not pass - and the remedy is different.
        let flag = check_revision("doc-1", pin(11, PinSource::Flag), Some(12))
            .unwrap_err()
            .to_string();
        assert!(flag.contains("--if-revision"), "{flag}");
        assert!(flag.contains("Read it again"), "{flag}");
        assert!(!flag.contains("--force"), "{flag}");

        let file = check_revision("doc-1", pin(11, PinSource::File), Some(12))
            .unwrap_err()
            .to_string();
        assert!(file.contains("recorded in the file"), "{file}");
        assert!(file.contains("Export it again"), "{file}");
        assert!(file.contains("--force"), "{file}");
        assert!(!file.contains("--if-revision"), "{file}");
    }

    #[test]
    fn a_conflict_is_explained_only_when_a_revision_was_pinned() {
        let rejected = || {
            CliError::new(
                ExitCode::ApiRequest,
                anyhow!("request rejected (HTTP 409): revision mismatch"),
            )
        };
        let explained = explain_conflict("doc-1", Some(12), rejected());
        assert!(explained.to_string().contains("revision 12"), "{explained}");
        assert_eq!(explained.code, ExitCode::ApiRequest);

        // Unpinned, so the 409 was about something else and is left alone.
        let untouched = explain_conflict("doc-1", None, rejected());
        assert!(!untouched.to_string().contains("pinned"), "{untouched}");

        // A different rejection is not reinterpreted as a conflict.
        let other = CliError::new(
            ExitCode::ApiRequest,
            anyhow!("request rejected (HTTP 400): bad input"),
        );
        let untouched = explain_conflict("doc-1", Some(12), other);
        assert!(!untouched.to_string().contains("pinned"), "{untouched}");
    }

    #[test]
    fn a_verified_patch_carries_the_anchor_and_the_pin() {
        let planned = patch_plan("new".to_string(), "old".to_string(), Some(4));
        assert_eq!(value(&planned.args, "text"), Some("new"));
        assert_eq!(value(&planned.args, "editMode"), Some("patch"));
        assert_eq!(value(&planned.args, "findText"), Some("old"));
        assert_eq!(planned.pinned, Some(4));
    }
}
