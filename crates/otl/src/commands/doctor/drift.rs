//! The two spec checks: which operation table this CLI dispatches from, and
//! how it differs from what the online API declares.
//!
//! # Almost nothing here can fail a `doctor` run
//!
//! Every finding about a spec is a warning, with ONE exception stated at the
//! end. That is a decision rather than an oversight:
//!
//! - a spec cache that cannot be used is documented as *not* an error:
//!   the CLI discards it and falls back to the spec compiled into the
//!   binary, so the environment still works;
//! - the online document lives on a third-party host that has nothing to do
//!   with the user's instance. A firewall that blocks it, or a 404 on a
//!   moved file, must not make `otl doctor` report an unusable environment -
//!   the CLI dispatches from its local table and never consults that host
//!   unless asked;
//! - drift itself is information, not damage: an operation the online API
//!   has and this build does not is a reason to run `otl spec sync`, not a
//!   reason for the exit code to say "broken".
//!
//! The exception is a `--spec-url` the fetch channel refuses locally: that
//! is not a third party failing, it is the invocation being wrong, and it is
//! graded the way `otl spec sync` grades the same mistake. See
//! [`unfetched`].
//!
//! # Nothing here writes a cache
//!
//! `doctor` reports; it must not change what the next command dispatches
//! from. The comparison compiles the fetched document in memory and throws
//! it away, and names `otl spec sync` as the remedy.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::commands::spec::{self, UpstreamTable};
use crate::exit::{CliError, ExitCode};
use crate::ops;
use crate::spec::cache;

use super::report::{Check, Status};

/// Longest list of operation names printed under one check.
///
/// The full lists are always in `--json`; this only bounds what a terminal
/// gets, because a document that renames every operation would otherwise
/// print a screenful of names nobody reads.
const MAX_LISTED_NAMES: usize = 8;

/// Characters of a spec hash shown, enough to compare by eye.
const SHORT_HASH_CHARS: usize = 12;

/// What this CLI dispatches from right now, and the state of the cache.
///
/// Returns the spec hash recorded in the cache in use, so the online check
/// can say whether the cache was built from the document it just fetched.
pub fn local_spec() -> (Check, Option<String>) {
    let operations = ops::table().len();
    let synced = ops::is_synced();
    let path = cache::path()
        .map(|path| Value::from(path.display().to_string()))
        .unwrap_or(Value::Null);
    let base = |status: Status, summary: String| {
        Check::new("local-spec", status, summary)
            .fact("operations", operations)
            .fact("synced", synced)
            .fact("cache_path", path.clone())
    };
    match cache::load() {
        Ok(None) => (
            base(
                Status::Ok,
                format!("{operations} operations, from the spec built into this binary"),
            )
            .fact("cache", Value::Null),
            None,
        ),
        Ok(Some(cached)) => {
            let hash = cached.meta.spec_hash.clone();
            (
                cached_check(base(Status::Ok, String::new()), &cached),
                Some(hash),
            )
        }
        // Discarded, not fatal: the built-in table took over and the command
        // the user ran still worked. Worth a warning because "the endpoint
        // that worked yesterday is unknown today" is otherwise a mystery.
        Err(error) => (
            base(
                Status::Warn,
                "the synced spec cache was discarded; the built-in spec is in use".to_string(),
            )
            .fact("cache", Value::Null)
            .detailed([error.to_string(), error.remedy().to_string()]),
            None,
        ),
    }
}

/// Fill in a check for a cache that loaded.
fn cached_check(check: Check, cached: &cache::CachedIr) -> Check {
    let meta = &cached.meta;
    let summary = format!(
        "{} operations, from a spec synced from {} {}",
        cached.ops.len(),
        meta.source,
        age_phrase(meta.synced_at_unix)
    );
    Check { summary, ..check }
        .fact("cache", true)
        .fact("cache_source", meta.source.clone())
        .fact("cache_spec_hash", meta.spec_hash.clone())
        .fact("cache_synced_at_unix", meta.synced_at_unix)
        .fact("cache_operations", cached.ops.len())
}

