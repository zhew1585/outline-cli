//! Workspace user listing for mention discovery.

use clap::{Args, Subcommand, ValueEnum};
use serde_json::Value;

use crate::commands::output;
use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;
use crate::session::{self, Session};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum UserStatus {
    All,
    Invited,
    Active,
    Suspended,
}

#[derive(Debug, Args)]
pub struct UsersArgs {
    #[command(subcommand)]
    command: UsersCommand,
}

#[derive(Debug, Subcommand)]
enum UsersCommand {
    /// List and filter workspace users.
    List(ListArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Search names or email addresses.
    #[arg(long)]
    query: Option<String>,
    /// Filter by membership status.
    #[arg(long, value_enum)]
    status: Option<UserStatus>,
    /// Filter by Outline role (admin, member, viewer, guest).
    #[arg(long)]
    role: Option<String>,
    /// Stop after N users.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    limit: Option<u64>,
}

pub fn run(args: &UsersArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    match &args.command {
        UsersCommand::List(args) => list(args, mode, overrides),
    }
}

fn list(args: &ListArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let mut request = Vec::new();
    push(&mut request, "query", args.query.as_ref());
    push(&mut request, "role", args.role.as_ref());
    if let Some(status) = args.status {
        request.push(("filter".to_string(), status_name(status).to_string()));
    }
    let rows = Session::open(overrides)?.call_rows("users.list", &request, args.limit)?;
    let incomplete = rows.incomplete().copied();
    output::emit(&Value::Array(rows.items), mode)?;
    match incomplete {
        Some(truncation) => Err(session::incomplete_error("the user listing", &truncation)),
        None => Ok(()),
    }
}

fn status_name(status: UserStatus) -> &'static str {
    match status {
        UserStatus::All => "all",
        UserStatus::Invited => "invited",
        UserStatus::Active => "active",
        UserStatus::Suspended => "suspended",
    }
}

fn push(request: &mut Vec<(String, String)>, name: &str, value: Option<&String>) {
    if let Some(value) = value {
        request.push((name.to_string(), value.clone()));
    }
}
