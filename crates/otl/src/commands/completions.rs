//! `otl completions <shell>` - print a shell completion script.
//!
//! The script is generated from the same clap command tree the binary parses
//! with, augmented with the operation names from the compiled IR table, so
//! nothing it offers can drift from what the binary can actually call.
//!
//! Subcommands and flag names complete in every supported shell. `otl api`
//! OPERATION names complete in bash, zsh, fish only - see the per-shell
//! coverage note below, and [`completes_operation_names`], which is the
//! single source the generated scripts, the CLI help and the tests all read.
//!
//! The augmentation is applied to a CLONE of the command used for generation
//! only: the real parser keeps accepting any operation name so that an
//! unknown one still produces `otl`'s own error message (with the
//! `otl api list` hint) instead of a clap usage error.
//!
//! Output is pipe-safe: the script goes to stdout, everything else to
//! stderr, and no `print!` is used (a closed pipe must not panic).
//!
//! Candidate text is CONSTRAINED, not trusted: every operation name is
//! checked against [`is_safe_operation_name`] before it is written into a
//! script, and every description has its control characters stripped. The
//! generated file is executable shell code, so a name carrying a quote,
//! `$(...)`, a backtick or a newline would be command substitution or quote
//! escape at completion time. The vendored spec is validated at build time
//! (`build.rs` rejects such an operation name outright), but a name reaching
//! the IR through a future `spec sync` cache would not be, and a filter here
//! is the last line before the text is executable.
//!
//! Per-shell coverage of the operation names is bounded by what the upstream
//! generators can express, and each script states its own coverage in a
//! header comment so an installed file is self-describing:
//!
//! - bash, zsh: candidates for positional arguments are emitted natively.
//! - fish: the generator "currently only supports named options
//!   (-o/--option), not positional arguments" (its own doc comment), so the
//!   operation candidates are appended as plain `complete` rules that reuse
//!   the generator's condition helper.
//! - powershell, elvish: those generators emit flags and subcommands only,
//!   for any positional value (they do not complete `otl completions
//!   <shell>` either). Subcommands and flags complete; operation names do
//!   not. Splicing candidates into their nested script structures would mean
//!   hand-writing shell code in two more dialects - the exact hazard the
//!   paragraph above is about - so the gap is reported instead.

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
/// The reserved operation names, with the summary shells that display one
/// will show. They complete alongside the real names, because from the
/// caller's side they are three things you can type in the same position.
///
/// The names come from `commands::api` rather than being spelled again
/// here: a copy would let the completion script offer a word the parser no
/// longer honours, or - worse - stop offering one it does.
const RESERVED_OPERATIONS: &[(&str, &str)] = &[
    (super::api::LIST_OPERATION, "List every callable operation"),
    (
        super::api::DESCRIBE_OPERATION,
        "Describe one operation's parameters and response",
    ),
];
/// Maximum length of a candidate description written into a script.
const MAX_DESCRIPTION_CHARS: usize = 120;
/// Comment marker for the coverage notice. `#` starts a line comment in
/// bash, zsh, fish, elvish and powershell alike.
const COMMENT: &str = "#";
/// The tag zsh's `compinit` looks for, on the first line and nowhere else.
const ZSH_COMPDEF_TAG: &str = "#compdef";

/// Arguments for `otl completions`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate a completion script for.
    ///
    /// All five shells complete subcommands and flag names. `api` operation
    /// names are completed for bash, zsh and fish only: the upstream
    /// generators for powershell and elvish emit no candidates for
    /// positional arguments. Each generated script repeats its own coverage
    /// in a header comment.
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
    let generated = String::from_utf8(buffer).map_err(|_| {
        CliError::failure(anyhow!(
            "the generated completion script is not valid UTF-8"
        ))
    })?;
    let mut script = with_coverage_notice(cmd.shell, &name, &generated);
    if cmd.shell == Shell::Fish {
        let rules = fish_operation_rules(&generated, &name);
        script.push_str(&rules);
    }
    stdio::write_data(&script)
}

