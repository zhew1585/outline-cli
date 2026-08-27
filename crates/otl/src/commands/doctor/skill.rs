//! The skill check: does the agent skill installed here match this binary?
//!
//! `otl skill install` copies a document into a directory an agent reads on
//! its own, and from then on the two can drift: the CLI is upgraded, the
//! copy is not, and an agent goes on following instructions written for an
//! older contract. Nothing tells anyone, because both halves keep working.
//!
//! So this check compares each installed copy with the document this binary
//! carries. It is LOCAL - it stats and reads files under the user's home
//! directory and contacts nothing - and it is never blocking: a stale skill,
//! or none at all, does not stop a single `otl` command from working.
//!
//! # Every state gets its own verdict, and its own remedy
//!
//! The states are not interchangeable and must not share a sentence. An
//! absent skill is nothing to fix. A copy that is behind is fixed by
//! `otl skill install`. A copy belonging to a DIFFERENT skill is not, and
//! saying so would prescribe a command that then refuses (exit 2) - so that
//! case names `--force`, and an unusable path says to look at it. This is
//! why [`State`] is an enum and not a boolean: the first version of this
//! check collapsed all three into "not installed", which reported `ok` for
//! a foreign document and told the user to run a command that could not
//! work.

use serde_json::Value;

use crate::commands::skill::{self, Installed, Target};

use super::report::{Check, Status};

/// The check key, which is also a `--json` object key.
const KEY: &str = "skill";

/// The command that fixes a copy of this skill that is out of step.
const REINSTALL: &str = "run `otl skill install`";

/// Compare every installed copy with the document this binary carries.
pub fn check() -> Check {
    let targets = skill::resolve(None);
    if targets.is_empty() {
        return Check::new(
            KEY,
            Status::Skipped,
            "no agent skills directory on this machine",
        )
        .fact("version", skill::SKILL_VERSION)
        .fact("installed", Value::Array(Vec::new()));
    }
    let found: Vec<Found> = targets.iter().map(examine).collect();
    Check::new(KEY, status(&found), summary(&found))
        .fact("version", skill::SKILL_VERSION)
        .fact(
            "installed",
            Value::Array(found.iter().map(Found::value).collect()),
        )
        .detailed(found.iter().map(Found::line))
}

/// What one target holds.
///
/// Each variant carries what the report needs to say about it, and
/// [`State::remedy`] is what distinguishes them for a reader: they are not
/// all fixed the same way.
enum State {
    /// The document this binary carries, byte for byte.
    Current,
    /// This skill, at another version.
    Behind(String),
    /// This skill, at this version, with different bytes.
    Edited,
    /// This skill, with no version in its frontmatter.
    Undeclared,
    /// Nothing is installed there.
    Absent,
    /// Another skill's document occupies that path.
    Foreign(Option<String>),
    /// The path cannot hold an installed copy as it stands; the string is
    /// the reason, which already carries its own remedy.
    Unusable(String),
}

