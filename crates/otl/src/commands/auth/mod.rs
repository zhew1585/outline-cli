//! `otl auth` - login, logout, API key storage, and status.
//!
//! Output follows the CLI contract: stdout carries the command's result
//! (human-readable on a terminal, JSON otherwise), stderr carries prompts,
//! progress and warnings. Nothing printed by this module is ever a
//! credential or a fragment of one.

mod output;

use std::io::{IsTerminal, Read};
use std::time::Duration;

use anyhow::anyhow;
use clap::{Args, Subcommand};

use crate::auth::error::StoreError;
use crate::auth::loopback;
use crate::auth::metadata::DEFAULT_SCOPE;
use crate::auth::report;
use crate::auth::{self, login, logout, AuthError, Identity};
use crate::commands::auth::output::{
    emit, info_output, login_output, logout_output, set_key_output,
};
use crate::config::{ConfigError, Overrides};
use crate::exit::{CliError, ExitCode};
use crate::render::OutputMode;
use crate::stdio;

/// Maximum accepted length of an API key read from stdin.
///
/// Outline keys are far shorter; the cap stops a mistyped `cat` of a large
/// file from being stored as a credential.
const MAX_API_KEY_BYTES: u64 = 4096;

/// Prompt shown when stdin is a terminal. Input is not echoed.
const KEY_PROMPT: &str = "Paste your Outline API key (Settings -> API) and press Enter. \
     Input is hidden.";

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

    /// Register a new application even if the stored one cannot be removed
    /// from the server first.
    ///
    /// Only needed when a previous registration's callback port is
    /// permanently unavailable AND the server refuses to delete it. It
    /// leaves an application an administrator has to remove by hand, so it
    /// is never the default.
    #[arg(long)]
    force_new_client: bool,
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

    /// Discard the local credentials even if the server could not be told.
    ///
    /// By default a step that could still succeed later - a revocation the
    /// instance was too unreachable to accept, say - keeps the credentials
    /// on disk, because they are the only thing that makes the retry
    /// possible. This says: I accept that these tokens will stay live on
    /// the server until they expire.
    #[arg(long)]
    force: bool,
}

/// Arguments for `otl auth info`.
#[derive(Debug, Args)]
#[command(after_long_help = "API contract:
  Without --offline this command makes one auth.info probe, to name the
  account and workspace the stored credential actually belongs to. With
  --offline it contacts nothing.

  Inspect it with:
    otl api describe auth.info --json

JSON shape:
  An object this CLI authors, not a server payload. Every string in it is
  scrubbed, and NO field ever carries a credential:

    profile                        active profile name
    instance                       base URL a request would go to, or null
    instance_problem               why it is null, or null
    method                         \"api-key\" / \"oauth\" / null
    available                      credential kinds this profile could use
    plaintext_key_in_environment   the key is visible in the environment
    scope, account, expires_in_seconds, renewable
                                   from the probe; null when --offline
    credential_file                path
    credential_file_exists         bool
    credential_file_permissions    human-readable, e.g. \"0600\"
    credential_file_usable         false means a command would refuse
    credential_directory           path
    credential_directory_mode      Unix mode, or null on Windows
    credential_directory_problem   why it is unsound, or null
    resolution_error               why no credential resolved, or null

  `otl doctor --json` answers the same questions inside a wider report; use
  this one when the credential is the whole question.")]
pub struct InfoArgs {
    /// Do not contact the instance; report stored state only.
    #[arg(long)]
    offline: bool,
}

/// Run the `auth` subcommand.
pub fn run(args: &AuthArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    match &args.command {
        AuthCommand::Login(login_args) => run_login(login_args, mode, overrides),
        AuthCommand::Logout(logout_args) => run_logout(logout_args, mode, overrides),
        AuthCommand::SetKey => run_set_key(mode, overrides),
        AuthCommand::Info(info_args) => run_info(info_args, mode, overrides),
    }
}

/// `otl auth login`.
fn run_login(args: &LoginArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let context = auth::open_store(overrides).map_err(auth::map_auth_error)?;
    let options = login::Options {
        client_id: args.client_id.clone(),
        scope: args.scope.clone(),
        timeout: args
            .timeout
            .map_or(loopback::AUTH_TIMEOUT, Duration::from_secs),
        open_browser: !args.no_browser,
        force_new_client: args.force_new_client,
    };
    let outcome = login::run(
        &context.base_url,
        &context.profile,
        &context.origin,
        &context.store,
        &options,
    )
    .map_err(auth::map_auth_error)?;
    emit(login_output(&context.profile, &outcome), mode)
}

