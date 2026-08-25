//! `otl completions <shell>` - print a shell completion script.
//!
//! The script is generated from the same clap command tree the binary parses
//! with, augmented with the operation names from the compiled IR table. So
//! subcommands, flags and `otl api` operation names all complete, and the
//! operation list can never drift from what the binary can actually call.
//!
//! The augmentation is applied to a CLONE of the command used for generation
//! only: the real parser keeps accepting any operation name so that an
//! unknown one still produces `otl`'s own error message (with the
//! `otl api list` hint) instead of a clap usage error.
//!
//! Output is pipe-safe: the script goes to stdout, everything else to
//! stderr, and no `print!` is used (a closed pipe must not panic).
//!
//! Per-shell coverage of the operation names is bounded by what the
//! upstream generators can express:
//!
//! - bash, zsh: candidates for positional arguments are emitted natively.
//! - fish: the generator "currently only supports named options
//!   (-o/--option), not positional arguments" (its own doc comment), so the
//!   operation candidates are appended as plain `complete` rules that reuse
//!   the generator's condition helper.
//! - powershell, elvish: those generators emit flags and subcommands only,
//!   for any positional value (they do not complete `otl completions
//!   <shell>` either). Subcommands and flags complete; operation names do
//!   not.

use std::fmt::Write as _;

use anyhow::anyhow;
use clap::{builder::PossibleValuesParser, Args, Command};
use clap_complete::Shell;

use crate::exit::CliError;
use crate::ops;
use crate::stdio;

/// Name of the `api` subcommand whose operation positional is completed
/// from the IR.
const API_SUBCOMMAND: &str = "api";
/// Id of that subcommand's operation positional.
const OPERATION_ARG: &str = "operation";
/// Reserved operation name that enumerates operations instead of calling
/// one; it completes alongside the real names.
const LIST_OPERATION: &str = "list";
/// Summary shown for [`LIST_OPERATION`] in shells that display one.
const LIST_SUMMARY: &str = "List every callable operation";

/// Arguments for `otl completions`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate a completion script for.
    #[arg(value_enum)]
    pub shell: Shell,
}

/// Print the completion script for the requested shell.
///
/// Purely local: needs no configuration, touches no network.
pub fn run(cmd: &CompletionsArgs, root: Command) -> Result<(), CliError> {
    let mut root = with_ir_operations(root);
    let name = root.get_name().to_string();
    let mut buffer: Vec<u8> = Vec::new();
    clap_complete::generate(cmd.shell, &mut root, &name, &mut buffer);
    let mut script = String::from_utf8(buffer).map_err(|_| {
        CliError::failure(anyhow!(
            "the generated completion script is not valid UTF-8"
        ))
    })?;
    if cmd.shell == Shell::Fish {
        let rules = fish_operation_rules(&script, &name);
        script.push_str(&rules);
    }
    stdio::write_data(&script)
}

/// Operation names offered as completion candidates, with their summaries.
fn operation_candidates() -> impl Iterator<Item = (&'static str, &'static str)> {
    std::iter::once((LIST_OPERATION, LIST_SUMMARY)).chain(
        ops::OPS
            .iter()
            .map(|op| (op.name.as_ref(), op.summary.as_ref())),
    )
}

/// `complete` rules adding the IR operation names to fish's `otl api`
/// candidates.
///
/// The fish generator emits nothing for positional arguments, so the rules
/// are appended here, reusing the condition helper the generator itself
/// defined. If that helper is ever renamed upstream, nothing is appended:
/// a rule whose condition never fires would be worse than none, and the
/// completion tests assert the rules are present, so the change surfaces.
fn fish_operation_rules(script: &str, name: &str) -> String {
    // Same escaping rule the fish generator applies to command names.
    let condition = format!(
        "__fish_{}_using_subcommand {API_SUBCOMMAND}",
        name.replace('-', "_")
    );
    if !script.contains(&condition) {
        return String::new();
    }
    let mut out = String::from(
        "\n# Operation candidates from the compiled IR table (the fish\n\
         # generator does not emit candidates for positional arguments).\n",
    );
    for (operation, summary) in operation_candidates() {
        let _ = writeln!(
            out,
            "complete -c {name} -n \"{condition}\" -f -a \"{operation}\" -d '{}'",
            fish_escape(summary)
        );
    }
    out
}

/// Escape a description for a fish single-quoted string.
fn fish_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Attach the IR's operation names to `api`'s operation positional, so the
/// generated script offers them as completion candidates.
///
/// Returns the command unchanged when the expected argument is absent, so a
/// future command-tree change degrades to fewer candidates rather than a
/// panic.
fn with_ir_operations(root: Command) -> Command {
    if root
        .find_subcommand(API_SUBCOMMAND)
        .and_then(|api| {
            api.get_arguments()
                .find(|arg| arg.get_id() == OPERATION_ARG)
        })
        .is_none()
    {
        return root;
    }
    let candidates: Vec<&'static str> = operation_candidates()
        .map(|(operation, _)| operation)
        .collect();
    // `mut_args`, not `mut_arg`: the latter re-appends the argument and so
    // renumbers the positionals, which trips clap's own debug assertions.
    root.mut_subcommand(API_SUBCOMMAND, |api| {
        api.mut_args(|arg| {
            if arg.get_id() != OPERATION_ARG {
                return arg;
            }
            // Not `hide_possible_values`: that flag also removes the values
            // from the generated script, which is the whole point here. The
            // real command tree is untouched, so `otl api --help` stays
            // free of the operation dump.
            arg.value_parser(PossibleValuesParser::new(candidates.clone()))
        })
    })
}
