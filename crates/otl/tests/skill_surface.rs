//! Guard: every command line the shipped skill shows must be real.
//!
//! # Why this file exists
//!
//! `crates/otl/skill/SKILL.md` is the document an agent reads INSTEAD OF
//! experimenting. That is its whole value and its whole risk: a command that
//! no longer exists, or a flag that was renamed, does not read as stale - it
//! reads as instruction, and the agent runs it.
//!
//! Until now nothing compared the document to the binary. `tests/
//! skill_install.rs` covers installing it, `tests/exit_code_tables.rs`
//! covers its exit-code table, and between them the ~70 command lines in its
//! code fences were unchecked. A review found the document claiming
//! `otl collections list` prints a document count in `--json` (it does not -
//! the count is a table-only figure the API cannot confirm) and claiming
//! every curated command returns "the operation's own item shape" (four of
//! them compose an object instead). Both were true when written.
//!
//! So this test extracts every `otl ...` line from the document's fenced
//! code blocks and holds each to the command tree:
//!
//! - the subcommand path exists;
//! - every `--flag` on it exists on that subcommand;
//! - every `<VALUE>` a positional value-enum is given is one it accepts.
//!
//! It deliberately does NOT check argument VALUES (`--collection <ID>` is a
//! placeholder, not a UUID) or run anything. This is about the surface, and
//! the surface is what a document can get wrong while still parsing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use clap::{Command, CommandFactory};

use otl::cli::Cli;

/// Words that follow `otl` in a shell line without being part of the
/// invocation: a shell operator, or a placeholder the prose explains.
const NOT_ARGUMENTS: &[&str] = &["#", "|", "<", ">", ">>", "&&", "||", ";", "..."];

/// Repository root, from `<repo>/crates/otl`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate manifest dir has a grandparent")
        .to_path_buf()
}

fn skill_path() -> PathBuf {
    repo_root().join("crates/otl/skill/SKILL.md")
}

/// One `otl ...` invocation lifted out of the document, with the line it
/// came from for the failure message.
#[derive(Debug)]
struct Invocation {
    line_number: usize,
    line: String,
    words: Vec<String>,
}

/// Every `otl` invocation inside a fenced code block.
///
/// Only fenced blocks: prose mentions commands in backticks too, and those
/// are frequently partial on purpose ("`--limit N` is a truncation you asked
/// for"). A fenced block is the part a reader copies.
fn invocations(document: &str) -> Vec<Invocation> {
    let mut found = Vec::new();
    let mut fenced = false;
    for (index, raw) in document.lines().enumerate() {
        if raw.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            continue;
        }
        let Some(words) = invocation_words(raw) else {
            continue;
        };
        found.push(Invocation {
            line_number: index + 1,
            line: raw.trim().to_string(),
            words,
        });
    }
    found
}

/// The words of an `otl` invocation on one line, or None if there is none.
///
/// A leading pipeline stage is followed: `printf %s "$KEY" | otl auth
/// set-key` is an `otl` invocation. Everything from a `#` comment, a
/// redirection or a further pipe onwards is dropped, because it belongs to
/// the shell rather than to `otl`.
fn invocation_words(line: &str) -> Option<Vec<String>> {
    let mut words = line.split_whitespace().skip_while(|word| *word != "otl");
    words.next()?;
    let mut kept = Vec::new();
    for word in words {
        if NOT_ARGUMENTS.contains(&word) || word.starts_with('>') || word.starts_with('#') {
            break;
        }
        kept.push(word.to_string());
    }
    Some(kept)
}

/// The command tree with clap's own preparation done.
///
/// `Cli::command()` returns the tree as DECLARED, where a `global = true`
/// flag lives only on the root and `--help` / `--version` do not exist yet.
/// `build()` is the step that propagates the globals down and adds the
/// generated flags, so it is the tree a user actually types against - and
/// therefore the only one a document should be checked against. Without it
/// every `--json` in the skill would be reported as unknown, which is a
/// guard that fails on the correct document.
fn command_tree() -> Command {
    let mut root = Cli::command();
    root.build();
    root
}

