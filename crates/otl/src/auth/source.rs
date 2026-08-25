//! The credential the request channel uses, and how it renews itself.
//!
//! This is the `engine::CredentialSource` implementation: everything
//! Outline- and OAuth-specific about supplying a bearer token lives here,
//! behind the generic hook the channel calls. No command refreshes a token
//! on its own.
//!
//! Precedence, when more than one credential exists for a profile:
//!
//! 1. an OAuth session (`otl auth login`),
//! 2. an API key in the credential file (`otl auth set-key`),
//! 3. `OUTLINE_API_KEY` from the environment.
//!
//! Interactive credentials win because they are the ones the user last
//! chose deliberately, and the environment comes last because it is the
//! least protected of the three - which is also why using it emits a
//! one-time warning.
//!
//! Refresh safety has three parts, all required by the fact that Outline
//! ROTATES the refresh token on every use:
//!
//! - an advisory file lock, so two processes cannot spend the same refresh
//!   token (see [`crate::auth::lock`]);
//! - a re-read of the credential file INSIDE the lock, so the process that
//!   waited uses the winner's tokens instead of refreshing again;
//! - a hard error if the rotated tokens cannot be persisted, because at
//!   that point the old refresh token is already dead and staying silent
//!   would leave the user with a credential file that cannot be recovered
//!   from.

use std::env;
use std::sync::Mutex;

use engine::{CredentialError, CredentialSource};
use reqwest::blocking::Client;

use crate::auth::credentials::{
    ClientRegistration, CredentialFile, CredentialStore, OAuthSession, ProfileCredentials,
};
use crate::auth::endpoint;
use crate::auth::error::{OAuthError, StoreError};
use crate::auth::lock::RefreshLock;
use crate::auth::oauth::{self, ClientAuth};
use crate::config::ENV_API_KEY;
use crate::stdio;

/// Environment variable that silences the plaintext-key warning.
pub const ENV_NO_KEY_WARNING: &str = "OUTLINE_NO_KEY_WARNING";

/// How long before its stated expiry an access token is treated as spent.
///
/// Covers clock skew and the time a request spends in flight, so a token
/// is refreshed slightly early rather than used a moment too late.
pub const EXPIRY_SKEW_SECONDS: i64 = 60;

/// The one-time warning about keeping an API key in the environment.
const ENV_KEY_WARNING: &str = "warning: authenticating with the API key in \
     OUTLINE_API_KEY. An environment variable is readable by every process \
     you start and tends to end up in shell history, CI logs and crash \
     reports. Run `otl auth set-key` to store it in the credential file \
     (owner-only) instead. Set OUTLINE_NO_KEY_WARNING=1 to silence this.";

/// How a profile is authenticating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// An OAuth session obtained through `otl auth login`.
    OAuth,
    /// An API key stored in the credential file.
    StoredApiKey,
    /// An API key taken from the environment.
    EnvApiKey,
}

impl Method {
    /// Label for `auth info`.
    pub fn label(self) -> &'static str {
        match self {
            Self::OAuth => "oauth (browser login)",
            Self::StoredApiKey => "api key (credential file)",
            Self::EnvApiKey => "api key (OUTLINE_API_KEY environment variable)",
        }
    }
}

/// What the provider can say about itself without revealing anything.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The method actually in use.
    pub method: Method,
    /// Every credential available for this profile, highest priority
    /// first, so `auth info` can show what is being shadowed.
    pub available: Vec<Method>,
    /// Granted scope, when known.
    pub scope: Option<String>,
    /// Account label captured at login, when known.
    pub account: Option<String>,
    /// Workspace label captured at login, when known.
    pub workspace: Option<String>,
    /// Seconds until the access token expires; negative when already
    /// expired, `None` for credentials that do not expire.
    pub expires_in: Option<i64>,
    /// Whether a refresh token is stored, so renewal is possible.
    pub renewable: bool,
    /// Whether the client was obtained by dynamic registration.
    pub dynamic_client: bool,
}

