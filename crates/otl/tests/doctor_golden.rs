//! Golden-file test for `otl doctor`'s human rendering.
//!
//! Per project rule, all output rendering is covered by golden files.
//! Regenerate with: `OTL_UPDATE_GOLDEN=1 cargo test -p outline-cli --test
//! doctor_golden` (then review the diff by eye before committing).
//!
//! The report here is SYNTHETIC. A real one names the machine's credential
//! path, its operation count and its clock, so a golden file over real
//! output would pin the developer's environment instead of the layout - and
//! it is the layout (status column, indentation, the closing verdict) that a
//! golden file is good at protecting.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use otl::commands::doctor::report::{Check, Report, Status};
use otl::exit::ExitCode;

/// Whether golden files may be rewritten instead of asserted.
///
/// Requires the exact value `1`, and never applies when `CI` is set: on CI a
/// rendering regression must fail, not overwrite the evidence.
fn golden_update_requested() -> bool {
    std::env::var("OTL_UPDATE_GOLDEN").ok().as_deref() == Some("1") && std::env::var("CI").is_err()
}

fn assert_golden(rendered: &str, golden_name: &str) {
    let rendered = format!("{rendered}\n");
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(golden_name);
    if golden_update_requested() {
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read golden file {}: {error}", path.display()));
    assert_eq!(
        rendered, expected,
        "doctor output does not match golden file {golden_name}"
    );
}

/// A report with every status, a multi-line detail, and a value carrying
/// terminal escapes and a newline - so the golden file records both the
/// layout and the scrub.
fn mixed_report() -> Report {
    Report {
        checks: vec![
            Check::new(
                "configuration",
                Status::Ok,
                "profile work, selected by --profile",
            ),
            Check::new(
                "instance",
                Status::Ok,
                "https://docs.example.com (url from OUTLINE_URL)",
            ),
            Check::new(
                "credentials",
                Status::Problem(ExitCode::Usage),
                "the credential file cannot be used as it stands",
            )
            .detailed([
                "credential file:  /home/u/.config/outline-cli/credentials.toml".to_string(),
                "permissions:      0644 - TOO OPEN, other users can read it; \
                 run `chmod 600` on it"
                    .to_string(),
            ]),
            Check::new("credential", Status::Skipped, "not checked: nothing to try"),
            Check::new(
                "connectivity",
                Status::Skipped,
                "--offline: the instance was not contacted",
            ),
            Check::new(
                "local-spec",
                Status::Warn,
                // A hostile value, as a cache's provenance record or a
                // server message could carry: it must reach the terminal
                // inert and on one line.
                "the synced spec cache was discarded\u{1b}]52;c;cGF3bmVk\u{7}",
            )
            .detailed([
                "run `otl spec sync` to rebuild it\nor `otl spec reset` to drop it".to_string(),
            ]),
            Check::new(
                "online-spec",
                Status::Warn,
                "2 missing, 0 withdrawn upstream, 1 deprecated upstream",
            )
            .detailed([
                "missing here (run `otl spec sync` to get them): things.brandNew, things.other"
                    .to_string(),
                "deprecated online, still callable here: things.old".to_string(),
            ]),
            Check::new(
                "skill",
                Status::Warn,
                "version 1.0.0; 1 installed copy does not match it (run `otl skill install`)",
            )
            .detailed([
                "/home/u/.claude/skills/outline-cli/SKILL.md (Claude Code): version 0.9.0, \
                 this binary ships 1.0.0"
                    .to_string(),
            ]),
        ],
    }
}

#[test]
fn the_human_report_matches_its_golden_file() {
    let report = mixed_report();
    assert_golden(&report.lines().join("\n"), "doctor_report.txt");
}

/// The golden file is evidence for the two properties that matter most, so
/// they are also asserted directly: a reviewer regenerating the file by
/// mistake would otherwise bless an escape sequence.
#[test]
fn the_golden_rendering_is_inert_and_one_line_per_entry() {
    let lines = mixed_report().lines();
    assert!(
        !lines.iter().any(|line| line.contains('\u{1b}')),
        "an escape sequence reached the terminal: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains('\n')),
        "a rendered entry spans lines: {lines:?}"
    );
    assert_eq!(mixed_report().exit_code(), ExitCode::Usage);
}