/// How the online API differs from the table in use.
pub fn online_spec(offline: bool, url: Option<&str>, cached_hash: Option<&str>) -> Check {
    if offline {
        return Check::new(
            "online-spec",
            Status::Skipped,
            "--offline: the online API description was not fetched",
        )
        .fact("checked", false);
    }
    match spec::upstream_table(url) {
        Ok(upstream) => compare(&upstream, cached_hash),
        Err(error) => unfetched(&error),
    }
}

/// The check for a comparison that could not be made.
///
/// Two quite different reasons land here, and folding them together would
/// be a defect: **the user's own flag being wrong** is not a third party's
/// failure. `--spec-url not-a-url` never reaches any host - the fetch
/// refuses it locally - and that is classed as a usage error (exit 2) for
/// `otl spec sync`. It must not become a warning here, or a CI job cannot
/// tell "the spec source is down" from "I typed the flag wrong".
///
/// So the split is made on the code the fetch domain already assigned:
/// `ExitCode::Usage` is only ever `FetchError::InvalidUrl`, i.e. nothing was
/// sent because the URL did not pass local checks. Everything else - a host
/// that is unreachable, a 404, a 500, an exhausted retry budget, a document
/// that will not compile - remains a warning, because the CLI keeps
/// dispatching from its local table and the environment still works.
///
/// Deliberately classified HERE rather than validated before the checks run:
/// pre-flight validation would mean a second copy of the fetch channel's URL
/// rules, and two copies of a rule is how they come to disagree. The
/// consequence is that this problem is graded last, so a blocking finding in
/// an earlier check preempts it - which is the same "fix the first thing
/// first" ordering the rest of the report follows.
fn unfetched(error: &CliError) -> Check {
    let local_flag = error.code == ExitCode::Usage;
    let (status, summary) = if local_flag {
        (
            Status::Problem(ExitCode::Usage),
            "the --spec-url value is not a document URL this CLI will fetch",
        )
    } else {
        (
            Status::Warn,
            "the online API description could not be fetched",
        )
    };
    let mut detail = vec![error.to_string()];
    if !local_flag {
        detail.push(
            "otl keeps dispatching from its local table; nothing about this \
             affects the commands you run."
                .to_string(),
        );
    }
    Check::new("online-spec", status, summary)
        .fact("checked", false)
        .detailed(detail)
}

/// The three ways the table in use and the online description can differ.
#[derive(Debug, Default)]
struct Drift {
    /// The online API has it; this build cannot call it at all.
    missing: Vec<String>,
    /// This build offers it; the online description no longer declares it,
    /// so calling it is a request the API may refuse.
    withdrawn: Vec<String>,
    /// Still callable here, and the online description says it is going
    /// away. Only the intersection is reported: a deprecation of something
    /// this build never had is not the user's problem.
    deprecated: Vec<String>,
}

impl Drift {
    /// How many differences there are in total.
    fn count(&self) -> usize {
        self.missing.len() + self.withdrawn.len() + self.deprecated.len()
    }
}

/// Diff the online table against the one in use.
///
/// Set-based rather than a nested scan: both sides come from documents that
/// may declare very many operations, and `contains` over two large lists is
/// quadratic before anything is printed.
fn diff(upstream: &UpstreamTable) -> Drift {
    let local: HashSet<&str> = ops::table().iter().map(|op| op.name.as_ref()).collect();
    let online: HashSet<&str> = upstream.names.iter().map(String::as_str).collect();
    let withdrawn: Vec<String> = ops::table()
        .iter()
        .map(|op| op.name.to_string())
        .filter(|name| !online.contains(name.as_str()))
        .collect();
    Drift {
        missing: sorted(
            upstream
                .names
                .iter()
                .filter(|name| !local.contains(name.as_str())),
        ),
        withdrawn: sorted(withdrawn.iter()),
        deprecated: sorted(
            upstream
                .deprecated
                .iter()
                .filter(|name| local.contains(name.as_str())),
        ),
    }
}

