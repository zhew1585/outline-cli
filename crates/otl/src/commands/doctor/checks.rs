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
    let status = if health.usable {
        Status::Ok
    } else {
        // Code 2, not 4: the credential may well be valid, and the fix is a
        // permission bit or a directory - see docs/exit-codes.md.
        Status::Problem(ExitCode::Usage)
    };
    // The summary says something the detail block below does not repeat:
    // HOW MANY profiles hold anything, plus the permission state. The detail
    // is `auth::report`'s own rendering, which Story 2.6 owns and which this
    // check must not paraphrase.
    let summary = match (health.usable, health.profiles.len()) {
        (false, _) => "the credential file cannot be used as it stands".to_string(),
        (true, 0) => format!("nothing stored yet ({})", health.permissions.describe()),
        (true, count) => format!(
            "{count} profile(s) with stored credentials ({})",
            health.permissions.describe()
        ),
    };
    Check::new("credentials", status, summary)
        .fact("credential_file", health.path.display().to_string())
        .fact("credential_file_exists", health.exists)
        .fact("permissions", health.permissions.describe())
        .fact("usable", health.usable)
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
    let (Ok(instance), Chosen::Approved(resolved)) = (instance, chosen) else {
        return skipped(
            "connectivity",
            "not contacted: there is no usable instance and credential to try",
        );
    };
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
        // timeout stays 7, a 500 stays 6. `doctor` classifies nothing of its
        // own here - it reports the code the same call would have produced
        // in any other command.
        Err(error) => problem("connectivity", &error, "the instance could not be reached")
            .fact("reachable", false),
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
    Check::new(
        key,
        Status::Problem(auth::exit_code_of(error)),
        summary.to_string(),
    )
    .detailed([error.to_string()])
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

    #[test]
    fn connectivity_is_skipped_offline_and_when_there_is_nothing_to_try() {
        let offline = connectivity(
            true,
            &Err(AuthError::Store(StoreError::NoConfigDir)),
            Chosen::Absent,
        );
        assert_eq!(offline.status, Status::Skipped);
        assert!(offline.summary.contains("--offline"), "{}", offline.summary);

        let nothing = connectivity(
            false,
            &Err(AuthError::Store(StoreError::NoConfigDir)),
            Chosen::Absent,
        );
        assert_eq!(nothing.status, Status::Skipped);
        assert!(
            nothing.summary.contains("no usable instance"),
            "{}",
            nothing.summary
        );
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
