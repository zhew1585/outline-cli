//! Turning an `otl auth` result into the two renderings the CLI contract
//! promises: human lines on a terminal, one JSON object otherwise.
//!
//! Split out of the command module, which had grown past the file-length
//! limit, along the seam that was already there: nothing here performs an
//! action, resolves a credential or touches the network. It receives what
//! the command decided and describes it - which is also why every function
//! here is straightforward to test without a mock server.
//!
//! Nothing in this module may print a credential or a fragment of one.
//! `info_json_never_carries_a_credential_field` and
//! `auth_info_never_prints_a_credential_or_a_fragment_of_one` pin that from
//! the two sides: the field names here, and the real binary's output.

use serde_json::{json, Value};

use crate::auth::report::{self, CredentialHealth};
use crate::auth::source::Snapshot;
use crate::auth::{login, logout, Identity};
use crate::exit::{CliError, ExitCode};
use crate::render::{self, OutputMode};
use crate::stdio;
use anyhow::anyhow;

/// Human lines plus the machine-readable object for `auth login`.
pub(super) fn login_output(profile: &str, outcome: &login::Outcome) -> Output {
    let identity = outcome.identity.as_ref();
    let account = identity.and_then(Identity::account);
    let workspace = identity.and_then(|identity| identity.workspace.clone());
    let mut lines = vec![
        "Signed in.".to_string(),
        format!("profile:          {profile}"),
    ];
    if let Some(account) = &account {
        lines.push(format!("account:          {account}"));
    }
    if let Some(workspace) = &workspace {
        lines.push(format!("workspace:        {workspace}"));
    }
    if let Some(scope) = &outcome.scope {
        lines.push(format!("scope:            {scope}"));
    }
    lines.push(format!(
        "credential file:  {}",
        outcome.credential_path.display()
    ));
    lines.push(format!(
        "client:           {}",
        outcome.client_source.label()
    ));
    Output {
        lines,
        value: json!({
            "profile": profile,
            "account": account,
            "workspace": workspace,
            "scope": outcome.scope,
            "credential_file": outcome.credential_path.display().to_string(),
            "client_source": format!("{:?}", outcome.client_source).to_lowercase(),
        }),
    }
}

/// Human lines plus the machine-readable object for `auth logout`.
pub(super) fn logout_output(profile: &str, report: &logout::Report) -> Output {
    let headline = if !report.had_credentials {
        format!("Nothing was stored for profile {profile}.")
    } else if report.kept_for_retry {
        format!(
            "Profile {profile} was NOT signed out: the server could not be \
             told, so the credentials were kept for a retry."
        )
    } else if report.survived_concurrent_write {
        format!(
            "Profile {profile} is NOT signed out: a credential written by \
             another process during this logout is still stored, and was \
             never revoked."
        )
    } else {
        format!("Signed out of profile {profile}.")
    };
    Output {
        lines: vec![
            headline,
            format!("tokens revoked on the server: {}", report.signed_out()),
            format!(
                "application deleted:          {}",
                report.registration_deleted
            ),
            format!("credential file removed:      {}", report.file_removed),
            format!("kept locally for retry:       {}", report.kept_for_retry),
        ],
        value: json!({
            "profile": profile,
            "had_credentials": report.had_credentials,
            // The profile-level claim, not just what this run managed to
            // revoke: a session another process wrote is still live.
            "revoked": report.signed_out(),
            "session_survived_concurrent_write": report.survived_concurrent_write,
            "registration_deleted": report.registration_deleted,
            "credential_file_removed": report.file_removed,
            "credentials_kept_for_retry": report.kept_for_retry,
            "warnings": report.warnings,
        }),
    }
}

/// Human lines plus the machine-readable object for `auth set-key`.
pub(super) fn set_key_output(profile: &str, health: &CredentialHealth) -> Output {
    Output {
        lines: vec![
            format!("API key stored for profile {profile}."),
            format!("credential file:  {}", health.path.display()),
            format!("permissions:      {}", health.permissions.describe()),
        ],
        value: json!({
            "profile": profile,
            "credential_file": health.path.display().to_string(),
            "permissions": health.permissions.describe(),
        }),
    }
}

/// Human lines plus the machine-readable object for `auth info`.
pub(super) fn info_output(
    profile: &str,
    instance: &Result<String, String>,
    health: &CredentialHealth,
    resolved: &Result<Option<()>, String>,
    snapshot: &Option<Snapshot>,
    identity: &Option<Result<Identity, String>>,
) -> Output {
    let snapshot = snapshot.as_ref();
    let mut lines = vec![
        format!("profile:          {profile}"),
        format!(
            "instance:         {}",
            match instance {
                Ok(origin) => origin.clone(),
                Err(reason) => format!("not usable ({reason})"),
            }
        ),
    ];
    lines.extend(method_lines(resolved, snapshot));
    lines.extend(identity_lines(snapshot, identity));
    lines.extend(health.lines());
    Output {
        lines,
        value: info_value(profile, instance, health, resolved, snapshot, identity),
    }
}

