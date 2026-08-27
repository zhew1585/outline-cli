//! `otl` binary entry point.

#![forbid(unsafe_code)]

use std::io::IsTerminal;

use clap::{CommandFactory, Parser};

use otl::cli::{Cli, Command};
use otl::commands::{
    api, attachments, auth, collections, comments, completions, docs, doctor, fetch, skill, spec,
    users,
};
use otl::exit::ExitCode;
use otl::render;
use otl::stdio;

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
