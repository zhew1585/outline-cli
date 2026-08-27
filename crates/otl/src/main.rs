//! `otl` binary entry point.

#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

use otl::commands::api::{self, ApiArgs};
use otl::commands::attachments::{self, AttachmentsArgs};
use otl::commands::auth::{self, AuthArgs};
use otl::commands::collections::{self, CollectionsArgs};
use otl::commands::comments::{self, CommentsArgs};
use otl::commands::completions::{self, CompletionsArgs};
use otl::commands::docs::{self, DocsArgs};
use otl::commands::doctor::{self, DoctorArgs};
use otl::commands::fetch::{self, FetchArgs};
use otl::commands::skill::{self, SkillArgs};
use otl::commands::spec::{self, SpecArgs};
use otl::commands::users::{self, UsersArgs};
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
    /// Request a pre-signed attachment upload.
    Attachments(AttachmentsArgs),
    /// Sign in, sign out, and inspect stored credentials.
    Auth(AuthArgs),
    /// List, read, create, update, move, delete, and export documents.
    Docs(DocsArgs),
    /// Work with collections.
    Collections(CollectionsArgs),
    /// List, create, update, resolve, and delete comments.
    Comments(CommentsArgs),
    /// Fetch a document, collection, user, or attachment by ID or URL.
    Fetch(FetchArgs),
    /// Manage the OpenAPI spec this CLI dispatches from.
    Spec(SpecArgs),
    /// Check this environment: credentials, instance, and spec drift.
    Doctor(DoctorArgs),
    /// Install the agent skill that ships with this binary, or print it.
    Skill(SkillArgs),
    /// Print a shell completion script (bash, zsh, fish, powershell, elvish).
    Completions(CompletionsArgs),
    /// List and filter workspace users.
    Users(UsersArgs),
}

fn main() -> std::process::ExitCode {
    // clap itself exits with code 2 on usage errors, matching the
    // documented exit-code table.
    let cli = Cli::parse();
    let mode = render::resolve_mode(cli.json, std::io::stdout().is_terminal());
    let result = match &cli.command {
        // `Cli::command` is passed, not called: `otl api` renders its own
        // help (so that `otl api <operation> --help` can describe THAT
        // operation instead), and it renders it from the real command tree
        // rather than a second copy. Building that tree costs nothing on
        // every other invocation because the builder is only invoked when
        // the help is actually wanted.
        Command::Api(args) => api::run(args, mode, &cli.overrides(), Cli::command),
        Command::Attachments(args) => attachments::run(args, mode, &cli.overrides()),
        Command::Auth(args) => auth::run(args, mode, &cli.overrides()),
        Command::Docs(args) => docs::run(args, mode, cli.json, &cli.overrides()),
        Command::Collections(args) => collections::run(args, mode, &cli.overrides()),
        Command::Comments(args) => comments::run(args, mode, &cli.overrides()),
        Command::Fetch(args) => fetch::run(args, mode, &cli.overrides()),
        Command::Spec(args) => spec::run(args, mode),
        Command::Doctor(args) => doctor::run(args, mode, &cli.overrides()),
        Command::Skill(args) => skill::run(args, mode),
        Command::Completions(args) => completions::run(args, Cli::command()),
        Command::Users(args) => users::run(args, mode, &cli.overrides()),
    };
    match result {
        Ok(()) => ExitCode::Success.into(),
        Err(error) => {
            stdio::write_diagnostic_line(&format!("error: {error}"));
            error.code.into()
        }
    }
}