impl State {
    /// Stable machine key for `--json`.
    fn key(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Behind(_) => "behind",
            Self::Edited => "edited",
            Self::Undeclared => "undeclared",
            Self::Absent => "absent",
            Self::Foreign(_) => "foreign",
            Self::Unusable(_) => "unusable",
        }
    }

    /// What is there, in words.
    fn description(&self) -> String {
        match self {
            Self::Current => "up to date".to_string(),
            Self::Behind(installed) => format!(
                "version {installed}, this binary ships {}",
                skill::SKILL_VERSION
            ),
            Self::Edited => "same version, edited locally".to_string(),
            Self::Undeclared => "installed, but declares no version".to_string(),
            Self::Absent => "not installed".to_string(),
            Self::Foreign(name) => format!(
                "another skill ({}) occupies that path",
                name.as_deref().unwrap_or("unnamed")
            ),
            Self::Unusable(reason) => reason.clone(),
        }
    }

    /// The command or action that answers this state, when one does.
    fn remedy(&self) -> Option<&'static str> {
        match self {
            Self::Current => None,
            Self::Behind(_) | Self::Edited | Self::Undeclared | Self::Absent => Some(REINSTALL),
            Self::Foreign(_) => Some("run `otl skill install --force` to replace it"),
            // The reason already ends in its own remedy.
            Self::Unusable(_) => None,
        }
    }

    /// The version installed there, when this skill is installed there.
    fn version(&self) -> Option<String> {
        match self {
            Self::Current => Some(skill::SKILL_VERSION.to_string()),
            Self::Behind(installed) => Some(installed.clone()),
            Self::Edited => Some(skill::SKILL_VERSION.to_string()),
            _ => None,
        }
    }

    /// Whether this target holds the current document.
    fn matches(&self) -> bool {
        matches!(self, Self::Current)
    }

    /// Whether this target is something the user may want to act on.
    ///
    /// `Absent` is deliberately NOT one: an `otl` driven only by people
    /// needs no skill, and a doctor that warned about it would nag every
    /// run of a perfectly sound environment.
    fn needs_attention(&self) -> bool {
        !matches!(self, Self::Current | Self::Absent)
    }
}

/// One target and what it holds.
struct Found {
    path: String,
    source: &'static str,
    state: State,
}

impl Found {
    fn value(&self) -> Value {
        serde_json::json!({
            "path": self.path,
            "source": self.source,
            "state": self.state.key(),
            "version": self.state.version().map_or(Value::Null, Value::from),
            "matches": self.state.matches(),
            "remedy": self.state.remedy().map_or(Value::Null, Value::from),
        })
    }

    fn line(&self) -> String {
        let mut line = format!(
            "{} ({}): {}",
            self.path,
            self.source,
            self.state.description()
        );
        if let Some(remedy) = self.state.remedy() {
            line.push_str(&format!(" - {remedy}"));
        }
        line
    }
}

/// Examine one target without changing anything.
fn examine(target: &Target) -> Found {
    let state = match skill::inspect(target) {
        Installed::Ours {
            current: true,
            version: _,
        } => State::Current,
        Installed::Ours { version, .. } => match version {
            Some(installed) if installed == skill::SKILL_VERSION => State::Edited,
            Some(installed) => State::Behind(installed),
            None => State::Undeclared,
        },
        Installed::Absent => State::Absent,
        Installed::Foreign { name } => State::Foreign(name),
        Installed::Unusable { reason } => State::Unusable(reason),
    };
    Found {
        path: crate::config::sanitize_path(&target.file()),
        source: target.source.label(),
        state,
    }
}

/// Warn about anything out of step; never block.
fn status(found: &[Found]) -> Status {
    if found.iter().any(|found| found.state.needs_attention()) {
        Status::Warn
    } else {
        Status::Ok
    }
}

/// The one-line verdict.
///
/// The counts are what a reader needs first; the per-target lines below the
/// summary carry which state each one is in and how to answer it.
fn summary(found: &[Found]) -> String {
    let version = skill::SKILL_VERSION;
    let attention = found
        .iter()
        .filter(|found| found.state.needs_attention())
        .count();
    if attention > 0 {
        return format!(
            "version {version}; {attention} installed {} out of step (see below)",
            plural(attention, "copy is", "copies are")
        );
    }
    let current = found.iter().filter(|found| found.state.matches()).count();
    if current == 0 {
        return format!("version {version}, not installed ({REINSTALL})");
    }
    format!(
        "version {version}, up to date in {current} {}",
        plural(current, "place", "places")
    )
}