/// Compare the fetched table with the one in use.
fn compare(upstream: &UpstreamTable, cached_hash: Option<&str>) -> Check {
    let drift = diff(upstream);
    let status = if drift.count() == 0 {
        Status::Ok
    } else {
        Status::Warn
    };
    let summary = if drift.count() == 0 {
        format!(
            "{} operations online, all of them known to this build",
            upstream.names.len()
        )
    } else {
        format!(
            "{} missing, {} withdrawn upstream, {} deprecated upstream",
            drift.missing.len(),
            drift.withdrawn.len(),
            drift.deprecated.len()
        )
    };
    Check::new("online-spec", status, summary)
        .fact("checked", true)
        .fact("source", upstream.source.clone())
        .fact("spec_hash", upstream.spec_hash.clone())
        .fact("online_operations", upstream.names.len())
        .fact("missing", strings(&drift.missing))
        .fact("withdrawn", strings(&drift.withdrawn))
        .fact("deprecated", strings(&drift.deprecated))
        .detailed(drift_lines(&drift))
        .detailed(hash_line(upstream, cached_hash))
}

/// The human lines for a comparison, one per non-empty category.
fn drift_lines(drift: &Drift) -> Vec<String> {
    let mut lines = Vec::new();
    if !drift.missing.is_empty() {
        lines.push(format!(
            "missing here (run `otl spec sync` to get them): {}",
            listed(&drift.missing)
        ));
    }
    if !drift.withdrawn.is_empty() {
        lines.push(format!(
            "no longer declared online (calling them may fail): {}",
            listed(&drift.withdrawn)
        ));
    }
    if !drift.deprecated.is_empty() {
        lines.push(format!(
            "deprecated online, still callable here: {}",
            listed(&drift.deprecated)
        ));
    }
    lines
}

/// The line about whether the cache in use came from this exact document.
fn hash_line(upstream: &UpstreamTable, cached_hash: Option<&str>) -> Option<String> {
    let cached = cached_hash?;
    Some(if cached == upstream.spec_hash {
        format!(
            "the synced cache was built from this exact document ({})",
            short_hash(cached)
        )
    } else {
        format!(
            "the synced cache was built from a different document ({} here, {} online); \
             `otl spec sync` would rewrite it",
            short_hash(cached),
            short_hash(&upstream.spec_hash)
        )
    })
}

/// A sorted, deduplicated list of names.
fn sorted<'a>(names: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut names: Vec<String> = names.cloned().collect();
    names.sort();
    names.dedup();
    names
}

/// Names as JSON strings.
fn strings(names: &[String]) -> Vec<Value> {
    names.iter().cloned().map(Value::from).collect()
}

/// Render a name list for humans, capped in length.
fn listed(names: &[String]) -> String {
    let head = names
        .iter()
        .take(MAX_LISTED_NAMES)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match names.len().checked_sub(MAX_LISTED_NAMES) {
        Some(rest) if rest > 0 => format!("{head}, and {rest} more"),
        _ => head,
    }
}

/// First characters of a spec hash, prefixed with its algorithm.
fn short_hash(hash: &str) -> String {
    let short: String = hash.chars().take(SHORT_HASH_CHARS).collect();
    format!("sha256:{short}")
}