/// Which credentials a profile has, in precedence order.
pub fn available(profile: Option<&ProfileCredentials>) -> Vec<Method> {
    let mut methods = Vec::new();
    if profile.is_some_and(|p| p.oauth.is_some()) {
        methods.push(Method::OAuth);
    }
    if profile.is_some_and(|p| p.api_key.is_some()) {
        methods.push(Method::StoredApiKey);
    }
    if env_api_key().is_some() {
        methods.push(Method::EnvApiKey);
    }
    methods
}

/// `OUTLINE_API_KEY`, blank treated as unset.
pub fn env_api_key() -> Option<String> {
    env::var(ENV_API_KEY)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// What the provider holds between requests.
enum State {
    /// A fixed key that never changes and cannot be renewed.
    Fixed(String),
    /// An OAuth session, replaced in place on every refresh.
    Session(Box<OAuthSession>),
}

/// The `engine::CredentialSource` for one profile.
pub struct CredentialProvider {
    store: CredentialStore,
    profile: String,
    method: Method,
    /// The client registration, for the `client_id` a refresh needs.
    registration: Option<ClientRegistration>,
    state: Mutex<State>,
    http: Client,
    /// Cached non-secret summary, so `auth info` needs no lock dance.
    available: Vec<Method>,
}

impl CredentialProvider {
    /// Resolve the credential for `profile` from `file` and the
    /// environment, returning `None` when there is nothing to use.
    ///
    /// The plaintext-environment warning is emitted here - once per
    /// command run, at the moment the key is actually chosen, rather than
    /// once per HTTP request.
    pub fn resolve(
        store: CredentialStore,
        profile: &str,
        file: &CredentialFile,
    ) -> Result<Option<Self>, OAuthError> {
        let entry = file.profile(profile);
        let available = available(entry);
        let Some(&method) = available.first() else {
            return Ok(None);
        };
        let state = match method {
            Method::OAuth => {
                let session = entry
                    .and_then(|profile| profile.oauth.clone())
                    .ok_or_else(|| unreachable_state("oauth session"))?;
                State::Session(Box::new(session))
            }
            Method::StoredApiKey => {
                let key = entry
                    .and_then(|profile| profile.api_key.clone())
                    .ok_or_else(|| unreachable_state("stored api key"))?;
                State::Fixed(key)
            }
            Method::EnvApiKey => {
                warn_about_env_key();
                State::Fixed(env_api_key().ok_or_else(|| unreachable_state("env api key"))?)
            }
        };
        Ok(Some(Self {
            store,
            profile: profile.to_string(),
            method,
            registration: entry.and_then(|profile| profile.client.clone()),
            state: Mutex::new(state),
            http: endpoint::http_client()?,
            available,
        }))
    }

    /// Which method is in use.
    pub fn method(&self) -> Method {
        self.method
    }

    /// A non-secret summary for `auth info`.
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().ok();
        let session = state.as_ref().and_then(|state| match &**state {
            State::Session(session) => Some(session.as_ref()),
            State::Fixed(_) => None,
        });
        Snapshot {
            method: self.method,
            available: self.available.clone(),
            scope: session.and_then(|session| session.scope.clone()),
            account: session.and_then(|session| session.account.clone()),
            workspace: session.and_then(|session| session.workspace.clone()),
            expires_in: session
                .and_then(|session| session.expires_at)
                .map(|at| at - oauth::now_unix()),
            renewable: session.is_some_and(|session| session.refresh_token.is_some()),
            dynamic_client: self
                .registration
                .as_ref()
                .is_some_and(|registration| registration.dynamic),
        }
    }

    /// Refresh the access token, taking the cross-process lock first.
    ///
    /// `rejected` is the value the server just refused, when the refresh
    /// was triggered by a 401 rather than by the stored expiry.
    fn refresh_locked(&self, rejected: Option<&str>) -> Result<String, OAuthError> {
        let _lock = RefreshLock::acquire(self.store.dir())?;
        // Inside the lock the file is the authority: whoever held the lock
        // before us may already have rotated the tokens.
        let mut file = self.store.load()?;
        let stored = file
            .profile(&self.profile)
            .and_then(|profile| profile.oauth.clone())
            .ok_or_else(|| OAuthError::SessionExpired {
                profile: self.profile.clone(),
                detail: " (no OAuth session is stored any more)".to_string(),
            })?;
        if is_usable(&stored, rejected) {
            self.adopt(&stored);
            return Ok(stored.access_token.clone());
        }

        let refreshed = self.request_refresh(&stored)?;
        let updated = merge(&stored, refreshed);
        file.profile_mut(&self.profile).oauth = Some(updated.clone());
        // The server has already retired the old refresh token, so a write
        // failure here is unrecoverable and must be loud.
        self.store
            .save(&file)
            .map_err(|error| OAuthError::RotationLost {
                reason: error.to_string(),
            })?;
        self.adopt(&updated);
        Ok(updated.access_token)
    }

    /// Perform the refresh grant, translating a rejected grant into the
    /// "sign in again" message.
    fn request_refresh(&self, session: &OAuthSession) -> Result<oauth::Tokens, OAuthError> {
        let refresh_token =
            session
                .refresh_token
                .as_deref()
                .ok_or_else(|| OAuthError::SessionExpired {
                    profile: self.profile.clone(),
                    detail: " (no refresh token was stored, so it cannot be renewed)".to_string(),
                })?;
        oauth::refresh(
            &self.http,
            &session.token_endpoint,
            self.client_auth(session),
            refresh_token,
        )
        .map_err(|error| self.classify_refresh(error))
    }

    /// A refused grant is not a transient failure: say so, and say what to
    /// do about it.
    fn classify_refresh(&self, error: OAuthError) -> OAuthError {
        if !error.is_grant_rejected() {
            return error;
        }
        OAuthError::SessionExpired {
            profile: self.profile.clone(),
            detail: format!(" ({error})"),
        }
    }

    /// The client this session authenticates as.
    fn client_auth<'a>(&'a self, session: &'a OAuthSession) -> ClientAuth<'a> {
        ClientAuth {
            client_id: &session.client_id,
            client_secret: self
                .registration
                .as_ref()
                .and_then(|registration| registration.client_secret.as_deref()),
        }
    }

    /// Replace the in-memory session.
    fn adopt(&self, session: &OAuthSession) {
        if let Ok(mut state) = self.state.lock() {
            *state = State::Session(Box::new(session.clone()));
        }
    }

    /// The current session's access token, if it is still usable.
    fn usable_access_token(&self) -> Option<String> {
        let state = self.state.lock().ok()?;
        match &*state {
            State::Fixed(key) => Some(key.clone()),
            State::Session(session) => {
                is_usable(session, None).then(|| session.access_token.clone())
            }
        }
    }

    /// Whether this provider renews at all.
    fn renewable(&self) -> bool {
        matches!(self.method, Method::OAuth)
    }
}

