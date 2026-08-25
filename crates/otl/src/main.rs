//! `otl` binary entry point.

#![forbid(unsafe_code)]

use std::io::IsTerminal;

use clap::{Parser, Subcommand};

use otl::commands::api::{self, ApiArgs};
use otl::commands::collections::{self, CollectionsArgs};
use otl::commands::docs::{self, DocsArgs};
use otl::exit::ExitCode;
use otl::render;
use otl::stdio;

/// Outline CLI: work with your Outline knowledge base from the terminal.
#[derive(Debug, Parser)]
#[command(name = "otl", version, about)]
struct Cli {
    /// Print raw JSON (the default whenever stdout is not a terminal).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Call any API operation by name (output format unstable).
    Api(ApiArgs),
    /// Work with documents: search, view, create, update, export.
    Docs(DocsArgs),
    /// Work with collections.
    Collections(CollectionsArgs),
}

fn main() -> std::process::ExitCode {
    // clap itself exits with code 2 on usage errors, matching docs/exit-codes.md.
    let cli = Cli::parse();
    let mode = render::resolve_mode(cli.json, std::io::stdout().is_terminal());
    let result = match &cli.command {
        Command::Api(args) => api::run(args, mode),
        Command::Docs(args) => docs::run(args, mode, cli.json),
        Command::Collections(args) => collections::run(args, mode),
    };
    match result {
        Ok(()) => ExitCode::Success.into(),
        Err(error) => {
            stdio::write_diagnostic_line(&format!("error: {error}"));
            error.code.into()
        }
    }
}
