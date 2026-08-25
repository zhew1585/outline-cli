//! One authenticated session, shared by every curated command.
//!
//! The curated commands (`otl docs ...`, `otl collections ...`) never build
//! a request of their own: they name a compiled operation and hand over
//! `key=value` arguments, exactly like `otl api` does. Everything below
//! funnels into [`engine::Client`], which owns the single HTTP request
//! channel - local validation, 429 backoff, error mapping.
//!
//! Pagination is likewise not reimplemented here: list operations go
//! through the engine's auto-pagination with the Outline descriptor from
//! [`crate::paging`], and the two things a paged fetch can have to say
//! (truncation, unconfirmed page boundaries) are reported on stderr by
//! [`warn_truncated`] and [`UNCONFIRMED_OFFSET_NOTICE`] - the same wording
//! `otl api` uses, from the same place, so the two can never drift.

use anyhow::anyhow;
use engine::{Client, Fetched, Truncation, TruncationCause, ValidationMode};
use serde_json::Value;

use crate::config::Config;
use crate::errors::map_engine_error;
use crate::exit::CliError;
use crate::ops;
use crate::paging;
use crate::stdio;

/// Notice for a list response that carried no pagination echo, so page
/// boundaries rest on the CLI's own offset counter.
pub const UNCONFIRMED_OFFSET_NOTICE: &str =
    "notice: the server did not echo the pagination offset, so page \
     boundaries could not be confirmed; results were paged by offset and \
     may repeat or omit rows if the server ignored it";

/// The response envelope field holding an operation's payload.
const DATA_FIELD: &str = "data";

/// An authenticated connection to one Outline instance.
pub struct Session {
    client: Client,
    /// `scheme://host[:port]` of the configured instance.
    ///
    /// Deliberately the ORIGIN and not the full base URL: a base URL path
    /// may embed credentials (token-in-path schemes), and this string ends
    /// up in user-visible document links. Outline is served from the root
    /// of its host and its `url` fields are root-relative, so an origin is
    /// all that is needed to build an absolute link.
    origin: String,
}

impl Session {
    /// Resolve configuration and build the request channel.
    ///
    /// Configuration problems are reported here, before any network I/O.
    pub fn open() -> Result<Self, CliError> {
        let config = Config::from_env().map_err(CliError::usage)?;
        let client = Client::new(&config.base_url, &config.api_key).map_err(map_engine_error)?;
        let origin = engine::base_url_origin(&config.base_url).ok_or_else(|| {
            // Unreachable in practice: `Client::new` accepted the URL, so it
            // parses. Kept as an error rather than an unwrap (no panics in
            // library code).
            CliError::usage(anyhow!(
                "the configured Outline base URL has no usable origin"
            ))
        })?;
        Ok(Self { client, origin })
    }

    /// The instance origin (`scheme://host[:port]`).
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Call one operation once and return its response envelope.
    pub fn call(&self, operation: &str, args: &[(String, String)]) -> Result<Value, CliError> {
        let op = self.operation(operation)?;
        self.client
            .execute(op, args, ValidationMode::Strict)
            .map_err(map_engine_error)
    }

    /// Call one operation and return its `data` payload.
    pub fn call_data(&self, operation: &str, args: &[(String, String)]) -> Result<Value, CliError> {
        let mut envelope = self.call(operation, args)?;
        Ok(take_data(&mut envelope))
    }

    /// Call one list operation, auto-paginating to the end (or to `limit`
    /// rows), and return the merged rows.
    ///
    /// Truncation and unconfirmed page boundaries are reported on stderr
    /// here, once per call: results are never silently short.
    pub fn call_rows(
        &self,
        operation: &str,
        args: &[(String, String)],
        limit: Option<u64>,
    ) -> Result<Vec<Value>, CliError> {
        let op = self.operation(operation)?;
        let spec = paging::spec_for(op).ok_or_else(|| {
            CliError::failure(anyhow!(
                "internal error: operation {operation:?} does not paginate, \
                 so it cannot be fetched as a list"
            ))
        })?;
        let fetched = self
            .client
            .execute_paged(op, args, ValidationMode::Strict, &spec, limit)
            .map_err(map_engine_error)?;
        Ok(report(fetched))
    }

    /// Look up a compiled operation by name.
    fn operation(&self, operation: &str) -> Result<&'static engine::OpSpec, CliError> {
        ops::find(operation).ok_or_else(|| {
            CliError::failure(anyhow!(
                "internal error: operation {operation:?} is missing from the \
                 compiled API spec"
            ))
        })
    }

    /// Absolute link to a server-provided root-relative path.
    ///
    /// The path is server-controlled, so it is accepted only in the shape
    /// Outline documents it: a root-relative path with no whitespace,
    /// control characters, backslashes, scheme, or authority. Anything else
    /// is rejected rather than pasted into a URL that would then be handed
    /// to a browser.
    pub fn absolute_url(&self, path: &str) -> Result<String, CliError> {
        if !is_safe_relative_path(path) {
            return Err(CliError::failure(anyhow!(
                "the server returned a document path that is not a plain \
                 root-relative path; refusing to build a link from it"
            )));
        }
        Ok(format!("{}{path}", self.origin))
    }
}

