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
    /// Generic failure, including network and server errors.
    Failure = 1,
    /// Usage or configuration error (bad arguments, missing config).
    Usage = 2,
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        Self::from(code as u8)
    }
}

/// A CLI-level error: a human-readable message paired with an exit code.
#[derive(Debug)]
pub struct CliError {
    /// The exit code the process must terminate with.
    pub code: ExitCode,
    /// The underlying error, rendered to stderr.
    pub source: anyhow::Error,
}

impl CliError {
    /// A usage/configuration error (exit code 2).
    pub fn usage(source: impl Into<anyhow::Error>) -> Self {
        Self {
            code: ExitCode::Usage,
            source: source.into(),
        }
    }

    /// A generic/network failure (exit code 1).
    pub fn failure(source: impl Into<anyhow::Error>) -> Self {
        Self {
            code: ExitCode::Failure,
            source: source.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#}", self.source)
    }
}
