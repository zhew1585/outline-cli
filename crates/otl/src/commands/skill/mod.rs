//! `otl skill install` and `otl skill show` - the agent skill that ships
//! with this binary.
//!
//! # Why the CLI carries a skill at all
//!
//! An agent driving `otl` has to learn three things that no `--help` page
//! puts in one place: which commands are stable, how to discover an
//! operation's contract without calling it, and what to do about the
//! environment when a command exits 2 or 4. The skill document is that one
//! place, and shipping it inside the binary is what keeps it from drifting:
//! the version an agent reads is the version of the CLI it is driving.
//!
//! # Nothing is fetched, nothing is discovered
//!
//! The document is compiled in by `build.rs`, exactly like the IR table, so
//! `otl skill show` and `otl doctor`'s skill check work offline and on a
//! machine with no spec, no credentials and no network. Installing is a
//! local file copy into directories that already exist.
//!
//! # What "install" is allowed to overwrite
//!
//! Its own document, and nothing else. A copy this CLI wrote (recognized by
//! the `name` in its frontmatter) is replaced, because that is what an
//! upgrade is. Another skill's `SKILL.md` needs `--force`.
//!
//! Two things are refused whatever the flags say, because following either
//! turns an install into a write somewhere else entirely: a document path
//! that is not a regular file, and a `<skills dir>/<skill>` directory that
//! is a symlink. The skills directory ABOVE that may be a symlink - it is
//! the one the user named or the one an agent created, and pointing it at a
//! dotfiles checkout is ordinary - so the line is drawn exactly where this
//! command stops following the user and starts creating paths of its own.

mod targets;

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::anyhow;
use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::config::sanitize_path;
use crate::exit::CliError;
use crate::render::{self, OutputMode};
use crate::stdio;

// Defines `SKILL_MARKDOWN`, `SKILL_NAME` and `SKILL_VERSION` from
// `skill/SKILL.md`, which is where the version is authored.
include!(concat!(env!("OUT_DIR"), "/skill_asset.rs"));

pub use targets::{frontmatter_value, inspect, resolve, Installed, Source, Target, ENV_SKILL_DIR};

/// Arguments for `otl skill`.
#[derive(Debug, Args)]
pub struct SkillArgs {
    #[command(subcommand)]
    command: SkillCommand,
}

#[derive(Debug, Subcommand)]
enum SkillCommand {
    /// Install (or upgrade) the agent skill for the agents on this machine.
    Install(InstallArgs),
    /// Print the skill document this binary carries.
    Show,
}

/// Arguments for `otl skill install`.
#[derive(Debug, Args)]
#[command(
    after_long_help = "Without --dir, every agent skills directory that already exists
under your home directory is a target (Claude Code, Codex, and the
agent-agnostic ~/.agents), so one install covers the agents you actually
run. OUTLINE_SKILL_DIR names one directory instead.

`otl doctor` reports the installed version and says when to run this
again."
)]
pub struct InstallArgs {
    /// Skills directory to install into, instead of the detected ones.
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,

    /// Replace a `SKILL.md` belonging to a different skill.
    #[arg(long)]
    force: bool,
}

/// Run the `skill` subcommand.
pub fn run(args: &SkillArgs, mode: OutputMode) -> Result<(), CliError> {
    match &args.command {
        SkillCommand::Install(install) => run_install(install, mode),
        // Mode-independent on purpose: the document IS the payload, the way
        // a completion script is, and wrapping markdown in a JSON string
        // would make `otl skill show > SKILL.md` produce a file no agent
        // can read.
        SkillCommand::Show => stdio::write_data(SKILL_MARKDOWN),
    }
}

/// What happened at one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Written where nothing was installed.
    Installed,
    /// Replaced an older copy of this skill.
    Upgraded,
    /// Replaced another skill's document, which needed `--force`.
    Replaced,
    /// Already byte-for-byte the current document.
    Unchanged,
    /// Deliberately not written, with the reason.
    Refused,
    /// The write itself failed.
    Failed,
}

impl Action {
    /// Stable machine key, and the label human output uses.
    fn key(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Upgraded => "upgraded",
            Self::Replaced => "replaced",
            Self::Unchanged => "unchanged",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }

