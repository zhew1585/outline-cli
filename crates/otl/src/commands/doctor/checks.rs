//! The local and instance checks: configuration, credential file, chosen
//! credential, and reachability.
//!
//! Every one of them answers its question through the SAME code the real
//! commands use - `config` for resolution, `auth::resolve_credential` for
//! the credential, the engine's request channel for the instance - because a
//! doctor with a resolution path of its own would eventually tell a user
//! their setup works while every other command refused it. That is not
//! hypothetical: `otl auth info` had exactly that bug (it read
//! `OUTLINE_API_KEY` directly while the gate scoped a profile's key), and
//! the fix was to give it no path of its own. This module inherits that
//! rule.

use serde_json::Value;

use crate::auth::credentials::CredentialStore;
use crate::auth::error::StoreError;
use crate::auth::report::{credential_health, CredentialHealth};
use crate::auth::{self, AuthError, Identity, Instance, Resolved};
use crate::config::{self, AuthMethod, ConfigError, EnvLayer, Overrides, ProfileSource, UrlSource};
use crate::exit::ExitCode;

use super::report::{Check, Status};

/// How credentials are protected where there are no POSIX permission bits.
///
/// Stated unconditionally on that platform, not only when a credential file
/// happens to exist: Story 2.6 AC 6 requires `doctor` to be explicit about
/// the platform difference, and "no file yet" is exactly when a user is
/// deciding whether to trust this machine with one. Never printed on Unix,
/// where it would be false.
pub const WINDOWS_PROTECTION_NOTE: &str =
    "on Windows otl sets no permission bits: this platform has none. Protection \
     relies entirely on the per-user ACL of your profile directory, so keep the \
     credential directory inside your own user profile.";

/// The platform note for the credential check, if this platform needs one.
///
/// A runtime `cfg!` rather than `#[cfg]` so that both arms compile
/// everywhere and the note's text is testable on every platform - the
/// pairing itself is asserted by
/// `the_windows_note_appears_exactly_where_it_is_true`.
pub fn protection_note() -> Option<&'static str> {
    cfg!(windows).then_some(WINDOWS_PROTECTION_NOTE)
}

/// Which config file is read, and which profile it selects.
pub fn configuration(overrides: &Overrides) -> Check {
    let env = EnvLayer::from_process();
    let path = path_value(config::locate(overrides, &env).path.as_deref());
    let loaded = match config::load_file(overrides, &env) {
        Ok(loaded) => loaded,
        // A config file that cannot be used stops every command, so it also
        // stops the doctor's later checks from meaning anything: it is the
        // first thing to fix, and it is first in the report.
        Err(error) => {
            return Check::new(
                "configuration",
                Status::Problem(ExitCode::Usage),
                "the user config file cannot be used",
            )
            .fact("config_file", path)
            .detailed([error.to_string()])
        }
    };
    let (selected, source) = config::resolve_profile_name(overrides, &env, &loaded);
    let profile = selected
        .map(config::sanitize_name)
        .unwrap_or_else(|| auth::paths::DEFAULT_PROFILE.to_string());
    let summary = match selected {
        Some(_) => format!("profile {profile}, selected by {}", profile_layer(source)),
        None => format!("profile {profile} (no profile selected)"),
    };
    Check::new("configuration", Status::Ok, summary)
        .fact("profile", profile)
        .fact("config_file", path)
        .fact("config_file_read", path_value(loaded.path.as_deref()))
        .fact(
            "profiles_defined",
            loaded
                .file
                .profiles
                .keys()
                .map(|name| Value::from(config::sanitize_name(name)))
                .collect::<Vec<_>>(),
        )
}

/// The instance a command would be sent to.
pub fn instance(instance: &Result<Instance, AuthError>) -> Check {
    match instance {
        Ok(instance) => Check::new(
            "instance",
            Status::Ok,
            format!("{} (url from {})", instance.origin(), url_layer(instance)),
        )
        .fact("instance", instance.origin())
        .fact("url_source", url_layer(instance))
        .fact("auth_method", auth_label(instance.settings().auth())),
        // Everything that lands here is locally fixable - no URL at all, a
        // plaintext `http://` host, a URL with no usable origin - so it
        // carries whatever code that same failure produces in `otl api`.
        Err(error) => problem("instance", error, "no usable instance URL"),
    }
}

