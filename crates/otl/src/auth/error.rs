//! Typed errors for credential storage and the OAuth flows.
//!
//! Credential hygiene works the same way as in `engine::error`: every field
//! is credential-free BY CONSTRUCTION, so Debug, Display and the whole
//! `source()` chain are safe to print.
//!
//! - Server-provided text (an OAuth `error_description`, a TOML parse
//!   message) is never carried verbatim. Endpoint text goes through
//!   [`engine::sanitize::clean_server_text`] with every secret we sent as
//!   the redaction key; TOML parse failures are reduced to a line/column,
//!   because the `toml` crate's own Display quotes the offending source
//!   line - which in a credential file is a token.
//! - Any URL-derived text is reduced to an origin (`scheme://host[:port]`).
//! - Filesystem paths ARE carried: the fix instructions required by the
//!   credential-hygiene story (`chmod 600 <path>`) are useless without
//!   them, and they are the user's own configuration paths.
//!
//! Preserve the invariant when adding variants: sanitize at construction,
//! never at display time.

use std::fmt;

use thiserror::Error;

use crate::auth::paths::ENV_CONFIG_DIR;

/// The credential-file format version this build writes and understands.
pub const CREDENTIAL_FORMAT_VERSION: u32 = 1;

/// Failures of the credential store (the credential file and its lock).
#[derive(Debug, Error)]
pub enum StoreError {
    /// No per-user configuration directory could be determined.
    #[error(
        "cannot determine a configuration directory for credentials; \
         set {ENV_CONFIG_DIR} to a directory only you can read"
    )]
    NoConfigDir,

    /// The credential file is readable or writable by someone else.
    ///
    /// Never auto-repaired: silently tightening a file whose contents may
    /// already have been read would hide the exposure instead of
    /// reporting it.
    #[error(
        "credential file {path} is accessible to users other than you \
         (permissions {mode}); refusing to use it.\n\
         Fix it with:\n\
         \x20 chmod 600 {path}"
    )]
    Permissions {
        /// Absolute path of the offending file.
        path: String,
        /// Octal permission bits, rendered (e.g. `0644`).
        mode: String,
    },

    /// The credential file could not be read.
    #[error("cannot read credential file {path}: {reason}")]
    Read {
        /// Absolute path of the file.
        path: String,
        /// I/O failure description (no file content).
        reason: String,
    },

    /// The credential file is not valid TOML.
    ///
    /// The reason is a position only. The `toml` crate's own error Display
    /// renders the offending source line, which here would be a token.
    #[error(
        "credential file {path} is not valid TOML ({reason}); \
         the file content is not shown because it holds credentials"
    )]
    Parse {
        /// Absolute path of the file.
        path: String,
        /// Position of the syntax error, never its content.
        reason: String,
    },

    /// The credential file was written by a newer `otl`.
    #[error(
        "credential file {path} uses format version {found}, but this build \
         understands version {supported}; upgrade otl (or move the file \
         aside and authenticate again)"
    )]
    Version {
        /// Absolute path of the file.
        path: String,
        /// Version found in the file.
        found: u32,
        /// Version this build supports.
        supported: u32,
    },

    /// The credential file could not be written.
    #[error("cannot write credential file {path}: {reason}")]
    Write {
        /// Absolute path of the file (or of its temporary sibling).
        path: String,
        /// I/O failure description (no file content).
        reason: String,
    },

    /// The credential directory could not be created.
    #[error("cannot create credential directory {path}: {reason}")]
    Directory {
        /// Absolute path of the directory.
        path: String,
        /// I/O failure description.
        reason: String,
    },

    /// The refresh lock could not be taken.
    #[error("cannot take the credential refresh lock at {path}: {reason}")]
    Lock {
        /// Absolute path of the lock file.
        path: String,
        /// I/O failure description.
        reason: String,
    },

    /// The active profile name cannot be used as a credential-file key.
    #[error(
        "profile name {name:?} is not usable: profile names may contain only \
         letters, digits, {allowed} and must be 1-{max} characters long"
    )]
    ProfileName {
        /// The rejected name (a profile name is not a secret).
        name: String,
        /// Pre-formatted list of the allowed punctuation.
        allowed: &'static str,
        /// Maximum accepted length.
        max: usize,
    },
}