    /// Whether the document is now installed at that target.
    fn succeeded(self) -> bool {
        matches!(
            self,
            Self::Installed | Self::Upgraded | Self::Replaced | Self::Unchanged
        )
    }
}

/// One target's outcome.
#[derive(Debug, Clone)]
struct Outcome {
    path: String,
    source: &'static str,
    action: Action,
    /// The version that was there before, when there was one.
    previous: Option<String>,
    /// Why it was refused, or why the write failed.
    reason: Option<String>,
}

/// Install into every resolved target, then report all of them.
///
/// Every target is attempted before anything is reported: a machine with
/// two agents installed should not learn about the second one's problem
/// only after fixing the first's.
fn run_install(args: &InstallArgs, mode: OutputMode) -> Result<(), CliError> {
    let targets = resolve(args.dir.as_deref());
    if targets.is_empty() {
        return Err(CliError::usage(anyhow!(
            "no agent skills directory was found under your home directory.\n\
             Name one with `otl skill install --dir <DIR>`, or set {ENV_SKILL_DIR}."
        )));
    }
    let outcomes: Vec<Outcome> = targets
        .iter()
        .map(|target| install_one(target, args.force))
        .collect();
    emit(&outcomes, mode)?;
    verdict(&outcomes)
}

/// What installing into one target should do, decided before anything is
/// written so that the refusals are all in one place.
enum Plan {
    /// Write the document; the action is what that write will have been.
    Write {
        action: Action,
        previous: Option<String>,
    },
    /// It is already the current document.
    Unchanged { previous: Option<String> },
    /// Leave it alone, for this reason.
    Refuse { reason: String },
}

/// Install into one target.
fn install_one(target: &Target, force: bool) -> Outcome {
    let outcome = |action: Action, previous: Option<String>, reason: Option<String>| Outcome {
        path: sanitize_path(&target.file()),
        source: target.source.label(),
        action,
        previous,
        reason,
    };
    match plan(target, force) {
        Plan::Unchanged { previous } => outcome(Action::Unchanged, previous, None),
        Plan::Refuse { reason } => outcome(Action::Refused, None, Some(reason)),
        Plan::Write { action, previous } => match write_document(target) {
            Ok(()) => outcome(action, previous, None),
            Err(error) => outcome(Action::Failed, previous, Some(error.to_string())),
        },
    }
}

/// Decide what to do about whatever is installed at one target.
fn plan(target: &Target, force: bool) -> Plan {
    match inspect(target) {
        Installed::Absent => Plan::Write {
            action: Action::Installed,
            previous: None,
        },
        Installed::Ours {
            version,
            current: true,
        } => Plan::Unchanged { previous: version },
        Installed::Ours { version, .. } => Plan::Write {
            action: Action::Upgraded,
            previous: version,
        },
        Installed::Foreign { .. } if force => Plan::Write {
            action: Action::Replaced,
            previous: None,
        },
        Installed::Foreign { name } => Plan::Refuse {
            reason: format!(
                "a different skill ({}) is installed there; --force replaces it",
                name.as_deref().unwrap_or("unnamed")
            ),
        },
        // Not overridable by `--force`, deliberately: see the module note.
        // The reason arrives complete, with the remedy that fits THAT case:
        // "remove it by hand" is wrong advice for a `--dir` that names a
        // file, and it was what a single appended suffix produced.
        Installed::Unusable { reason } => Plan::Refuse { reason },
    }
}

