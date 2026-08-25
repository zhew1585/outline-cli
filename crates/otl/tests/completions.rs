//! Story 4.4: `otl completions <shell>` emits an IR-driven completion
//! script on stdout, with diagnostics kept on stderr.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;

/// Every shell the CLI must be able to generate a script for.
const SHELLS: [&str; 5] = ["bash", "zsh", "fish", "powershell", "elvish"];

/// `otl` with the environment scrubbed: completion generation is purely
/// local, so it must not need (or read) any configuration.
fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE")
        .env("OUTLINE_CONFIG", "");
    cmd
}

fn script_for(shell: &str) -> String {
    let output = otl().args(["completions", shell]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{shell}: {stderr}");
    assert!(stderr.is_empty(), "{shell} wrote to stderr: {stderr}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn every_supported_shell_produces_a_script_on_stdout() {
    for shell in SHELLS {
        let script = script_for(shell);
        assert!(
            script.len() > 200,
            "{shell} script suspiciously short: {script}"
        );
        assert!(script.contains("otl"), "{shell} script does not name otl");
    }
}

#[test]
fn scripts_complete_subcommands_and_global_flags() {
    for shell in SHELLS {
        let script = script_for(shell);
        // Flag names appear with or without their dashes depending on the
        // shell (fish writes `-l json`), so the bare name is asserted.
        for token in ["api", "completions", "json", "profile", "config", "url"] {
            assert!(
                script.contains(token),
                "{shell} script is missing {token}: {script}"
            );
        }
    }
}

/// Shells whose completion script carries the `otl api` operation names.
///
/// bash and zsh emit candidates for positional arguments natively; fish gets
/// them from appended `complete` rules. powershell and elvish are excluded:
/// their upstream generators emit flags and subcommands only, for any
/// positional value.
const SHELLS_WITH_OPERATIONS: [&str; 3] = ["bash", "zsh", "fish"];

#[test]
fn scripts_complete_api_operation_names_from_the_ir() {
    // The candidates come from the compiled IR table, so they can never
    // drift from what the binary can actually call.
    for shell in SHELLS_WITH_OPERATIONS {
        let script = script_for(shell);
        for op in ["documents.info", "collections.list", "auth.info"] {
            assert!(
                script.contains(op),
                "{shell} script does not offer {op}: no IR-driven operation candidates"
            );
        }
        assert!(
            script.contains("documents.import"),
            "{shell} script omits an operation the IR knows about"
        );
    }
}

#[test]
fn operation_candidates_cover_the_whole_ir_table() {
    for shell in SHELLS_WITH_OPERATIONS {
        let script = script_for(shell);
        let missing: Vec<&str> = otl::ops::OPS
            .iter()
            .map(|op| op.name.as_ref())
            .filter(|name| !script.contains(*name))
            .collect();
        assert!(
            missing.is_empty(),
            "{shell}: operations not completable: {missing:?}"
        );
        // `otl api list` is a reserved name, not a spec operation, and must
        // complete too.
        assert!(script.contains("list"), "{shell}");
    }
}

#[test]
fn the_fish_script_keeps_using_the_generators_condition_helper() {
    // The appended operation rules reuse the helper clap_complete defines.
    // If an upgrade renames it, nothing is appended - and this fails, which
    // is the point.
    let script = script_for("fish");
    assert!(
        script.contains("__fish_otl_using_subcommand api"),
        "fish condition helper changed: {script}"
    );
    assert!(
        script.contains("-a \"documents.info\""),
        "operation rules were not appended: {script}"
    );
    // Descriptions are escaped for a fish single-quoted string.
    assert!(!script.contains("-d ''''"), "broken escaping: {script}");
}

#[test]
fn an_unsupported_shell_is_a_usage_error() {
    otl()
        .args(["completions", "tcsh"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::is_empty());
}

#[test]
fn generation_never_needs_configuration() {
    // No OUTLINE_URL, no API key, no config file: still exit 0.
    otl().args(["completions", "bash"]).assert().success();
}

#[test]
fn help_does_not_list_every_operation_as_a_possible_value() {
    // The IR candidates exist for the completion script only; `otl api
    // --help` must stay readable, and the real parser must keep accepting
    // any name so that an unknown one gets otl's own error message.
    let output = otl().args(["api", "--help"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("possible values"),
        "help became an operation dump: {stdout}"
    );
    // `documents.info` appears as an example in the argument's own help
    // text; a dump would drag the rest of the table in with it.
    assert!(
        !stdout.contains("collections.list") && !stdout.contains("auth.info"),
        "help became an operation dump: {stdout}"
    );
}

#[test]
fn an_unknown_operation_still_gets_the_cli_error_not_a_clap_error() {
    otl()
        .args(["api", "nonexistent.op"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unknown API operation"))
        .stderr(predicate::str::contains("otl api list"));
}

#[test]
fn a_closed_stdout_pipe_does_not_panic() {
    // Completion scripts are long; a consumer that stops reading early
    // (`otl completions bash | head -1`) must exit quietly, never 101.
    use std::io::Read;
    use std::process::{Command as StdCommand, Stdio};

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("otl"))
        .env("OUTLINE_CONFIG", "")
        .env_remove("OUTLINE_PROFILE")
        .args(["completions", "bash"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0_u8; 1];
    let _ = stdout.read(&mut first);
    drop(stdout);
    let output = child.wait_with_output().unwrap();
    let code = output.status.code();
    assert!(
        code == Some(0),
        "closed pipe should exit 0, got {code:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
