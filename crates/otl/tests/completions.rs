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
        // Fed on stdin, not as a path. `bash` exists on GitHub's Windows
        // runners (Git Bash), so the probe above does not skip there - but a
        // native path handed to it is not one it can open, and it exits
        // non-zero with an empty message, which reads exactly like a syntax
        // error in the generated script. `-n` reads the script from stdin
        // when given no file, so there is no path to translate and the check
        // means the same thing on every platform that has the shell.
        let script = script_for(shell);
        let mut child = std::process::Command::new(program)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), script.as_bytes()).unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
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
        // Located, not assumed to be first: zsh's `#compdef` tag owns line 1,
        // so the notice sits just below it there.
        let start = script
            .lines()
            .position(|line| line.starts_with("# otl completion script for"))
            .unwrap_or_else(|| panic!("{name}: no coverage notice found"));
        let header = script
            .lines()
            .skip(start)
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
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
    // R2-5 / R3-7 / R4-4 / R5-2. Each round found the check too weak in a new
    // direction; the last was a MIXED sentence - "powershell does not
    // complete operation names, but elvish does" classified as a denial as a
    // whole, so the affirmative second clause was never examined.
    //
    // Two granularities, because the two questions need different ones:
    //
    //   1. some SENTENCE positively claims completion and names every covered
    //      shell (a comma list has to survive intact for this);
    //   2. no CLAUSE affirms completion for an uncovered shell, and no clause
    //      denies it for a covered one.
    let doc = module_doc();
    assert!(!doc.is_empty(), "no module documentation found");

    let covered = shell_names(true);
    let uncovered = shell_names(false);

    let positive_naming_all_covered = doc.split(['.', ';']).any(|sentence| {
        mentions_operation_completion(sentence)
            && !is_denial(sentence)
            && covered.iter().all(|shell| sentence.contains(shell))
    });
    assert!(
        positive_naming_all_covered,
        "no sentence positively states that {covered:?} complete operation names: {doc}"
    );

    for clause in clauses(&doc) {
        if is_denial(clause) {
            for shell in &covered {
                assert!(
                    !clause.contains(shell),
                    "rustdoc denies that {shell} completes operation names: {clause:?}"
                );
            }
        } else if affirms_completion(clause) {
            for shell in &uncovered {
                assert!(
                    !clause.contains(shell),
                    "rustdoc claims {shell} completes operation names: {clause:?}"
                );
            }
        }
    }
}

#[test]
fn the_coverage_check_catches_drift_in_every_direction() {
    // Guards the guard, with the sample each review supplied. Every one is
    // run through the same classification the check above uses.
    let over_claim = "powershell and elvish operation names complete";
    let denial = "bash, zsh, fish do not complete operation names";
    let mixed = "powershell does not complete operation names, but elvish does";

    // R3's over-claim: an affirmative clause naming an uncovered shell.
    assert!(clauses(over_claim)
        .into_iter()
        .any(|c| affirms_completion(c) && shell_names(false).iter().any(|s| c.contains(s))));

    // R4's denial: a denying clause naming a covered shell. A comma list
    // splits across clauses, so the check needs one such clause, not one
    // naming all three.
    assert!(clauses(denial)
        .into_iter()
        .any(|c| is_denial(c) && shell_names(true).iter().any(|s| c.contains(s))));

    // R5's mixed sentence: the SECOND clause affirms for an uncovered shell,
    // even though the sentence as a whole reads as a denial.
    assert!(
        is_denial(mixed),
        "the mixed sample must still look like a denial as a whole"
    );
    assert!(
        clauses(mixed)
            .into_iter()
            .any(|c| affirms_completion(c) && shell_names(false).iter().any(|s| c.contains(s))),
        "the mixed sample's affirmative clause is not detected"
    );

    // R6: every other way of joining the two halves. ` - ` matters most,
    // being this module's own house style for joining clauses.
    for joiner in [
        " and ",
        ": ",
        " - ",
        " though ",
        " although ",
        " yet ",
        "; ",
        ", but ",
    ] {
        let drifted = format!("powershell does not complete operation names{joiner}elvish does");
        assert!(
            clauses(&drifted)
                .into_iter()
                .any(|clause| affirms_completion(clause)
                    && shell_names(false).iter().any(|s| clause.contains(s))),
            "a clause joined by {joiner:?} hides its affirmation"
        );
    }

    // A denial that never says "not": the other direction.
    for phrasing in [
        "bash never completes operation names",
        "zsh lacks operation name completion",
        "fish operation name completion is unavailable",
    ] {
        assert!(
            is_denial(phrasing),
            "{phrasing:?} is not recognised as a denial"
        );
        assert!(
            shell_names(true).iter().any(|s| phrasing.contains(s)),
            "the sample does not name a covered shell"
        );
    }
}

/// Split documentation into clauses, not just sentences.
///
/// A sentence can carry an affirmation and a denial at once ("X does not
/// complete them, but Y does"), so anything classified as a whole sentence
/// hides half of what it says.
fn clauses(doc: &str) -> Vec<&str> {
    const CONJUNCTIONS: &[&str] = &[
        " but ",
        " while ",
        " whereas ",
        " however ",
        " and ",
        " though ",
        " although ",
        " yet ",
        " - ",
        " \u{2013} ",
        " \u{2014} ",
    ];
    let mut parts: Vec<&str> = doc.split(['.', ';', ',', ':']).collect();
    for conjunction in CONJUNCTIONS {
        parts = parts
            .into_iter()
            .flat_map(|part| part.split(conjunction))
            .collect();
    }
    parts
}

