//! `otl auth` - login, logout, API key storage, and status.
//!
//! Output follows the CLI contract: stdout carries the command's result
//! (human-readable on a terminal, JSON otherwise), stderr carries prompts,
//! progress and warnings. Nothing printed by this module is ever a
//! credential or a fragment of one.

use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::auth::error::StoreError;
use crate::auth::loopback;
use crate::auth::metadata::DEFAULT_SCOPE;
use crate::auth::report::{self, CredentialHealth};
use crate::auth::source::{CredentialProvider, Snapshot};
use crate::auth::{self, login, logout, AuthError, Identity};
use crate::exit::{CliError, ExitCode};
use crate::render::{self, OutputMode};
use crate::stdio;

/// Maximum accepted length of an API key read from stdin.
///
/// Outline keys are far shorter; the cap stops a mistyped `cat` of a large
/// file from being stored as a credential.
const MAX_API_KEY_BYTES: u64 = 4096;

/// Prompt shown when stdin is a terminal.
const KEY_PROMPT: &str = "Paste your Outline API key (Settings -> API) and press Enter.\n\
     Note: the key will be visible as you type - hidden input is not \
     implemented yet. Piping avoids that: otl auth set-key < key.txt";

/// `otl auth <subcommand>`.
#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

/// The `otl auth` subcommands.
#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Sign in with a browser (OAuth 2.0 authorization code + PKCE).
    Login(LoginArgs),
    /// Forget this profile's credentials and revoke them on the server.
    Logout(LogoutArgs),
    /// Store an API key in the credential file, read from stdin.
    SetKey,
    /// Show which credential is in use and where it is stored.
    Info(InfoArgs),
}

/// Arguments for `otl auth login`.
#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Client id of an application an administrator registered.
    ///
    /// Without it, otl registers itself dynamically when the instance
    /// allows it.
    #[arg(long, value_name = "ID")]
    client_id: Option<String>,

    /// OAuth scope to request.
    #[arg(long, default_value = DEFAULT_SCOPE)]
    scope: String,

    /// Print the authorization URL instead of opening a browser.
    #[arg(long)]
    no_browser: bool,

    /// Seconds to wait for the browser redirect.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    timeout: Option<u64>,
}

/// Arguments for `otl auth logout`.
#[derive(Debug, Args)]
pub struct LogoutArgs {
    /// Also delete the application otl registered for itself on the server.
    ///
    /// A dynamically registered client cannot be removed from Outline's
    /// admin UI, so this is the only way to clean one up. An application an
    /// administrator created is never touched.
    #[arg(long)]
    purge: bool,
}

/// Arguments for `otl auth info`.
#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Do not contact the instance; report stored state only.
    #[arg(long)]
    offline: bool,
}

/// Run the `auth` subcommand.
pub fn run(args: &AuthArgs, mode: OutputMode) -> Result<(), CliError> {
    match &args.command {
        AuthCommand::Login(login_args) => run_login(login_args, mode),
        AuthCommand::Logout(logout_args) => run_logout(logout_args, mode),
        AuthCommand::SetKey => run_set_key(mode),
        AuthCommand::Info(info_args) => run_info(info_args, mode),
    }
}

/// `otl auth login`.
fn run_login(args: &LoginArgs, mode: OutputMode) -> Result<(), CliError> {
    let base_url = auth::base_url().map_err(auth::map_auth_error)?;
    let (profile, store, file) = auth::open_store().map_err(auth::map_auth_error)?;
    let options = login::Options {
        client_id: args.client_id.clone(),
        scope: args.scope.clone(),
        timeout: args
            .timeout
            .map_or(loopback::AUTH_TIMEOUT, Duration::from_secs),
        open_browser: !args.no_browser,
    };
    let outcome =
        login::run(&base_url, &profile, &store, file, &options).map_err(auth::map_auth_error)?;
    emit(login_output(&profile, &outcome), mode)
}

/// `otl auth logout`.
fn run_logout(args: &LogoutArgs, mode: OutputMode) -> Result<(), CliError> {
    let (profile, store, _) = auth::open_store().map_err(auth::map_auth_error)?;
    let report = logout::run(&profile, &store, logout::Options { purge: args.purge })
        .map_err(auth::map_auth_error)?;
    for warning in &report.warnings {
        stdio::write_diagnostic_line(&format!("warning: {warning}"));
    }
    emit(logout_output(&profile, &report), mode)
}

