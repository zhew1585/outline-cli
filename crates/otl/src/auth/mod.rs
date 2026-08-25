//! Authentication: credential storage, OAuth login, and the credential the
//! request channel uses.
//!
//! Module map:
//!
//! - [`paths`]: where the credential file lives, and which profile is active.
//! - [`secret_file`]: the create-owner-only / validate / atomic-write
//!   filesystem primitives every credential read and write goes through.
//! - [`credentials`]: the credential file's contents.
//! - [`lock`]: the advisory lock that makes token refresh single-flight.
//! - [`endpoint`]: the one place that speaks HTTP to an OAuth endpoint.
//! - [`metadata`], [`pkce`], [`loopback`], [`oauth`], [`dcr`]: the pieces of
//!   the authorization-code flow.
//! - [`source`]: the `engine::CredentialSource` the request channel calls.
//! - [`login`], [`logout`], [`report`]: what the `otl auth` subcommands do.
//!
//! The profile helper here is deliberately minimal (an environment variable
//! and a default): the full configuration and profile system lives
//! elsewhere, and this module only needs a name to file credentials under.

pub mod browser;
pub mod credentials;
pub mod dcr;
pub mod endpoint;
pub mod error;
pub mod lock;
pub mod login;
pub mod logout;
pub mod loopback;
pub mod metadata;
pub mod oauth;
pub mod paths;
pub mod pkce;
pub mod report;
pub mod secret_file;
pub mod source;

use std::sync::Arc;

use engine::EngineError;
use thiserror::Error;

use crate::auth::credentials::{CredentialFile, CredentialStore};
use crate::auth::error::{OAuthError, StoreError};
use crate::auth::source::CredentialProvider;
use crate::config::{ConfigError, ENV_API_KEY, ENV_URL};
use crate::errors::map_engine_error;
use crate::exit::{CliError, ExitCode};

/// Operation name used to identify the authenticated user.
///
/// Goes through the ordinary request channel like any other call; there is
/// no bespoke HTTP for it.
pub const IDENTITY_OPERATION: &str = "auth.info";

/// Anything that can go wrong before a request is even attempted.
#[derive(Debug, Error)]
pub enum AuthError {
    /// The credential store is unusable.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// An OAuth interaction failed.
    #[error(transparent)]
    OAuth(#[from] OAuthError),
    /// The instance URL is not configured.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A request through the ordinary channel failed.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// Nothing at all is available to authenticate with.
    #[error(
        "no credentials for profile {profile:?}.\n\
         Sign in with a browser:\n\
         \x20 otl auth login\n\
         or store an API key (Settings -> API in Outline):\n\
         \x20 otl auth set-key\n\
         or, for CI, set {ENV_API_KEY} in the environment."
    )]
    NoCredentials {
        /// The profile that has nothing stored.
        profile: String,
    },
}

/// Everything one command needs in order to talk to an instance.
pub struct Session {
    /// Base URL of the instance.
    pub base_url: String,
    /// Active profile name.
    pub profile: String,
    /// The credential store backing this profile.
    pub store: CredentialStore,
    /// The credential the request channel will use.
    pub provider: Arc<CredentialProvider>,
}

/// The instance base URL, from the environment.
///
/// Kept local and minimal on purpose - the configuration system that will
/// resolve this from a profile file lives elsewhere - but it reuses the
/// same error, so the message a user sees is identical either way.
pub fn base_url() -> Result<String, AuthError> {
    std::env::var(ENV_URL)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or(AuthError::Config(ConfigError::MissingUrl))
}

/// Resolve the active profile's credentials.
pub fn open_session() -> Result<Session, AuthError> {
    // URL first: with no instance to talk to, which credential would be
    // used is not yet an interesting question.
    let base_url = base_url()?;
    let profile = paths::active_profile()?;
    let store = CredentialStore::discover()?;
    let file = store.load()?;
    let provider =
        CredentialProvider::resolve(store.clone(), &profile, &file)?.ok_or_else(|| {
            AuthError::NoCredentials {
                profile: profile.clone(),
            }
        })?;
    Ok(Session {
        base_url,
        profile,
        store,
        provider: Arc::new(provider),
    })
}

