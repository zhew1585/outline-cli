//! What a REAL shell does with the generated script.
//!
//! Split from `completions.rs` by prerequisite, not by size. Everything here
//! spawns an external shell, so it can only assert where that shell exists
//! and works - and every one of these checks exists because a substring
//! assertion could not see the defect:
//!
//! * five rounds of them missed the coverage notice pushing zsh's `#compdef`
//!   tag off line 1, which is the only line `compinit` reads when scanning
//!   `$fpath`, so the documented install produced a file zsh silently
//!   declined to register;
//! * a quoting bug in generated code is a syntax error at source time, which
//!   only the shell's own parser can tell you about.
//!
//! Skipping where a shell is absent is allowed; skipping everywhere is not.
//! See `at_least_one_posix_shell_checked_the_scripts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::completions::{script_for, working_shell};

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
fn bash_and_zsh_scripts_pass_their_own_syntax_check() {
    for shell in ["bash", "zsh"] {
        let Some(program) = working_shell(shell) else {
            eprintln!("skipping {shell}: no working {shell} on this machine");
            continue;
        };
        // Fed on stdin rather than as a path: `-n` reads the script from
        // stdin when given no file, so there is no native path for a shell
        // with different path conventions to fail to open.
        let script = script_for(shell);
        let mut child = std::process::Command::new(&program)
            .arg("-n")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), script.as_bytes()).unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        // Both streams: the failure that sent us here reported an empty
        // stderr because the shell had written its complaint to stdout.
        assert!(
            output.status.success(),
            "{shell} script fails {shell} -n (exit {:?})\nstderr: {}\nstdout: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim(),
        );
    }
}

#[test]
fn zsh_registers_the_generated_completion() {
    // The end-to-end check the substring tests could not make: install the
    // script exactly as the README says and ask zsh whether it registered.
    let Some(zsh) = working_shell("zsh") else {
        eprintln!("skipping: no working zsh on this machine");
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

/// Skipping is allowed where a POSIX shell is not part of the platform, but
/// it must not become the normal outcome.
///
/// Without this, a CI image that stopped shipping bash would turn every
/// check in this file into a no-op and nothing would say so - and these are
/// what stand between a quoting bug in generated code and a completion
/// script that breaks the user's shell.
#[test]
#[cfg(unix)]
fn at_least_one_posix_shell_checked_the_scripts() {
    let usable: Vec<_> = ["bash", "zsh"]
        .into_iter()
        .filter(|shell| working_shell(shell).is_some())
        .collect();
    assert!(
        !usable.is_empty(),
        "neither bash nor zsh was usable, so the generated scripts were \
         never parsed by a shell on a platform where one is expected"
    );
}