/// `otl auth logout`.
///
/// Deliberately does NOT resolve an instance URL: see
/// `auth::open_store_without_instance`. Exits non-zero when a server-side
/// step the user asked for did not happen - the local credentials may be
/// gone, but "the tokens are still live on the server" is not success, and
/// a script has to be able to see the difference.
fn run_logout(args: &LogoutArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let (profile, store) =
        auth::open_store_without_instance(overrides).map_err(auth::map_auth_error)?;
    let report = logout::run(
        &profile,
        &store,
        logout::Options {
            purge: args.purge,
            force: args.force,
        },
    )
    .map_err(auth::map_auth_error)?;
    for warning in &report.warnings {
        stdio::write_diagnostic_line(&format!("warning: {warning}"));
    }
    emit(logout_output(&profile, &report), mode)?;
    if report.remote_cleanup_failed {
        // Exit 9 (partial failure), not 3: the local half of the logout DID
        // happen (or was deliberately kept for a retry), so this is not a
        // request that simply failed - it is a job that got part-way, which
        // is exactly what code 9 means everywhere else in this CLI.
        return Err(CliError::new(
            ExitCode::Partial,
            anyhow!("{}", logout_failure(&report)),
        ));
    }
    Ok(())
}

/// The one-line summary of a logout that did not fully succeed.
fn logout_failure(report: &logout::Report) -> String {
    if report.kept_for_retry {
        return "signed out on the server was not possible, so the local \
                credentials were KEPT to allow a retry; see the warnings above"
            .to_string();
    }
    if report.survived_concurrent_write {
        return "a credential written by another process during this logout is \
                still stored and was never revoked; run `otl auth logout` \
                again"
            .to_string();
    }
    "signed out locally, but not everything could be done on the server; \
     see the warnings above"
        .to_string()
}

/// `otl auth set-key`.
///
/// The key is read BEFORE the credential lock is taken, and the file is
/// then re-read inside it. Holding the lock across a prompt would block
/// every other `otl` process for as long as the terminal sits there, and
/// saving a snapshot read before the prompt would write back whatever a
/// concurrent token refresh had already rotated.
fn run_set_key(mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let context = auth::open_store(overrides).map_err(auth::map_auth_error)?;
    // Checked before the prompt: asking for a secret and then refusing it
    // would be rude.
    auth::ensure_bindable(
        context.stored_profile().as_ref(),
        &context.profile,
        &context.origin,
    )
    .map_err(auth::map_auth_error)?;
    let key = read_api_key()?;
    context
        .store
        .update(
            |file: &mut auth::credentials::CredentialFile| -> Result<(), AuthError> {
                // Re-checked INSIDE the transaction: another process may
                // have bound this profile elsewhere while the prompt was
                // open. Writing here would rewrite `origin` and leave that
                // instance's OAuth session in place - and OAuth outranks an
                // API key, so the next request would send the wrong
                // instance's token.
                auth::ensure_bindable(
                    file.profile(&context.profile),
                    &context.profile,
                    &context.origin,
                )?;
                let entry = file.profile_mut(&context.profile);
                entry.origin = Some(context.origin.clone());
                entry.api_key = Some(key);
                Ok(())
            },
        )
        .map_err(auth::map_auth_error)?;
    let health = report::credential_health(&context.store);
    emit(set_key_output(&context.profile, &health), mode)
}

/// `otl auth info`.
///
/// Reports through the SAME resolution every other command uses
/// ([`auth::resolve_credential`]), not a second one of its own. It had one:
/// it resolved the instance through config but the credential through a path
/// that read `OUTLINE_API_KEY` directly, so with a profile selected
/// `otl api` refused the global key ("OUTLINE_API_KEY_WORK is not set") while
/// `otl auth info` sent it to the profile's instance. A report whose
/// credential path differs from the real one is worse than no report: it
/// tells the user their setup works.
fn run_info(args: &InfoArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let profile = auth::active_profile(overrides).map_err(auth::map_auth_error)?;
    let store = auth::credentials::CredentialStore::discover()
        .map_err(|error| auth::map_auth_error(AuthError::Store(error)))?;
    let health = report::credential_health(&store);
    // The same resolution every other command performs, including the
    // transport rule, so `auth info` reports the instance exactly as it
    // would be used - a plaintext remote URL is shown as unusable here
    // rather than quietly accepted and refused everywhere else.
    let instance = auth::resolve_instance(overrides).map_err(|error| error.to_string());

    // A report must work even when nothing can be used - that is exactly
    // when it is needed - so failures become part of the output rather
    // than aborting it.
    let resolved = resolve_for_info(&instance, &store);
    let summary = resolved
        .as_ref()
        .ok()
        .and_then(|resolved| resolved.as_ref())
        .map(|resolved| resolved.summary.clone());
    let reported = resolved
        .as_ref()
        .map(|resolved| resolved.as_ref().map(|_| ()))
        .map_err(String::clone);
    let identity = live_identity(args, &instance, resolved);
    let instance = instance
        .as_ref()
        .map(|instance| instance.origin().to_string())
        .map_err(String::clone);
    emit(
        info_output(&profile, &instance, &health, &reported, &summary, &identity),
        mode,
    )
}

