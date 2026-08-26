//! The shape of a `doctor` report, and its two renderings.
//!
//! One check produces one [`Check`]; the report is the ordered list of them.
//! The order is the DEPENDENCY order (configuration, then credentials, then
//! the instance, then the spec), which is what makes
//! [`Report::blocking`] meaningful: the first blocking finding is the one to
//! fix first, and everything after it was measured in a broken environment.
//!
//! # Nothing here may print a credential
//!
//! Every line and every fact is built from paths, booleans, authored labels
//! and names taken from a compiled operation table. There is deliberately no
//! field a secret could travel in, and
//! `the_report_never_carries_a_credential_field` pins it from the other
//! side.
//!
//! # The human rendering is scrubbed at the sink
//!
//! `doctor`'s report goes to stdout, so it does not pass through
//! [`crate::stdio::write_diagnostic_line`]'s scrub - and it interpolates
//! text this CLI did not write: a profile name from a config file, a path
//! from an environment variable, an operation name from a fetched document,
//! an error message from a server. So the scrub happens here, once, for
//! every line of every check ([`human_line`]), exactly the way `stdio` does
//! it for stderr. Doing it per call site is how a surface ends up with one
//! forgotten interpolation.
//!
//! `--json` is exempt, for the reason stated in [`crate::text`]: JSON is the
//! payload, not a rendering, and altering it to protect a terminal would
//! corrupt the data a script consumes.

use serde_json::{Map, Value};

use crate::exit::{CliError, ExitCode};
use crate::render::{self, OutputMode};
use crate::stdio;

/// Width of the bracketed status column in human output.
const STATUS_WIDTH: usize = 9;

/// Indentation of a check's detail lines, aligned under the check name:
/// `[` + the padded status field + one space.
const DETAIL_INDENT: &str = "           ";

/// What one check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing to do.
    Ok,
    /// Worth knowing, but the environment works as it stands.
    Warn,
    /// The environment does not work until this is fixed.
    ///
    /// Carries the exit code that same condition produces in any other
    /// command: `doctor` invents no code of its own, so a script can branch
    /// on `doctor`'s exit exactly as it branches on `otl api`'s.
    Problem(ExitCode),
    /// Not checked, with the reason in the summary.
    Skipped,
}

impl Status {
    /// The label shown in human output.
    ///
    /// `PROBLEM` is upper-case for the same reason `TOO OPEN` is in the
    /// credential report: it is the one thing a reader must not skim past.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Problem(_) => "PROBLEM",
            Self::Skipped => "skipped",
        }
    }

    /// The machine-readable form.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Problem(_) => "problem",
            Self::Skipped => "skipped",
        }
    }

    /// The exit code of a blocking finding.
    pub fn code(&self) -> Option<ExitCode> {
        match self {
            Self::Problem(code) => Some(*code),
            _ => None,
        }
    }
}

/// One check's result.
#[derive(Debug, Clone)]
pub struct Check {
    /// Stable machine key (`credentials`, `connectivity`, ...).
    pub key: &'static str,
    /// What the check found.
    pub status: Status,
    /// One-line verdict.
    pub summary: String,
    /// Supporting lines, one per entry; never multi-line (see
    /// [`Check::detailed`]).
    pub detail: Vec<String>,
    /// Machine-readable facts, in the order they should appear in JSON.
    pub facts: Vec<(&'static str, Value)>,
}

impl Check {
    /// A check with no detail and no facts.
    pub fn new(key: &'static str, status: Status, summary: impl Into<String>) -> Self {
        Self {
            key,
            status,
            summary: summary.into(),
            detail: Vec::new(),
            facts: Vec::new(),
        }
    }