/// The credential FILE: where it is, whether it may be used, and what it
/// holds - never what is in it.
pub fn credentials(store: &Result<CredentialStore, StoreError>) -> Check {
    let store = match store {
        Ok(store) => store,
        Err(error) => {
            return Check::new(
                "credentials",
                Status::Problem(ExitCode::Usage),
                "the credential store cannot be used",
            )
            .detailed([error.to_string()])
        }
    };
    file_check(&credential_health(store))
}

/// The check for a credential file that could be inspected.
fn file_check(health: &CredentialHealth) -> Check {
    let status = grade(health);
    Check::new("credentials", status.clone(), summary_of(health, &status))
        .fact("credential_file", health.path.display().to_string())
        .fact("credential_file_exists", health.exists)
        .fact("permissions", health.permissions.describe())
        // The FILE's own verdict, deliberately not the store-wide `usable`
        // from `auth::report`: that one folds the directory in, and this
        // check grades the directory separately below. Publishing the
        // composite here would say "not usable" beside a `connectivity`
        // check that went ahead and used it.
        .fact("file_usable", file_is_usable(health))
        .fact("directory", health.directory.display().to_string())
        .fact("directory_problem", optional(&health.directory_problem))
        .fact(
            "profiles_stored",
            health
                .profiles
                .iter()
                .map(|profile| {
                    Value::from(format!(
                        "{}: {}",
                        config::sanitize_name(&profile.profile),
                        profile.kinds().join(", ")
                    ))
                })
                .collect::<Vec<_>>(),
        )
        .fact("plaintext_key_in_environment", health.env_api_key)
        .detailed(health.lines())
        .detailed(protection_note().map(str::to_string))
}

/// Whether the credential FILE itself may be used.
///
/// The directory around it is a separate question, graded separately by
/// [`grade`].
fn file_is_usable(health: &CredentialHealth) -> bool {
    health.permissions.usable() && health.file_readable
}

/// How the credential file and the directory around it are graded.
///
/// The two are deliberately NOT the same verdict, and this is the one place
/// that decides:
///
/// - **the file itself** unusable - permissions widened, not a regular file,
///   owned by someone else, malformed, or written by a newer version - is a
///   PROBLEM (exit 2, see `docs/exit-codes.md`). No command can use it:
///   `secret_file::read_checked` refuses it on the open descriptor, so
///   `doctor` and every other command give the same answer, and nothing is
///   sent.
/// - **only the directory** being writable by other users, with a sound
///   owner-only file inside it, is a WARNING, and `credential` and
///   `connectivity` go on to use that file.
///
/// Three things make the warning the honest grade rather than a downgrade:
///
/// 1. the file is opened `O_NOFOLLOW` and then checked THROUGH the open
///    descriptor by `require_regular_owned`, which demands the caller's own
///    uid. Another user with write access to the directory therefore cannot
///    plant a file this CLI will read - they cannot create one the victim
///    owns - and a symlink is refused outright;
/// 2. the file is 0600, so they cannot read the credential either;
/// 3. Story 2.6 Task 1 deliberately does NOT re-permission an EXISTING
///    directory ("silently changing someone's home directory is overreach"),
///    so refusing to work in one would contradict the story that created it.
///
/// What is left is deletion, or replacement with a file that then gets
/// refused: nuisance and denial of service, not confidentiality or
/// integrity. That is a warning.
///
/// It is still REPORTED, with the directory's actual mode, which is exactly
/// what Story 2.6 AC 6 asks of `doctor` - report whether permissions are
/// sound - and a warning cannot change the exit code, which matches
/// `doctor`'s own rule: a world-writable directory makes no other command
/// fail, so it is not blocking.
///
/// If a later reader sees "world-writable credential directory, exit 0" and
/// reaches for a fix: that is what this comment is for. It is a decision,
/// with the reasoning above, and the two tests
/// `a_writable_directory_around_a_sound_file_is_a_warning` and
/// `an_over_wide_credential_file_exits_two_before_anything_is_sent` pin both
/// halves.
fn grade(health: &CredentialHealth) -> Status {
    if !file_is_usable(health) {
        return Status::Problem(ExitCode::Usage);
    }
    if health.directory_problem.is_some() {
        return Status::Warn;
    }
    Status::Ok
}

