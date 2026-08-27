//! Credential supply for the single request channel.
//!
//! The engine knows nothing about where a credential comes from, how it is
//! stored, or which grant produced it. It asks a [`CredentialSource`] for
//! the bearer value to put on the next request and - when the server
//! answers HTTP 401 - asks the same source once for a renewed value before
//! replaying that request. How a credential is obtained, stored, protected
//! or renewed is entirely the caller's business and stays outside this
//! crate, along with every protocol name involved.
//!
//! Renewal lives here, in the channel, and nowhere else: a command that
//! refreshed credentials on its own would be a second, unaudited request
//! path.

use std::fmt;

use thiserror::Error;

use crate::sanitize::REDACTED;

/// What kind of problem a [`CredentialSource`] ran into.
///
/// Deliberately coarse: it is the only classification the engine needs in
/// order to let a caller pick an exit code, and it names situations rather
/// than mechanisms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialFault {
    /// No usable credential could be produced from local state: it is
    /// absent, unreadable, stored unsafely, or could not be written back.
    /// The user fixes this locally, without re-authenticating.
    Unavailable,
    /// A stored credential was rejected and cannot be renewed without the
    /// user authenticating interactively again.
    ReauthRequired,
}

/// A [`CredentialSource`] failure.
///
/// Carries only authored, credential-free text: the source composes the
/// message and the caller renders it verbatim, so nothing here may ever be
/// interpolated from a token, a URL, or unsanitized server text.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct CredentialError {
    /// How the caller should classify this failure.
    pub fault: CredentialFault,
    /// Human-readable, credential-free explanation, already actionable.
    pub message: String,
}

impl CredentialError {
    /// A credential that could not be produced from local state.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            fault: CredentialFault::Unavailable,
            message: message.into(),
        }
    }

    /// A credential that only interactive re-authentication can restore.
    pub fn reauth_required(message: impl Into<String>) -> Self {
        Self {
            fault: CredentialFault::ReauthRequired,
            message: message.into(),
        }
    }
}

/// Supplies - and, where possible, renews - the bearer credential used by
/// the request channel.
///
/// Implementations are shared across threads and may be consulted once per
/// HTTP request, so [`CredentialSource::bearer`] must stay cheap in the
/// common case (no network call when the current credential is still
/// usable).
pub trait CredentialSource: Send + Sync {
    /// The credential to send with the next request.
    fn bearer(&self) -> Result<String, CredentialError>;

    /// Renew after the server rejected `rejected` with HTTP 401.
    ///
    /// - `Ok(Some(fresh))`: the channel replays the request once, with
    ///   `fresh`.
    /// - `Ok(None)`: this source cannot renew; the 401 is surfaced to the
    ///   caller unchanged. This is the default.
    /// - `Err(_)`: renewal was attempted and failed.
    ///
    /// `rejected` is the exact value that failed, so an implementation
    /// whose backing store is shared between processes can tell "the one I
    /// sent is stale" from "another process already replaced it".
    ///
    /// The channel calls this at most once per request, so an
    /// implementation never has to guard against a renewal loop.
    fn renew(&self, rejected: &str) -> Result<Option<String>, CredentialError> {
        let _ = rejected;
        Ok(None)
    }
}

/// A fixed credential that never changes and cannot be renewed.
///
/// [`Client::new`] wraps its token argument in one of these.
///
/// [`Client::new`]: crate::client::Client::new
pub struct StaticCredential(String);

impl StaticCredential {
    /// Wrap a fixed bearer value.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }
}

impl fmt::Debug for StaticCredential {
    /// Manual impl: the credential must never appear in Debug output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StaticCredential").field(&REDACTED).finish()
    }
}

impl CredentialSource for StaticCredential {
    fn bearer(&self) -> Result<String, CredentialError> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_credential_debug_hides_the_token() {
        let rendered = format!("{:?}", StaticCredential::new("super-secret-token"));
        assert!(
            !rendered.contains("super-secret-token"),
            "credential leaked: {rendered}"
        );
        assert!(rendered.contains(REDACTED));
    }

    #[test]
    fn static_credential_cannot_renew() {
        let source = StaticCredential::new("k");
        assert_eq!(source.renew("k"), Ok(None));
    }
}
