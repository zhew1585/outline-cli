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

// ---------------------------------------------------------------------------
// Generated scripts are executable text (R1 finding 4).
//
// An operation name reaches bash's `opts=` string, zsh's `_arguments` value
// list and fish's `-a` argument. A name carrying a quote, `$(...)`, a
// backtick, a newline or a control character would be command substitution or
// quote escape when the user sources the script or presses Tab.
// ---------------------------------------------------------------------------

/// Names that must never be written into a script.
const HOSTILE_NAMES: &[&str] = &[
    "documents.info\"; touch /tmp/pwned; \"",
    "documents.$(touch /tmp/pwned)",
    "documents.`touch /tmp/pwned`",
    "documents.info'\ndocuments.evil",
    "documents.info\ntouch /tmp/pwned",
    "documents.info;id",
    "documents.info|id",
    "documents.info&id",
    "documents.info>out",
    "documents.info)",
    "documents.info}",
    "documents.*",
    "documents info",
    "documents.info\u{1b}[31m",
    "documents.info\u{0}",
    "",
];

#[test]
fn the_operation_name_filter_rejects_shell_metacharacters() {
    for name in HOSTILE_NAMES {
        assert!(
            !otl::commands::completions::is_safe_operation_name(name),
            "accepted hostile operation name {name:?}"
        );
    }
    // The shapes a real RPC operation name takes are all accepted.
    for name in [
        "documents.info",
        "documents.search_titles",
        "collections.add_group",
        "list",
        "a-b.c_d1",
    ] {
        assert!(
            otl::commands::completions::is_safe_operation_name(name),
            "rejected legitimate operation name {name:?}"
        );
    }
}

#[test]
fn every_compiled_operation_name_is_script_safe() {
    // The build script refuses to compile an unsafe name, so this asserts
    // the guarantee holds for what actually shipped.
    let unsafe_names: Vec<&str> = otl::ops::OPS
        .iter()
        .map(|op| op.name.as_ref())
        .filter(|name| !otl::commands::completions::is_safe_operation_name(name))
        .collect();
    assert!(unsafe_names.is_empty(), "unsafe in IR: {unsafe_names:?}");
}

#[test]
fn no_generated_script_carries_control_or_null_bytes() {
    // The generators' own code legitimately uses `$(...)` (bash's compgen
    // calls), so the guarantee is about what CANDIDATE text can add: nothing
    // outside the allow-list, and never a control byte.
    for shell in SHELLS {
        let script = script_for(shell);
        assert!(
            !script.contains('\u{0}'),
            "{shell} script contains a NUL byte"
        );
        assert!(
            !script.contains('\u{1b}'),
            "{shell} script contains an ESC byte"
        );
    }
}