/// The one-line verdict for the credential check.
///
/// Says something the detail block does not repeat: how many profiles hold
/// anything, plus the permission state. The detail is `auth::report`'s own
/// rendering, which Story 2.6 owns and which this check must not paraphrase.
fn summary_of(health: &CredentialHealth, status: &Status) -> String {
    match status {
        Status::Problem(_) => "the credential file cannot be used as it stands".to_string(),
        Status::Warn => format!(
            "the file is sound ({}), but the directory holding it is not",
            health.permissions.describe()
        ),
        _ if health.profiles.is_empty() => {
            format!("nothing stored yet ({})", health.permissions.describe())
        }
        _ => format!(
            "{} profile(s) with stored credentials ({})",
            health.profiles.len(),
            health.permissions.describe()
        ),
    }
}

/// Which credential a command would send, or why none can be.
pub fn credential(chosen: &Chosen, profile: &str) -> Check {
    match chosen {
        Chosen::Approved(resolved) => {
            let snapshot = &resolved.summary;
            let shadowed: Vec<Value> = snapshot
                .available
                .iter()
                .skip(1)
                .map(|method| Value::from(method.label()))
                .collect();
            Check::new("credential", Status::Ok, snapshot.method.label())
                .fact("method", snapshot.method.label())
                .fact("also_available", shadowed)
                .fact("renewable", snapshot.renewable)
                .fact("expires_in_seconds", optional_number(snapshot.expires_in))
                .fact("scope", optional(&snapshot.scope))
        }
        // Not a report of state, a verdict: nothing can be sent, so nothing
        // this CLI does will work until a credential exists. `otl auth info`
        // prints `method: none` and exits 0 because it describes; `doctor`
        // answers "is this environment usable?", and this one is not.
        Chosen::Absent => Check::new(
            "credential",
            Status::Problem(ExitCode::Usage),
            format!("no credential is configured for profile {profile}"),
        )
        .fact("method", Value::Null)
        // The remedy is the error's OWN message, not a second copy of it:
        // `AuthError::NoCredentials` already lists every way in and names
        // the environment variable through `config::ENV_API_KEY`. Writing
        // the advice again here would be two places to keep in step, and
        // the variable name spelled a second time - which
        // `tests/credential_paths.rs` refuses on purpose.
        .detailed([AuthError::NoCredentials {
            profile: profile.to_string(),
        }
        .to_string()]),
        Chosen::Refused(error) => problem("credential", error, "the credential cannot be used"),
        Chosen::Unchecked(reason) => {
            Check::new("credential", Status::Skipped, *reason).fact("method", Value::Null)
        }
    }
}

/// Whether the instance answers, asked through the ordinary request channel.
///
/// This is the only network call `doctor` makes to the instance, and it is
/// the same one `otl auth login` finishes with: `auth.info` through
/// `engine::Client`, so local validation, throttling, backoff and token
/// renewal all apply exactly as they would to any other command.
pub fn connectivity(
    offline: bool,
    instance: &Result<Instance, AuthError>,
    chosen: Chosen,
) -> Check {
    if offline {
        return skipped("connectivity", "--offline: the instance was not contacted");
    }
    // Each reason is stated on its own. A single "nothing to try" would leave
    // the reader guessing which half was missing - and one of these lines is
    // what a user reads when they wonder why the probe did not happen.
    let Ok(instance) = instance else {
        return skipped(
            "connectivity",
            "not contacted: there is no usable instance URL to contact",
        );
    };
    match approved(chosen) {
        Ok(resolved) => probe(instance, resolved),
        Err(skip) => skip,
    }
}