/// Write the document, replacing whatever is there in one step.
///
/// A temporary file in the destination directory plus `persist` rather than
/// a truncate-and-write: an agent reading the file while it is rewritten
/// must never see half a document, and `persist` replaces the destination
/// on Windows too, which `std::fs::rename` does not.
fn write_document(target: &Target) -> std::io::Result<()> {
    let dir = target.dir();
    // Re-checked here, not just in `inspect`: `create_dir_all` succeeds on a
    // symlink that points at a directory, and everything after it would
    // then write through the link. Narrow rather than closed - the check and
    // the write are still two steps - but it is the difference between a
    // symlink being followed as a matter of course and only inside a race.
    if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the skill directory is a symlink",
        ));
    }
    std::fs::create_dir_all(&dir)?;
    let mut file = tempfile::Builder::new()
        .prefix(".skill-")
        .suffix(".tmp")
        .tempfile_in(&dir)?;
    file.write_all(SKILL_MARKDOWN.as_bytes())?;
    file.flush()?;
    // The document is not a secret and an agent may run as a different
    // process than the shell that installed it, so it gets ordinary
    // document permissions rather than the 0600 a temporary file is made
    // with. Credentials are the file that stays owner-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644))?;
    }
    file.persist(target.file()).map_err(|error| error.error)?;
    Ok(())
}

/// The exit code the run as a whole earns.
///
/// A partial install is code 9 for the same reason a partial export is:
/// what was written is real, and something asked for did not happen. Only
/// when nothing at all was installed does the underlying failure's own code
/// stand - a refusal is something to fix locally (2), a failed write is a
/// failure (1).
fn verdict(outcomes: &[Outcome]) -> Result<(), CliError> {
    let failed: Vec<&Outcome> = outcomes
        .iter()
        .filter(|outcome| !outcome.action.succeeded())
        .collect();
    let Some(first) = failed.first() else {
        return Ok(());
    };
    let message = anyhow!(
        "{}: {}",
        first.path,
        first
            .reason
            .as_deref()
            .unwrap_or("the skill was not installed")
    );
    if outcomes.iter().any(|outcome| outcome.action.succeeded()) {
        return Err(CliError::partial(message));
    }
    match first.action {
        Action::Failed => Err(CliError::failure(message)),
        _ => Err(CliError::usage(message)),
    }
}

/// Print what happened, in the resolved output mode.
fn emit(outcomes: &[Outcome], mode: OutputMode) -> Result<(), CliError> {
    match mode {
        // Scrubbed, not verbatim: this object is one `otl` authored, and it
        // interpolates paths that came from the environment and a skill name
        // that came from a file on disk.
        OutputMode::Json => {
            let text = render::render_json_scrubbed(&json(outcomes))
                .map_err(|error| CliError::failure(anyhow!("failed to render: {error}")))?;
            stdio::write_data_line(&text)
        }
        OutputMode::Table => stdio::write_data_line(&lines(outcomes).join("\n")),
    }
}

/// The machine-readable report.
fn json(outcomes: &[Outcome]) -> Value {
    json!({
        "skill": SKILL_NAME,
        "version": SKILL_VERSION,
        "complete": outcomes.iter().all(|outcome| outcome.action.succeeded()),
        "targets": Value::Array(outcomes.iter().map(target_json).collect()),
    })
}

fn target_json(outcome: &Outcome) -> Value {
    json!({
        "path": outcome.path,
        "source": outcome.source,
        "action": outcome.action.key(),
        "previous_version": outcome.previous.clone().map_or(Value::Null, Value::from),
        "reason": outcome.reason.clone().map_or(Value::Null, Value::from),
    })
}

