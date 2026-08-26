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
use std::fmt;
use std::sync::Mutex;

use engine::{CredentialError, CredentialSource};
use reqwest::blocking::Client;

pub use crate::auth::selection::{
    available, check_binding, env_api_key, fresh_enough, superseded_by, Method, Snapshot,
    EXPIRY_SKEW_SECONDS,
};

use crate::auth::credentials::{ClientRegistration, CredentialFile, CredentialStore, OAuthSession};
use crate::auth::endpoint;
use crate::auth::error::{OAuthError, StoreError};
use crate::auth::lock::CredentialLock;
use crate::auth::oauth::{self, ClientAuth};
use crate::auth::transport;
use crate::stdio;

/// Environment variable that silences the plaintext-key warning.
pub const ENV_NO_KEY_WARNING: &str = "OUTLINE_NO_KEY_WARNING";

/// The one-time warning about keeping an API key in the environment.
const ENV_KEY_WARNING: &str = "warning: authenticating with the API key in \
     OUTLINE_API_KEY. An environment variable is readable by every process \
     you start and tends to end up in shell history, CI logs and crash \
     reports. Run `otl auth set-key` to store it in the credential file \
     (owner-only) instead. Set OUTLINE_NO_KEY_WARNING=1 to silence this.";

/// What the provider holds between requests.
enum State {
    /// A fixed key that never changes and cannot be renewed.
    Fixed(String),
    /// An OAuth session, replaced in place on every refresh.
    Session(Box<OAuthSession>),
}

/// The `engine::CredentialSource` for one profile.
///
/// `Debug` is hand-written (see below): the state it holds is a credential.
pub struct CredentialProvider {
    store: CredentialStore,
    profile: String,
    /// Instance origin this provider was resolved for. Stored endpoints are
    /// re-checked against it before any credential is sent to them.
    origin: String,
    method: Method,
    /// The client registration, for the `client_id` a refresh needs.
    registration: Option<ClientRegistration>,
    state: Mutex<State>,
    http: Client,
    /// Cached non-secret summary, so `auth info` needs no lock dance.
    available: Vec<Method>,
}

impl fmt::Debug for CredentialProvider {
    /// Manual impl: the state holds an access token or an API key, so only
    /// the non-secret shape of this provider is printable.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialProvider")
            .field("profile", &self.profile)
            .field("method", &self.method)
            .field("credential", &"***")
            .finish_non_exhaustive()
    }
}

