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
use crate::auth::{self, AuthError, Identity, Instance, Resolved};
use crate::config::{self, AuthMethod, ConfigError, EnvLayer, Overrides, ProfileSource, UrlSource};
use crate::exit::ExitCode;

use super::report::{optional, optional_number, path_value, Check, Status};

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
            // Two independent questions, and neither is derivable from the
            // other: `exit_code_of` says how bad it is, `instance_answered`
            // says whether anything was actually sent and answered. Deriving
            // the second from the first is what made this check report
            // `reachable: true` for a request that could not even be built.
            let code = auth::exit_code_of(&error);
            let answered = auth::instance_answered(&error);
            problem_with(
                "connectivity",
                &error,
                code,
                outcome_summary(code, answered),
            )
            // Honest in both directions: a 401 or a malformed body means
            // the instance ANSWERED, and a header this machine could not
            // build means it never heard from us at all.
            .fact("reachable", answered)
        }
    }
}

/// How to describe a failed probe, without claiming more than is known.
///
/// `answered` decides the half of the sentence that matters most: whether
/// the instance was heard from at all. It is passed in rather than inferred
/// from `code`, because one code covers both cases - `Usage` is a header
/// this machine could not build (nothing sent) as well as a parameter the
/// spec rejected (nothing sent), while `Failure` is both a client that could
/// not be built (nothing sent) and a response that was not JSON (sent, and
/// answered). Only a transport failure is allowed to say "could not be
/// reached": `docs/exit-codes.md` says code 7 is the one that may never have
/// arrived.
fn outcome_summary(code: ExitCode, answered: bool) -> &'static str {
    match (answered, code) {
        (false, ExitCode::Network) => "the instance could not be reached",
        // A credential that could not be renewed fails at the token
        // endpoint, before any request to the instance.
        (false, ExitCode::Auth) => {
            "nothing was sent: the stored credential could not be renewed first"
        }
        (false, _) => "nothing was sent: the request could not even be built locally",
        (true, ExitCode::Auth) => "the instance answered, and rejected the credential",
        (true, ExitCode::Server) => "the instance answered with a server error",
        (true, ExitCode::RateLimited) => "the instance answered, and is rate-limiting this client",
        (true, ExitCode::NotFound) => "the instance answered, but has no such operation",
        (true, _) => "the instance answered, but not with something usable",
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

    /// `reachable` and the summary are about whether the INSTANCE was heard
    /// from, which is not what the exit code answers. A request that could
    /// not even be built must not be reported as an answer.
    #[test]
    fn an_unsent_request_is_never_described_as_an_answer() {
        for code in [
            ExitCode::Usage,
            ExitCode::Failure,
            ExitCode::ApiRequest,
            ExitCode::Server,
        ] {
            let unsent = outcome_summary(code, false);
            assert!(
                unsent.contains("nothing was sent"),
                "{code:?} unsent: {unsent}"
            );
            assert!(!unsent.contains("answered"), "{code:?} unsent: {unsent}");
        }
        // A transport failure keeps its own wording: it is the one outcome
        // that may have arrived, so "could not be reached" is the honest
        // thing to say and "nothing was sent" would be a claim too strong.
        assert_eq!(
            outcome_summary(ExitCode::Network, false),
            "the instance could not be reached"
        );
        // A credential that could not be renewed never reached the instance
        // either, and says which half failed.
        let renew = outcome_summary(ExitCode::Auth, false);
        assert!(renew.contains("nothing was sent"), "{renew}");
        assert!(renew.contains("renewed"), "{renew}");
        // Answered outcomes say so.
        for code in [ExitCode::Auth, ExitCode::Server, ExitCode::Failure] {
            let answered = outcome_summary(code, true);
            assert!(answered.contains("answered"), "{code:?}: {answered}");
            assert!(
                !answered.contains("nothing was sent"),
                "{code:?}: {answered}"
            );
        }
    }

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
