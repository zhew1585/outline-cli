//! The OAuth SESSION as the request channel's credential, and how it renews
//! itself.
//!
//! This is the `engine::CredentialSource` implementation: everything
//! Outline- and OAuth-specific about supplying a bearer token lives here,
//! behind the generic hook the channel calls. No command refreshes a token
//! on its own.
//!
//! # A session, and nothing else
//!
//! This provider used to serve any of the three credential kinds, choosing
//! between them by precedence - and its environment branch read
//! `OUTLINE_API_KEY` directly, which meant every caller that went through it
//! bypassed the config gate's rule that a profile-scoped credential must
//! come from `OUTLINE_API_KEY_<PROFILE>`. `otl auth info` did exactly that,
//! and sent a global key that `otl api` refused on the same configuration.
//!
//! So the split is now by CAPABILITY rather than by preference: a session is
//! the only credential that renews, and renewing is the only reason a
//! credential needs to live behind a `CredentialSource` at all. A fixed key
//! needs no state and no lock, so it goes to the engine as a plain string -
//! obtained from `config::Config::release` and nowhere else. Nothing in this
//! module can produce a key, which is what makes that "nowhere else" hold.
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

use std::fmt;
use std::sync::Mutex;

use engine::{CredentialError, CredentialSource};
use reqwest::blocking::Client;

pub use crate::auth::selection::{
    available, check_binding, fresh_enough, superseded_by, warn_about_env_key, Method, Snapshot,
    ENV_NO_KEY_WARNING, EXPIRY_SKEW_SECONDS,
};

use crate::auth::credentials::{ClientRegistration, CredentialFile, CredentialStore, OAuthSession};
use crate::auth::endpoint;
use crate::auth::error::{OAuthError, StoreError};
use crate::auth::lock::CredentialLock;
use crate::auth::oauth::{self, ClientAuth};
use crate::auth::transport;

/// The session detail `otl auth info` reports. No secret.
#[derive(Debug, Clone)]
pub struct SessionDetail {
    /// Granted scope, when the server stated one.
    pub scope: Option<String>,
    /// Account label captured at login.
    pub account: Option<String>,
    /// Workspace label captured at login.
    pub workspace: Option<String>,
    /// Seconds until the access token expires; negative when already past.
    pub expires_in: Option<i64>,
    /// Whether a refresh token is stored, so renewal is possible.
    pub renewable: bool,
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
    /// The client registration, for the `client_id` a refresh needs.
    registration: Option<ClientRegistration>,
    /// The session, replaced in place on every refresh.
    state: Mutex<OAuthSession>,
    http: Client,
}

impl fmt::Debug for CredentialProvider {
    /// Manual impl: the state holds an access token or an API key, so only
    /// the non-secret shape of this provider is printable.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialProvider")
            .field("profile", &self.profile)
            .field("credential", &"***")
            .finish_non_exhaustive()
    }
}

impl CredentialProvider {
    /// Build the provider from the OAuth SESSION stored for `profile`, or
    /// `None` when there is no session there.
    ///
    /// `origin` is the instance this command is pointed at, and a stored
    /// credential is only offered if it was issued BY that instance. Without
    /// the check, re-pointing the URL at another host would hand it this
    /// profile's bearer token - and, once the token expired, would mint a
    /// fresh one at the original instance and send that too. The check
    /// refuses rather than warns: a warning is printed after the credential
    /// has already gone to the request channel.
    ///
    /// `None` is not "nothing is configured": a stored API key or an
    /// environment key may well be usable. It means "there is nothing HERE",
    /// and the caller
    /// ([`crate::auth::resolve_credential`]) goes to the config gate for the
    /// fixed-key cases. This function cannot produce a fixed key at all,
    /// which is what stops it from becoming a second way to obtain one.
    pub fn for_session(
        store: CredentialStore,
        profile: &str,
        file: &CredentialFile,
        origin: &str,
    ) -> Result<Option<Self>, OAuthError> {
        let entry = file.profile(profile);
        check_binding(entry, profile, origin)?;
        let Some(session) = entry.and_then(|profile| profile.oauth.clone()) else {
            return Ok(None);
        };
        Ok(Some(Self {
            store,
            profile: profile.to_string(),
            origin: origin.to_string(),
            registration: entry.and_then(|profile| profile.client.clone()),
            state: Mutex::new(session),
            http: endpoint::http_client()?,
        }))
    }

