//! Shared fixtures for the two completion suites.
//!
//! `completions.rs` covers what the generated script CONTAINS - candidates,
//! quoting, coverage claims - and needs nothing but the binary.
//! `completions_shell.rs` covers what a real shell DOES with it, and only
//! runs where that shell exists. They are split because those are different
//! failure modes with different prerequisites, not because the combined file
//! grew past a line count.

#![allow(dead_code)]

use assert_cmd::Command;

/// Every shell the CLI must be able to generate a script for.
pub const SHELLS: [&str; 5] = ["bash", "zsh", "fish", "powershell", "elvish"];

/// `otl` with the environment scrubbed: completion generation is purely
/// local, so it must not need (or read) any configuration.
pub fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE")
        .env("OUTLINE_CONFIG", "");
    cmd
}

/// The script for one shell, with the dual-state contract checked in passing:
/// the script is data on stdout and stderr stays empty.
pub fn script_for(shell: &str) -> String {
    let output = otl().args(["completions", shell]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{shell}: {stderr}");
    assert!(stderr.is_empty(), "{shell} wrote to stderr: {stderr}");
    String::from_utf8(output.stdout).unwrap()
}

/// A shell that is present AND works, or `None`.
///
/// "Present" is deliberately not "spawn succeeded". `Command::output()`
/// returns `Ok` for a command that ran and exited non-zero, so a probe
/// testing only `is_err()` passes for anything that merely exists on PATH.
/// GitHub's Windows runners ship `C:\Windows\System32\bash.exe`, the WSL
/// launcher: with no distribution installed it spawns, fails, and writes its
/// complaint to stdout. A caller trusting that probe then reports the shell's
/// own failure as a defect in the generated script, with an empty message.
///
/// So: a successful exit, and version output that names the shell.
pub fn working_shell(name: &str) -> Option<std::path::PathBuf> {
    let probe = std::process::Command::new(name).arg("--version").output();
    let out = probe.ok()?;
    if !out.status.success() {
        return None;
    }
    if !String::from_utf8_lossy(&out.stdout)
        .to_ascii_lowercase()
        .contains(name)
    {
        return None;
    }
    Some(std::path::PathBuf::from(name))
}
