//! `otl` binary entry point.

#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

use otl::commands::api::{self, ApiArgs};
use otl::commands::auth::{self, AuthArgs};
use otl::commands::collections::{self, CollectionsArgs};
use otl::commands::completions::{self, CompletionsArgs};
use otl::commands::docs::{self, DocsArgs};
use otl::commands::doctor::{self, DoctorArgs};
use otl::commands::spec::{self, SpecArgs};
use otl::config::Overrides;
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

    /// Named profile from the user config file (env: OUTLINE_PROFILE).
    #[arg(long, global = true, value_name = "NAME")]
    profile: Option<String>,

    /// Outline instance base URL, overriding the profile (env: OUTLINE_URL).
    #[arg(long, global = true, value_name = "URL")]
    url: Option<String>,

    /// User config file to read (env: OUTLINE_CONFIG).
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

impl Cli {
    /// The command-line layer of the configuration, which outranks the
    /// environment and the config file key by key.
    fn overrides(&self) -> Overrides {
        Overrides {
            profile: self.profile.clone(),
            url: self.url.clone(),
            config_path: self.config.clone(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Call any API operation by name (output format unstable).
    Api(ApiArgs),
    /// Sign in, sign out, and inspect stored credentials.
    Auth(AuthArgs),
    /// Work with documents: search, view, create, update, export.
    Docs(DocsArgs),
    /// Work with collections.
    Collections(CollectionsArgs),
    /// Manage the OpenAPI spec this CLI dispatches from.
    Spec(SpecArgs),
    /// Check this environment: credentials, instance, and spec drift.
    Doctor(DoctorArgs),
    /// Print a shell completion script (bash, zsh, fish, powershell, elvish).
    Completions(CompletionsArgs),
}

fn main() -> std::process::ExitCode {
    // clap itself exits with code 2 on usage errors, matching docs/exit-codes.md.
    let cli = Cli::parse();
    let mode = render::resolve_mode(cli.json, std::io::stdout().is_terminal());
    let result = match &cli.command {
        Command::Api(args) => api::run(args, mode, &cli.overrides()),
        Command::Auth(args) => auth::run(args, mode, &cli.overrides()),
        Command::Docs(args) => docs::run(args, mode, cli.json, &cli.overrides()),
        Command::Collections(args) => collections::run(args, mode, &cli.overrides()),
        Command::Spec(args) => spec::run(args, mode),
        Command::Doctor(args) => doctor::run(args, mode, &cli.overrides()),
        Command::Completions(args) => completions::run(args, Cli::command()),
    };
    match result {
        Ok(()) => ExitCode::Success.into(),
        Err(error) => {
            stdio::write_diagnostic_line(&format!("error: {error}"));
            error.code.into()
        }
    }
}