impl CredentialProvider {
    /// Resolve the credential for `profile` from `file` and the
    /// environment, returning `None` when there is nothing to use.
    ///
    /// `origin` is the instance this command is pointed at, and stored
    /// credentials are only offered if they were issued BY that instance.
    /// Without the check, re-pointing `OUTLINE_URL` at another host would
    /// hand it this profile's bearer token - and, once the token expired,
    /// would mint a fresh one at the original instance and send that too.
    /// The check refuses rather than warns: a warning is printed after the
    /// credential has already gone to the request channel.
    ///
    /// An environment API key needs no stored binding of its own - it is
    /// supplied per invocation alongside `OUTLINE_URL` - so a profile that
    /// holds only a leftover client registration does not block it. It is
    /// NOT exempt from the check as a whole: if the profile also holds a
    /// stored key or session for another instance, that conflict is
    /// reported rather than silently resolved by falling through to the
    /// environment.
    ///
    /// The plaintext-environment warning is emitted here - once per
    /// command run, at the moment the key is actually chosen, rather than
    /// once per HTTP request.
    pub fn resolve(
        store: CredentialStore,
        profile: &str,
        file: &CredentialFile,
        origin: &str,
    ) -> Result<Option<Self>, OAuthError> {
        let entry = file.profile(profile);
        check_binding(entry, profile, origin)?;
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
            origin: origin.to_string(),
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
    /// `superseded` is the access token this process considers spent: the
    /// value the server just refused with a 401, or the one that reached
    /// its expiry. Whatever is on disk under a DIFFERENT value was put
    /// there by another process that already did this work.
    fn refresh_locked(&self, superseded: &str) -> Result<String, OAuthError> {
        let _lock = CredentialLock::acquire(self.store.dir())?;
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
        if superseded_by(&stored, superseded) {
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
        // The endpoint comes off disk, so it is re-validated here rather
        // than trusted because a past login stored it: a credential file
        // can be hand-edited, can predate this rule, or can be copied
        // between machines - and a refresh token is about to be posted to
        // whatever this says.
        transport::require_stored_secure(
            &session.token_endpoint,
            &self.profile,
            "OAuth token endpoint",
        )?;
        transport::require_same_origin(
            &session.token_endpoint,
            &self.origin,
            &self.profile,
            "OAuth token endpoint",
        )?;
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

    /// The credential to send, or the one that needs replacing.
    fn current_token(&self) -> Current {
        let Ok(state) = self.state.lock() else {
            // A poisoned mutex means another thread panicked mid-refresh;
            // treat the in-memory view as unusable rather than guess.
            return Current::Spent(String::new());
        };
        match &*state {
            State::Fixed(key) => Current::Usable(key.clone()),
            State::Session(session) => {
                if fresh_enough(session) {
                    Current::Usable(session.access_token.clone())
                } else {
                    Current::Spent(session.access_token.clone())
                }
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
        match self.current_token() {
            Current::Usable(token) => Ok(token),
            // Only an OAuth session can get here: a fixed key is always
            // usable, so there is nothing to refresh.
            Current::Spent(spent) => self.refresh_locked(&spent).map_err(credential_error),
        }
    }

    fn renew(&self, rejected: &str) -> Result<Option<String>, CredentialError> {
        if !self.renewable() {
            return Ok(None);
        }
        self.refresh_locked(rejected)
            .map(Some)
            .map_err(credential_error)
    }
}

/// What the in-memory state can offer right now.
enum Current {
    /// A credential good to send.
    Usable(String),
    /// A credential that needs replacing; carries the spent value so the
    /// refresh can tell "mine is stale" from "someone else replaced it".
    Spent(String),
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
    use crate::auth::credentials::ProfileCredentials;

    const ORIGIN: &str = "https://docs.example.com";

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

    /// A profile bound to [`ORIGIN`] holding the given credentials.
    fn bound(api_key: Option<&str>, oauth: Option<OAuthSession>) -> ProfileCredentials {
        ProfileCredentials {
            origin: Some(ORIGIN.to_string()),
            api_key: api_key.map(str::to_string),
            oauth,
            client: None,
        }
    }

    // --- expiry and the safety margin ----------------------------------

    #[test]
    fn a_token_well_before_its_expiry_is_used_as_is() {
        assert!(fresh_enough(&session(Some(3600), Some("rt"))));
    }

    #[test]
    fn a_token_inside_the_skew_window_is_refreshed_early() {
        assert!(
            !fresh_enough(&session(Some(EXPIRY_SKEW_SECONDS - 1), Some("rt"))),
            "a token expiring within the skew window must be refreshed early"
        );
    }

    #[test]
    fn an_expired_token_is_not_used() {
        assert!(!fresh_enough(&session(Some(-10), Some("rt"))));
    }

    #[test]
    fn a_token_without_a_recorded_expiry_is_used_rather_than_refreshed_every_time() {
        assert!(fresh_enough(&session(None, Some("rt"))));
    }

    // --- single flight: what a waiter accepts --------------------------

    #[test]
    fn a_waiter_accepts_the_token_another_process_just_obtained() {
        let mut stored = session(Some(3600), Some("rt"));
        stored.access_token = "access-2".to_string();
        assert!(
            superseded_by(&stored, "access-1"),
            "a waiter must reuse the winner's token instead of refreshing again"
        );
    }

    #[test]
    fn a_waiter_accepts_a_short_lived_token_rather_than_refreshing_again() {
        // The reported bug: with the safety margin applied here, every
        // queued process would refresh again whenever the server issues a
        // short-lived token, spending one single-use refresh token each and
        // breaking the "refresh once" guarantee outright.
        let mut stored = session(Some(EXPIRY_SKEW_SECONDS - 1), Some("rt"));
        stored.access_token = "access-2".to_string();
        assert!(
            superseded_by(&stored, "access-1"),
            "a freshly issued short-lived token must still be reused"
        );
        // It is nonetheless below the margin for the proactive path, which
        // is what makes the two predicates different.
        assert!(!fresh_enough(&stored));
    }

    #[test]
    fn a_waiter_does_not_accept_the_token_that_was_just_refused() {
        let stored = session(Some(3600), Some("rt"));
        assert!(
            !superseded_by(&stored, "access-1"),
            "the value the server refused must never be reused"
        );
    }

    #[test]
    fn a_waiter_does_not_accept_an_already_expired_token() {
        let mut stored = session(Some(-10), Some("rt"));
        stored.access_token = "access-2".to_string();
        assert!(!superseded_by(&stored, "access-1"));
    }

    // --- rotation merge ------------------------------------------------

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

    // --- instance binding ----------------------------------------------

    #[test]
    fn credentials_bound_to_another_instance_are_refused() {
        // The attack: log in to A, then point OUTLINE_URL at B. Without
        // this check B receives A's bearer token, and once it expires the
        // CLI mints a fresh one at A and sends that to B as well.
        let entry = bound(Some("key"), Some(session(Some(3600), Some("rt"))));
        let error = check_binding(Some(&entry), "default", "https://evil.example.net")
            .expect_err("credentials for another instance must be refused");
        let text = error.to_string();
        assert!(text.contains("https://docs.example.com"), "{text}");
        assert!(text.contains("https://evil.example.net"), "{text}");
        assert!(text.contains("OUTLINE_PROFILE"), "{text}");
        assert!(text.contains("otl auth login"), "{text}");
    }

    #[test]
    fn credentials_bound_to_this_instance_are_accepted() {
        let entry = bound(Some("key"), None);
        assert!(check_binding(Some(&entry), "default", ORIGIN).is_ok());
    }

    #[test]
    fn credentials_with_no_recorded_binding_fail_closed() {
        // A hand-written or pre-binding file must not be treated as valid
        // everywhere: the safe default for "unknown" is refusal.
        let entry = ProfileCredentials {
            origin: None,
            api_key: Some("key".to_string()),
            oauth: None,
            client: None,
        };
        let error = check_binding(Some(&entry), "default", ORIGIN)
            .expect_err("an unbound credential must not be usable");
        assert!(error.to_string().contains("unrecorded"), "{error}");
    }

    #[test]
    fn an_empty_or_absent_profile_has_nothing_to_misdirect() {
        assert!(check_binding(None, "default", ORIGIN).is_ok());
        assert!(check_binding(Some(&ProfileCredentials::default()), "default", ORIGIN).is_ok());
    }

    #[test]
    fn resolution_refuses_a_profile_bound_elsewhere() {
        let store = CredentialStore::at(std::path::PathBuf::from("/nonexistent"));
        let mut file = CredentialFile::default();
        *file.profile_mut("default") = bound(Some("key"), None);
        let error = CredentialProvider::resolve(store, "default", &file, "https://other.example")
            .expect_err("resolution must refuse a foreign binding");
        assert!(error.to_string().contains("refused"), "{error}");
    }

    // --- precedence ----------------------------------------------------

    #[test]
    fn precedence_is_oauth_then_stored_key_then_environment() {
        let mut profile = bound(Some("stored"), None);
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
    fn a_fixed_key_provider_never_claims_it_can_renew() {
        let store = CredentialStore::at(std::path::PathBuf::from("/nonexistent"));
        let mut file = CredentialFile::default();
        *file.profile_mut("default") = bound(Some("k"), None);
        let provider = CredentialProvider::resolve(store, "default", &file, ORIGIN)
            .unwrap()
            .expect("a stored key resolves");
        assert_eq!(provider.method(), Method::StoredApiKey);
        assert_eq!(provider.bearer().unwrap(), "k");
        assert_eq!(provider.renew("k").unwrap(), None);
    }

    #[test]
    fn a_profile_with_nothing_stored_has_no_method() {
        let store = CredentialStore::at(std::path::PathBuf::from("/nonexistent"));
        let file = CredentialFile::default();
        if env_api_key().is_none() {
            assert!(CredentialProvider::resolve(store, "default", &file, ORIGIN)
                .unwrap()
                .is_none());
        }
    }

    // --- error classification ------------------------------------------

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
