//! Central exit-code table. Public API: documented in `docs/exit-codes.md`.
//!
//! Published codes must never change meaning; new error classes get new
//! codes registered in the doc.

use std::fmt;

/// Process exit codes for `otl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Success.
    Success = 0,
    /// Generic failure (unexpected internal error, malformed response).
    Failure = 1,
    /// Usage or configuration error (bad arguments, missing config).
    Usage = 2,
    /// The API rejected the request (4xx other than auth/not-found).
    ApiRequest = 3,
    /// Authentication or permission error (HTTP 401/403).
    Auth = 4,
    /// The requested resource does not exist (HTTP 404).
    NotFound = 5,
    /// The server failed to process the request (HTTP 5xx).
    Server = 6,
    /// Network/transport failure (DNS, connect, TLS, timeout).
    Network = 7,
    /// Rate limited: the server kept answering HTTP 429 until the retry
    /// budget was exhausted.
    RateLimited = 8,
    /// Partial success: a batch command completed, but some items in the
    /// batch failed. Whatever succeeded is on disk / on stdout, and the
    /// failures are summarized on stderr.
    Partial = 9,
}

impl ExitCode {
    /// The stable machine-readable name of this code.
    ///
    /// Not a second taxonomy: it is the same nine classes the numeric table
    /// publishes, spelled so a reader does not have to keep the table in
    /// their head. Renaming one is exactly as breaking as changing what a
    /// number means, and is governed by the same rule.
    pub fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Usage => "usage",
            Self::ApiRequest => "api-request",
            Self::Auth => "auth",
            Self::NotFound => "not-found",
            Self::Server => "server",
            Self::Network => "network",
            Self::RateLimited => "rate-limited",
            Self::Partial => "partial",
        }
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        Self::from(code as u8)
    }
}

/// A CLI-level error: a human-readable message paired with an exit code.
///
/// The derived Debug is credential-free because every wrapped error is
/// credential-free by construction (see `engine::error`); preserve that
/// invariant when wrapping new error types.
#[derive(Debug)]
pub struct CliError {
    /// The exit code the process must terminate with.
    pub code: ExitCode,
    /// The underlying error, rendered to stderr.
    pub source: anyhow::Error,
}

impl CliError {
    /// An error with an explicit exit code.
    pub fn new(code: ExitCode, source: impl Into<anyhow::Error>) -> Self {
        Self {
            code,
            source: source.into(),
        }
    }

    /// A usage/configuration error (exit code 2).
    pub fn usage(source: impl Into<anyhow::Error>) -> Self {
        Self::new(ExitCode::Usage, source)
    }

    /// A generic failure (exit code 1).
    pub fn failure(source: impl Into<anyhow::Error>) -> Self {
        Self::new(ExitCode::Failure, source)
    }

    /// A partial failure of a batch command (exit code 9).
    ///
    /// Reserved for commands that keep going after an individual item
    /// fails: the successful part of the work stands, and this code tells a
    /// script that the batch was incomplete without pretending it failed
    /// wholesale.
    pub fn partial(source: impl Into<anyhow::Error>) -> Self {
        Self::new(ExitCode::Partial, source)
    }
}

impl fmt::Display for CliError {
    /// Prints only the top-level error message, never the source chain:
    /// underlying transport errors (reqwest) can embed the full request
    /// URL, whose path may carry secrets. Engine error Displays are
    /// crafted to be complete and credential-free on their own.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.source)
    }
}