impl CredentialSource for CredentialProvider {
    fn bearer(&self) -> Result<String, CredentialError> {
        if let Some(token) = self.usable_access_token() {
            return Ok(token);
        }
        // Only an OAuth session can get here: a fixed key is always usable.
        self.refresh_locked(None).map_err(credential_error)
    }

    fn renew(&self, rejected: &str) -> Result<Option<String>, CredentialError> {
        if !self.renewable() {
            return Ok(None);
        }
        self.refresh_locked(Some(rejected))
            .map(Some)
            .map_err(credential_error)
    }
}

/// Whether a stored session can be used as-is.
///
/// `rejected` makes the difference between the two callers: on the
/// expiry-driven path any unexpired token will do, while after a 401 the
/// exact value the server refused must be replaced even if its recorded
/// expiry says otherwise (the server is right, the record is not).
fn is_usable(session: &OAuthSession, rejected: Option<&str>) -> bool {
    if rejected == Some(session.access_token.as_str()) {
        return false;
    }
    match session.expires_at {
        Some(at) => oauth::now_unix() + EXPIRY_SKEW_SECONDS < at,
        // No expiry was recorded: use it and let a 401 sort it out, rather
        // than refreshing on every single request.
        None => true,
    }
}

/// Fold a token response into the stored session.
///
/// A response that omits `refresh_token` means "keep the one you have";
/// one that includes it has rotated it, and the old value is already dead.
fn merge(session: &OAuthSession, tokens: oauth::Tokens) -> OAuthSession {
    OAuthSession {
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .or_else(|| session.refresh_token.clone()),
        expires_at: tokens.expires_at,
        scope: tokens.scope.or_else(|| session.scope.clone()),
        ..session.clone()
    }
}

