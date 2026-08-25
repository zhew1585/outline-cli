//! Typed errors for the engine request channel.
//!
//! Error messages never echo credentials: base-URL problems report only a
//! reason (the raw value may embed userinfo), and request URLs are built
//! from a validated, credential-free base.

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
    #[error("request to {url} failed: {source}")]
    Transport {
        /// The URL that was requested (credential-free by construction).
        url: String,
        /// The underlying transport error.
        #[source]
        source: reqwest::Error,
    },

    /// The server answered with a non-success HTTP status.
    #[error("server returned HTTP {status}: {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Sanitized, length-capped message extracted from the response.
        message: String,
    },

    /// The response body was not valid JSON.
    #[error("invalid JSON in response from {url}: {source}")]
    InvalidResponse {
        /// The URL that was requested.
        url: String,
        /// The underlying decode error.
        #[source]
        source: reqwest::Error,
    },
}