/// The `auth info` JSON object.
///
/// Split from the human lines so neither is long enough to hide a field. The
/// two describe the same state and must stay in step; anything that appears
/// in only one of them is a bug in whichever is missing it.
fn info_value(
    profile: &str,
    instance: &Result<String, String>,
    health: &CredentialHealth,
    resolved: &Result<Option<()>, String>,
    snapshot: Option<&Snapshot>,
    identity: &Option<Result<Identity, String>>,
) -> Value {
    json!({
        "profile": profile,
        "instance": instance.as_ref().ok().map(String::as_str),
        "instance_problem": instance.as_ref().err(),
        "method": snapshot.map(|snapshot| snapshot.method.label()),
        "available": snapshot
            .map(|snapshot| snapshot.available.iter().map(|m| m.label()).collect::<Vec<_>>())
            .unwrap_or_default(),
        // Reported separately from `available`, because it is an OBSERVATION
        // and not a candidate. `available` lists what the release gate would
        // hand over for these settings; a plaintext key sitting in the
        // environment may not be one of those (a profile scopes its key to
        // `OUTLINE_API_KEY_<PROFILE>`) and yet is exactly what a user needs
        // told when they wonder why the key they exported is not in use.
        //
        // Named for the hygiene fact rather than for the variable: a field
        // name containing `api_key` trips
        // `info_json_never_carries_a_credential_field`, and that guard's
        // proxy - no credential-bearing NAME in the output - is worth more
        // than a field name that echoes the variable.
        "plaintext_key_in_environment": health.env_api_key,
        "scope": snapshot.and_then(|snapshot| snapshot.scope.clone()),
        "account": identity_value(snapshot, identity),
        "expires_in_seconds": snapshot.and_then(|snapshot| snapshot.expires_in),
        "renewable": snapshot.is_some_and(|snapshot| snapshot.renewable),
        "credential_file": health.path.display().to_string(),
        "credential_file_exists": health.exists,
        "credential_file_permissions": health.permissions.describe(),
        "credential_file_usable": health.usable,
        "resolution_error": resolved.as_ref().err(),
    })
}

/// The `method` / `available` lines of `auth info`.
fn method_lines(resolved: &Result<Option<()>, String>, snapshot: Option<&Snapshot>) -> Vec<String> {
    match (resolved, snapshot) {
        (Err(error), _) => unavailable_lines(error),
        (Ok(_), Some(snapshot)) => {
            let mut lines = vec![report::method_line(snapshot.method)];
            let shadowed: Vec<&str> = snapshot
                .available
                .iter()
                .skip(1)
                .map(|method| method.label())
                .collect();
            if !shadowed.is_empty() {
                lines.push(format!("also available:   {}", shadowed.join(", ")));
            }
            if let Some(scope) = &snapshot.scope {
                lines.push(format!("scope:            {scope}"));
            }
            if let Some(seconds) = snapshot.expires_in {
                lines.push(format!("access token:     {}", expiry_phrase(seconds)));
            }
            lines
        }
        (Ok(None), None) => vec!["method:           none (no credentials stored)".to_string()],
        (Ok(Some(())), None) => vec!["method:           unknown".to_string()],
    }
}

/// The `method: unavailable` block, keeping every line of the reason.
///
/// The reason is often a remedy spanning several lines ("set
/// `OUTLINE_API_KEY_WORK`", "run `otl auth login`"). Folding it into one
/// parenthesised line would cut exactly the part the user needs, and this is
/// the command they ran BECAUSE something is wrong.
fn unavailable_lines(reason: &str) -> Vec<String> {
    let mut lines = vec!["method:           unavailable".to_string()];
    lines.extend(
        reason
            .lines()
            .map(|line| format!("                  {}", line.trim_end())),
    );
    lines
}

/// The identity lines of `auth info`.
fn identity_lines(
    snapshot: Option<&Snapshot>,
    identity: &Option<Result<Identity, String>>,
) -> Vec<String> {
    match identity {
        Some(Ok(identity)) => {
            let mut lines = Vec::new();
            if let Some(account) = identity.account() {
                lines.push(format!("account:          {account}"));
            }
            if let Some(workspace) = &identity.workspace {
                lines.push(format!("workspace:        {workspace}"));
            }
            lines
        }
        Some(Err(error)) => vec![format!("account:          could not be checked ({error})")],
        // Offline, or nothing to ask with: fall back to what login cached.
        None => cached_identity_lines(snapshot),
    }
}

