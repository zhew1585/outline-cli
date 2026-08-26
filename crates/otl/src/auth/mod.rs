//! Authentication: credential storage, OAuth login, and the credential the
//! request channel uses.
//!
//! Module map:
//!
//! - [`paths`]: where the credential file lives, and which profile is active.
//! - [`secret_file`]: the create-owner-only / atomic-write filesystem
//!   primitives every credential read and write goes through.
//! - [`file_guard`]: what is allowed to hold credentials - permissions,
//!   file type, ownership, and the directory around them.
//! - [`credentials`]: the credential file's contents.
//! - [`lock`]: the advisory lock that makes token refresh single-flight.
//! - [`endpoint`]: the one place that speaks HTTP to an OAuth endpoint.
//! - [`metadata`], [`pkce`], [`loopback`], [`callback_request`], [`oauth`],
//!   [`dcr`]: the pieces of the authorization-code flow.
//! - [`transport`]: the TLS rule every OAuth URL has to pass.
//! - [`selection`]: which credential a profile offers, and whether it may
//!   be used against the instance in hand.
//! - [`source`]: the `engine::CredentialSource` the request channel calls.
//! - [`client_acquisition`]: which OAuth client a login speaks as.
//! - [`login`], [`logout`], [`logout_remote`], [`report`]: what the
//!   `otl auth` subcommands do.
//!
//! The profile helper here is deliberately minimal (an environment variable
//! and a default): the full configuration and profile system lives
//! elsewhere, and this module only needs a name to file credentials under.

pub mod callback_request;
pub mod client_acquisition;
pub mod credentials;
pub mod dcr;
pub mod endpoint;
pub mod error;
pub mod file_guard;
pub mod lock;
pub mod login;
pub mod logout;
pub mod logout_remote;
pub mod loopback;
pub mod metadata;
pub mod oauth;
pub mod paths;
pub mod pkce;
pub mod report;
pub mod secret_file;
pub mod selection;
pub mod source;
pub mod transport;

use std::fmt;
use std::sync::Arc;

use engine::EngineError;
use thiserror::Error;

use crate::auth::credentials::CredentialStore;
use crate::auth::error::{OAuthError, StoreError};
use crate::auth::selection::{available, check_binding, Method, Snapshot};
use crate::auth::source::{warn_about_env_key, CredentialProvider};
use crate::config::{self, ConfigError, EnvLayer, Overrides, Settings, ENV_API_KEY};
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

/// The instance a command is pointed at, resolved through the config layer.
///
/// Wraps [`Settings`] rather than re-deriving anything: `--profile`,
/// `--url`, `--config`, `OUTLINE_PROFILE`, `OUTLINE_URL` and the user config
/// file are all config's business, and `auth` reading the environment on its
/// own is how the two would come to disagree about which instance is in
/// play.
pub struct Instance {
    settings: Settings,
    env: EnvLayer,
    origin: String,
}

impl Instance {
    /// Base URL of the instance.
    pub fn base_url(&self) -> &str {
        self.settings.base_url()
    }

    /// Origin (`scheme://host[:port]`) stored credentials are bound to.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The credential file key for the selected profile.
    ///
    /// Config leaves "no profile selected" as `None`; the credential file
    /// still needs a table name, and [`DEFAULT_PROFILE`] is it.
    pub fn profile_key(&self) -> &str {
        self.settings.profile().unwrap_or(paths::DEFAULT_PROFILE)
    }

    /// The resolved settings, for the credential release gate.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The environment layer, for the environment credential source.
    pub fn env(&self) -> &EnvLayer {
        &self.env
    }
}

/// Resolve which instance is in play, and refuse it if it is not usable.
///
/// The transport rule is applied HERE, at the one place every command's
/// instance is resolved, and not in `auth login` alone: the engine is
/// generic and accepts plain `http` on purpose, so the policy that a
/// credential may not travel in the clear has to live at this boundary.
pub fn resolve_instance(overrides: &Overrides) -> Result<Instance, AuthError> {
    let env = EnvLayer::from_process();
    let loaded = config::load_file(overrides, &env)?;
    let settings = config::resolve_settings(overrides, &env, &loaded)?;
    let origin = usable_origin(settings.base_url())?;
    Ok(Instance {
        settings,
        env,
        origin,
    })
}