/// The credential store and profile, without requiring any credential.
///
/// Used by `auth login` and `auth set-key`, which run precisely when there
/// is nothing stored yet.
pub fn open_store() -> Result<(String, CredentialStore, CredentialFile), AuthError> {
    let profile = paths::active_profile()?;
    let store = CredentialStore::discover()?;
    let file = store.load()?;
    Ok((profile, store, file))
}

/// Build the request-channel client for the active profile.
///
/// The single entry point every command uses: it wires the resolved
/// credential - API key or auto-renewing OAuth session - into
/// `engine::Client`, so renewal happens inside the one request channel
/// rather than in each command.
pub fn client() -> Result<engine::Client, CliError> {
    let session = open_session().map_err(map_auth_error)?;
    engine::Client::with_credentials(&session.base_url, session.provider).map_err(map_engine_error)
}

/// Who the credential belongs to, as the instance reports it.
#[derive(Debug, Clone, Default)]
pub struct Identity {
    /// Display name of the authenticated user.
    pub user: Option<String>,
    /// Email of the authenticated user.
    pub email: Option<String>,
    /// Workspace (team) name.
    pub workspace: Option<String>,
}

impl Identity {
    /// A single-line account label, or `None` when nothing is known.
    pub fn account(&self) -> Option<String> {
        match (&self.user, &self.email) {
            (Some(user), Some(email)) => Some(format!("{user} <{email}>")),
            (Some(user), None) => Some(user.clone()),
            (None, Some(email)) => Some(email.clone()),
            (None, None) => None,
        }
    }
}

/// Ask the instance who this credential belongs to.
///
/// Goes through `engine::Client`, i.e. the ordinary request channel: local
/// validation, throttling, backoff and token renewal all apply.
pub fn fetch_identity(client: &engine::Client) -> Result<Identity, AuthError> {
    let op = crate::ops::find(IDENTITY_OPERATION).ok_or_else(|| {
        AuthError::OAuth(OAuthError::Malformed {
            stage: error::Stage::Discovery,
            origin: "the compiled operation table".to_string(),
            reason: format!("operation {IDENTITY_OPERATION:?} is not in the vendored spec"),
        })
    })?;
    let response = client.execute(op, &[], engine::ValidationMode::Strict)?;
    let data = response.get("data").unwrap_or(&response);
    Ok(Identity {
        user: string_at(data, &["user", "name"]),
        email: string_at(data, &["user", "email"]),
        workspace: string_at(data, &["team", "name"]),
    })
}