/// Resolve the profile's credential for reporting, tolerating failure.
///
/// `Ok(None)` means "no instance is configured, so nothing could be checked
/// against one" - the report then describes the credential FILE rather than
/// claiming a usable credential. `Err` is a resolution that was attempted and
/// refused, which is the interesting case and is printed as such.
fn resolve_for_info(
    instance: &Result<auth::Instance, String>,
    store: &auth::credentials::CredentialStore,
) -> Result<Option<auth::Resolved>, String> {
    let Ok(instance) = instance else {
        return Ok(None);
    };
    let file = store
        .load()
        .map_err(|error: StoreError| error.to_string())?;
    match auth::resolve_credential(instance, store, &file) {
        Ok(resolved) => Ok(Some(resolved)),
        // "There is nothing anywhere" is not a failure of the report: it is
        // the state the report exists to describe, and `method: none` says
        // it better than an error would.
        Err(AuthError::NoCredentials { .. }) => Ok(None),
        Err(AuthError::Config(ConfigError::MissingApiKey)) => Ok(None),
        // Everything else IS worth saying: a profile whose own variable is
        // unset while a global one is exported, an `auth = "oauth"` profile
        // with an empty credential file, a credential bound elsewhere. Each
        // of those has a remedy, and each is a state where the user believes
        // they are configured. Reported, not swallowed as "none".
        Err(error) => Err(error.to_string()),
    }
}

/// Ask the instance who we are, unless told not to or unable to.
///
/// Uses the credential [`auth::resolve_credential`] approved, by turning it
/// into a request channel - the only thing that can be done with one. There
/// is no path here that could assemble a different credential.
fn live_identity(
    args: &InfoArgs,
    instance: &Result<auth::Instance, String>,
    resolved: Result<Option<auth::Resolved>, String>,
) -> Option<Result<Identity, String>> {
    if args.offline {
        return None;
    }
    let (Ok(instance), Ok(Some(resolved))) = (instance, resolved) else {
        return None;
    };
    let outcome = resolved
        .into_client(instance.base_url())
        .and_then(|client| auth::fetch_identity(&client))
        .map_err(|error| error.to_string());
    Some(outcome)
}

/// Read an API key from stdin, refusing anything unusable as a header.
///
/// On a terminal the key is read with ECHO DISABLED, so it never appears on
/// screen, in a screen recording, or in the terminal's scrollback. Telling
/// the user to pipe instead is not a substitute: a pipe puts the key in
/// shell history or in another process's arguments, which is the same
/// exposure in a different place.
///
/// When stdin is not a terminal (a pipe, a file, a test harness) there is no
/// echo to suppress and the bytes are read directly.
fn read_api_key() -> Result<String, CliError> {
    let raw = if std::io::stdin().is_terminal() {
        stdio::write_diagnostic_line(KEY_PROMPT);
        rpassword::read_password()
            .map_err(|error| CliError::usage(anyhow!("could not read the API key: {error}")))?
    } else {
        read_piped_key()?
    };
    if raw.len() as u64 > MAX_API_KEY_BYTES {
        return Err(CliError::usage(anyhow!(
            "the input is longer than {MAX_API_KEY_BYTES} bytes, which an API \
             key never is; nothing was stored"
        )));
    }
    validate_api_key(raw.trim())
}

/// Read a key from a non-terminal stdin, capped.
fn read_piped_key() -> Result<String, CliError> {
    let mut raw = String::new();
    std::io::stdin()
        .lock()
        .take(MAX_API_KEY_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|error| {
            CliError::usage(anyhow!("could not read the API key from stdin: {error}"))
        })?;
    Ok(raw)
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
}