/// `otl auth set-key`.
fn run_set_key(mode: OutputMode) -> Result<(), CliError> {
    let (profile, store, mut file) = auth::open_store().map_err(auth::map_auth_error)?;
    let key = read_api_key()?;
    file.profile_mut(&profile).api_key = Some(key);
    store
        .save(&file)
        .map_err(|error| auth::map_auth_error(AuthError::Store(error)))?;
    let health = report::credential_health(&store);
    emit(set_key_output(&profile, &health), mode)
}

/// `otl auth info`.
fn run_info(args: &InfoArgs, mode: OutputMode) -> Result<(), CliError> {
    let profile = auth::paths::active_profile()
        .map_err(|error| auth::map_auth_error(AuthError::Store(error)))?;
    let store = auth::credentials::CredentialStore::discover()
        .map_err(|error| auth::map_auth_error(AuthError::Store(error)))?;
    let health = report::credential_health(&store);
    let base_url = auth::base_url().ok();

    // A report must work even when the file cannot be used - that is
    // exactly when it is needed - so resolution failures become part of
    // the output rather than aborting it.
    let resolved = resolve_for_info(&store, &profile);
    let identity = live_identity(args, &base_url, &resolved);
    emit(
        info_output(&profile, base_url.as_deref(), &health, &resolved, &identity),
        mode,
    )
}

/// Resolve the profile's credential for reporting, tolerating failure.
fn resolve_for_info(
    store: &auth::credentials::CredentialStore,
    profile: &str,
) -> Result<Option<Arc<CredentialProvider>>, String> {
    let file = store
        .load()
        .map_err(|error: StoreError| error.to_string())?;
    CredentialProvider::resolve(store.clone(), profile, &file)
        .map(|provider| provider.map(Arc::new))
        .map_err(|error| error.to_string())
}

/// Ask the instance who we are, unless told not to or unable to.
fn live_identity(
    args: &InfoArgs,
    base_url: &Option<String>,
    resolved: &Result<Option<Arc<CredentialProvider>>, String>,
) -> Option<Result<Identity, String>> {
    if args.offline {
        return None;
    }
    let (Some(base_url), Ok(Some(provider))) = (base_url, resolved) else {
        return None;
    };
    let source: Arc<dyn engine::CredentialSource> = Arc::clone(provider) as _;
    let outcome = engine::Client::with_credentials(base_url, source)
        .map_err(AuthError::Engine)
        .and_then(|client| auth::fetch_identity(&client))
        .map_err(|error| error.to_string());
    Some(outcome)
}

/// Read an API key from stdin, refusing anything unusable as a header.
fn read_api_key() -> Result<String, CliError> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        stdio::write_diagnostic_line(KEY_PROMPT);
    }
    let mut raw = String::new();
    stdin
        .lock()
        .take(MAX_API_KEY_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|error| {
            CliError::usage(anyhow!("could not read the API key from stdin: {error}"))
        })?;
    if raw.len() as u64 > MAX_API_KEY_BYTES {
        return Err(CliError::usage(anyhow!(
            "the input is longer than {MAX_API_KEY_BYTES} bytes, which an API \
             key never is; nothing was stored"
        )));
    }
    validate_api_key(raw.trim())
}

/// Check a key can be used, without ever echoing it.
///
/// An API key with a control character in it cannot be sent as an HTTP
/// header at all, so rejecting it here turns a confusing request-build
/// failure on every later command into one clear message now.
fn validate_api_key(key: &str) -> Result<String, CliError> {
    if key.is_empty() {
        return Err(CliError::usage(anyhow!(
            "no API key was read from stdin; nothing was stored.\n\
             Pipe it in (otl auth set-key < key.txt) or paste it when prompted."
        )));
    }
    if key.chars().any(char::is_control) || key.chars().any(char::is_whitespace) {
        return Err(CliError::usage(anyhow!(
            "the API key contains whitespace or control characters, so it \
             cannot be sent as an HTTP header; nothing was stored. \
             (The value is not shown here in case it is a real secret.)"
        )));
    }
    Ok(key.to_string())
}