/// Whether a server-provided path is a plain root-relative URL path.
///
/// Rejects protocol-relative (`//host/...`) and scheme-bearing values, any
/// whitespace or control character (which could smuggle a second argument
/// or a terminal escape), backslashes (a Windows path separator and a URL
/// escape hatch), and `..` segments.
fn is_safe_relative_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.contains(':')
        && !path.split('/').any(|segment| segment == "..")
        && !path
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\u{7f}')
}

/// Take the `data` payload out of a response envelope.
///
/// A response without `data` is returned whole: the same rule `otl api`
/// follows, so an envelope the vendored spec did not predict is still
/// visible rather than silently becoming `null`.
pub fn take_data(envelope: &mut Value) -> Value {
    match envelope.get_mut(DATA_FIELD) {
        Some(data) => data.take(),
        None => envelope.take(),
    }
}

/// Surface everything a paged fetch has to say, then hand back its rows.
fn report(fetched: Fetched) -> Vec<Value> {
    let Fetched {
        mut value,
        truncation,
        offset_unconfirmed,
    } = fetched;
    if offset_unconfirmed {
        stdio::write_diagnostic_line(UNCONFIRMED_OFFSET_NOTICE);
    }
    if let Some(truncation) = &truncation {
        warn_truncated(truncation);
    }
    match take_data(&mut value) {
        Value::Array(rows) => rows,
        // Unreachable: the engine only accepts a page when the descriptor's
        // items pointer holds an array. Treated as "no rows" rather than a
        // panic.
        _ => Vec::new(),
    }
}

/// Explicit stderr warning whenever results may be incomplete (hard rule:
/// pagination never truncates silently), including how to get more.
///
/// Only [`TruncationCause::is_definite`] causes are stated as fact; the
/// others say results *may* be truncated, because the data could have
/// ended exactly at the boundary.
pub fn warn_truncated(truncation: &Truncation) {
    let remedy = match truncation.cause {
        TruncationCause::MaxItems => "raise or drop --limit to fetch more",
        TruncationCause::PageLimit => {
            "narrow the query, or continue from this point with an \
             `offset=` argument"
        }
        TruncationCause::ManualPage => {
            "a `limit=` argument fetches one page only; drop it to fetch \
             every page, or page manually with `offset=`"
        }
        TruncationCause::OffsetSpaceExhausted => {
            "the pagination offset space is exhausted; narrow the query"
        }
    };
    let certainty = if truncation.cause.is_definite() {
        "results truncated"
    } else {
        "results may be truncated"
    };
    stdio::write_diagnostic_line(&format!(
        "warning: {certainty} after {} items; {remedy}",
        truncation.fetched
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_root_relative_paths() {
        assert!(is_safe_relative_path("/doc/welcome-abc123"));
        assert!(is_safe_relative_path("/collection/eng-9f2"));
    }

    #[test]
    fn rejects_absolute_and_protocol_relative_paths() {
        // A server that answers with someone else's origin must not get a
        // browser opened at it.
        assert!(!is_safe_relative_path("https://evil.example/doc/x"));
        assert!(!is_safe_relative_path("//evil.example/doc/x"));
        assert!(!is_safe_relative_path("javascript:alert(1)"));
        assert!(!is_safe_relative_path("/doc/x?next=http://evil:1"));
    }

    #[test]
    fn rejects_paths_with_control_characters_or_spaces() {
        // Whitespace could split into a second argv entry for an opener,
        // and an escape sequence could rewrite the terminal.
        assert!(!is_safe_relative_path("/doc/a b"));
        assert!(!is_safe_relative_path("/doc/a\nb"));
        assert!(!is_safe_relative_path("/doc/\u{1b}[31m"));
        assert!(!is_safe_relative_path("/doc/a\\b"));
        assert!(!is_safe_relative_path("/doc/../../etc/passwd"));
        assert!(!is_safe_relative_path("doc/relative"));
        assert!(!is_safe_relative_path(""));
    }

    #[test]
    fn take_data_returns_whole_envelope_when_data_is_absent() {
        let mut envelope = serde_json::json!({ "ok": true });
        assert_eq!(take_data(&mut envelope), serde_json::json!({ "ok": true }));
    }

    #[test]
    fn take_data_extracts_the_data_field() {
        let mut envelope = serde_json::json!({ "data": { "id": "d1" }, "ok": true });
        assert_eq!(take_data(&mut envelope), serde_json::json!({ "id": "d1" }));
    }
}