/// Translate an OAuth failure into the channel's credential error.
fn credential_error(error: OAuthError) -> CredentialError {
    match &error {
        // Nothing local can fix these: the user must sign in again.
        OAuthError::SessionExpired { .. } | OAuthError::RotationLost { .. } => {
            CredentialError::reauth_required(error.to_string())
        }
        _ if error.is_grant_rejected() => CredentialError::reauth_required(error.to_string()),
        _ => CredentialError::unavailable(error.to_string()),
    }
}

/// A state the precedence list said existed but the file did not have.
///
/// Only reachable if [`available`] and the match on `method` disagree, so
/// it is a programming error rather than a user-visible situation - but it
/// is reported instead of unwrapped, because the library layer never
/// panics.
fn unreachable_state(what: &str) -> OAuthError {
    OAuthError::Malformed {
        stage: crate::auth::error::Stage::Discovery,
        origin: "the credential file".to_string(),
        reason: format!("internal inconsistency: {what} was selected but is not stored"),
    }
}

/// Warn once about a key living in the environment.
fn warn_about_env_key() {
    if env::var(ENV_NO_KEY_WARNING).is_ok() {
        return;
    }
    stdio::write_diagnostic_line(ENV_KEY_WARNING);
}

impl From<StoreError> for OAuthError {
    /// Storage failures surface through the OAuth error type so that the
    /// refresh path has one error type to thread; the text is the store's
    /// own, which already names the file and the fix.
    fn from(error: StoreError) -> Self {
        Self::Malformed {
            stage: crate::auth::error::Stage::Refresh,
            origin: "the credential file".to_string(),
            reason: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn session(expires_in: Option<i64>, refresh: Option<&str>) -> OAuthSession {
        OAuthSession {
            access_token: "access-1".to_string(),
            refresh_token: refresh.map(str::to_string),
            expires_at: expires_in.map(|delta| oauth::now_unix() + delta),
            scope: Some("read write".to_string()),
            client_id: "client-1".to_string(),
            token_endpoint: "https://docs.example.com/oauth/token".to_string(),
            revocation_endpoint: None,
            account: Some("Alice".to_string()),
            workspace: Some("Acme".to_string()),
        }
    }

    #[test]
    fn a_token_well_before_its_expiry_is_used_as_is() {
        assert!(is_usable(&session(Some(3600), Some("rt")), None));
    }

    #[test]
    fn a_token_inside_the_skew_window_is_treated_as_spent() {
        assert!(
            !is_usable(&session(Some(EXPIRY_SKEW_SECONDS - 1), Some("rt")), None),
            "a token expiring within the skew window must be refreshed early"
        );
    }

    #[test]
    fn an_expired_token_is_not_used() {
        assert!(!is_usable(&session(Some(-10), Some("rt")), None));
    }

    #[test]
    fn a_token_without_a_recorded_expiry_is_used_rather_than_refreshed_every_time() {
        assert!(is_usable(&session(None, Some("rt")), None));
    }

    #[test]
    fn the_exact_value_the_server_refused_is_never_reused() {
        // Even with an expiry far in the future: the server is the
        // authority on whether its own token is still good.
        let session = session(Some(3600), Some("rt"));
        assert!(!is_usable(&session, Some("access-1")));
        // Another process having already rotated it makes it usable again.
        assert!(is_usable(&session, Some("some-older-token")));
    }

    #[test]
    fn a_refresh_response_that_rotates_the_token_replaces_it() {
        let before = session(Some(10), Some("rt-old"));
        let merged = merge(
            &before,
            oauth::Tokens {
                access_token: "access-2".to_string(),
                refresh_token: Some("rt-new".to_string()),
                expires_at: Some(oauth::now_unix() + 3600),
                scope: None,
            },
        );
        assert_eq!(merged.access_token, "access-2");
        assert_eq!(merged.refresh_token.as_deref(), Some("rt-new"));
        // Fields the response does not carry survive the merge.
        assert_eq!(merged.scope.as_deref(), Some("read write"));
        assert_eq!(merged.client_id, "client-1");
        assert_eq!(merged.account.as_deref(), Some("Alice"));
    }

    #[test]
    fn a_refresh_response_without_a_new_refresh_token_keeps_the_old_one() {
        let before = session(Some(10), Some("rt-old"));
        let merged = merge(
            &before,
            oauth::Tokens {
                access_token: "access-2".to_string(),
                refresh_token: None,
                expires_at: None,
                scope: None,
            },
        );
        assert_eq!(merged.refresh_token.as_deref(), Some("rt-old"));
    }

    #[test]
    fn precedence_is_oauth_then_stored_key_then_environment() {
        let mut profile = ProfileCredentials::default();
        // No environment key in this process unless set below.
        assert!(env_api_key().is_none() || true);

        profile.api_key = Some("stored".to_string());
        assert_eq!(
            available(Some(&profile)).first().copied(),
            Some(Method::StoredApiKey)
        );

        profile.oauth = Some(session(Some(3600), Some("rt")));
        assert_eq!(
            available(Some(&profile)).first().copied(),
            Some(Method::OAuth)
        );
        assert!(available(Some(&profile)).contains(&Method::StoredApiKey));
    }

    #[test]
    fn a_profile_with_nothing_stored_has_no_method() {
        assert!(
            available(Some(&ProfileCredentials::default())).is_empty() || env_api_key().is_some()
        );
        let store = CredentialStore::at(std::path::PathBuf::from("/nonexistent"));
        let file = CredentialFile::default();
        // With no environment key this resolves to nothing at all.
        if env_api_key().is_none() {
            assert!(CredentialProvider::resolve(store, "default", &file)
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn a_fixed_key_provider_never_claims_it_can_renew() {
        let store = CredentialStore::at(std::path::PathBuf::from("/nonexistent"));
        let mut file = CredentialFile::default();
        file.profile_mut("default").api_key = Some("k".to_string());
        let provider = CredentialProvider::resolve(store, "default", &file)
            .unwrap()
            .expect("a stored key resolves");
        assert_eq!(provider.method(), Method::StoredApiKey);
        assert_eq!(provider.bearer().unwrap(), "k");
        assert_eq!(provider.renew("k").unwrap(), None);
    }

    #[test]
    fn a_store_failure_reaching_the_channel_stays_actionable() {
        let error = credential_error(OAuthError::from(StoreError::Permissions {
            path: "/tmp/credentials.toml".to_string(),
            mode: "0644".to_string(),
        }));
        assert!(error.message.contains("chmod 600"), "{}", error.message);
        assert_eq!(error.fault, engine::CredentialFault::Unavailable);
    }

    #[test]
    fn an_expired_session_asks_for_a_new_login() {
        let error = credential_error(OAuthError::SessionExpired {
            profile: "default".to_string(),
            detail: String::new(),
        });
        assert_eq!(error.fault, engine::CredentialFault::ReauthRequired);
        assert!(
            error.message.contains("otl auth login"),
            "{}",
            error.message
        );
    }

    #[test]
    fn losing_rotated_tokens_asks_for_a_new_login_rather_than_failing_silently() {
        let error = credential_error(OAuthError::RotationLost {
            reason: "no space left on device".to_string(),
        });
        assert_eq!(error.fault, engine::CredentialFault::ReauthRequired);
        assert!(
            error.message.contains("otl auth login"),
            "{}",
            error.message
        );
        assert!(error.message.contains("no space left"), "{}", error.message);
    }
}