/// Identity as captured at login time, when no live call was made.
fn cached_identity_lines(snapshot: Option<&Snapshot>) -> Vec<String> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if let Some(account) = &snapshot.account {
        lines.push(format!("account:          {account} (as of last login)"));
    }
    if let Some(workspace) = &snapshot.workspace {
        lines.push(format!("workspace:        {workspace} (as of last login)"));
    }
    lines
}

/// Account value for the JSON output.
fn identity_value(
    snapshot: Option<&Snapshot>,
    identity: &Option<Result<Identity, String>>,
) -> Value {
    match identity {
        Some(Ok(identity)) => json!(identity.account()),
        Some(Err(_)) => Value::Null,
        None => json!(snapshot.and_then(|snapshot| snapshot.account.clone())),
    }
}

/// Describe a token lifetime in words.
fn expiry_phrase(seconds: i64) -> String {
    if seconds <= 0 {
        return "expired (it will be renewed on the next request)".to_string();
    }
    let minutes = seconds / 60;
    if minutes < 1 {
        return format!("expires in {seconds}s");
    }
    format!("expires in {minutes}m")
}

/// A command result in both renderings.
pub(super) struct Output {
    lines: Vec<String>,
    value: Value,
}

/// Print a command result: human text on a terminal, JSON otherwise.
pub(super) fn emit(output: Output, mode: OutputMode) -> Result<(), CliError> {
    match mode {
        OutputMode::Json => {
            // `render_json`, not `render`: this value is a summary this
            // module built, not an operation's response payload, so there is
            // no response schema to pick table columns from.
            let rendered = render::render_json(&output.value).map_err(|error| {
                CliError::new(
                    ExitCode::Failure,
                    anyhow!("failed to render the result: {error}"),
                )
            })?;
            stdio::write_data_line(&rendered)
        }
        OutputMode::Table => stdio::write_data_line(&output.lines.join("\n")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn expiry_is_described_in_words_a_human_can_act_on() {
        assert!(expiry_phrase(-5).contains("expired"));
        assert!(expiry_phrase(-5).contains("renewed"));
        assert_eq!(expiry_phrase(30), "expires in 30s");
        assert_eq!(expiry_phrase(3600), "expires in 60m");
    }

    #[test]
    fn info_reports_an_unusable_credential_file_instead_of_giving_up() {
        let health = CredentialHealth {
            path: std::path::PathBuf::from("/home/u/.config/outline-cli/credentials.toml"),
            exists: true,
            permissions: crate::auth::secret_file::Permissions::TooOpen {
                mode: "0644".to_string(),
            },
            usable: false,
            directory: std::path::PathBuf::from("/home/u/.config/outline-cli"),
            directory_mode: Some("0700".to_string()),
            directory_problem: None,
            profiles: Vec::new(),
            env_api_key: false,
        };
        let resolved = Err("credential file is accessible to users other than you".to_string());
        let output = info_output(
            "default",
            &Err("OUTLINE_URL is not set".to_string()),
            &health,
            &resolved,
            &None,
            &None,
        );
        let rendered = output.lines.join("\n");
        assert!(rendered.contains("0644"), "{rendered}");
        assert!(rendered.contains("unavailable"), "{rendered}");
        assert!(rendered.contains("not usable"), "{rendered}");
    }

    #[test]
    fn info_json_never_carries_a_credential_field() {
        let health = CredentialHealth {
            path: std::path::PathBuf::from("/tmp/credentials.toml"),
            exists: true,
            permissions: crate::auth::secret_file::Permissions::OwnerOnly {
                mode: "0600".to_string(),
            },
            usable: true,
            directory: std::path::PathBuf::from("/tmp"),
            directory_mode: Some("0700".to_string()),
            directory_problem: None,
            profiles: Vec::new(),
            env_api_key: true,
        };
        let output = info_output(
            "default",
            &Ok("https://docs.example.com".to_string()),
            &health,
            &Ok(None),
            &Some(Snapshot::fixed(
                crate::auth::selection::Method::EnvApiKey,
                Vec::new(),
            )),
            &None,
        );
        let rendered = serde_json::to_string(&output.value).unwrap();
        for forbidden in [
            "access_token",
            "refresh_token",
            "api_key",
            "client_secret",
            "registration_access_token",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden} appears in auth info output: {rendered}"
            );
        }
    }
}
