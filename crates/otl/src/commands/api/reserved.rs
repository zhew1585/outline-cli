//! What the first positional of `otl api` means.
//!
//! `otl api` takes an operation NAME, so every local sub-feature it grows
//! has to live somewhere in that same namespace. Two words are reserved:
//! `list` (Story 1.2) and `describe`.
//!
//! # Why reserved words rather than clap subcommands
//!
//! A clap subcommand would occupy exactly the same namespace - `otl api
//! describe` cannot simultaneously be a subcommand and an operation called
//! `describe` - so it buys no safety, and it costs the grammar that makes
//! the current one work: the operation positional is required, and a struct
//! that has both a required positional and subcommands needs
//! `subcommand_negates_reqs` plus `args_conflicts_with_subcommands` before
//! it parses at all. The reserved word is the smaller mechanism for the
//! same collision, and `list` already established it.
//!
//! # What happens if a spec declares one of those names
//!
//! It cannot happen with the vendored document (all 113 operation names are
//! `resource.method`, and `api_reserved_words` pins that), but a synced
//! table is compiled from a document this CLI did not write:
//! `spec_compile::is_safe_op_name` accepts any run of ASCII letters,
//! digits, `.`, `_` and `-`, so a document with a `/describe` path compiles
//! to an operation named `describe`.
//!
//! The reserved word wins - discovery must not disappear because a server
//! chose a name - but it does not win SILENTLY. [`warn_if_shadowed`] says
//! on stderr that the operation exists and is unreachable here. A silent
//! shadow is the same failure this whole story is about: a caller that
//! believes it got an answer about the thing it asked for.

use anyhow::anyhow;
use clap::Command;
use engine::OpSpec;

use super::{describe, list, ApiArgs, DESCRIBE_OPERATION, LIST_OPERATION};
use crate::exit::CliError;
use crate::ops;
use crate::render::OutputMode;
use crate::stdio;

/// Name of the `api` subcommand inside the root clap command.
const API_SUBCOMMAND: &str = "api";

/// What is left to do after the local paths have had their turn.
pub(super) enum Next {
    /// Handled here, locally; nothing was sent and nothing remains.
    Done,
    /// A real operation the caller wants called.
    Call(&'static OpSpec),
}

/// Resolve one invocation of `otl api` to either a local answer or an
/// operation to call.
///
/// Ordering matters and is asserted by tests: `--help` is answered before
/// the operation name is looked up (so `otl api documents.info --help`
/// describes rather than calls), and every local path returns before
/// [`crate::auth::client`] would be built.
pub(super) fn dispatch(
    cmd: &ApiArgs,
    mode: OutputMode,
    root: fn() -> Command,
) -> Result<Next, CliError> {
    let Some(operation) = cmd.operation.as_deref() else {
        // clap's `required_unless_present = "help"` leaves the operation
        // open in exactly one case, which is this one.
        return print_command_help(root).map(|()| Next::Done);
    };
    if cmd.help {
        return help_for(operation, mode, root).map(|()| Next::Done);
    }
    match operation {
        LIST_OPERATION => {
            warn_if_shadowed(operation);
            reject_request_flags(cmd, operation)?;
            reject_extra_arguments(cmd, operation, 0)?;
            list::run(mode).map(|()| Next::Done)
        }
        DESCRIBE_OPERATION => {
            warn_if_shadowed(operation);
            reject_request_flags(cmd, operation)?;
            reject_extra_arguments(cmd, operation, 1)?;
            describe::run(find(subject(cmd)?)?, mode).map(|()| Next::Done)
        }
        _ => find(operation).map(Next::Call),
    }
}

/// Answer `--help`.
///
/// A named operation gets ITS contract - the same text `otl api describe`
/// prints, in the same two output states - because that is what the flag
/// was asked about. Anything else (no operation, or a reserved word, which
/// names no operation) gets the command's own help.
///
/// An unknown operation is deliberately an ERROR here rather than a
/// fallback to the generic help: printing authoritative-looking text about
/// something the caller did not ask about is the bug this replaces.
fn help_for(operation: &str, mode: OutputMode, root: fn() -> Command) -> Result<(), CliError> {
    if operation == LIST_OPERATION || operation == DESCRIBE_OPERATION {
        return print_command_help(root);
    }
    describe::run(find(operation)?, mode)
}

/// Render `otl api --help` from the real command tree.
///
/// Built rather than borrowed as declared: [`Command::build`] is what
/// propagates the global flags (`--json`, `--profile`, `--url`, `--config`)
/// down into the subcommand, so this prints the same text clap printed
/// before the help flag was taken over, instead of a second copy that would
/// drift.
fn print_command_help(root: fn() -> Command) -> Result<(), CliError> {
    let mut command = root();
    command.build();
    let rendered = command
        .find_subcommand_mut(API_SUBCOMMAND)
        .map(Command::render_long_help)
        .ok_or_else(|| {
            CliError::failure(anyhow!(
                "internal error: no `{API_SUBCOMMAND}` subcommand to describe"
            ))
        })?;
    stdio::write_data(&rendered.to_string())
}

/// The operation `otl api describe <operation>` is about.
fn subject(cmd: &ApiArgs) -> Result<&str, CliError> {
    cmd.args.first().map(String::as_str).ok_or_else(|| {
        CliError::usage(anyhow!(
            "`otl api {DESCRIBE_OPERATION}` needs an operation to describe, \
             e.g. `otl api {DESCRIBE_OPERATION} documents.info` \
             (run `otl api {LIST_OPERATION}` to see all operations)"
        ))
    })
}

/// Look up an operation in the effective table.
///
/// The message names the CONVENTION and the way to enumerate, because the
/// two mistakes it answers are "I guessed the name" and "I do not know what
/// exists". It never suggests the closest match: a near-miss on an
/// operation name is a request sent somewhere the caller did not intend.
fn find(operation: &str) -> Result<&'static OpSpec, CliError> {
    ops::find(operation).ok_or_else(|| {
        CliError::usage(anyhow!(
            "unknown API operation {operation:?}; operation names follow the \
             `resource.method` form, e.g. `documents.info` (run `otl api \
             {LIST_OPERATION}` to see all operations, or `otl api \
             {DESCRIBE_OPERATION} <operation>` for one operation's parameters)"
        ))
    })
}