/// How long ago a sync happened, in words.
///
/// A clock that has moved backwards (a corrected system time, a cache from
/// another machine) yields "in the future" rather than a nonsensical
/// duration: the report states what it can see instead of computing an
/// underflow.
fn age_phrase(synced_at_unix: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    let Some(seconds) = now.checked_sub(synced_at_unix) else {
        return "at a time in the future (check your clock)".to_string();
    };
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match seconds {
        0..MINUTE => "less than a minute ago".to_string(),
        MINUTE..HOUR => format!("{} minute(s) ago", seconds / MINUTE),
        HOUR..DAY => format!("{} hour(s) ago", seconds / HOUR),
        _ => format!("{} day(s) ago", seconds / DAY),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn table(names: &[&str]) -> UpstreamTable {
        UpstreamTable {
            source: "https://spec.example".to_string(),
            spec_hash: "a".repeat(64),
            names: names.iter().map(|name| name.to_string()).collect(),
            deprecated: Vec::new(),
        }
    }

    /// The local table is the built-in one in tests (the suite isolates the
    /// cache directory), so a name it certainly has and one it certainly
    /// does not are both available.
    const KNOWN: &str = "documents.info";
    const UNKNOWN: &str = "things.brandNew";

    #[test]
    fn an_identical_table_reports_no_drift() {
        let names: Vec<String> = ops::table().iter().map(|op| op.name.to_string()).collect();
        let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
        let check = compare(&table(&borrowed), None);
        assert_eq!(check.status, Status::Ok);
        assert!(check.detail.is_empty(), "{:?}", check.detail);
        assert!(
            check.summary.contains("all of them known"),
            "{}",
            check.summary
        );
    }

    /// An operation the online API has and this build does not: reported by
    /// NAME, with the command that fixes it, and only as a warning.
    #[test]
    fn an_operation_only_online_is_named_as_missing() {
        let check = compare(&table(&[KNOWN, UNKNOWN]), None);
        assert_eq!(check.status, Status::Warn);
        let rendered = check.detail.join("\n");
        assert!(rendered.contains(UNKNOWN), "{rendered}");
        assert!(rendered.contains("otl spec sync"), "{rendered}");
        // And it is the MISSING list it lands in, not another one.
        let facts: Vec<_> = check.facts.iter().collect();
        let missing = facts.iter().find(|(name, _)| *name == "missing").unwrap();
        assert_eq!(missing.1, Value::from(vec![Value::from(UNKNOWN)]));
    }

    /// An operation this build has and the online API no longer declares.
    /// The whole built-in table minus one name is the shape of a document
    /// that dropped an endpoint.
    #[test]
    fn an_operation_only_here_is_named_as_withdrawn() {
        let names: Vec<String> = ops::table()
            .iter()
            .map(|op| op.name.to_string())
            .filter(|name| name != KNOWN)
            .collect();
        let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
        let check = compare(&table(&borrowed), None);
        assert_eq!(check.status, Status::Warn);
        let rendered = check.detail.join("\n");
        assert!(rendered.contains("no longer declared online"), "{rendered}");
        assert!(rendered.contains(KNOWN), "{rendered}");
    }

    /// A deprecation is only interesting for an operation this build can
    /// still call; one it never had is not the user's problem.
    #[test]
    fn only_deprecations_of_operations_this_build_has_are_reported() {
        let mut upstream = table(&[KNOWN, UNKNOWN]);
        upstream.deprecated = vec![KNOWN.to_string(), UNKNOWN.to_string()];
        let check = compare(&upstream, None);
        let deprecated = check
            .facts
            .iter()
            .find(|(name, _)| *name == "deprecated")
            .map(|(_, value)| value.clone())
            .unwrap();
        assert_eq!(deprecated, Value::from(vec![Value::from(KNOWN)]));
        let rendered = check.detail.join("\n");
        assert!(rendered.contains("deprecated online"), "{rendered}");
        assert!(rendered.contains(KNOWN), "{rendered}");
    }

    #[test]
    fn the_cache_hash_is_compared_with_the_document_that_was_fetched() {
        let upstream = table(&["x.y"]);
        let same = hash_line(&upstream, Some(&upstream.spec_hash)).unwrap();
        assert!(same.contains("this exact document"), "{same}");
        assert!(same.contains("sha256:aaaaaaaaaaaa"), "{same}");

        let other = hash_line(&upstream, Some(&"b".repeat(64))).unwrap();
        assert!(other.contains("a different document"), "{other}");
        assert!(other.contains("otl spec sync"), "{other}");

        // No cache: nothing to compare, so nothing is claimed.
        assert!(hash_line(&upstream, None).is_none());
    }

    #[test]
    fn long_name_lists_are_capped_for_humans_but_whole_in_json() {
        let names: Vec<String> = (0..12).map(|index| format!("a.op{index}")).collect();
        let text = listed(&names);
        assert!(text.ends_with("and 4 more"), "{text}");
        assert_eq!(strings(&names).len(), 12);
    }

    #[test]
    fn an_age_is_phrased_in_units_a_person_reads() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(age_phrase(now).contains("less than a minute"));
        assert!(age_phrase(now - 120).contains("2 minute(s)"));
        assert!(age_phrase(now - 7_200).contains("2 hour(s)"));
        assert!(age_phrase(now - 3 * 86_400).contains("3 day(s)"));
        // A clock that moved backwards must not underflow into nonsense.
        assert!(age_phrase(now + 86_400).contains("future"));
    }

    #[test]
    fn the_offline_flag_skips_the_online_check_without_contacting_anything() {
        let check = online_spec(true, None, None);
        assert_eq!(check.status, Status::Skipped);
        assert!(check.summary.contains("--offline"), "{}", check.summary);
    }
}