    /// Add detail lines, splitting anything multi-line into one entry per
    /// line.
    ///
    /// The split is what keeps the human rendering honest: a value that
    /// arrived with a newline in it would otherwise print as an extra,
    /// unlabelled line that looks like a finding of its own. Multi-line
    /// remedies (an engine error plus its hint) are legitimately several
    /// entries.
    #[must_use]
    pub fn detailed(mut self, lines: impl IntoIterator<Item = String>) -> Self {
        for line in lines {
            self.detail
                .extend(line.lines().map(|part| part.trim_end().to_string()));
        }
        self
    }

    /// Add one machine-readable fact.
    #[must_use]
    pub fn fact(mut self, name: &'static str, value: impl Into<Value>) -> Self {
        self.facts.push((name, value.into()));
        self
    }

    /// The JSON object for this check.
    fn value(&self) -> Value {
        let mut object = Map::new();
        object.insert("check".to_string(), Value::from(self.key));
        object.insert("status".to_string(), Value::from(self.status.key()));
        object.insert("summary".to_string(), Value::from(self.summary.clone()));
        for (name, value) in &self.facts {
            object.insert((*name).to_string(), value.clone());
        }
        object.insert(
            "exit_code".to_string(),
            match self.status.code() {
                Some(code) => Value::from(code as u8),
                None => Value::Null,
            },
        );
        object.insert("detail".to_string(), Value::from(self.detail.clone()));
        Value::Object(object)
    }

    /// The human lines for this check.
    fn lines(&self) -> Vec<String> {
        let mut lines = vec![human_line(&format!(
            "[{:width$} {}: {}",
            format!("{}]", self.status.label()),
            self.key,
            self.summary,
            width = STATUS_WIDTH,
        ))];
        lines.extend(
            self.detail
                .iter()
                .map(|line| human_line(&format!("{DETAIL_INDENT}{line}"))),
        );
        lines
    }
}

/// One line of human output, with everything a terminal would execute
/// removed.
///
/// The line is also forced onto ONE line: a foreign value that arrived with
/// a newline must not be able to pose as another check's verdict.
fn human_line(text: &str) -> String {
    stdio::scrub_terminal_controls(text).replace('\n', " ")
}

// --- fact values -------------------------------------------------------
//
// The small conversions every check uses to build a `--json` fact. They live
// with the JSON shape rather than with any one check, because both check
// modules produce facts and a second copy of "how an absent value is
// rendered" is how two checks come to disagree about `null`.

/// A `Some` string as JSON, `null` otherwise.
pub(super) fn optional(value: &Option<String>) -> Value {
    value.clone().map_or(Value::Null, Value::from)
}

/// A `Some` number as JSON, `null` otherwise.
pub(super) fn optional_number(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::from)
}

/// A path as JSON, sanitized because it can come from an environment
/// variable or a flag, and `null` when there is none.
pub(super) fn path_value(path: Option<&std::path::Path>) -> Value {
    path.map(crate::config::sanitize_path)
        .map_or(Value::Null, Value::from)
}

/// Everything `otl doctor` found.
#[derive(Debug, Clone)]
pub struct Report {
    /// The checks, in dependency order.
    pub checks: Vec<Check>,
}

impl Report {
    /// The first blocking finding, which decides the exit code.
    ///
    /// FIRST, not worst: the checks run in dependency order, so an earlier
    /// problem is both the cause of what follows and the thing to fix first.
    /// Reporting the "worst" code instead would point a user at a network
    /// failure that is really a missing URL.
    pub fn blocking(&self) -> Option<&Check> {
        self.checks
            .iter()
            .find(|check| matches!(check.status, Status::Problem(_)))
    }

    /// The exit code for the whole run.
    pub fn exit_code(&self) -> ExitCode {
        self.blocking()
            .and_then(|check| check.status.code())
            .unwrap_or(ExitCode::Success)
    }

    /// How many checks reported a problem, and how many a warning.
    fn counts(&self) -> (usize, usize) {
        let count = |wanted: fn(&Status) -> bool| {
            self.checks
                .iter()
                .filter(|check| wanted(&check.status))
                .count()
        };
        (
            count(|status| matches!(status, Status::Problem(_))),
            count(|status| matches!(status, Status::Warn)),
        )
    }