/// Say so when the effective spec declares an operation by a reserved name.
///
/// Purely local and free on the normal path: only the two reserved words
/// ever ask, so a call to `documents.info` does not pay for this lookup.
fn warn_if_shadowed(word: &str) {
    if ops::find(word).is_none() {
        return;
    }
    stdio::write_diagnostic_line(&format!(
        "warning: the effective spec declares an operation named {word:?}, and \
         `otl api {word}` cannot call it: that word is reserved for this CLI's \
         own discovery command"
    ));
}

/// Refuse the request-shaping flags on a local path.
///
/// They describe a request that is not going to happen, so accepting them
/// would be accepting an instruction the command cannot carry out.
fn reject_request_flags(cmd: &ApiArgs, word: &str) -> Result<(), CliError> {
    let named = cmd.body.is_some() || cmd.show_server_message || cmd.no_validate;
    if !named && cmd.limit.is_none() {
        return Ok(());
    }
    Err(CliError::usage(anyhow!(
        "`otl api {word}` sends no request, so it takes no request flags \
         (--body, --show-server-message, --no-validate, --limit)"
    )))
}

/// Refuse more positionals than a local path can mean.
fn reject_extra_arguments(cmd: &ApiArgs, word: &str, allowed: usize) -> Result<(), CliError> {
    if cmd.args.len() <= allowed {
        return Ok(());
    }
    let expected = match allowed {
        0 => "no further arguments".to_string(),
        _ => format!("exactly {allowed} further argument"),
    };
    Err(CliError::usage(anyhow!(
        "`otl api {word}` takes {expected}, and {} were given",
        cmd.args.len()
    )))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Neither reserved word can shadow anything in the table this binary
    /// ships, and the reason is structural rather than lucky: an operation
    /// name is `resource.method`, and no reserved word contains a `.`.
    ///
    /// A synced table is not covered by this - the document is not ours -
    /// which is what [`warn_if_shadowed`] is for.
    #[test]
    fn api_reserved_words_name_no_built_in_operation() {
        for word in [LIST_OPERATION, DESCRIBE_OPERATION] {
            assert!(
                ops::find(word).is_none(),
                "the built-in table declares {word:?}, which `otl api {word}` cannot reach"
            );
        }
        let table = ops::table();
        assert!(!table.is_empty(), "an empty table would make this vacuous");
        for op in table {
            assert!(
                op.name.contains('.'),
                "{:?} is not in `resource.method` form, so a reserved word could \
                 collide with a name like it",
                op.name
            );
        }
    }
}