/// A non-empty string at a JSON path.
fn string_at(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Map an authentication failure to a documented exit code.
///
/// The split that matters to a script: exit 4 means "authenticate again",
/// exit 2 means "fix something locally". See `docs/exit-codes.md`.
pub fn map_auth_error(error: AuthError) -> CliError {
    match error {
        AuthError::Engine(inner) => map_engine_error(inner),
        AuthError::OAuth(inner) => {
            let code = oauth_exit_code(&inner);
            CliError::new(code, inner)
        }
        // Every store failure is something the user fixes on their own
        // machine: a permission bit, a path, a stale lock.
        AuthError::Store(inner) => CliError::new(ExitCode::Usage, inner),
        AuthError::Config(inner) => CliError::new(ExitCode::Usage, inner),
        AuthError::NoCredentials { .. } => CliError::new(ExitCode::Usage, error),
    }
}

/// Exit code for one OAuth failure.
fn oauth_exit_code(error: &OAuthError) -> ExitCode {
    match error {
        // The login itself did not complete, or the stored session is
        // finished: all "authenticate again".
        OAuthError::SessionExpired { .. }
        | OAuthError::RotationLost { .. }
        | OAuthError::AuthorizationDenied { .. }
        | OAuthError::StateMismatch
        | OAuthError::CallbackTimeout { .. }
        | OAuthError::ForeignEndpoint { .. } => ExitCode::Auth,
        // Fixable locally: get a client id, free a port.
        OAuthError::RegistrationUnavailable { .. } | OAuthError::NoCallbackPort { .. } => {
            ExitCode::Usage
        }
        OAuthError::Transport { .. } => ExitCode::Network,
        OAuthError::Endpoint { status, .. } => match status {
            401 | 403 => ExitCode::Auth,
            429 => ExitCode::RateLimited,
            500..=599 => ExitCode::Server,
            _ => ExitCode::ApiRequest,
        },
        OAuthError::Malformed { .. }
        | OAuthError::Callback { .. }
        | OAuthError::Random { .. }
        | OAuthError::Browser { .. } => ExitCode::Failure,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn the_no_credentials_message_names_every_way_in() {
        let text = AuthError::NoCredentials {
            profile: "default".to_string(),
        }
        .to_string();
        assert!(text.contains("otl auth login"), "{text}");
        assert!(text.contains("otl auth set-key"), "{text}");
        assert!(text.contains(ENV_API_KEY), "{text}");
    }

    #[test]
    fn a_finished_session_exits_with_the_authentication_code() {
        let mapped = map_auth_error(AuthError::OAuth(OAuthError::SessionExpired {
            profile: "default".to_string(),
            detail: String::new(),
        }));
        assert_eq!(mapped.code, ExitCode::Auth);
        assert!(mapped.to_string().contains("otl auth login"));
    }

    #[test]
    fn a_local_problem_exits_with_the_configuration_code() {
        for error in [
            AuthError::Store(StoreError::NoConfigDir),
            AuthError::OAuth(OAuthError::RegistrationUnavailable {
                redirect_uri: "http://127.0.0.1:8586/callback".to_string(),
            }),
            AuthError::OAuth(OAuthError::NoCallbackPort {
                ports: "8586".to_string(),
            }),
            AuthError::NoCredentials {
                profile: "default".to_string(),
            },
        ] {
            assert_eq!(
                map_auth_error(error).code,
                ExitCode::Usage,
                "a locally fixable problem must exit 2"
            );
        }
    }

    #[test]
    fn oauth_endpoint_statuses_map_to_their_documented_codes() {
        let endpoint = |status| OAuthError::Endpoint {
            stage: error::Stage::Refresh,
            origin: "https://docs.example.com".to_string(),
            status,
            detail: String::new(),
        };
        assert_eq!(oauth_exit_code(&endpoint(401)), ExitCode::Auth);
        assert_eq!(oauth_exit_code(&endpoint(403)), ExitCode::Auth);
        assert_eq!(oauth_exit_code(&endpoint(400)), ExitCode::ApiRequest);
        assert_eq!(oauth_exit_code(&endpoint(429)), ExitCode::RateLimited);
        assert_eq!(oauth_exit_code(&endpoint(503)), ExitCode::Server);
    }

    #[test]
    fn a_network_failure_during_login_exits_with_the_network_code() {
        let mapped = map_auth_error(AuthError::OAuth(OAuthError::Transport {
            stage: error::Stage::Discovery,
            origin: "https://docs.example.com".to_string(),
            reason: "connection failed (DNS, refused, or TLS)".to_string(),
        }));
        assert_eq!(mapped.code, ExitCode::Network);
    }

    #[test]
    fn identity_is_read_out_of_the_outline_envelope() {
        let response = json!({
            "data": {
                "user": { "name": "Alice Example", "email": "alice@example.com" },
                "team": { "name": "Acme" }
            }
        });
        let data = response.get("data").unwrap();
        let identity = Identity {
            user: string_at(data, &["user", "name"]),
            email: string_at(data, &["user", "email"]),
            workspace: string_at(data, &["team", "name"]),
        };
        assert_eq!(
            identity.account().as_deref(),
            Some("Alice Example <alice@example.com>")
        );
        assert_eq!(identity.workspace.as_deref(), Some("Acme"));
    }

    #[test]
    fn a_response_without_identity_fields_yields_no_account_label() {
        let identity = Identity::default();
        assert!(identity.account().is_none());
        assert!(string_at(&json!({ "user": { "name": "  " } }), &["user", "name"]).is_none());
        assert!(string_at(&json!({}), &["user", "name"]).is_none());
    }

    #[test]
    fn the_identity_operation_exists_in_the_compiled_spec() {
        assert!(
            crate::ops::find(IDENTITY_OPERATION).is_some(),
            "{IDENTITY_OPERATION} is missing from the IR, so login could not \
             report who signed in"
        );
    }
}