#[test]
fn every_command_the_skill_shows_exists() {
    let document = std::fs::read_to_string(skill_path()).expect("read SKILL.md");
    let root = command_tree();
    let found = invocations(&document);
    assert!(
        found.len() > 40,
        "only {} `otl` invocations were found in SKILL.md; the extraction \
         must have broken, and a vacuous guard is worse than none",
        found.len()
    );
    let mut problems = Vec::new();
    for invocation in &found {
        if let Err(problem) = check(&root, &invocation.words) {
            problems.push(format!(
                "  SKILL.md:{}  {}\n    {problem}",
                invocation.line_number, invocation.line
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "the shipped skill shows command lines this binary does not accept. An \
         agent reads that document INSTEAD OF experimenting, so a stale line \
         is an instruction, not a typo:\n{}",
        problems.join("\n")
    );
}

/// Hold one invocation's words to the command tree.
fn check(root: &Command, words: &[String]) -> Result<(), String> {
    let mut node = root;
    let mut path = "otl".to_string();
    let mut in_arguments = false;
    for word in words {
        // `otl api documents.info id=<ID>` and `otl api describe X`: past the
        // `api` subcommand the words are an operation name and its
        // key=value arguments, which `tests/curated_index.rs` and the spec
        // parity tests cover instead.
        if path == "otl api" {
            return Ok(());
        }
        if let Some(flag) = word.strip_prefix("--") {
            let name = flag.split('=').next().unwrap_or(flag);
            let known = node
                .get_arguments()
                .any(|arg| arg.get_long() == Some(name) || arg.get_id() == name);
            if !known {
                return Err(format!("`{path}` has no --{name}"));
            }
            in_arguments = true;
            continue;
        }
        if in_arguments {
            // A flag's value, or a positional the flags already started.
            continue;
        }
        if let Some(sub) = node.get_subcommands().find(|sub| sub.get_name() == *word) {
            node = sub;
            path = format!("{path} {word}");
            continue;
        }
        // Not a subcommand: it has to be a value one of this command's
        // positionals accepts. A positional with no enumerated values takes
        // anything, so a placeholder like <ID> passes there and only a
        // value-enum is actually checked.
        let positionals: Vec<_> = node.get_positionals().collect();
        if positionals.is_empty() {
            return Err(format!("`{path}` takes no positional argument `{word}`"));
        }
        let enumerated: Vec<String> = positionals
            .iter()
            .flat_map(|arg| arg.get_possible_values())
            .map(|value| value.get_name().to_string())
            .collect();
        if !enumerated.is_empty() && !enumerated.contains(word) {
            return Err(format!(
                "`{path}` does not accept `{word}`; it takes one of {enumerated:?}"
            ));
        }
        in_arguments = true;
    }
    Ok(())
}

/// The document must not name an environment variable the CLI does not read.
///
/// Same failure mode as a renamed flag, and the same cost: a variable that
/// does nothing looks exactly like one that works.
#[test]
fn every_environment_variable_the_skill_names_is_read_by_the_binary() {
    let document = std::fs::read_to_string(skill_path()).expect("read SKILL.md");
    let sources = std::fs::read_to_string(repo_root().join("crates/otl/src/config/mod.rs"))
        .unwrap_or_default()
        + &read_all_sources();
    let mut unknown = Vec::new();
    for word in document.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        let looks_like_a_variable = word.starts_with("OUTLINE_") && word.len() > "OUTLINE_".len();
        if !looks_like_a_variable {
            continue;
        }
        // A profile-scoped key is built at run time from the profile name,
        // so only its prefix exists in the source.
        let stem = match word.starts_with("OUTLINE_API_KEY") {
            true => "OUTLINE_API_KEY",
            false => word,
        };
        if !sources.contains(stem) && !unknown.contains(&word.to_string()) {
            unknown.push(word.to_string());
        }
    }
    assert!(
        unknown.is_empty(),
        "SKILL.md names environment variable(s) {unknown:?} that appear nowhere \
         in crates/otl/src. A variable that does nothing reads exactly like one \
         that works."
    );
}

/// Every `.rs` file under `crates/otl/src`, concatenated.
fn read_all_sources() -> String {
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    walk(&repo_root().join("crates/otl/src"), &mut out);
    out
}

#[test]
fn the_extractor_reads_a_fenced_line_and_ignores_prose() {
    let document = "text `otl not-a-command` more\n\n```sh\notl docs view <ID> --raw   # comment\nprintf x | otl auth set-key\n```\n";
    let found = invocations(document);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].words, ["docs", "view", "<ID>", "--raw"]);
    assert_eq!(found[1].words, ["auth", "set-key"]);
}

#[test]
fn the_checker_rejects_a_renamed_flag_and_an_invented_subcommand() {
    let root = command_tree();
    let words = |line: &str| -> Vec<String> { line.split(' ').map(str::to_string).collect() };
    assert!(check(&root, &words("docs view <ID> --raw")).is_ok());
    // A global flag reaches every subcommand, which is only true of a tree
    // clap has built.
    assert!(check(&root, &words("docs view <ID> --json")).is_ok());
    assert!(check(&root, &words("docs view <ID> --rawr")).is_err());
    assert!(check(&root, &words("docs vieww <ID>")).is_err());
    assert!(check(&root, &words("fetch collection <ID>")).is_ok());
    assert!(check(&root, &words("fetch nonsense <ID>")).is_err());
}
