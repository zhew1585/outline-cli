//! The command tree, as data.
//!
//! `Cli` lives in the library rather than in `main.rs` for one reason: the
//! help text is a contract with the agents that drive this CLI, and a
//! contract needs a machine to guard it. `tests/help_coverage.rs` and
//! `tests/skill_surface.rs` walk this tree with `CommandFactory` and assert
//! that every argument carries help and that every command the shipped
//! skill names really exists. Neither test can reach a private type in a
//! binary crate, and a second copy of the tree written for the tests would
//! guard the copy instead of the thing that ships.
//!
//! `main.rs` keeps the dispatch, which is the part that has no contract.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::commands::api::ApiArgs;
use crate::commands::attachments::AttachmentsArgs;
use crate::commands::auth::AuthArgs;
use crate::commands::collections::CollectionsArgs;
use crate::commands::comments::CommentsArgs;
use crate::commands::completions::CompletionsArgs;
use crate::commands::docs::DocsArgs;
use crate::commands::doctor::DoctorArgs;
use crate::commands::fetch::FetchArgs;
use crate::commands::skill::SkillArgs;
use crate::commands::spec::SpecArgs;
use crate::commands::users::UsersArgs;
use crate::config::Overrides;

/// Outline CLI: work with your Outline knowledge base from the terminal.
#[derive(Debug, Parser)]
#[command(name = "otl", version, about)]
pub struct Cli {
    /// Print raw JSON (the default whenever stdout is not a terminal).
    #[arg(long, global = true)]
    pub json: bool,

    /// Named profile from the user config file (env: OUTLINE_PROFILE).
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,

    /// Outline instance base URL, overriding the profile (env: OUTLINE_URL).
    #[arg(long, global = true, value_name = "URL")]
    pub url: Option<String>,

    /// User config file to read (env: OUTLINE_CONFIG).
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// The command-line layer of the configuration, which outranks the
    /// environment and the config file key by key.
    pub fn overrides(&self) -> Overrides {
        Overrides {
            profile: self.profile.clone(),
            url: self.url.clone(),
            config_path: self.config.clone(),
        }
    }
}

/// Every top-level subcommand.
#[derive(Debug, Subcommand)]
pub enum Command {
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
