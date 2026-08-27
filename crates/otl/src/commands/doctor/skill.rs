//! The skill check: does the agent skill installed here match this binary?
//!
//! `otl skill install` copies a document into a directory an agent reads on
//! its own, and from then on the two can drift: the CLI is upgraded, the
//! copy is not, and an agent goes on following instructions written for an
//! older contract. Nothing tells anyone, because both halves keep working.
//!
//! So this check compares the version in each installed copy with the one
//! this binary carries. It is LOCAL - it stats and reads files under the
//! user's home directory and contacts nothing - and it is never blocking:
//! a stale skill, or none at all, does not stop a single `otl` command from
//! working. Drift is a warning with the one-command remedy; everything else
//! is reported as ok.

use serde_json::Value;

use crate::commands::skill::{self, Installed, Target};

use super::report::{Check, Status};

/// The check key, which is also a `--json` object key.
const KEY: &str = "skill";

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
    let stale: Vec<&Found> = found.iter().filter(|found| found.stale).collect();
    let present = found.iter().filter(|found| found.version.is_some()).count();
    Check::new(KEY, status(&stale, present), summary(&stale, present))
        .fact("version", skill::SKILL_VERSION)
        .fact(
            "installed",
            Value::Array(found.iter().map(Found::value).collect()),
        )
        .detailed(found.iter().map(Found::line))
}

/// What one target holds.
struct Found {
    path: String,
    source: &'static str,
    /// The version installed there, when this skill is installed there.
    version: Option<String>,
    /// One line saying what is there, for the human report.
    state: String,
    /// Whether this target is a reason to run `otl skill install` again.
    stale: bool,
}

impl Found {
    fn value(&self) -> Value {
        serde_json::json!({
            "path": self.path,
            "source": self.source,
            "version": self.version.clone().map_or(Value::Null, Value::from),
            "state": self.state,
            "matches": !self.stale,
        })
    }

    fn line(&self) -> String {
        format!("{} ({}): {}", self.path, self.source, self.state)
    }
}

/// Examine one target without changing anything.
fn examine(target: &Target) -> Found {
    let (version, state, stale) = match skill::inspect(target) {
        Installed::Ours {
            version,
            current: true,
        } => (version, "up to date".to_string(), false),
        // A copy of this skill whose bytes differ: an older version, or one
        // edited by hand. Both are answered by reinstalling, and the report
        // says which it is rather than guessing.
        Installed::Ours { version, .. } => {
            let state = match &version {
                Some(installed) if installed == skill::SKILL_VERSION => {
                    "same version, edited locally".to_string()
                }
                Some(installed) => format!(
                    "version {installed}, this binary ships {}",
                    skill::SKILL_VERSION
                ),
                None => "no version declared".to_string(),
            };
            (version, state, true)
        }
        Installed::Absent => (None, "not installed".to_string(), true),
        Installed::Foreign { name } => (
            None,
            format!(
                "another skill ({}) occupies that path",
                name.as_deref().unwrap_or("unnamed")
            ),
            true,
        ),
        Installed::Unusable { reason } => (None, format!("the path {reason}"), true),
    };
    Found {
        path: crate::config::sanitize_path(&target.file()),
        source: target.source.label(),
        version,
        state,
        stale,
    }
}

/// Warn only when something is out of step; never block.
fn status(stale: &[&Found], present: usize) -> Status {
    if stale.is_empty() {
        return Status::Ok;
    }
    // Nothing installed anywhere is not a fault: an `otl` used only by
    // people needs no skill. Anything else is a copy an agent may already
    // be reading, so it is worth a warning.
    if present == 0 && stale.iter().all(|found| found.version.is_none()) {
        return Status::Ok;
    }
    Status::Warn
}

/// The one-line verdict.
fn summary(stale: &[&Found], present: usize) -> String {
    let version = skill::SKILL_VERSION;
    if stale.is_empty() {
        return format!(
            "version {version}, installed in {present} {}",
            plural(present, "place", "places")
        );
    }
    if present == 0 && stale.iter().all(|found| found.version.is_none()) {
        return format!("version {version}, not installed (run `otl skill install`)");
    }
    let count = stale.len();
    format!(
        "version {version}; {count} installed {} {} not match it (run `otl skill install`)",
        plural(count, "copy", "copies"),
        plural(count, "does", "do")
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

    #[test]
    fn an_up_to_date_copy_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        write(&target, SKILL_MARKDOWN);
        let found = examine(&target);
        assert!(!found.stale, "{}", found.state);
        assert_eq!(found.version.as_deref(), Some(SKILL_VERSION));
        assert_eq!(status(&[], 1), Status::Ok);
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
        let found = examine(&target);
        assert!(found.stale);
        assert!(found.state.contains("0.0.1"), "{}", found.state);
        assert!(found.state.contains(SKILL_VERSION), "{}", found.state);
        assert_eq!(status(&[&found], 1), Status::Warn);
        let summary = summary(&[&found], 1);
        assert!(summary.contains("otl skill install"), "{summary}");
    }

    /// A skill that was never installed is not a fault, so it does not
    /// warn - but the report still says how to install it.
    #[test]
    fn nothing_installed_is_reported_without_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let found = examine(&target(dir.path()));
        assert!(found.stale);
        assert_eq!(found.version, None);
        assert_eq!(status(&[&found], 0), Status::Ok);
        assert!(
            summary(&[&found], 0).contains("otl skill install"),
            "the remedy is missing"
        );
    }

    #[test]
    fn a_hand_edited_copy_of_the_same_version_is_named_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        write(&target, &format!("{SKILL_MARKDOWN}\nlocal note\n"));
        let found = examine(&target);
        assert!(found.stale);
        assert!(found.state.contains("edited locally"), "{}", found.state);
        assert_eq!(status(&[&found], 1), Status::Warn);
    }

    #[test]
    fn a_foreign_document_is_reported_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        write(&target, "---\nname: someone-elses\nversion: 1.0.0\n---\n");
        let found = examine(&target);
        assert!(found.stale);
        assert!(found.state.contains("someone-elses"), "{}", found.state);
        assert!(found.path.contains(SKILL_NAME), "{}", found.path);
    }

    /// The check reports state and nothing else: no credential, and no
    /// content of the installed document, reaches the report.
    #[test]
    fn the_check_carries_only_paths_and_versions() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path());
        write(&target, SKILL_MARKDOWN);
        let value = examine(&target).value();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["path", "source", "version", "state", "matches"]);
    }
}