/// Add the coverage notice without displacing a line the shell requires
/// first.
///
/// zsh's `compinit` scans `$fpath` and reads only the FIRST line of each
/// `_*` file, looking for the `#compdef` tag `clap_complete` emits there.
/// A comment above it is not a comment as far as zsh is concerned - it is
/// the absence of a tag, and the completion is silently never registered,
/// which is exactly what the documented `otl completions zsh > ~/.zfunc/_otl`
/// install produced. The notice therefore goes after that line, and
/// `the_zsh_script_starts_with_the_compdef_tag` pins the ordering.
///
/// No other supported shell reserves its first line, so they keep the notice
/// at the top where it is most visible.
fn with_coverage_notice(shell: Shell, name: &str, generated: &str) -> String {
    let notice = coverage_notice(shell, name);
    let Some(reserved) = reserved_first_line(shell, generated) else {
        return format!("{notice}{generated}");
    };
    let rest = &generated[reserved.len()..];
    format!(
        "{reserved}\n{notice}{}",
        rest.strip_prefix('\n').unwrap_or(rest)
    )
}

/// The line this shell requires to come first, if it has one and the
/// generator actually emitted it.
///
/// Returns `None` when the expectation does not hold, so an upstream change
/// degrades to the old layout rather than corrupting the script by splitting
/// it at the wrong place.
fn reserved_first_line(shell: Shell, generated: &str) -> Option<&str> {
    if shell != Shell::Zsh {
        return None;
    }
    let first = generated.lines().next()?;
    first.starts_with(ZSH_COMPDEF_TAG).then_some(first)
}

/// Header comment stating what this shell's script does and does not
/// complete, so an installed file never over-claims.
fn coverage_notice(shell: Shell, name: &str) -> String {
    let operations = completes_operation_names(shell);
    let detail = if operations {
        "subcommands, flags and `api` operation names (from the compiled \
         operation table)"
    } else {
        "subcommands and flags only: the upstream clap_complete generator for \
         this shell emits no candidates for positional arguments, so `api` \
         operation names are NOT completed here (use bash, zsh or fish for \
         those, or `otl api list`)"
    };
    format!("{COMMENT} {name} completion script for {shell}.\n{COMMENT} Completes {detail}.\n")
}

/// Whether this shell's generated script carries the operation names.
///
/// Public so the surface can be asserted in tests and reported by the
/// command's own help rather than only documented in prose.
pub fn completes_operation_names(shell: Shell) -> bool {
    matches!(shell, Shell::Bash | Shell::Zsh | Shell::Fish)
}

/// Operation names offered as completion candidates, with their summaries.
///
/// Names that are not [`is_safe_operation_name`] are dropped: a candidate
/// that cannot be written safely is worth less than a script that misbehaves.
fn operation_candidates() -> impl Iterator<Item = (&'static str, &'static str)> {
    RESERVED_OPERATIONS
        .iter()
        .copied()
        .chain(
            ops::OPS
                .iter()
                .map(|op| (op.name.as_ref(), op.summary.as_ref())),
        )
        .filter(|(name, _)| is_safe_operation_name(name))
}

/// Whether an operation name is safe to write into generated shell code.
///
/// The allowed set is exactly what an RPC operation name needs -
/// `resource.method`, with `-` and `_` tolerated - so nothing that any of
/// the five shells treats specially can appear: no quote, backslash,
/// backtick, `$`, parenthesis, brace, bracket, semicolon, pipe, redirect,
/// glob, whitespace or control character. Enforced as an allow-list, because
/// a deny-list would have to be right about five dialects at once.
pub fn is_safe_operation_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_DESCRIPTION_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
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
///
/// Backslash and quote are escaped as fish requires, and every character
/// [`crate::text::hazard`] flags is dropped: a summary comes from the spec,
/// and an ESC byte surviving into a completion description would be an escape
/// sequence the terminal renders when the candidate is displayed - as would a
/// bidi override, for exactly the same reason. The length is capped as a table
/// cell's is.
fn fish_escape(text: &str) -> String {
    text.chars()
        .filter(|c| crate::text::hazard(*c).is_none())
        .take(MAX_DESCRIPTION_CHARS)
        .flat_map(|c| match c {
            '\\' => vec!['\\', '\\'],
            '\'' => vec!['\\', '\''],
            other => vec![other],
        })
        .collect()
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