/// Human lines plus the machine-readable object for `auth login`.
fn login_output(profile: &str, outcome: &login::Outcome) -> Output {
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
fn logout_output(profile: &str, report: &logout::Report) -> Output {
    let headline = if report.had_credentials {
        format!("Signed out of profile {profile}.")
    } else {
        format!("Nothing was stored for profile {profile}.")
    };
    Output {
        lines: vec![
            headline,
            format!("tokens revoked on the server: {}", report.revoked),
            format!(
                "application deleted:          {}",
                report.registration_deleted
            ),
            format!("credential file removed:      {}", report.file_removed),
        ],
        value: json!({
            "profile": profile,
            "had_credentials": report.had_credentials,
            "revoked": report.revoked,
            "registration_deleted": report.registration_deleted,
            "credential_file_removed": report.file_removed,
            "warnings": report.warnings,
        }),
    }
}

/// Human lines plus the machine-readable object for `auth set-key`.
fn set_key_output(profile: &str, health: &CredentialHealth) -> Output {
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
fn info_output(
    profile: &str,
    base_url: Option<&str>,
    health: &CredentialHealth,
    resolved: &Result<Option<Arc<CredentialProvider>>, String>,
    identity: &Option<Result<Identity, String>>,
) -> Output {
    let snapshot = resolved
        .as_ref()
        .ok()
        .and_then(|provider| provider.as_ref())
        .map(|provider| provider.snapshot());
    let mut lines = vec![format!("profile:          {profile}")];
    lines.push(format!(
        "instance:         {}",
        base_url
            .and_then(engine::base_url_origin)
            .unwrap_or_else(|| "not configured (set OUTLINE_URL)".to_string())
    ));
    lines.extend(method_lines(resolved, snapshot.as_ref()));
    lines.extend(identity_lines(snapshot.as_ref(), identity));
    lines.extend(health.lines());
    Output {
        lines,
        value: json!({
            "profile": profile,
            "instance": base_url.and_then(engine::base_url_origin),
            "method": snapshot.as_ref().map(|snapshot| snapshot.method.label()),
            "available": snapshot
                .as_ref()
                .map(|snapshot| snapshot.available.iter().map(|m| m.label()).collect::<Vec<_>>())
                .unwrap_or_default(),
            "scope": snapshot.as_ref().and_then(|snapshot| snapshot.scope.clone()),
            "account": identity_value(snapshot.as_ref(), identity),
            "expires_in_seconds": snapshot.as_ref().and_then(|snapshot| snapshot.expires_in),
            "renewable": snapshot.as_ref().is_some_and(|snapshot| snapshot.renewable),
            "credential_file": health.path.display().to_string(),
            "credential_file_exists": health.exists,
            "credential_file_permissions": health.permissions.describe(),
            "credential_file_usable": health.usable,
            "resolution_error": resolved.as_ref().err(),
        }),
    }
}

/// The `method` / `available` lines of `auth info`.
fn method_lines(
    resolved: &Result<Option<Arc<CredentialProvider>>, String>,
    snapshot: Option<&Snapshot>,
) -> Vec<String> {
    match (resolved, snapshot) {
        (Err(error), _) => vec![format!("method:           unavailable ({error})")],
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
        (Ok(Some(_)), None) => vec!["method:           unknown".to_string()],
    }
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
struct Output {
    lines: Vec<String>,
    value: Value,
}

/// Print a command result: human text on a terminal, JSON otherwise.
fn emit(output: Output, mode: OutputMode) -> Result<(), CliError> {
    match mode {
        OutputMode::Json => {
            let rendered = render::render(&output.value, mode).map_err(|error| {
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
    fn an_empty_key_is_refused_with_instructions() {
        let error = validate_api_key("").expect_err("an empty key must be refused");
        assert_eq!(error.code, ExitCode::Usage);
        let text = error.to_string();
        assert!(text.contains("set-key < key.txt"), "{text}");
        assert!(text.contains("nothing was stored"), "{text}");
    }

    #[test]
    fn a_key_with_a_newline_is_refused_without_echoing_it() {
        let error = validate_api_key("bad\nkey-SECRET-9c7a")
            .expect_err("a key with a control character cannot be a header");
        let text = format!("{error} / {error:?}");
        assert!(!text.contains("SECRET-9c7a"), "key leaked: {text}");
        assert!(text.contains("HTTP header"), "{text}");
    }

    #[test]
    fn a_key_with_an_inner_space_is_refused_without_echoing_it() {
        let error =
            validate_api_key("two SECRET-words").expect_err("a space cannot be in a header");
        assert!(!error.to_string().contains("SECRET"), "{error}");
    }

    #[test]
    fn an_ordinary_key_is_accepted_and_trimmed_by_the_caller() {
        assert_eq!(validate_api_key("ol_api_abc123").unwrap(), "ol_api_abc123");
    }

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
            profiles: Vec::new(),
            env_api_key: false,
        };
        let resolved = Err("credential file is accessible to users other than you".to_string());
        let output = info_output("default", None, &health, &resolved, &None);
        let rendered = output.lines.join("\n");
        assert!(rendered.contains("0644"), "{rendered}");
        assert!(rendered.contains("unavailable"), "{rendered}");
        assert!(rendered.contains("not configured"), "{rendered}");
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
            profiles: Vec::new(),
            env_api_key: true,
        };
        let output = info_output(
            "default",
            Some("https://docs.example.com"),
            &health,
            &Ok(None),
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
