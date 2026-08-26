//! Which credential a profile offers, and whether it may be used against
//! the instance in hand.
//!
//! Split out of [`crate::auth::source`], which owns the provider that
//! serves and renews the chosen credential. The decisions here are pure
//! functions over stored state, which is what makes them cheap to test
//! exhaustively - and they are the ones that decide whether a bearer token
//! goes on the wire, so exhaustive is the standard they are held to.

use std::env;

use crate::auth::credentials::{OAuthSession, ProfileCredentials};
use crate::auth::error::OAuthError;
use crate::auth::oauth;
use crate::config::ENV_API_KEY;

/// How long before its stated expiry an access token is treated as spent.
///
/// Covers clock skew and the time a request spends in flight, so a token is
/// refreshed slightly early rather than used a moment too late.
pub const EXPIRY_SKEW_SECONDS: i64 = 60;

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

/// Whether a session may be sent without refreshing it first.
///
/// Applies the safety margin: a token inside [`EXPIRY_SKEW_SECONDS`] of its
/// expiry is refreshed EARLY, so clock skew and time in flight cannot turn
/// it into a failed request.
pub fn fresh_enough(session: &OAuthSession) -> bool {
    match session.expires_at {
        Some(at) => oauth::now_unix() + EXPIRY_SKEW_SECONDS < at,
        // No expiry was recorded: use it and let a 401 sort it out, rather
        // than refreshing on every single request.
        None => true,
    }
}

/// Whether the session on disk already replaces `superseded`.
///
/// This is the single-flight acceptance test, and it deliberately does NOT
/// apply the safety margin. The margin exists to refresh early; applying it
/// here would make a queue of waiting processes each refresh again whenever
/// the server issues a short-lived token, spending one single-use refresh
/// token per waiter and turning a successful batch into an authentication
/// failure. What matters to a waiter is only: is this a different token
/// than the one we know is spent, and has it not actually expired yet.
pub fn superseded_by(session: &OAuthSession, superseded: &str) -> bool {
    if session.access_token == superseded {
        return false;
    }
    match session.expires_at {
        Some(at) => oauth::now_unix() < at,
        None => true,
    }
}

/// Refuse stored credentials that belong to another instance.
///
/// Two independent checks, because one of them can be defeated by a write:
///
/// 1. The profile-level binding. Covers the API key, which carries no
///    origin information of its own.
/// 2. The OAuth session's OWN origin, taken from the token endpoint
///    captured at login. A write that rewrote `profile.origin` while
///    leaving another instance's session in place would pass check 1 - and
///    OAuth outranks the API key, so that session is exactly what would be
///    sent. This check makes such a state unusable rather than dangerous.
///
/// A client registration is deliberately NOT considered. It cannot
/// authenticate anything, so a leftover one must not stop an environment
/// API key that was supplied for a different instance.
pub fn check_binding(
    entry: Option<&ProfileCredentials>,
    profile: &str,
    origin: &str,
) -> Result<(), OAuthError> {
    let Some(entry) = entry else {
        return Ok(());
    };
    // Nothing that could authenticate means nothing to misdirect.
    if !entry.has_authenticator() {
        return Ok(());
    }
    let mismatch = |stored: String| OAuthError::InstanceMismatch {
        profile: profile.to_string(),
        stored,
        current: origin.to_string(),
    };
    if !entry.is_bound_to(origin) {
        return Err(mismatch(
            entry
                .origin
                .clone()
                .unwrap_or_else(|| "an unrecorded instance".to_string()),
        ));
    }
    match entry.session_origin() {
        Some(session_origin) if session_origin != origin => Err(mismatch(session_origin)),
        _ => Ok(()),
    }
}
