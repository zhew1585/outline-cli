//! `otl doctor` - one command that says whether this environment works
//! (Story 4.3, FR23).
//!
//! Seven checks, in dependency order: the config file, the instance URL, the
//! credential file, the credential that would be sent, whether the instance
//! answers, which operation table is in use, and how that table differs from
//! the online API description.
//!
//! # It answers a question, so it has an exit code
//!
//! `otl doctor` exits **0** when nothing is blocking and **the code the
//! first blocking finding would have produced in any other command**
//! otherwise: 2 for something to fix locally, 4 for "authenticate again", 7
//! for a network failure, and so on (`docs/exit-codes.md`). No new code is
//! introduced and no code changes meaning.
//!
//! Three consequences worth stating, because each is a decision:
//!
//! 1. **The report is printed either way.** A blocking finding does not
//!    abort the run: every check still runs, the whole report reaches
//!    stdout, and only then does the process exit non-zero. `--json`
//!    consumers get the same object whatever the verdict.
//! 2. **A warning never changes the exit code.** A spec cache that had to be
//!    discarded, a table that is behind the online one, a plaintext key in
//!    the environment: all real findings, none of which stops `otl` from
//!    working. `doctor` would be useless in CI if it failed on those.
//! 3. **`doctor` classifies nothing itself.** Every blocking code comes from
//!    `auth::exit_code_of`, the borrowing half of the mapper every command
//!    uses. A `doctor` with its own table would eventually disagree with the
//!    command it was diagnosing.
//!
//! # Network access
//!
//! Two requests, both only because the user typed `otl doctor`, and both
//! through channels that already exist:
//!
//! - `auth.info` to the instance, through the engine's request channel, with
//!   the credential the gate approved (the same call `otl auth login` ends
//!   with);
//! - the online API description, through `spec sync`'s document channel -
//!   `otl doctor` calls [`crate::commands::spec::upstream_table`] rather
//!   than the fetcher, so the codebase still has exactly three places that
//!   send a request.
//!
//! `--offline` skips both. Nothing here runs on any other command's path, so
//! NFR4 (no phone home) is untouched: `otl doctor` is a command, not a
//! background check.

mod checks;
mod credentials;
mod drift;
// Public so that the golden-file test can render a SYNTHETIC report: the
// real one is a function of the machine it runs on (paths, operation
// counts, clocks), and a golden file over that would pin a developer's
// environment rather than the layout.
pub mod report;

use clap::Args;

use crate::auth::credentials::CredentialStore;
use crate::auth::{self};
use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;

use report::{Check, Report};

/// Arguments for `otl doctor`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Contact nothing; report local state only.
    #[arg(long)]
    offline: bool,

    /// Compare against this OpenAPI document instead of the upstream one.
    ///
    /// The same override `otl spec sync --url` takes, for the same reasons:
    /// a mirror, an internal copy, or a test server.
    ///
    /// Deliberately NOT declared as conflicting with `--offline`, even though
    /// `--offline` ignores it: accepting both together is what makes the
    /// offline guarantee testable. A test that passed `--offline` alone would
    /// have to point the fetch at the real upstream host to prove nothing is
    /// fetched, which is the very thing the guarantee forbids.
    #[arg(long, value_name = "URL")]
    spec_url: Option<String>,
}

/// Run `otl doctor`.
///
/// The report is emitted BEFORE the exit code is decided: a diagnosis the
/// user cannot read is worthless, and a `--json` consumer must get the same
/// object whether or not something was wrong.
pub fn run(args: &DoctorArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let report = examine(args, overrides);
    report::emit(&report, mode)?;
    match report.blocking() {
        Some(check) => Err(CliError::new(
            report.exit_code(),
            anyhow::anyhow!("{}: {}", check.key, check.summary),
        )),
        None => Ok(()),
    }
}

/// Run every check, in the order a user should read them.
///
/// Nothing here can fail: a check that cannot be performed becomes a finding
/// rather than an early return, because "the credential file is unreadable"
/// is exactly when the rest of the report is needed.
fn examine(args: &DoctorArgs, overrides: &Overrides) -> Report {
    let instance = auth::resolve_instance(overrides);
    let store = CredentialStore::discover();
    // Sanitized: a profile name comes from a flag, an environment variable
    // or a config file, and it is interpolated into a summary line. The
    // sink-level scrub in `report` would neutralize an escape sequence
    // anyway; this also bounds the LENGTH, which the scrub does not.
    let profile = crate::config::sanitize_name(
        &instance
            .as_ref()
            .map(|instance| instance.profile_key().to_string())
            .unwrap_or_else(|_| default_profile(overrides)),
    );
    let chosen = checks::choose(&instance, &store);

    let mut all: Vec<Check> = vec![
        checks::configuration(overrides),
        checks::instance(&instance),
        credentials::check(&store),
        checks::credential(&chosen, &profile),
        // Consumes the credential: `into_client` is the only thing that can
        // be done with one, by design.
        checks::connectivity(args.offline, &instance, chosen),
    ];
    let (local, cached_hash) = drift::local_spec();
    all.push(local);
    all.push(drift::online_spec(
        args.offline,
        args.spec_url.as_deref(),
        cached_hash.as_deref(),
    ));
    Report { checks: all }
}

/// The profile name to report when no instance could be resolved.
///
/// Profile selection does not depend on a usable URL, so the report still
/// names the profile whose credentials it is talking about. Falls back to
/// the default name when even the config file cannot be read - the
/// configuration check has already said so.
fn default_profile(overrides: &Overrides) -> String {
    auth::active_profile(overrides).unwrap_or_else(|_| auth::paths::DEFAULT_PROFILE.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::report::Status;
    use super::*;
    use crate::exit::ExitCode;

    /// Every check key is unique and stable: they are the keys of a `--json`
    /// object, so a duplicate would silently drop one check from the report
    /// a script reads.
    #[test]
    fn the_report_has_one_entry_per_check_with_distinct_keys() {
        let report = Report {
            checks: vec![
                Check::new("configuration", Status::Ok, ""),
                Check::new("instance", Status::Ok, ""),
                Check::new("credentials", Status::Ok, ""),
                Check::new("credential", Status::Ok, ""),
                Check::new("connectivity", Status::Ok, ""),
                Check::new("local-spec", Status::Ok, ""),
                Check::new("online-spec", Status::Ok, ""),
            ],
        };
        let mut keys: Vec<&str> = report.checks.iter().map(|check| check.key).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "duplicate check key");
    }

    /// The failing path still prints the report, and the error `main` shows
    /// names the check that decided the code.
    #[test]
    fn a_blocking_finding_becomes_an_error_that_names_its_check() {
        let report = Report {
            checks: vec![
                Check::new("configuration", Status::Ok, "profile default"),
                Check::new(
                    "credential",
                    Status::Problem(ExitCode::Usage),
                    "no credential is configured for profile default",
                ),
            ],
        };
        let check = report.blocking().expect("a blocking check");
        let error = CliError::new(
            report.exit_code(),
            anyhow::anyhow!("{}: {}", check.key, check.summary),
        );
        assert_eq!(error.code, ExitCode::Usage);
        let text = error.to_string();
        assert!(text.contains("credential"), "{text}");
        assert!(text.contains("no credential is configured"), "{text}");
    }
}