/// The profile whose credentials a command should act on, WITHOUT resolving
/// an instance.
///
/// `otl auth logout` needs exactly this and nothing else: every URL it
/// contacts comes out of the credential file, so demanding a usable
/// `OUTLINE_URL` would make cleanup impossible in the states that most need
/// it. Profile selection comes from config's own resolver so the two cannot
/// disagree about which profile is active.
pub fn active_profile(overrides: &Overrides) -> Result<String, AuthError> {
    let env = EnvLayer::from_process();
    let loaded = config::load_file(overrides, &env)?;
    let (name, _) = config::resolve_profile_name(overrides, &env, &loaded);
    Ok(name.unwrap_or(paths::DEFAULT_PROFILE).to_string())
}

/// The origin stored credentials are bound to for a given instance URL,
/// after the shape and transport rules have both passed.
///
/// Origin, not the whole URL: a bearer credential is scoped to a host, and
/// a differing path (a trailing slash, a sub-path) does not make it a
/// different server. A URL with no usable origin cannot be matched against
/// anything, so it is refused here rather than treated as a wildcard.
pub fn usable_origin(base_url: &str) -> Result<String, AuthError> {
    // Validate through the engine first, so a malformed URL produces the
    // engine's own specific diagnosis (a query string, embedded userinfo, a
    // bad scheme) rather than a vaguer one invented here.
    engine::check_base_url(base_url)?;
    // Then the transport rule, for EVERY command and not just `auth login`.
    // The engine is generic and accepts plain http on purpose; the policy
    // that a credential may not travel in the clear belongs here, at the
    // one place that decides which instance a credential is for. Without
    // this, `otl api ...` against `http://remote-host` would put the bearer
    // token on the wire unprotected.
    transport::require_secure(base_url, "the instance URL")?;
    engine::base_url_origin(base_url).ok_or_else(|| {
        AuthError::Engine(EngineError::InvalidBaseUrl {
            reason: "it has no usable scheme/host origin, so credentials \
                     cannot be bound to it"
                .to_string(),
        })
    })
}

/// Refuse to add credentials for `origin` to a profile that already belongs
/// to a different instance.
///
/// The read-side check ([`source`]) refuses to USE mismatched credentials.
/// This is the other half: it stops the mismatched state from being created
/// at all. Without it a write can rewrite `profile.origin` and leave the
/// previous instance's higher-priority credentials in place, which is
/// exactly the state the read-side check was built to catch - and the state
/// it would then wrongly accept, since the profile-level binding now
/// "matches".
///
/// Must be called INSIDE the credential transaction as well as before any
/// network work: another process can bind the profile in between.
pub fn ensure_bindable(
    entry: Option<&credentials::ProfileCredentials>,
    profile: &str,
    origin: &str,
) -> Result<(), AuthError> {
    let Some(entry) = entry else {
        return Ok(());
    };
    if entry.is_empty() || entry.is_bound_to(origin) {
        return Ok(());
    }
    let dynamic_client = entry
        .client
        .as_ref()
        .is_some_and(|registration| registration.dynamic);
    Err(AuthError::OAuth(OAuthError::ProfileBoundElsewhere {
        profile: profile.to_string(),
        stored: entry
            .origin
            .clone()
            .unwrap_or_else(|| "an unrecorded instance".to_string()),
        current: origin.to_string(),
        // A dynamic registration can only be removed with the token stored
        // alongside it, so point at the flag that does that rather than let
        // the user strand an application on the server.
        purge_hint: if dynamic_client { " --purge" } else { "" },
    }))
}

/// The credential store and profile, WITHOUT requiring an instance URL.
///
/// `otl auth logout` uses this: every URL it talks to comes out of the
/// credential file, so requiring `OUTLINE_URL` - and putting it through the
/// transport rule - would make cleanup impossible in exactly the states
/// that need cleaning up most: no instance configured, the wrong one
/// configured, or a plaintext value stored before that rule existed. The
/// only alternative left to a user then is deleting the file by hand, which
/// takes the `registration_access_token` with it and orphans the DCR
/// registration for good.
pub fn open_store_without_instance(
    overrides: &Overrides,
) -> Result<(String, CredentialStore), AuthError> {
    Ok((active_profile(overrides)?, CredentialStore::discover()?))
}

/// The credential store, profile and instance origin, without requiring
/// any credential to exist yet.
///
/// Used by `auth login` and `auth set-key`, which run precisely when there
/// is nothing stored.
pub fn open_store(overrides: &Overrides) -> Result<StoreContext, AuthError> {
    let instance = resolve_instance(overrides)?;
    Ok(StoreContext {
        profile: instance.profile_key().to_string(),
        store: CredentialStore::discover()?,
        base_url: instance.base_url().to_string(),
        origin: instance.origin,
    })
}

