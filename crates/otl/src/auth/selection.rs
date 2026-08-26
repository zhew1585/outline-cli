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

/// Environment variable that silences the plaintext-key warning.
pub const ENV_NO_KEY_WARNING: &str = "OUTLINE_NO_KEY_WARNING";

/// The one-time warning about keeping an API key in the environment.
///
/// `{variable}` is the variable the key actually came from. Naming it matters
/// now that the warning also covers a profile-scoped key: it used to fire
/// only for the global `OUTLINE_API_KEY`, because it was decided by a direct
/// read of that one variable, and stayed silent for
/// `OUTLINE_API_KEY_<PROFILE>` - which has exactly the same exposure and is
/// the variable a CI setup is most likely to use.
const ENV_KEY_WARNING: &str = "warning: authenticating with the API key in \
     {variable}. An environment variable is readable by every process \
     you start and tends to end up in shell history, CI logs and crash \
     reports. Run `otl auth set-key` to store it in the credential file \
     (owner-only) instead. Set OUTLINE_NO_KEY_WARNING=1 to silence this.";

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

/// What `otl auth info` prints about the chosen credential. No secret.
///
/// Built by [`crate::auth::resolve_credential`], the one place a credential
/// is chosen, so the report cannot describe a different decision from the one
/// that was made. It used to be built by the provider, which is why a fixed
/// key had no report at all when the provider stopped serving one.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The method actually in use.
    pub method: Method,
    /// Every credential available for this profile, highest priority
    /// first, so `auth info` can show what is being shadowed.
    pub available: Vec<Method>,
    /// Granted scope, when known. Sessions only.
    pub scope: Option<String>,
    /// Account label captured at login, when known. Sessions only.
    pub account: Option<String>,
    /// Workspace label captured at login, when known. Sessions only.
    pub workspace: Option<String>,
    /// Seconds until the access token expires; negative when already
    /// expired, `None` for credentials that do not expire.
    pub expires_in: Option<i64>,
    /// Whether the credential renews itself. Sessions only.
    pub renewable: bool,
}

impl Snapshot {
    /// The report for a fixed key: no expiry, no scope, no renewal.
    ///
    /// `available` lists what else the profile could have used, `method` at
    /// its head, so the "also available" line names what is shadowed.
    pub fn fixed(method: Method, available: Vec<Method>) -> Self {
        let available = Self::ordered(method, available);
        Self {
            method,
            available,
            scope: None,
            account: None,
            workspace: None,
            expires_in: None,
            renewable: false,
        }
    }

    /// The report for a session, from the provider's non-secret detail.
    pub fn from_session(
        available: Vec<Method>,
        detail: crate::auth::source::SessionDetail,
    ) -> Self {
        let available = Self::ordered(Method::OAuth, available);
        Self {
            method: Method::OAuth,
            available,
            scope: detail.scope,
            account: detail.account,
            workspace: detail.workspace,
            expires_in: detail.expires_in,
            renewable: detail.renewable,
        }
    }

    /// `method` first, everything else after it, no duplicates.
    ///
    /// `auth info` reads "the rest of this list is shadowed" off the tail, so
    /// the head has to be the method actually in use - which it would not be
    /// if the caller assembled the list in file order and the chosen method
    /// came from the environment.
    fn ordered(method: Method, available: Vec<Method>) -> Vec<Method> {
        let mut ordered = vec![method];
        ordered.extend(available.into_iter().filter(|other| *other != method));
        ordered
    }
}

/// Which credentials the CREDENTIAL FILE holds for a profile, in precedence
/// order.
///
/// The file only. An environment key used to be discovered here, by reading
/// `OUTLINE_API_KEY` directly, and that was a hole: config scopes an
/// environment key to the selected profile (`OUTLINE_API_KEY_<PROFILE>`) and
/// deliberately refuses to fall back to the global variable, because falling
/// back is what sends one workspace's key to another workspace's server.
/// This function had no notion of a profile-scoped variable, so every caller
/// that went through it got the global one - which is how `otl auth info`
/// came to send a key that `otl api`, on the same configuration, refused.
///
/// So the environment is not this module's business at all. It is a config
/// STORE, reachable only through the release gate, and
/// [`crate::auth::resolve_credential`] is the one place the two are combined.
pub fn available(profile: Option<&ProfileCredentials>) -> Vec<Method> {
    let mut methods = Vec::new();
    if profile.is_some_and(|p| p.oauth.is_some()) {
        methods.push(Method::OAuth);
    }
    if profile.is_some_and(|p| p.api_key.is_some()) {
        methods.push(Method::StoredApiKey);
    }
    methods
}

/// Warn once about a key living in the environment.
///
/// Emitted when the key is CHOSEN, once per command run, rather than once
/// per request - and by the one place that chooses it, so it cannot be
/// printed for a key that was not used or skipped for one that was.
pub fn warn_about_env_key(variable: &str) {
    if env::var(ENV_NO_KEY_WARNING).is_ok() {
        return;
    }
    crate::stdio::write_diagnostic_line(&ENV_KEY_WARNING.replace("{variable}", variable));
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