/// The human report: one line per target, plus the closing summary.
fn lines(outcomes: &[Outcome]) -> Vec<String> {
    let mut lines = vec![format!("skill {SKILL_NAME} {SKILL_VERSION}")];
    for outcome in outcomes {
        let mut line = format!(
            "  {:10} {} ({})",
            outcome.action.key(),
            outcome.path,
            outcome.source
        );
        if let Some(previous) = &outcome.previous {
            line.push_str(&format!(", was {previous}"));
        }
        if let Some(reason) = &outcome.reason {
            line.push_str(&format!(": {reason}"));
        }
        lines.push(stdio::scrub_to_one_line(&line));
    }
    lines
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::exit::ExitCode;

    fn target(root: &std::path::Path) -> Target {
        Target {
            root: root.to_path_buf(),
            source: Source::Flag,
        }
    }

    #[test]
    fn a_fresh_directory_gets_the_document_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        let outcome = install_one(&target, false);
        assert_eq!(outcome.action, Action::Installed);
        assert_eq!(outcome.previous, None);
        assert_eq!(
            std::fs::read_to_string(target.file()).unwrap(),
            SKILL_MARKDOWN
        );
        // And installing again changes nothing, so a doctor run stays quiet.
        assert_eq!(install_one(&target, false).action, Action::Unchanged);
    }

    #[test]
    fn an_older_copy_is_upgraded_and_the_old_version_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        std::fs::create_dir_all(target.dir()).unwrap();
        let older = SKILL_MARKDOWN.replace(&format!("version: {SKILL_VERSION}"), "version: 0.0.1");
        std::fs::write(target.file(), older).unwrap();

        let outcome = install_one(&target, false);
        assert_eq!(outcome.action, Action::Upgraded);
        assert_eq!(outcome.previous.as_deref(), Some("0.0.1"));
        assert_eq!(
            std::fs::read_to_string(target.file()).unwrap(),
            SKILL_MARKDOWN
        );
    }

    #[test]
    fn another_skill_is_refused_until_force_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        std::fs::create_dir_all(target.dir()).unwrap();
        let foreign = "---\nname: someone-elses\nversion: 2.0.0\n---\n\n# Theirs\n";
        std::fs::write(target.file(), foreign).unwrap();

        let refused = install_one(&target, false);
        assert_eq!(refused.action, Action::Refused);
        assert!(
            refused.reason.as_deref().unwrap().contains("someone-elses"),
            "{refused:?}"
        );
        // Nothing was touched.
        assert_eq!(std::fs::read_to_string(target.file()).unwrap(), foreign);
        assert_eq!(
            verdict(&[refused]).unwrap_err().code,
            ExitCode::Usage,
            "a refusal with nothing installed is a local problem"
        );

        assert_eq!(install_one(&target, true).action, Action::Replaced);
        assert_eq!(
            std::fs::read_to_string(target.file()).unwrap(),
            SKILL_MARKDOWN
        );
    }

    /// `--force` does not extend to a path that is not a regular file:
    /// writing through a symlink is a write to wherever it points.
    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        std::fs::create_dir_all(target.dir()).unwrap();
        let elsewhere = dir.path().join("elsewhere.md");
        std::fs::write(&elsewhere, "untouched").unwrap();
        std::os::unix::fs::symlink(&elsewhere, target.file()).unwrap();

        let outcome = install_one(&target, true);
        assert_eq!(outcome.action, Action::Refused);
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "untouched");
    }

    /// One target written and one refused is a partial result (code 9), not
    /// a success and not a plain failure.
    #[test]
    fn a_mixed_run_is_partial() {
        let installed = Outcome {
            path: "/a/SKILL.md".to_string(),
            source: "--dir",
            action: Action::Installed,
            previous: None,
            reason: None,
        };
        let refused = Outcome {
            path: "/b/SKILL.md".to_string(),
            source: "Codex",
            action: Action::Refused,
            previous: None,
            reason: Some("a different skill is installed there".to_string()),
        };
        let error = verdict(&[installed.clone(), refused.clone()]).unwrap_err();
        assert_eq!(error.code, ExitCode::Partial);
        assert!(error.to_string().contains("/b/SKILL.md"), "{error}");
        assert_eq!(json(&[installed.clone(), refused])["complete"], false);
        assert_eq!(json(&[installed])["complete"], true);
    }

    /// The report names every target and stays on one line each, so a
    /// hostile value in a path or a foreign skill name cannot forge a line.
    #[test]
    fn the_human_report_is_one_inert_line_per_target() {
        let outcome = Outcome {
            path: "/a/SKILL.md".to_string(),
            source: "Claude Code",
            action: Action::Refused,
            previous: None,
            reason: Some("a different skill (\u{1b}]52;c;x\u{7}evil\nsecond) is there".to_string()),
        };
        let rendered = lines(&[outcome]);
        assert_eq!(rendered.len(), 2, "{rendered:?}");
        assert!(rendered.iter().all(|line| !line.contains('\u{1b}')));
        assert!(rendered.iter().all(|line| !line.contains('\n')));
        assert!(rendered[0].contains(SKILL_VERSION), "{rendered:?}");
    }
}
