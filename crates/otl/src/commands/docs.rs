//! `otl docs ...` - the curated document commands.
//!
//! Everything here is a thin, opinionated shell over the generic engine:
//! flags are turned into `key=value` arguments for a compiled operation,
//! sent through the one request channel, and rendered by picking fields
//! (see [`crate::fields`]). There is no bespoke HTTP, no bespoke
//! pagination, and no bespoke table code in this module tree.
//!
//! Unlike `otl api`, these flags and their output ARE a stable contract
//! (semver), which is why the surface is deliberately small.

mod anchor;
mod content;
mod create;
mod detail;
mod dir;
mod export;
mod lifecycle;
mod list;
mod outdir;
mod search;
mod section;
mod target;
mod tree;
mod update;
mod view;

use clap::{Args, Subcommand};

use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;

/// Arguments for `otl docs`.
#[derive(Debug, Args)]
pub struct DocsArgs {
    #[command(subcommand)]
    command: DocsCommand,
}

/// The curated document subcommands.
#[derive(Debug, Subcommand)]
enum DocsCommand {
    /// List recent documents, or search when a query is supplied.
    List(list::ListArgs),
    /// Search documents by full-text query.
    Search(search::SearchArgs),
    /// Print a document's markdown.
    View(view::ViewArgs),
    /// Create a document from stdin or a file.
    Create(create::CreateArgs),
    /// Update a document's title and/or body.
    Update(update::UpdateArgs),
    /// Export a whole collection to local markdown files.
    Export(export::ExportArgs),
    /// Move or reorder a document.
    Move(lifecycle::MoveArgs),
    /// Move a document to trash, or archive it.
    Delete(lifecycle::DeleteArgs),
}

/// Run the requested `otl docs` subcommand.
///
/// `json_requested` is the literal `--json` flag, which only `docs view`
/// needs: its default output is the document's markdown, so a pipe gets
/// markdown and JSON has to be asked for explicitly. Every other
/// subcommand follows the usual dual-state rule, already encoded in `mode`.
pub fn run(
    args: &DocsArgs,
    mode: OutputMode,
    json_requested: bool,
    overrides: &Overrides,
) -> Result<(), CliError> {
    match &args.command {
        DocsCommand::List(args) => list::run(args, mode, overrides),
        DocsCommand::Search(args) => search::run(args, mode, overrides),
        DocsCommand::View(args) => view::run(args, mode, json_requested, overrides),
        DocsCommand::Create(args) => create::run(args, mode, overrides),
        DocsCommand::Update(args) => update::run(args, mode, overrides),
        DocsCommand::Export(args) => export::run(args, mode, overrides),
        DocsCommand::Move(args) => lifecycle::move_document(args, mode, overrides),
        DocsCommand::Delete(args) => lifecycle::delete_document(args, mode, overrides),
    }
}