/// Pick a word by count, so a report does not say "1 copy/copies".
fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 {
        one
    } else {
        many
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::commands::skill::{Source, SKILL_MARKDOWN, SKILL_NAME, SKILL_VERSION};

    fn target(root: &std::path::Path) -> Target {
        Target {
            root: root.to_path_buf(),
            source: Source::Flag,
        }
    }

    fn write(target: &Target, document: &str) {
        std::fs::create_dir_all(target.dir()).unwrap();
        std::fs::write(target.file(), document).unwrap();
    }

    fn found_at(target: &Target) -> Found {
        examine(target)
    }

    #[test]
    fn an_up_to_date_copy_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        write(&target, SKILL_MARKDOWN);
        let found = vec![found_at(&target)];
        assert_eq!(found[0].state.key(), "current");
        assert_eq!(found[0].state.version().as_deref(), Some(SKILL_VERSION));
        assert_eq!(status(&found), Status::Ok);
        assert!(summary(&found).contains("up to date in 1 place"));
    }

    /// The finding the check exists for: an older copy an agent is still
    /// reading. A warning, with the remedy, and never an exit code.
    #[test]
    fn an_older_copy_warns_and_names_both_versions() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        write(
            &target,
            &SKILL_MARKDOWN.replace(&format!("version: {SKILL_VERSION}"), "version: 0.0.1"),
        );
        let found = vec![found_at(&target)];
        assert_eq!(found[0].state.key(), "behind");
        let line = found[0].line();
        assert!(line.contains("0.0.1"), "{line}");
        assert!(line.contains(SKILL_VERSION), "{line}");
        assert!(line.contains("otl skill install"), "{line}");
        assert_eq!(status(&found), Status::Warn);
    }

    /// A skill that was never installed is not a fault, so it does not
    /// warn - but the report still says how to install it.
    #[test]
    fn nothing_installed_is_reported_without_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let found = vec![found_at(&target(dir.path()))];
        assert_eq!(found[0].state.key(), "absent");
        assert_eq!(found[0].state.version(), None);
        assert_eq!(status(&found), Status::Ok);
        assert!(
            summary(&found).contains("otl skill install"),
            "the remedy is missing"
        );
    }

    #[test]
    fn a_hand_edited_copy_of_the_same_version_is_named_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        write(&target, &format!("{SKILL_MARKDOWN}\nlocal note\n"));
        let found = vec![found_at(&target)];
        assert_eq!(found[0].state.key(), "edited");
        assert!(found[0].line().contains("edited locally"));
        assert_eq!(status(&found), Status::Warn);
    }

    /// The regression this file's header is about: a foreign document is a
    /// WARNING, and its remedy is the one that actually works. Reporting
    /// `ok` plus a bare `otl skill install` sent the user at a command that
    /// refuses with exit 2.
    #[test]
    fn a_foreign_document_warns_and_prescribes_force() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        write(&target, "---\nname: someone-elses\nversion: 1.0.0\n---\n");
        let found = vec![found_at(&target)];
        assert_eq!(found[0].state.key(), "foreign");
        assert_eq!(status(&found), Status::Warn);
        let line = found[0].line();
        assert!(line.contains("someone-elses"), "{line}");
        assert!(line.contains("--force"), "{line}");
        assert!(found[0].path.contains(SKILL_NAME), "{}", found[0].path);
        assert!(
            summary(&found).contains("out of step"),
            "{found:?}",
            found = summary(&found)
        );
    }

    /// A path that cannot hold a copy is a warning too, and it must not be
    /// told to reinstall: the install refuses that path on purpose.
    #[cfg(unix)]
    #[test]
    fn an_unusable_path_warns_and_does_not_prescribe_reinstalling() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, target.dir()).unwrap();
        let found = vec![found_at(&target)];
        assert_eq!(found[0].state.key(), "unusable");
        assert_eq!(status(&found), Status::Warn);
        let line = found[0].line();
        assert!(line.contains("is a link to somewhere else"), "{line}");
        assert!(
            !line.contains("run `otl skill install`"),
            "prescribes a command that refuses this path: {line}"
        );
    }

    /// The check reports state and nothing else: no credential, and no
    /// content of the installed document, reaches the report.
    #[test]
    fn the_check_carries_only_paths_versions_and_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        write(&target, SKILL_MARKDOWN);
        let value = found_at(&target).value();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec!["path", "source", "state", "version", "matches", "remedy"]
        );
    }
}