#[test]
fn candidate_tokens_in_a_script_are_all_allow_listed() {
    // Every operation-shaped token the script offers as a candidate must be
    // one the filter accepts. Checked on fish, whose appended rules put each
    // candidate in a `-a "<name>"` argument that can be extracted exactly.
    let script = script_for("fish");
    let offered: Vec<&str> = script
        .lines()
        .filter_map(|line| line.split(" -a \"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert!(offered.len() > 100, "no candidates extracted");
    for name in offered {
        assert!(
            otl::commands::completions::is_safe_operation_name(name),
            "script offers an unsafe candidate {name:?}"
        );
    }
}

#[test]
fn candidate_descriptions_carry_no_control_characters() {
    // Summaries come from the vendored spec and end up inside fish's
    // single-quoted description; an ESC there would reach the terminal when
    // the candidate is displayed.
    let script = script_for("fish");
    assert!(
        script.chars().all(|c| c == '\n' || !c.is_control()),
        "control character in the fish script"
    );
}

#[test]
fn bash_and_zsh_scripts_pass_their_own_syntax_check() {
    // A quoting bug in generated code is a syntax error at source time; only
    // run the check when the shell is present.
    for (shell, program, args) in [("bash", "bash", vec!["-n"]), ("zsh", "zsh", vec!["-n"])] {
        if std::process::Command::new(program)
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: {program} not installed");
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("otl.{shell}"));
        std::fs::write(&path, script_for(shell)).unwrap();
        let output = std::process::Command::new(program)
            .args(&args)
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell} script fails {program} -n: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// Per-shell coverage is stated, not implied (R1 finding 5).
// ---------------------------------------------------------------------------

#[test]
fn each_script_states_its_own_coverage() {
    use clap_complete::Shell;
    for (shell, name) in [
        (Shell::Bash, "bash"),
        (Shell::Zsh, "zsh"),
        (Shell::Fish, "fish"),
        (Shell::PowerShell, "powershell"),
        (Shell::Elvish, "elvish"),
    ] {
        let script = script_for(name);
        let header = script.lines().take(2).collect::<Vec<_>>().join(" ");
        assert!(header.starts_with('#'), "{name}: no header comment");
        let claims_operations = header.contains("operation names (from");
        assert_eq!(
            claims_operations,
            otl::commands::completions::completes_operation_names(shell),
            "{name}: header claim does not match what is delivered"
        );
        if !claims_operations {
            assert!(
                header.contains("NOT completed"),
                "{name}: gap not stated: {header}"
            );
            assert!(
                header.contains("otl api list"),
                "{name}: no alternative offered: {header}"
            );
        }
    }
}

#[test]
fn a_shell_that_claims_operation_names_actually_carries_them() {
    use clap_complete::Shell;
    for (shell, name) in [
        (Shell::Bash, "bash"),
        (Shell::Zsh, "zsh"),
        (Shell::Fish, "fish"),
        (Shell::PowerShell, "powershell"),
        (Shell::Elvish, "elvish"),
    ] {
        let script = script_for(name);
        let has_operations = script.contains("documents.info");
        assert_eq!(
            has_operations,
            otl::commands::completions::completes_operation_names(shell),
            "{name}: coverage table disagrees with the generated script"
        );
    }
}

#[test]
fn the_public_module_documentation_matches_the_delivered_coverage() {
    // R2-5 / R3-7 / R4-4. Each round the check was too weak in a new
    // direction, so it is now stated as two symmetric rules over SENTENCES,
    // both driven by the same predicate the generator uses:
    //
    //   1. some positive claim names exactly the covered shells;
    //   2. no positive claim names an uncovered shell, and no denial names a
    //      covered one.
    //
    // Substring searches were what let the previous versions pass: "bash,
    // zsh, fish" appears just as happily in "bash, zsh, fish do not complete
    // operation names".
    let doc = module_doc();
    assert!(!doc.is_empty(), "no module documentation found");

    let covered = shell_names(true);
    let uncovered = shell_names(false);
    let mut positive_naming_all_covered = false;

    for sentence in doc.split(['.', ';']) {
        if !mentions_operation_completion(sentence) {
            continue;
        }
        if is_denial(sentence) {
            for shell in &covered {
                assert!(
                    !sentence.contains(shell),
                    "rustdoc denies that {shell} completes operation names: {sentence:?}"
                );
            }
            continue;
        }
        for shell in &uncovered {
            assert!(
                !sentence.contains(shell),
                "rustdoc claims {shell} completes operation names: {sentence:?}"
            );
        }
        if covered.iter().all(|shell| sentence.contains(shell)) {
            positive_naming_all_covered = true;
        }
    }
    assert!(
        positive_naming_all_covered,
        "no sentence positively states that {covered:?} complete operation names: {doc}"
    );
}

#[test]
fn the_coverage_check_catches_drift_in_both_directions() {
    // Guards the guard, with one sample per direction: the over-claim the R3
    // review supplied, and the denial the R4 review supplied. Each is run
    // through the same classification the check above uses.
    let over_claim = "powershell and elvish operation names complete";
    assert!(mentions_operation_completion(over_claim));
    assert!(!is_denial(over_claim), "would be skipped as a denial");
    assert!(
        shell_names(false).iter().any(|s| over_claim.contains(s)),
        "the over-claim sample names no uncovered shell"
    );

    let denial = "bash, zsh, fish do not complete operation names";
    assert!(mentions_operation_completion(denial));
    assert!(is_denial(denial), "a denial must be classified as one");
    assert!(
        shell_names(true).iter().all(|s| denial.contains(s)),
        "the denial sample does not name the covered shells"
    );
}

/// Whether a sentence is about completing operation names at all.
fn mentions_operation_completion(sentence: &str) -> bool {
    let lower = sentence.to_ascii_lowercase();
    lower.contains("operation") && (lower.contains("complete") || lower.contains("candidate"))
}

/// Whether such a sentence denies rather than asserts that completion.
///
/// Deliberately not keyed on "only": "complete in bash, zsh, fish only" is a
/// POSITIVE claim about those three (and silence about the rest), which is
/// exactly the sentence the documentation is supposed to contain. A denial
/// has to negate the completion itself.
fn is_denial(sentence: &str) -> bool {
    let lower = sentence.to_ascii_lowercase();
    lower.contains("not complete")
        || lower.contains("not completed")
        || lower.contains("no candidates")
        || lower.contains("do not")
        || lower.contains("does not")
        || lower.contains("cannot")
}

/// The module-level `//!` documentation of the completions module.
fn module_doc() -> String {
    include_str!("../src/commands/completions.rs")
        .lines()
        .take_while(|line| line.starts_with("//!") || line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shell names whose scripts do (or do not) carry the operation names,
/// straight from the predicate the generator uses.
fn shell_names(with_operations: bool) -> Vec<&'static str> {
    use clap_complete::Shell;
    [
        (Shell::Bash, "bash"),
        (Shell::Zsh, "zsh"),
        (Shell::Fish, "fish"),
        (Shell::PowerShell, "powershell"),
        (Shell::Elvish, "elvish"),
    ]
    .into_iter()
    .filter(|(shell, _)| {
        otl::commands::completions::completes_operation_names(*shell) == with_operations
    })
    .map(|(_, name)| name)
    .collect()
}