impl StoreContext {
    /// What is currently stored for the active profile, if anything.
    ///
    /// Best effort: an unreadable file is reported by the operation that
    /// actually needs it, not by this convenience accessor.
    pub fn stored_profile(&self) -> Option<credentials::ProfileCredentials> {
        self.store
            .load()
            .ok()
            .and_then(|file| file.profile(&self.profile).cloned())
    }
}

/// Where credentials for this invocation live, and which instance they are
/// for.
pub struct StoreContext {
    /// Active profile name.
    pub profile: String,
    /// The credential store.
    pub store: CredentialStore,
    /// Instance base URL.
    pub base_url: String,
    /// Instance origin, which credentials get bound to.
    pub origin: String,
}

/// The credential a command will authenticate with.
///
/// Deliberately opaque and deliberately not `Clone`: the only thing that can
/// be done with one is [`Resolved::into_client`], which consumes it. There is
/// no accessor that hands the secret back, so a future command cannot
/// "just read the key" and route it somewhere the gate never saw.
enum Credential {
    /// A renewable OAuth session. Goes into the engine as a
    /// `CredentialSource` so refresh happens inside the request channel.
    Session(Arc<CredentialProvider>),
    /// One fixed key, released by [`config::Config::release`] and by nothing
    /// else.
    Fixed(String),
}

/// A credential the gate has approved, plus the non-secret summary of it.
///
/// Returned by [`resolve_credential`], which is the ONE place a credential
/// is chosen. `otl api`, the curated commands and `otl auth info` all come
/// through here; before this, `auth info` had its own path and released a
/// global environment key that `otl api` refused on the same configuration.
pub struct Resolved {
    credential: Credential,
    /// Everything `otl auth info` prints. Contains no secret.
    pub summary: Snapshot,
}

impl Resolved {
    /// Turn the credential into a request channel, consuming it.
    ///
    /// The only exit. `base_url` is the caller's, but the credential was
    /// approved for the settings that produced it, so callers pass the same
    /// instance's URL - `open_client` and `auth info` both do.
    pub fn into_client(self, base_url: &str) -> Result<engine::Client, AuthError> {
        match self.credential {
            Credential::Session(provider) => {
                let source: Arc<dyn engine::CredentialSource> = provider as _;
                engine::Client::with_credentials(base_url, source)
            }
            Credential::Fixed(key) => engine::Client::new(base_url, &key),
        }
        .map_err(AuthError::Engine)
    }
}

impl fmt::Debug for Resolved {
    /// Manual impl: this type holds a credential.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("method", &self.summary.method)
            .field("credential", &"***")
            .finish_non_exhaustive()
    }
}

/// Choose the credential for `instance`, applying every rule exactly once.
///
/// # Why this is one function and not three
///
/// "Credentials must not cross instances" has now been fixed three times on
/// three different paths: the read path (R1), the write path (R2), and
/// `otl auth info`'s live identity check (R6). Each fix was correct and each
/// one left the other paths to be found later, because each path did its own
/// resolution. So the paths are gone: there is one, and the rules are stated
/// here once.
///
/// 1. **Instance binding.** [`check_binding`] refuses a stored credential
///    another instance issued, before anything is chosen.
/// 2. **A session outranks a fixed key.** `auth = "..."` names the login
///    FLOW, not a filter on what is already stored, so a session
///    `otl auth login` wrote is used even at the default setting - and the
///    summary reports what that shadows.
/// 3. **A fixed key comes from the config gate and nowhere else.** Which
///    store supplies it - the credential file or the environment - and
///    whether the environment may supply it at all for these settings is
///    [`config::Config::release`]'s decision. That is the rule
///    `auth info` used to bypass: it read `OUTLINE_API_KEY` directly, while
///    the gate scopes a profile's key to `OUTLINE_API_KEY_<PROFILE>` and
///    refuses to fall back, because falling back sends one workspace's key
///    to another workspace's server.
pub fn resolve_credential(
    instance: &Instance,
    store: &CredentialStore,
    file: &credentials::CredentialFile,
) -> Result<Resolved, AuthError> {
    let profile = instance.profile_key();
    let entry = file.profile(profile);
    check_binding(entry, profile, instance.origin())?;
    let stored = entry.and_then(|entry| entry.api_key.as_deref());
    let mut available = available(entry);

    if let Some(provider) =
        CredentialProvider::for_session(store.clone(), profile, file, instance.origin())?
    {
        let detail = provider.detail();
        return Ok(Resolved {
            credential: Credential::Session(Arc::new(provider)),
            summary: Snapshot::from_session(available, detail),
        });
    }

    // No session: a fixed key, if the gate releases one for these settings.
    let candidate = config::StoredCredential::new(stored, store.path());
    let source = config::select_credential_source(instance.settings(), candidate.is_present());
    let released = config::Config::release(instance.settings(), instance.env(), &candidate)?;
    let method = match source {
        config::CredentialSource::CredentialFile => Method::StoredApiKey,
        config::CredentialSource::Environment => {
            // Warned here, where the key is CHOSEN: once per command run
            // rather than once per request, and only for a key the gate
            // actually released - which is also how the warning learns WHICH
            // variable to name, since a selected profile has its own.
            warn_about_env_key(&env_key_variable(instance.settings()));
            available.push(Method::EnvApiKey);
            Method::EnvApiKey
        }
    };
    Ok(Resolved {
        credential: Credential::Fixed(released.api_key),
        summary: Snapshot::fixed(method, available),
    })
}