    /// The non-secret session detail `auth info` reports.
    pub fn detail(&self) -> SessionDetail {
        let state = self.state.lock().ok();
        let session = state.as_deref();
        SessionDetail {
            scope: session.and_then(|session| session.scope.clone()),
            account: session.and_then(|session| session.account.clone()),
            workspace: session.and_then(|session| session.workspace.clone()),
            expires_in: session
                .and_then(|session| session.expires_at)
                .map(|at| at - oauth::now_unix()),
            renewable: session.is_some_and(|session| session.refresh_token.is_some()),
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
            *state = session.clone();
        }
    }

    /// The credential to send, or the one that needs replacing.
    fn current_token(&self) -> Current {
        let Ok(state) = self.state.lock() else {
            // A poisoned mutex means another thread panicked mid-refresh;
            // treat the in-memory view as unusable rather than guess.
            return Current::Spent(String::new());
        };
        if fresh_enough(&state) {
            Current::Usable(state.access_token.clone())
        } else {
            Current::Spent(state.access_token.clone())
        }
    }
}

impl CredentialSource for CredentialProvider {
    fn bearer(&self) -> Result<String, CredentialError> {
        match self.current_token() {
            Current::Usable(token) => Ok(token),
            Current::Spent(spent) => self.refresh_locked(&spent).map_err(credential_error),
        }
    }

    fn renew(&self, rejected: &str) -> Result<Option<String>, CredentialError> {
        // Always renewable: this provider only ever holds a session. A fixed
        // key never reaches the engine as a `CredentialSource`, so there is
        // no "cannot renew" case left to report.
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
        let error =
            CredentialProvider::for_session(store, "default", &file, "https://other.example")
                .expect_err("resolution must refuse a foreign binding");
        assert!(error.to_string().contains("refused"), "{error}");
    }

    // --- precedence ----------------------------------------------------

    #[test]
    fn a_session_outranks_a_stored_key_in_the_file() {
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
    fn the_file_is_the_only_thing_available_reports() {
        // The environment used to be discovered here, by reading
        // OUTLINE_API_KEY directly - which is how `auth info` came to send a
        // global key that the config gate refuses for a selected profile.
        // This function now answers about the FILE and cannot see the
        // environment at all; `credential_paths.rs` pins that structurally
        // (no module under `auth` may name the variable) and
        // `auth_curated_path.rs` pins the behaviour end to end. Setting the
        // variable HERE would prove less than either, and would mutate
        // process state that the other tests in this binary share.
        assert!(available(None).is_empty());
        assert_eq!(
            available(Some(&bound(Some("stored"), None))),
            vec![Method::StoredApiKey]
        );
    }

    #[test]
    fn a_fixed_key_never_becomes_a_renewing_provider() {
        // A stored API key is not a session, so this constructor must not
        // produce a provider for it: a fixed key reaches the engine as a
        // plain string, released by the config gate, and nothing here can
        // make one.
        let store = CredentialStore::at(std::path::PathBuf::from("/nonexistent"));
        let mut file = CredentialFile::default();
        *file.profile_mut("default") = bound(Some("k"), None);
        assert!(
            CredentialProvider::for_session(store, "default", &file, ORIGIN)
                .unwrap()
                .is_none(),
            "a stored API key was served as a renewable session"
        );
    }

    #[test]
    fn a_profile_with_no_session_yields_no_provider() {
        let store = CredentialStore::at(std::path::PathBuf::from("/nonexistent"));
        let file = CredentialFile::default();
        assert!(
            CredentialProvider::for_session(store, "default", &file, ORIGIN)
                .unwrap()
                .is_none()
        );
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
