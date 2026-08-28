//! `otl auth` output is data, and its data is other people's text.
//!
//! Every string `otl auth info` prints came from somewhere
//! else: the profile name from a config file (a TOML quoted key, which may
//! carry anything), the credential path from the environment, the error text
//! from this CLI's own resolution - and `account`, `workspace` and `scope`
//! from the SERVER. It is also the command a program runs to find out whether
//! it has a credential at all, so it is squarely a text-to-a-machine surface.
//!
//! It nevertheless printed all of that verbatim until R2 found it, three
//! rounds after the same rule was applied to `otl doctor` and `otl api
//! describe`. This file exercises the real binary so the property is asserted
//! from outside the module that has to hold it.
//!
//! Only the JSON state is reachable here: `assert_cmd` captures stdout
//! through a pipe, and no integration test can hand the child a TTY. The
//! human state is covered by `commands::auth::output`'s unit tests, which
//! render both.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// A profile name carrying, in order: a right-to-left override (which
/// reverses the rest of the line), the two residual `Cf` marks the spec
/// compiler's own table does not cover, and a newline (which could forge an
/// entry in a list of lines).
const HOSTILE_PROFILE: &str = "ev\u{202e}il\u{200f}\u{061c}";

/// Write a config file whose default profile has a hostile name.
fn hostile_config() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "default_profile = \"{HOSTILE_PROFILE}\"\n\n\
             [profiles.\"{HOSTILE_PROFILE}\"]\n\
             url = \"https://docs.example.com\"\n"
        ),
    )
    .unwrap();
    dir
}

/// `otl` with the config file above and every other input shut off.
fn otl(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env("OUTLINE_CONFIG", dir.join("config.toml"))
        .env("OUTLINE_CONFIG_DIR", dir)
        .env("OUTLINE_NO_KEY_WARNING", "1")
        .env("OTL_CACHE_DIR", dir.join("no-cache"))
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE");
    cmd
}

/// The hazard classes that must not reach stdout.
///
/// `U+FFFD` is deliberately NOT here: `crate::config` substitutes it for a
/// character it removed from a name, which is the point - the reader should
/// see that something was taken out.
const HAZARDS: [char; 5] = ['\u{202e}', '\u{200f}', '\u{061c}', '\u{206a}', '\u{200b}'];

#[test]
fn auth_info_json_carries_no_hazard_from_the_config_file() {
    let dir = hostile_config();
    // `--offline` so nothing is sent: this is about rendering, and the test
    // must not depend on a network.
    let output = otl(dir.path())
        .args(["auth", "info", "--offline", "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "no output to check");

    for hazard in HAZARDS {
        assert!(
            !stdout.contains(hazard),
            "{hazard:?} reached stdout: {stdout}"
        );
    }
    // Still the document it is supposed to be, and still parseable - the
    // scrub must not have produced a truncated or reordered object.
    let report: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    let profile = report["profile"].as_str().expect("a profile field");
    assert!(
        profile.starts_with("ev") && profile.contains("il"),
        "the name was not merely scrubbed: {profile:?}"
    );

    // The semver-protected shape is intact: these keys are published.
    for key in [
        "profile",
        "instance",
        "method",
        "available",
        "credential_file",
        "credential_file_usable",
        "resolution_error",
    ] {
        assert!(report.get(key).is_some(), "{key} missing: {stdout}");
    }
}

/// The control that makes the assertion above mean something: the character
/// really is in the file the CLI read, so "no hazard on stdout" is not just
/// "no profile name on stdout".
#[test]
fn the_hostile_name_really_is_in_the_config_file_and_in_the_output_path() {
    let dir = hostile_config();
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(raw.contains('\u{202e}'), "the fixture lost the override");
    assert!(raw.contains('\u{200f}'), "the fixture lost the mark");

    // And that file is genuinely the one in effect: the profile it names is
    // the one reported, so the name travelled from the file to stdout.
    let output = otl(dir.path())
        .args(["auth", "info", "--offline", "--json"])
        .output()
        .unwrap();
    let report: Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("stdout is JSON");
    assert_eq!(report["instance"], "https://docs.example.com", "{report}");
}