/// Whether a clause is about completing operation names at all.
fn mentions_operation_completion(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("operation") && (lower.contains("complete") || lower.contains("candidate"))
}

/// Whether a clause AFFIRMS completion.
///
/// Includes clauses whose verb is elided ("but elvish does", "elvish too"),
/// which is how the mixed sentence smuggled its claim past a check that only
/// looked for the word "complete".
fn affirms_completion(text: &str) -> bool {
    if is_denial(text) {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    mentions_operation_completion(text)
        || lower.contains(" does")
        || lower.contains(" do ")
        || lower.contains(" is ")
        || lower.contains(" are ")
        || lower.contains(" too")
        || lower.contains(" as well")
        || lower.contains(" also")
}

/// Whether a clause DENIES completion.
///
/// Deliberately not keyed on "only": "complete in bash, zsh, fish only" is a
/// POSITIVE claim about those three (and silence about the rest), which is
/// exactly the sentence the documentation is supposed to contain.
fn is_denial(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        // Negated verbs.
        "not complete",
        "not completed",
        "do not",
        "does not",
        "cannot",
        "no candidates",
        // Denials that never say "not" at all - the second direction the
        // review found, where a false denial about a supported shell was
        // being read as an affirmation.
        "never",
        "lacks",
        "unavailable",
        "unsupported",
        "without",
        "excluded",
        "omitted",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
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

// ---------------------------------------------------------------------------
// Script SHAPE, not just script content (R6 finding 1).
//
// Five rounds of substring assertions could not see that the coverage notice
// pushed zsh's `#compdef` tag off line 1, which is the one line zsh's
// `compinit` reads when it scans `$fpath`. The documented install
// (`otl completions zsh > ~/.zfunc/_otl`) therefore produced a file zsh
// silently declined to register.
// ---------------------------------------------------------------------------

#[test]
fn the_zsh_script_starts_with_the_compdef_tag() {
    let script = script_for("zsh");
    let first = script.lines().next().unwrap_or_default();
    assert_eq!(
        first, "#compdef otl",
        "zsh reads only the first line of a file in $fpath when looking for \
         the #compdef tag; anything above it makes the completion invisible \
         to compinit"
    );
    // The coverage notice must still be there, just not first.
    assert!(
        script.contains("# otl completion script for zsh."),
        "the coverage notice was lost: {script}"
    );
}

#[test]
fn every_script_keeps_its_shell_required_first_line() {
    // Stated per shell so a future notice cannot quietly take the slot again.
    for (shell, required_prefix) in [
        ("zsh", Some("#compdef")),
        ("bash", None),
        ("fish", None),
        ("powershell", None),
        ("elvish", None),
    ] {
        let script = script_for(shell);
        let first = script.lines().next().unwrap_or_default();
        match required_prefix {
            Some(prefix) => assert!(
                first.starts_with(prefix),
                "{shell}: first line must start with {prefix:?}, got {first:?}"
            ),
            // The others carry the notice first; assert that too, so the
            // notice cannot silently disappear from them either.
            None => assert!(
                first.starts_with('#'),
                "{shell}: expected the coverage notice first, got {first:?}"
            ),
        }
    }
}

#[test]
fn zsh_registers_the_generated_completion() {
    // The end-to-end check the substring tests could not make: install the
    // script exactly as the README says and ask zsh whether it registered.
    let Ok(zsh) = which_shell("zsh") else {
        eprintln!("skipping: zsh not installed");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let fpath = dir.path().join("zfunc");
    std::fs::create_dir(&fpath).unwrap();
    std::fs::write(fpath.join("_otl"), script_for("zsh")).unwrap();

    let script = format!(
        "fpath=({} $fpath); autoload -Uz compinit; compinit -u -d {}; \
         print -r -- ${{_comps[otl]:-NOT-REGISTERED}}",
        fpath.display(),
        dir.path().join("zcompdump").display()
    );
    let output = std::process::Command::new(zsh)
        .arg("-f")
        .arg("-c")
        .arg(&script)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("_otl"),
        "zsh did not register the completion: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Locate a shell on PATH, or report that it is absent.
fn which_shell(name: &str) -> Result<std::path::PathBuf, ()> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .output()
        .map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        Err(())
    } else {
        Ok(std::path::PathBuf::from(path))
    }
}

#[test]
fn candidate_descriptions_carry_no_bidi_or_invisible_characters() {
    // R6 finding 9: descriptions filtered `is_control()` only, so a summary
    // carrying U+202E would reorder the fish completion menu for the same
    // reason an ESC byte would recolour it.
    let script = script_for("fish");
    for ch in [
        '\u{202e}', '\u{202a}', '\u{2066}', '\u{2069}', '\u{200f}', '\u{200e}', '\u{61c}',
        '\u{200b}', '\u{feff}', '\u{00ad}', '\u{2060}',
    ] {
        assert!(
            !script.contains(ch),
            "U+{:04X} reached the fish script",
            ch as u32
        );
    }
}