/// The environment variable a fixed key was released from.
///
/// Config owns the naming rule (`OUTLINE_API_KEY_<PROFILE>`, or the global
/// variable when no profile is in effect); this asks it rather than
/// reconstructing it, so the warning cannot name a variable the gate would
/// not have read. A profile whose name has no usable variable form cannot
/// have released a key from the environment at all, so the fallback is only
/// reachable if that rule changes.
fn env_key_variable(settings: &Settings) -> String {
    settings
        .profile()
        .and_then(config::api_key_var)
        .unwrap_or_else(|| ENV_API_KEY.to_string())
}

/// Build the request-channel client for the active profile.
///
/// The single entry point every command uses: it wires the resolved
/// credential - API key or auto-renewing OAuth session - into
/// `engine::Client`, so renewal happens inside the one request channel
/// rather than in each command.
pub fn client(overrides: &Overrides) -> Result<engine::Client, CliError> {
    open_client(overrides).map(|(client, _)| client)
}

/// Build the request-channel client AND report which instance it points at.
///
/// The origin is returned rather than re-derived by the caller: it is the
/// origin the credential was checked against, and a caller that computed its
/// own could end up printing links for an instance the credential was never
/// released for.
pub fn open_client(overrides: &Overrides) -> Result<(engine::Client, String), CliError> {
    let instance = resolve_instance(overrides).map_err(map_auth_error)?;
    let store = CredentialStore::discover().map_err(store_failed)?;
    let file = store.load().map_err(store_failed)?;
    let resolved = resolve_credential(&instance, &store, &file).map_err(map_auth_error)?;
    let client = resolved
        .into_client(instance.base_url())
        .map_err(map_auth_error)?;
    Ok((client, instance.origin().to_string()))
}

/// A credential-store failure on the client path.
fn store_failed(error: StoreError) -> CliError {
    map_auth_error(AuthError::Store(error))
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
        | OAuthError::ForeignEndpoint { .. }
        | OAuthError::IssuerMismatch { .. } => ExitCode::Auth,
        // Fixable locally: get a client id, free a port, point the CLI at
        // the instance the credentials belong to, use https.
        OAuthError::RegistrationUnavailable { .. }
        | OAuthError::NoCallbackPort { .. }
        | OAuthError::InstanceMismatch { .. }
        | OAuthError::ProfileBoundElsewhere { .. }
        | OAuthError::InsecureTransport { .. }
        | OAuthError::InsecureStoredEndpoint { .. }
        | OAuthError::RetireFailed { .. }
        | OAuthError::ConcurrentLogin { .. } => ExitCode::Usage,
        // A registration exists on the server that nothing can remove. Not
        // a local configuration problem, and not something a retry fixes:
        // it needs an administrator, so it gets the generic failure code
        // rather than pretending to be actionable here.
        OAuthError::OrphanedRegistration { .. } => ExitCode::Failure,
        OAuthError::Transport { .. } => ExitCode::Network,
        OAuthError::Endpoint { status, .. } => match status {
            401 | 403 => ExitCode::Auth,
            429 => ExitCode::RateLimited,
            500..=599 => ExitCode::Server,
            _ => ExitCode::ApiRequest,
        },
        OAuthError::Malformed { .. } | OAuthError::Callback { .. } | OAuthError::Random { .. } => {
            ExitCode::Failure
        }
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
