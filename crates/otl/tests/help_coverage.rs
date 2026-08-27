//! Guard: `--help` is the contract, so no part of it may be blank.
//!
//! # Why this file exists
//!
//! The `otl` surface is designed to be learnable without leaving the
//! terminal - that premise is what `README.md` calls "discovering what to
//! call" and what the shipped skill tells an agent to rely on. A `#[arg]`
//! with no doc comment still compiles, still parses, and still shows up in
//! `--help` as a flag name followed by nothing at all. The reader is then
//! worse off than if the flag were missing: it looks documented.
//!
//! A review found six such flags on `otl comments create` alone - including
//! the `--anchor-text` / `--anchor-prefix` / `--anchor-suffix` trio, whose
//! entire purpose (turning a document comment into an inline one, and
//! disambiguating a phrase that repeats) was unstated. Every one of them had
//! passed every other test in this directory.
//!
//! So: two rules, both walked over the real command tree from
//! `otl::cli::Cli` rather than a copy.
//!
//! 1. Every argument, positional or flag, carries help text.
//! 2. Every command that prints data declares the JSON shape it prints.
//!
//! Rule 2 exists because the shape is the part a script actually binds to,
//! and it was the part nothing stated. `otl docs list` alone returns two
//! different shapes depending on whether a query was supplied, because it
//! dispatches to two different operations - and its help named both
//! operations while saying nothing about the consequence.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clap::{Command, CommandFactory};

use otl::cli::Cli;

/// The marker a data-printing command's `after_long_help` must carry.
const JSON_SHAPE_MARKER: &str = "JSON shape";

/// Commands that print no data, so rule 2 does not apply.
///
/// Each entry is a full command path. Being on this list is a claim that
/// the command's stdout is either empty, or not JSON in any state - not
/// that documenting it would be inconvenient.
const NO_DATA_OUTPUT: &[&[&str]] = &[
    // The root, and the group nodes: dispatch only, they print their own
    // help.
    &["otl"],
    &["otl", "api"],
    &["otl", "attachments"],
    &["otl", "auth"],
    &["otl", "collections"],
    &["otl", "comments"],
    &["otl", "docs"],
    &["otl", "skill"],
    &["otl", "spec"],
    &["otl", "users"],
    // A shell script on stdout, never JSON.
    &["otl", "completions"],
    // The skill document itself on stdout, never JSON.
    &["otl", "skill", "show"],
    // `help` is clap's own, and its output is clap's.
    &["otl", "help"],
];

/// Commands whose JSON is documented somewhere other than
/// `after_long_help`, with the reason.
///
/// `doctor`'s report is the one long enough that a field-by-field table in
/// `--help` would bury the flags, so it lives in the shipped skill and in
/// `README.md`; `tests/skill_surface.rs` keeps that copy honest. The `auth`
/// write commands and the `spec`/`skill` commands report on an action they
/// took rather than returning a resource, and each names its own fields in
/// the prose of its help.
const DOCUMENTED_ELSEWHERE: &[&[&str]] = &[
    &["otl", "doctor"],
    &["otl", "auth", "login"],
    &["otl", "auth", "logout"],
    &["otl", "auth", "set-key"],
    &["otl", "skill", "install"],
    &["otl", "spec", "sync"],
    &["otl", "spec", "reset"],
];

#[test]
fn every_argument_carries_help_text() {
    let root = Cli::command();
    let mut missing = Vec::new();
    walk(&root, &mut vec!["otl".to_string()], &mut |command, path| {
        for arg in command.get_arguments() {
            let documented = arg.get_help().is_some() || arg.get_long_help().is_some();
            if !documented {
                missing.push(format!("{} :: {}", path.join(" "), arg.get_id()));
            }
        }
    });
    assert!(
        missing.is_empty(),
        "these arguments show up in `--help` with nothing beside them, which reads \
         as documented while saying nothing. Give each a doc comment:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn every_data_printing_command_declares_its_json_shape() {
    let root = Cli::command();
    let mut missing = Vec::new();
    walk(&root, &mut vec!["otl".to_string()], &mut |command, path| {
        if exempt(path) {
            return;
        }
        let declared = command
            .get_after_long_help()
            .map(|help| help.to_string())
            .is_some_and(|help| help.contains(JSON_SHAPE_MARKER));
        if !declared {
            missing.push(path.join(" "));
        }
    });
    assert!(
        missing.is_empty(),
        "these commands write JSON to stdout without saying what shape it has. \
         The shape is what a script binds to, so add a `{JSON_SHAPE_MARKER}:` \
         section to the command's `after_long_help`:\n  {}",
        missing.join("\n  ")
    );
}

/// Whether rule 2 skips this command, and why the list it is on exists.
fn exempt(path: &[String]) -> bool {
    let matches = |entry: &&[&str]| entry.len() == path.len() && entry.iter().eq(path.iter());
    NO_DATA_OUTPUT.iter().any(matches) || DOCUMENTED_ELSEWHERE.iter().any(matches)
}

/// Visit `command` and every subcommand beneath it, depth first, handing
/// each one its full path (`["otl", "docs", "list"]`).
fn walk(command: &Command, path: &mut Vec<String>, visit: &mut impl FnMut(&Command, &[String])) {
    visit(command, path);
    for sub in command.get_subcommands() {
        path.push(sub.get_name().to_string());
        walk(sub, path, visit);
        path.pop();
    }
}

/// The root command itself is not on either exemption list by name, and the
/// walker has to reach it for rule 1 to cover the global flags. This pins
/// the traversal so a refactor that starts at the subcommands instead fails
/// here rather than silently halving the coverage.
#[test]
fn the_walk_reaches_the_root_and_the_leaves() {
    let root = Cli::command();
    let mut seen = Vec::new();
    walk(&root, &mut vec!["otl".to_string()], &mut |_, path| {
        seen.push(path.join(" "));
    });
    for expected in [
        "otl",
        "otl docs",
        "otl docs list",
        "otl comments create",
        "otl api",
    ] {
        assert!(
            seen.iter().any(|path| path == expected),
            "the walk never reached `{expected}`; it saw {seen:?}"
        );
    }
}