    /// The report as human-readable lines.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for check in &self.checks {
            lines.extend(check.lines());
        }
        lines.push(String::new());
        lines.push(human_line(&self.verdict()));
        lines
    }

    /// The closing line: what was found, and what it means for the exit code.
    fn verdict(&self) -> String {
        let (problems, warnings) = self.counts();
        if problems == 0 && warnings == 0 {
            return "no problems found.".to_string();
        }
        let code = self.exit_code() as u8;
        format!(
            "{problems} problem(s), {warnings} warning(s); exit code {code}. \
             A warning does not stop otl from working."
        )
    }

    /// The report as one JSON object.
    ///
    /// `healthy` means "nothing is BLOCKING", not "nothing was found": it is
    /// `exit_code == 0` by another name, and it is deliberately reported
    /// beside `warnings` so a consumer can tell the two apart. A run with
    /// three warnings and no problem is healthy - `otl` works in that
    /// environment - and `warnings > 0` is how a caller that cares finds out.
    pub fn value(&self) -> Value {
        let (problems, warnings) = self.counts();
        let mut object = Map::new();
        object.insert(
            "healthy".to_string(),
            Value::from(self.blocking().is_none()),
        );
        object.insert("problems".to_string(), Value::from(problems));
        object.insert("warnings".to_string(), Value::from(warnings));
        object.insert("exit_code".to_string(), Value::from(self.exit_code() as u8));
        object.insert(
            "checks".to_string(),
            Value::Array(self.checks.iter().map(Check::value).collect()),
        );
        Value::Object(object)
    }
}