/// Which OAuth interaction failed, for use in messages.
///
/// A closed set of authored labels, so no caller can inject text through
/// an error message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Fetching `/.well-known/oauth-authorization-server`.
    Discovery,
    /// RFC 7591 dynamic client registration.
    Registration,
    /// RFC 7592 registration deletion.
    Deregistration,
    /// Authorization-code exchange at the token endpoint.
    TokenExchange,
    /// Refresh-token grant at the token endpoint.
    Refresh,
    /// Token revocation.
    Revocation,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Discovery => "OAuth metadata discovery",
            Self::Registration => "dynamic client registration",
            Self::Deregistration => "client registration removal",
            Self::TokenExchange => "authorization code exchange",
            Self::Refresh => "access token refresh",
            Self::Revocation => "token revocation",
        };
        f.write_str(text)
    }
}

/// Failures of the OAuth flows.
#[derive(Debug, Error)]
pub enum OAuthError {
    /// A request to an OAuth endpoint never got an answer.
    #[error("{stage} request to {origin} failed: {reason}")]
    Transport {
        /// Which interaction failed.
        stage: Stage,
        /// Origin (`scheme://host[:port]`) of the endpoint.
        origin: String,
        /// Failure category (no URL, no header values).
        reason: String,
    },

    /// An OAuth endpoint answered with a non-success status.
    #[error("{stage} was rejected by {origin} (HTTP {status}){detail}")]
    Endpoint {
        /// Which interaction failed.
        stage: Stage,
        /// Origin (`scheme://host[:port]`) of the endpoint.
        origin: String,
        /// HTTP status code.
        status: u16,
        /// Sanitized, capped server detail, prefixed with `: ` when
        /// present and empty otherwise.
        detail: String,
    },

    /// A response was not the document the relevant RFC requires.
    #[error("{stage} response from {origin} is not usable: {reason}")]
    Malformed {
        /// Which interaction failed.
        stage: Stage,
        /// Origin (`scheme://host[:port]`) of the endpoint.
        origin: String,
        /// Authored reason (a missing field name, a bad URL shape).
        reason: String,
    },

    /// The metadata document points an endpoint at another host.
    ///
    /// Refused rather than followed: a tampered metadata document could
    /// otherwise redirect the authorization code, the PKCE verifier or the
    /// refresh token to a server the user never chose.
    #[error(
        "OAuth metadata from {origin} points its {endpoint} at a different \
         host; continuing would send your credentials somewhere other than \
         the instance you asked for, so this is refused"
    )]
    ForeignEndpoint {
        /// Origin (`scheme://host[:port]`) of the instance.
        origin: String,
        /// Which advertised endpoint is off-origin.
        endpoint: &'static str,
    },

    /// The instance does not offer dynamic client registration.
    #[error(
        "this Outline instance does not offer dynamic client registration, \
         so otl cannot register itself.\n\
         Ask a workspace admin to create an application under \
         Settings -> Applications with the redirect URI\n\
         \x20 {redirect_uri}\n\
         and then run:\n\
         \x20 otl auth login --client-id <client-id>"
    )]
    RegistrationUnavailable {
        /// The exact redirect URI the admin must register.
        redirect_uri: String,
    },

    /// No loopback callback port could be bound.
    #[error(
        "could not bind a local callback port (tried {ports}); another \
         program is using all of them, so close it or wait and retry"
    )]
    NoCallbackPort {
        /// Pre-formatted list of the ports that were tried.
        ports: String,
    },

    /// The browser never completed the redirect.
    #[error(
        "timed out after {seconds}s waiting for the browser to come back; \
         run `otl auth login` again and complete the consent screen"
    )]
    CallbackTimeout {
        /// How long the command waited.
        seconds: u64,
    },

    /// The authorization server reported an error on the redirect.
    #[error("authorization was not granted: {code}{detail}")]
    AuthorizationDenied {
        /// Sanitized OAuth error code from the redirect query.
        code: String,
        /// Sanitized description, prefixed with `: ` when present.
        detail: String,
    },

    /// The redirect carried an unexpected `state`.
    #[error(
        "the browser redirect carried an unexpected state value, so it did \
         not come from the login this command started; aborting without \
         exchanging the code"
    )]
    StateMismatch,

    /// The local callback server failed.
    #[error("local callback server failed: {reason}")]
    Callback {
        /// I/O failure description.
        reason: String,
    },

    /// The operating system would not supply randomness.
    #[error("could not generate a random {what}: {reason}")]
    Random {
        /// What was being generated (`state`, `PKCE verifier`).
        what: &'static str,
        /// Failure description.
        reason: String,
    },

    /// A stored OAuth session cannot be renewed any more.
    #[error(
        "the stored OAuth session for profile {profile:?} is no longer \
         valid{detail}.\nRun `otl auth login` to sign in again."
    )]
    SessionExpired {
        /// Active profile name.
        profile: String,
        /// Sanitized server detail, prefixed with ` (` .. `)` when present.
        detail: String,
    },

    /// Storing refreshed tokens failed after the server already rotated
    /// them, so the old refresh token is dead and the new one is lost.
    #[error(
        "the access token was refreshed but the new tokens could not be \
         saved: {reason}.\n\
         The previous refresh token was rotated by the server and no longer \
         works, so run `otl auth login` to sign in again."
    )]
    RotationLost {
        /// Why the write failed (a [`StoreError`] rendered).
        reason: String,
    },

    /// The browser could not be launched.
    #[error("could not open a browser ({reason}); open this URL manually")]
    Browser {
        /// I/O failure description.
        reason: String,
    },
}

