//! Guard: the operation -> curated-command index in `otl api list`.
//!
//! # Why this file exists
//!
//! `otl api list` publishes a `curated_command` for every operation, so a
//! caller can tell the semver-stable path from the unstable one without
//! reading a second document. That is only worth publishing if it is true,
//! and it is a hand-written table, so it has three ways to go wrong:
//!
//! 1. it names an operation this binary does not have (a spec change);
//! 2. it names a command that does not exist (a rename);
//! 3. a NEW curated command is added and nobody updates the table, which is
//!    the failure that leaves the index quietly incomplete rather than
//!    visibly wrong.
//!
//! The third is the one no ordinary test catches, so it gets the unusual
//! check: every curated command's own `--help` already names the operations
//! it drives, in the `otl api describe <operation> --json` lines of its
//! `API contract(s):` block. Those lines are the command's own claim about
//! what it covers, written next to the code that does the covering. This
//! test reads them back out of the built command tree and requires each
//! named operation to appear somewhere in the index.
//!
//! "Somewhere", not "mapped to this command": `otl docs search` names
//! `collections.list` because it labels a table column from it, and the
//! command to reach `collections.list` is still `otl collections list`.
//! Requiring the exact pairing would force the index to lie about which
//! command owns an operation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use clap::{Command, CommandFactory};

use otl::cli::Cli;

mod common;

/// The prefix a curated command's help uses to name an operation.
const DESCRIBE_PREFIX: &str = "otl api describe ";

/// Operations a curated command's help names but which are deliberately
/// absent from the index, with the reason.
///
/// `auth.config` and the rest of the OAuth discovery surface are driven by
/// `otl auth login`, which is a browser flow rather than a front door onto
/// an operation: telling a caller to "prefer otl auth login" for
/// `auth.config` would be advice to start a consent screen.
const NOT_A_FRONT_DOOR: &[&str] = &["auth.config"];

/// The index as `otl api list --json` publishes it.
fn index() -> Vec<(String, Option<String>)> {
    let output = common::otl()
        .args(["api", "list", "--json"])
        .output()
        .expect("otl api list");
    assert!(output.status.success(), "otl api list failed");
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json array");
    rows.as_array()
        .expect("array")
        .iter()
        .map(|row| {
            (
                row["name"].as_str().expect("name").to_string(),
                row["curated_command"].as_str().map(str::to_string),
            )
        })
        .collect()
}

#[test]
fn every_indexed_command_exists_in_the_command_tree() {
    let root = Cli::command();
    for (operation, command) in index() {
        let Some(command) = command else { continue };
        let (node, path, rest) = resolve(&root, &command);
        // Whatever the subcommand walk did not consume has to be a flag of
        // the command it stopped at, or a value its positional accepts -
        // `otl fetch attachment` selects an operation with a value-enum
        // rather than a subcommand, and the index has to be able to say so.
        for word in rest {
            let usable = match word.strip_prefix("--") {
                Some(flag) => node.get_arguments().any(|arg| arg.get_id() == flag),
                None => node.get_positionals().any(|arg| {
                    arg.get_possible_values()
                        .iter()
                        .any(|value| value.get_name() == word)
                }),
            };
            assert!(
                usable,
                "`{command}` (index entry for {operation}) ends in `{word}`, which \
                 `{path}` accepts neither as a flag nor as a positional value"
            );
        }
    }
}

/// Walk `command`'s words down the tree as far as they are subcommands.
///
/// Returns the node reached, its path, and the words left over.
fn resolve<'a>(root: &'a Command, command: &str) -> (&'a Command, String, Vec<String>) {
    let words: Vec<&str> = command.split_whitespace().collect();
    assert_eq!(words.first(), Some(&"otl"), "{command}");
    let mut node = root;
    let mut path = vec!["otl".to_string()];
    let mut rest = Vec::new();
    for word in &words[1..] {
        match node.get_subcommands().find(|sub| sub.get_name() == *word) {
            Some(sub) if rest.is_empty() => {
                node = sub;
                path.push((*word).to_string());
            }
            _ => rest.push((*word).to_string()),
        }
    }
    (node, path.join(" "), rest)
}

#[test]
fn every_operation_a_curated_command_names_is_in_the_index() {
    let known: BTreeSet<String> = index()
        .into_iter()
        .filter(|(_, command)| command.is_some())
        .map(|(operation, _)| operation)
        .collect();
    let mut unindexed = Vec::new();
    let root = Cli::command();
    walk(&root, &mut vec!["otl".to_string()], &mut |command, path| {
        for operation in operations_named_in_help(command) {
            if known.contains(&operation) || NOT_A_FRONT_DOOR.contains(&operation.as_str()) {
                continue;
            }
            unindexed.push(format!("{} names {operation}", path.join(" ")));
        }
    });
    assert!(
        unindexed.is_empty(),
        "these operations are driven by a curated command but `otl api list` still \
         reports curated_command: null for them, so a caller reading the list is \
         sent down the unstable path. Add them to commands/api/curated.rs:\n  {}",
        unindexed.join("\n  ")
    );
}

/// A curated command that covers an operation must be the command the index
/// names for it - otherwise the index would point somewhere the help does
/// not, which is worse than pointing nowhere.
#[test]
fn the_index_agrees_with_the_help_on_the_commands_it_does_name() {
    let root = Cli::command();
    for (operation, command) in index() {
        let Some(command) = command else { continue };
        // The help block belongs to the subcommand; a trailing flag or
        // resource word is how that subcommand selects between the several
        // operations its one block names.
        let (node, path, _) = resolve(&root, &command);
        assert!(
            operations_named_in_help(node).contains(&operation),
            "the index sends {operation} to `{command}`, but `{path} --help` does \
             not name {operation} in its API contract block"
        );
    }
}

/// The operations a command's `after_long_help` names, in the
/// `otl api describe <operation> --json` lines of its contract block.
fn operations_named_in_help(command: &Command) -> Vec<String> {
    let Some(help) = command.get_after_long_help() else {
        return Vec::new();
    };
    help.to_string()
        .lines()
        .filter_map(|line| line.trim().strip_prefix(DESCRIBE_PREFIX))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// Visit `command` and every subcommand beneath it, depth first.
fn walk(command: &Command, path: &mut Vec<String>, visit: &mut impl FnMut(&Command, &[String])) {
    visit(command, path);
    for sub in command.get_subcommands() {
        path.push(sub.get_name().to_string());
        walk(sub, path, visit);
        path.pop();
    }
}

/// The extraction itself must not be vacuous: if the help format changes and
/// this stops finding anything, the two tests above pass by finding nothing.
#[test]
fn the_help_scraper_finds_the_operations_it_is_looking_for() {
    let root = Cli::command();
    let mut total = 0usize;
    walk(&root, &mut vec!["otl".to_string()], &mut |command, _| {
        total += operations_named_in_help(command).len();
    });
    assert!(
        total > 25,
        "only {total} operations were scraped out of the command tree; the \
         `{DESCRIBE_PREFIX}` convention must have changed"
    );
}
