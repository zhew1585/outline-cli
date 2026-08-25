//! `otl` binary entry point.

#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};

use otl::commands::api::{self, ApiArgs};
use otl::exit::ExitCode;

/// Outline CLI: work with your Outline knowledge base from the terminal.
#[derive(Debug, Parser)]
#[command(name = "otl", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Call any API operation by name (output format unstable).
    Api(ApiArgs),
}

fn main() -> std::process::ExitCode {
    // clap itself exits with code 2 on usage errors, matching docs/exit-codes.md.
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Api(args) => api::run(args),
    };
    match result {
        Ok(()) => ExitCode::Success.into(),
        Err(error) => {
            eprintln!("error: {error}");
            error.code.into()
        }
    }
}