/// Print a report: human lines on a terminal, one JSON object otherwise.
///
/// The report is DATA and goes to stdout, like `otl auth info`'s: it is what
/// the user asked for, not a diagnostic about something else they asked for.
/// The one thing on stderr is the blocking finding, printed by `main` as the
/// error that carries the exit code.
pub fn emit(report: &Report, mode: OutputMode) -> Result<(), CliError> {
    match mode {
        OutputMode::Json => {
            let rendered = render::render_json(&report.value()).map_err(|error| {
                CliError::failure(anyhow::anyhow!("failed to render the report: {error}"))
            })?;
            stdio::write_data_line(&rendered)
        }
        OutputMode::Table => stdio::write_data_line(&report.lines().join("\n")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn report(statuses: Vec<(&'static str, Status)>) -> Report {
        Report {
            checks: statuses
                .into_iter()
                .map(|(key, status)| Check::new(key, status, "summary"))
                .collect(),
        }
    }

    #[test]
    fn a_clean_report_exits_zero_and_says_so() {
        let report = report(vec![("instance", Status::Ok), ("credentials", Status::Ok)]);
        assert_eq!(report.exit_code(), ExitCode::Success);
        assert!(report.blocking().is_none());
        let rendered = report.lines().join("\n");
        assert!(rendered.contains("no problems found"), "{rendered}");
        assert_eq!(report.value()["healthy"], Value::from(true));
        assert_eq!(report.value()["exit_code"], Value::from(0));
    }

    /// The rule that makes the exit code mean "fix this first": an earlier
    /// check's code wins over a later one's, even when the later one is
    /// numerically higher.
    #[test]
    fn the_first_blocking_check_decides_the_exit_code() {
        let report = report(vec![
            ("instance", Status::Ok),
            ("credentials", Status::Problem(ExitCode::Usage)),
            ("connectivity", Status::Problem(ExitCode::Network)),
        ]);
        assert_eq!(report.exit_code(), ExitCode::Usage);
        assert_eq!(
            report.blocking().map(|check| check.key),
            Some("credentials")
        );
        assert_eq!(report.value()["exit_code"], Value::from(2));
        assert_eq!(report.value()["healthy"], Value::from(false));
    }

    /// A warning is not a problem: it is reported and the exit code stays 0,
    /// which is the whole difference between "your spec is behind" and "you
    /// cannot reach your instance".
    #[test]
    fn warnings_alone_never_change_the_exit_code() {
        let report = report(vec![
            ("local-spec", Status::Warn),
            ("online-spec", Status::Warn),
            ("connectivity", Status::Skipped),
        ]);
        assert_eq!(report.exit_code(), ExitCode::Success);
        assert_eq!(report.counts(), (0, 2));
        let rendered = report.lines().join("\n");
        assert!(rendered.contains("2 warning(s)"), "{rendered}");
        assert!(rendered.contains("exit code 0"), "{rendered}");
    }

    #[test]
    fn every_status_is_counted_and_labelled() {
        let report = report(vec![
            ("a", Status::Ok),
            ("b", Status::Warn),
            ("c", Status::Problem(ExitCode::Auth)),
            ("d", Status::Skipped),
        ]);
        assert_eq!(report.counts(), (1, 1));
        let rendered = report.lines().join("\n");
        for label in ["[ok]", "[warn]", "[PROBLEM]", "[skipped]"] {
            assert!(rendered.contains(label), "{label} missing from {rendered}");
        }
        let value = report.value();
        let statuses: Vec<&str> = value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|check| check["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, vec!["ok", "warn", "problem", "skipped"]);
        assert_eq!(value["checks"][2]["exit_code"], Value::from(4));
        assert_eq!(value["checks"][0]["exit_code"], Value::Null);
    }

    /// The scrub that has to be at the sink: a value that arrived from a
    /// config file, a server or a document cannot reach the terminal with
    /// escape sequences intact, and cannot forge a line of its own.
    #[test]
    fn human_output_is_scrubbed_and_kept_on_one_line() {
        let check = Check::new(
            "credentials",
            Status::Warn,
            "profile \u{1b}]52;c;cGF3bmVk\u{7}evil",
        )
        .detailed(vec!["first\nsecond".to_string()]);
        let report = Report {
            checks: vec![check],
        };
        let lines = report.lines();
        assert!(
            !lines.iter().any(|line| line.contains('\u{1b}')),
            "an escape sequence survived: {lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains('\u{7}')),
            "a BEL survived: {lines:?}"
        );
        // The multi-line detail became two detail entries, both indented,
        // rather than one line that ends the report early.
        assert!(lines.iter().any(|line| line.trim() == "first"), "{lines:?}");
        assert!(
            lines.iter().any(|line| line.trim() == "second"),
            "{lines:?}"
        );
        assert!(
            lines.iter().all(|line| !line.contains('\n')),
            "a rendered line still contains a newline: {lines:?}"
        );
    }

    #[test]
    fn the_report_never_carries_a_credential_field() {
        let check = Check::new("credentials", Status::Ok, "stored")
            .fact("credential_file", "/tmp/credentials.toml")
            .fact("permissions", "0600 (owner read/write only)");
        let rendered = serde_json::to_string(
            &Report {
                checks: vec![check],
            }
            .value(),
        )
        .unwrap();
        for forbidden in [
            "access_token",
            "refresh_token",
            "api_key",
            "client_secret",
            "registration_access_token",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden} appears in doctor output: {rendered}"
            );
        }
    }

    /// Facts keep the order they were added: the JSON is read by people as
    /// well as by `jq`, and a shuffled object is harder to diff.
    #[test]
    fn json_facts_keep_insertion_order() {
        let check = Check::new("local-spec", Status::Ok, "built-in")
            .fact("operations", 113)
            .fact("synced", false)
            .fact("cache_path", "/tmp/ir-cache.bin");
        let value = check.value();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "check",
                "status",
                "summary",
                "operations",
                "synced",
                "cache_path",
                "exit_code",
                "detail"
            ]
        );
    }
}
