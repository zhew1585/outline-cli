//! Typed errors for the engine request channel.
//!
//! Every field of [`EngineError`] is credential-free BY CONSTRUCTION, so
//! the derived Debug, the Display, and the full `source()` chain are all
//! safe to log:
//!
//! - base-URL problems report only a reason (the raw value may embed
//!   userinfo);
//! - any URL-derived text is reduced to the origin
//!   (`scheme://host[:port]`) - never path, query, or userinfo, which may
//!   all carry secrets;
//! - server-provided text is sanitized, token-redacted, and length-capped;
//! - retained `reqwest::Error` sources are stripped of their request URL
//!   via `without_url()` before storage (reqwest prints the full URL in
//!   both Display and Debug otherwise).
//!
//! Preserve this invariant when adding variants: sanitize at construction
//! rather than at display time.

use std::fmt;

use thiserror::Error;

/// Errors produced by the engine.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The configured base URL could not be parsed or is not usable.
    ///
    /// Deliberately does not carry the offending URL: a malformed value may
    /// contain embedded credentials which must never reach logs or stderr.
    #[error("invalid base URL: {reason}")]
    InvalidBaseUrl {
        /// Human-readable reason.
        reason: String,
    },

    /// The underlying HTTP client could not be constructed.
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(#[source] reqwest::Error),

    /// A transport-level failure (DNS, TLS, connection, timeout, ...).
    ///
    /// Display shows only the request origin and a failure category, never
    /// the raw transport error text (which can embed the full request URL).
    /// The underlying error remains available via `source()` for
    /// programmatic use only.
    #[error("request to {origin} failed: {kind}")]
    Transport {
        /// Origin (`scheme://host[:port]`) of the request - no path/query.
        origin: String,
        /// Classified failure category.
        kind: TransportKind,
        /// The underlying transport error, stored URL-stripped
        /// (`without_url()`), so even its Debug/Display are
        /// credential-free.
        #[source]
        source: reqwest::Error,
    },

    /// The server answered with a non-success HTTP status.
    #[error("server returned HTTP {status}: {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Sanitized, length-capped machine-readable error code extracted
        /// from the response envelope (e.g. its `error` field), if any.
        code: Option<String>,
        /// Sanitized, length-capped message extracted from the response.
        message: String,
    },

    /// The response body was not valid JSON.
    #[error("invalid JSON in response from {origin}")]
    InvalidResponse {
        /// Origin (`scheme://host[:port]`) of the request - no path/query.
        origin: String,
        /// The underlying decode error, stored URL-stripped
        /// (`without_url()`).
        #[source]
        source: reqwest::Error,
    },
}

/// Coarse classification of a transport failure, safe to display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// Request timed out.
    Timeout,
    /// TCP/TLS connection could not be established (DNS, refused, TLS, ...).
    Connect,
    /// A redirect policy was violated.
    Redirect,
    /// The request or response body failed mid-transfer.
    Body,
    /// Any other transport-level failure.
    Other,
}

impl TransportKind {
    /// Classify a reqwest error into a displayable category.
    pub fn classify(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else if error.is_connect() {
            Self::Connect
        } else if error.is_redirect() {
            Self::Redirect
        } else if error.is_body() || error.is_decode() {
            Self::Body
        } else {
            Self::Other
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Timeout => "request timed out",
            Self::Connect => "connection failed (DNS, refused, or TLS)",
            Self::Redirect => "redirect policy violated",
            Self::Body => "body transfer failed",
            Self::Other => "transport error",
        };
        f.write_str(text)
    }
}