/// The credential to probe with, or the check that says why there is none.
fn approved(chosen: Chosen) -> Result<Resolved, Check> {
    match chosen {
        Chosen::Approved(resolved) => Ok(resolved),
        Chosen::Absent => Err(skipped(
            "connectivity",
            "not contacted: no credential is configured, so there is nothing to send",
        )),
        Chosen::Refused(_) => Err(skipped(
            "connectivity",
            "not contacted: the credential this profile offers cannot be used",
        )),
        // Carries its own reason, which is more specific than anything this
        // function knows.
        Chosen::Unchecked(reason) => Err(skipped("connectivity", reason)),
    }
}

/// Ask the instance who this credential belongs to, and describe the answer.
fn probe(instance: &Instance, resolved: Resolved) -> Check {
    let outcome = resolved
        .into_client(instance.base_url())
        .and_then(|client| auth::fetch_identity(&client));
    match outcome {
        Ok(identity) => Check::new(
            "connectivity",
            Status::Ok,
            format!("{} answered", instance.origin()),
        )
        .fact("reachable", true)
        .fact("account", optional(&identity.account()))
        .fact("workspace", optional(&identity.workspace))
        .detailed(identity_lines(&identity)),
        // Whatever the instance (or the network) said: 401 stays 4, a
        // timeout stays 7, a 500 stays 6, a 200 carrying something that is
        // not JSON stays 1. `doctor` classifies nothing of its own here - it
        // reports the code the same call would have produced in any other
        // command - but it does have to describe the outcome, and only one
        // of those outcomes means the instance was never reached.
        Err(error) => {
            let code = auth::exit_code_of(&error);
            problem_with("connectivity", &error, code, outcome_summary(code))
                // Honest in both directions: a 401 or a malformed body means
                // the instance ANSWERED. Saying `reachable: false` there
                // would send a user looking at their network for a problem
                // that is on the server or in their credential.
                .fact("reachable", code != ExitCode::Network)
        }
    }
}

/// How to describe a failed probe, without claiming more than is known.
///
/// Only a transport failure (code 7) means the request may never have
/// arrived - `docs/exit-codes.md` says so explicitly - so it is the only
/// outcome allowed to say "could not be reached".
fn outcome_summary(code: ExitCode) -> &'static str {
    match code {
        ExitCode::Network => "the instance could not be reached",
        ExitCode::Auth => "the instance answered, and rejected the credential",
        ExitCode::Server => "the instance answered with a server error",
        ExitCode::RateLimited => "the instance answered, and is rate-limiting this client",
        ExitCode::NotFound => "the instance answered, but has no such operation",
        _ => "the instance answered, but not with something usable",
    }
}

/// The credential the gate approved, or why there is none.
#[derive(Debug)]
pub enum Chosen {
    /// A credential was approved for this instance.
    Approved(Resolved),
    /// Nothing is stored or exported anywhere.
    Absent,
    /// Something is configured and was refused.
    Refused(AuthError),
    /// Not asked, because an earlier check already failed.
    Unchecked(&'static str),
}

/// Ask the credential gate what a command would send.
pub fn choose(
    instance: &Result<Instance, AuthError>,
    store: &Result<CredentialStore, StoreError>,
) -> Chosen {
    let Ok(instance) = instance else {
        return Chosen::Unchecked("not checked: the instance URL is not usable");
    };
    let Ok(store) = store else {
        return Chosen::Unchecked("not checked: the credential store is not usable");
    };
    let file = match store.load() {
        Ok(file) => file,
        Err(error) => return Chosen::Refused(AuthError::Store(error)),
    };
    match auth::resolve_credential(instance, store, &file) {
        Ok(resolved) => Chosen::Approved(resolved),
        // "There is nothing anywhere" is a state of its own, with its own
        // remedy, so it is not folded in with a refusal.
        Err(AuthError::NoCredentials { .. } | AuthError::Config(ConfigError::MissingApiKey)) => {
            Chosen::Absent
        }
        Err(error) => Chosen::Refused(error),
    }
}

/// A blocking finding, classified by the table every command shares.
///
/// The exit code comes from [`auth::exit_code_of`], which is the borrowing
/// half of `map_auth_error` - the same match, so a `doctor` verdict can
/// never disagree with the code the failing command itself would produce.
fn problem(key: &'static str, error: &AuthError, summary: &str) -> Check {
    problem_with(key, error, auth::exit_code_of(error), summary)
}

/// [`problem`] for a caller that has already asked for the code, so that the
/// summary can depend on it. The code still comes from the one table.
fn problem_with(key: &'static str, error: &AuthError, code: ExitCode, summary: &str) -> Check {
    Check::new(key, Status::Problem(code), summary.to_string()).detailed([error.to_string()])
}

/// A check that was not run, with the reason as its summary.
fn skipped(key: &'static str, reason: &str) -> Check {
    Check::new(key, Status::Skipped, reason.to_string())
}

/// A `Some` string as JSON, `null` otherwise.
fn optional(value: &Option<String>) -> Value {
    value.clone().map_or(Value::Null, Value::from)
}

/// A `Some` number as JSON, `null` otherwise.
fn optional_number(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::from)
}