impl OAuthError {
    /// Whether the failure means the grant itself was rejected, so no
    /// retry can help and the user has to authenticate again.
    ///
    /// A 4xx from the token endpoint is the server saying "this code or
    /// refresh token is not valid" - `invalid_grant` and friends. 429 is
    /// excluded: that is throttling, and the grant may still be good.
    pub fn is_grant_rejected(&self) -> bool {
        matches!(
            self,
            Self::Endpoint { status, .. } if (400..500).contains(status) && *status != 429
        )
    }

    /// Whether the failure means the endpoint is simply not offered.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Endpoint { status: 404, .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_4xx_from_the_token_endpoint_counts_as_a_rejected_grant() {
        let rejected = |status| OAuthError::Endpoint {
            stage: Stage::Refresh,
            origin: "https://docs.example.com".to_string(),
            status,
            detail: String::new(),
        };
        assert!(rejected(400).is_grant_rejected());
        assert!(rejected(401).is_grant_rejected());
        assert!(rejected(404).is_grant_rejected());
        // Throttling is not a rejected grant, and neither is a server bug.
        assert!(!rejected(429).is_grant_rejected());
        assert!(!rejected(500).is_grant_rejected());
        assert!(!rejected(503).is_grant_rejected());
        assert!(rejected(404).is_not_found());
        assert!(!rejected(400).is_not_found());
    }

    #[test]
    fn permission_error_states_the_fix_command() {
        let error = StoreError::Permissions {
            path: "/home/u/.config/outline-cli/credentials.toml".to_string(),
            mode: "0644".to_string(),
        };
        let text = error.to_string();
        assert!(
            text.contains("chmod 600 /home/u/.config/outline-cli/credentials.toml"),
            "no fix command: {text}"
        );
    }

    #[test]
    fn registration_unavailable_names_the_admin_path_and_redirect_uri() {
        let error = OAuthError::RegistrationUnavailable {
            redirect_uri: "http://127.0.0.1:8586/callback".to_string(),
        };
        let text = error.to_string();
        assert!(text.contains("Settings -> Applications"), "{text}");
        assert!(text.contains("http://127.0.0.1:8586/callback"), "{text}");
        assert!(text.contains("--client-id"), "{text}");
    }

    #[test]
    fn rotation_loss_tells_the_user_to_log_in_again() {
        let error = OAuthError::RotationLost {
            reason: "disk full".to_string(),
        };
        let text = error.to_string();
        assert!(text.contains("otl auth login"), "{text}");
        assert!(text.contains("rotated"), "{text}");
    }
}