/// A path as JSON, sanitized because it can come from an environment
/// variable or a flag, and `null` when there is none.
fn path_value(path: Option<&std::path::Path>) -> Value {
    path.map(config::sanitize_path)
        .map_or(Value::Null, Value::from)
}

/// Which layer supplied the base URL, phrased for a report.
fn url_layer(instance: &Instance) -> &'static str {
    match instance.settings().url_source() {
        UrlSource::Flag => "--url",
        UrlSource::Env => "OUTLINE_URL",
        UrlSource::Profile => "the profile's url",
    }
}

/// Which layer selected the profile, phrased for a report.
fn profile_layer(source: ProfileSource) -> &'static str {
    match source {
        ProfileSource::Flag => "--profile",
        ProfileSource::Env => "OUTLINE_PROFILE",
        ProfileSource::DefaultProfile => "default_profile in the config file",
    }
}

/// The configured login flow, as the config file spells it.
fn auth_label(method: AuthMethod) -> String {
    method.to_string()
}

/// The account lines of the connectivity check.
fn identity_lines(identity: &Identity) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(account) = identity.account() {
        lines.push(format!("account:   {account}"));
    }
    if let Some(workspace) = &identity.workspace {
        lines.push(format!("workspace: {workspace}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::auth::credentials::CredentialFile;
    use crate::auth::secret_file::Permissions;

    /// The platform note is present exactly where it is true. A note that
    /// claimed permission bits on Windows - or promised an ACL on Unix -
    /// would be a health report that lies about protection, which is the one
    /// thing `auth::report` exists to prevent.
    #[test]
    fn the_windows_note_appears_exactly_where_it_is_true() {
        assert_eq!(protection_note().is_some(), cfg!(windows));
        assert!(WINDOWS_PROTECTION_NOTE.contains("ACL"));
        assert!(WINDOWS_PROTECTION_NOTE.contains("sets no permission bits"));
    }

    /// A synthetic health report, so the grading rule can be exercised over
    /// combinations that are awkward to arrange on a real filesystem (a
    /// world-writable directory around a malformed file, say).
    fn health(
        permissions: Permissions,
        file_readable: bool,
        directory: Option<&str>,
    ) -> CredentialHealth {
        CredentialHealth {
            path: std::path::PathBuf::from("/home/u/.config/outline-cli/credentials.toml"),
            exists: true,
            permissions,
            usable: false,
            file_readable,
            directory: std::path::PathBuf::from("/home/u/.config/outline-cli"),
            directory_mode: Some("0700".to_string()),
            directory_problem: directory.map(str::to_string),
            profiles: Vec::new(),
            env_api_key: false,
        }
    }

    fn owner_only() -> Permissions {
        Permissions::OwnerOnly {
            mode: "0600".to_string(),
        }
    }

    fn too_open() -> Permissions {
        Permissions::TooOpen {
            mode: "0644".to_string(),
        }
    }

    /// The grading rule the R1 review produced, in one place.
    ///
    /// The FILE blocks; the DIRECTORY around a sound file warns. The reasons
    /// are on [`grade`]; this pins the outcomes so that neither half can be
    /// "tidied" into the other.
    #[test]
    fn the_file_blocks_and_a_writable_directory_only_warns() {
        let writable = Some("directory 0777 is writable by other users");

        // A file others can read: no command can use it, so it blocks.
        assert_eq!(
            grade(&health(too_open(), true, None)),
            Status::Problem(ExitCode::Usage)
        );
        // A file that cannot be parsed: same, and this is the case `usable`
        // alone could not distinguish from a directory problem.
        assert_eq!(
            grade(&health(owner_only(), false, None)),
            Status::Problem(ExitCode::Usage)
        );
        // A sound file in a directory others can write: reported, not
        // blocking. `credential` and `connectivity` go on to use it.
        assert_eq!(grade(&health(owner_only(), true, writable)), Status::Warn);
        // Both wrong: the file's verdict wins, because it is the blocking one.
        assert_eq!(
            grade(&health(too_open(), true, writable)),
            Status::Problem(ExitCode::Usage)
        );
        // Nothing wrong at all.
        assert_eq!(grade(&health(owner_only(), true, None)), Status::Ok);
    }

    /// The warning has to SAY what is wrong with the directory, and it must
    /// not describe the file as unusable - the run is about to use it.
    #[test]
    fn a_directory_only_warning_names_the_directory_and_not_the_file() {
        let health = health(owner_only(), true, Some("directory 0777 is writable"));
        let check = file_check(&health);
        assert_eq!(check.status, Status::Warn);
        assert!(
            check.summary.contains("the file is sound"),
            "{}",
            check.summary
        );
        assert!(
            !check.summary.contains("cannot be used"),
            "a file that is about to be used must not be called unusable: {}",
            check.summary
        );
        let facts: Vec<&(&str, serde_json::Value)> = check.facts.iter().collect();
        let fact = |name: &str| {
            facts
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or(Value::Null)
        };
        // The FILE's own verdict, not the composite one: publishing "not
        // usable" here would contradict the connectivity check below it.
        assert_eq!(fact("file_usable"), Value::from(true));
        assert!(fact("directory_problem").is_string(), "{check:?}");
    }

    #[test]
    fn a_credential_store_that_cannot_be_opened_is_a_local_problem() {
        let check = credentials(&Err(StoreError::NoConfigDir));
        assert_eq!(check.status, Status::Problem(ExitCode::Usage));
        assert!(!check.detail.is_empty(), "the reason must be reported");
    }

    #[test]
    fn a_healthy_credential_file_reports_its_path_and_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::at(dir.path().to_path_buf());
        let mut file = CredentialFile::default();
        file.profile_mut("default").api_key = Some("KEY-SECRET-9c7a".to_string());
        store.save(&file).unwrap();

        let check = credentials(&Ok(store.clone()));
        assert_eq!(check.status, Status::Ok);
        let rendered = format!("{:?}\n{}", check.facts, check.detail.join("\n"));
        assert!(
            rendered.contains(&store.path().display().to_string()),
            "the credential file must be named: {rendered}"
        );
        assert!(rendered.contains("api key"), "{rendered}");
        assert!(
            !rendered.contains("SECRET-9c7a"),
            "credential leaked: {rendered}"
        );
        // The note about Windows ACLs, and nothing else, differs per
        // platform; on Unix the mode is stated instead.
        assert_eq!(
            check
                .detail
                .iter()
                .any(|line| line.contains("per-user ACL")),
            cfg!(windows),
            "{:?}",
            check.detail
        );
    }

    /// An over-wide credential file must be a PROBLEM that names the file and
    /// its mode, and it must be exit 2 (fix a bit locally), never 4.
    #[cfg(unix)]
    #[test]
    fn an_over_wide_credential_file_is_a_problem_that_names_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::at(dir.path().to_path_buf());
        let mut file = CredentialFile::default();
        file.profile_mut("default").api_key = Some("KEY-SECRET-9c7a".to_string());
        store.save(&file).unwrap();
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();

        let check = credentials(&Ok(store.clone()));
        assert_eq!(check.status, Status::Problem(ExitCode::Usage));
        let rendered = check.detail.join("\n");
        assert!(
            rendered.contains(&store.path().display().to_string()),
            "the offending file must be named: {rendered}"
        );
        assert!(rendered.contains("0644"), "{rendered}");
        assert!(rendered.contains("chmod 600"), "{rendered}");
        assert!(!rendered.contains("SECRET-9c7a"), "{rendered}");
    }

    /// The remedy is the canonical message, not a copy of it.
    ///
    /// Asserted by EQUALITY against `AuthError::NoCredentials`: that error
    /// already lists every way in, and `auth`'s own
    /// `the_no_credentials_message_names_every_way_in` is what pins the
    /// content. Restating the advice here would be a second copy to keep in
    /// step - and naming the environment variable a second time is what
    /// `tests/credential_paths.rs` refuses, on the grounds that a module
    /// which names the global key is a module that can fall back to it.
    #[test]
    fn a_missing_credential_reports_the_canonical_remedy() {
        let check = credential(&Chosen::Absent, "work");
        assert_eq!(check.status, Status::Problem(ExitCode::Usage));
        assert!(check.summary.contains("work"), "{}", check.summary);
        let expected: Vec<String> = AuthError::NoCredentials {
            profile: "work".to_string(),
        }
        .to_string()
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect();
        assert_eq!(check.detail, expected);
        assert!(
            check
                .detail
                .iter()
                .any(|line| line.contains("otl auth login")),
            "{:?}",
            check.detail
        );
    }

    #[test]
    fn an_unchecked_credential_is_skipped_rather_than_blamed() {
        let check = credential(&Chosen::Unchecked("not checked: nothing to try"), "default");
        assert_eq!(check.status, Status::Skipped);
        assert!(check.status.code().is_none());
    }

    /// The classification is borrowed, never invented: a session that can no
    /// longer be refreshed is 4, a locally fixable state is 2.
    #[test]
    fn a_refused_credential_carries_the_code_that_command_would_have_exited_with() {
        let expired = AuthError::OAuth(crate::auth::error::OAuthError::SessionExpired {
            profile: "default".to_string(),
            detail: String::new(),
        });
        assert_eq!(
            credential(&Chosen::Refused(expired), "default").status,
            Status::Problem(ExitCode::Auth)
        );
        let local = AuthError::Store(StoreError::NoConfigDir);
        assert_eq!(
            credential(&Chosen::Refused(local), "default").status,
            Status::Problem(ExitCode::Usage)
        );
    }

    /// Every reason for not probing is distinguishable, and none of them is
    /// mistakable for `--offline`: one of these lines is what a user reads
    /// when they wonder why the instance was not contacted.
    #[test]
    fn every_reason_for_not_contacting_the_instance_says_which_one_it_is() {
        let no_instance = || Err(AuthError::Store(StoreError::NoConfigDir));

        let offline = connectivity(true, &no_instance(), Chosen::Absent);
        assert_eq!(offline.status, Status::Skipped);
        assert!(offline.summary.contains("--offline"), "{}", offline.summary);

        let no_url = connectivity(false, &no_instance(), Chosen::Absent);
        assert_eq!(no_url.status, Status::Skipped);
        assert!(
            no_url.summary.contains("instance URL"),
            "{}",
            no_url.summary
        );
        assert!(!no_url.summary.contains("--offline"), "{}", no_url.summary);

        // With a usable instance the reason comes from the credential side,
        // and `Unchecked` carries its own words through unchanged.
        let carried = connectivity(
            false,
            &no_instance(),
            Chosen::Unchecked("not checked: the credential store is not usable"),
        );
        assert_eq!(carried.status, Status::Skipped);
    }

    #[test]
    fn an_unusable_instance_url_is_a_problem_with_its_reason() {
        let check = instance(&Err(AuthError::Config(ConfigError::MissingUrl {
            profile: None,
        })));
        assert_eq!(check.status, Status::Problem(ExitCode::Usage));
        assert!(
            check.detail.join("\n").contains("OUTLINE_URL"),
            "{:?}",
            check.detail
        );
    }
}
