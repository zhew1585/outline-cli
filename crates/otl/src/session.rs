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

use crate::config::{Config, Overrides};
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
    /// `overrides` is the command-line layer, which outranks the
    /// environment and the config file key by key - the curated commands
    /// honour `--profile`, `--url` and `--config` exactly as `otl api`
    /// does, because they resolve configuration the same way.
    ///
    /// Configuration problems are reported here, before any network I/O.
    pub fn open(overrides: &Overrides) -> Result<Self, CliError> {
        let config = Config::load(overrides).map_err(CliError::usage)?;
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
    /// rows), and return the merged rows together with WHY the fetch
    /// stopped.
    ///
    /// Truncation and unconfirmed page boundaries are reported on stderr
    /// here, once per call, but the truncation is also RETURNED: a warning
    /// alone is not enough for a caller whose output is an artifact rather
    /// than a stream (see [`Rows::incomplete`] and `otl docs export`).
    pub fn call_rows(
        &self,
        operation: &str,
        args: &[(String, String)],
        limit: Option<u64>,
    ) -> Result<Rows, CliError> {
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

/// Percent-encodings that a URL parser turns back into a path separator or
/// a dot segment. Compared case-insensitively.
const ENCODED_SEPARATORS: &[&str] = &["%2e", "%2f", "%5c"];

/// Whether a server-provided path is a plain root-relative URL path.
///
/// Rejects protocol-relative (`//host/...`) and scheme-bearing values, any
/// whitespace or control character (which could smuggle a second argument
/// or a terminal escape), backslashes (a Windows path separator and a URL
/// escape hatch), and `..` segments.
///
/// Two subtler classes are rejected as well, because this string is both
/// printed to the user AND handed to a browser, and the two must agree:
///
/// - **percent-encoded separators and dots** (`%2e%2e%2f`, `%5c`): the
///   literal check above sees an ordinary segment, while the browser
///   decodes and then normalizes, so the address opened would not be the
///   one validated. Outline's own `url` values are plain slugs, so there is
///   nothing legitimate to lose here.
/// - **invisible and bidirectional formatting characters**: they survive
///   into stdout and make the printed link read as a different path. The
///   set is the whole Unicode `Cf` category (see [`crate::text`]), not a
///   hand-picked list - a hand-picked one here had already let U+061C and
///   U+206A..U+206F through.
fn is_safe_relative_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.contains(':')
        && !path.split('/').any(|segment| segment == "..")
        && !ENCODED_SEPARATORS
            .iter()
            .any(|encoded| lowered.contains(encoded))
        // Any hazard at all, of any category: a URL path has no use for a
        // control character, a bidi override, an invisible pad or a joiner,
        // and this string is both printed to the user and handed to a
        // browser - the two must agree about what it says.
        && !path
            .chars()
            .any(|c| c.is_whitespace() || crate::text::hazard(c).is_some())
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

/// The result of an auto-paginated fetch: the rows, plus why it stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct Rows {
    /// The merged rows.
    pub items: Vec<Value>,
    /// Present exactly when the row set may be short of the full result.
    pub truncation: Option<Truncation>,
}

impl Rows {
    /// Whether the row set is short of what the caller ASKED for.
    ///
    /// [`TruncationCause::MaxItems`] is excluded: it is only reachable when
    /// the caller passed `--limit`, so stopping there is the requested
    /// outcome, not a shortfall. Every other cause means the CLI gave up
    /// before the data ran out - the page-count safety cap, an exhausted
    /// offset space, a pinned page size - and a command whose result must
    /// be trustworthy has to treat that as an incomplete result rather
    /// than as a warning it hopes someone reads.
    pub fn incomplete(&self) -> Option<&Truncation> {
        self.truncation
            .as_ref()
            .filter(|truncation| truncation.cause != TruncationCause::MaxItems)
    }

    /// How many rows came back.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether no rows came back.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Surface everything a paged fetch has to say, then hand back its rows.
fn report(fetched: Fetched) -> Rows {
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
    let items = match take_data(&mut value) {
        Value::Array(rows) => rows,
        // Unreachable: the engine only accepts a page when the descriptor's
        // items pointer holds an array. Treated as "no rows" rather than a
        // panic.
        _ => Vec::new(),
    };
    Rows { items, truncation }
}

/// The stderr-and-exit-code message for an incomplete result set.
///
/// Shared by every curated list command so the wording (and the exit code
/// it carries) cannot drift between them.
pub fn incomplete_error(what: &str, truncation: &Truncation) -> CliError {
    CliError::partial(anyhow!(
        "{what} is incomplete: only {} item(s) could be fetched before the \
         CLI's own pagination limit stopped the fetch. The rows that were \
         fetched are valid; narrow the query (for example by collection) and \
         run again to cover the rest.",
        truncation.fetched
    ))
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
    fn rejects_percent_encoded_separators_and_dot_segments() {
        // A browser decodes and then normalizes these, so the address it
        // opens is not the one the literal check saw.
        for path in [
            "/%2e%2e/admin",
            "/%2E%2E/admin",
            "/doc/..%2fadmin",
            "/doc/%5cadmin",
            "/doc/a%2Fb",
        ] {
            assert!(!is_safe_relative_path(path), "accepted {path:?}");
        }
    }

    #[test]
    fn rejects_invisible_and_bidirectional_formatting_characters() {
        // `report-<RLO>fdp` prints as `report-pdf`: the link shown to the
        // user would not be the link the browser opens.
        for path in [
            "/doc/report-\u{202e}fdp",
            "/doc/a\u{200b}b",
            "/doc/\u{feff}x",
            "/doc/\u{2066}x\u{2069}",
            // Previously missed: not control characters, not whitespace,
            // and not on the old hand-picked list.
            "/doc/a\u{061c}b",  // ARABIC LETTER MARK
            "/doc/a\u{206a}b",  // INHIBIT SYMMETRIC SWAPPING (deprecated)
            "/doc/a\u{206e}b",  // NATIONAL DIGIT SHAPES (deprecated)
            "/doc/a\u{00ad}b",  // SOFT HYPHEN
            "/doc/a\u{2060}b",  // WORD JOINER
            "/doc/a\u{fff9}b",  // INTERLINEAR ANNOTATION ANCHOR
            "/doc/a\u{e0041}b", // TAG LATIN CAPITAL LETTER A
        ] {
            assert!(!is_safe_relative_path(path), "accepted {path:?}");
        }
    }

    #[test]
    fn the_rejection_set_is_the_whole_format_category() {
        // Previously a hand-picked list; these are the ones it missed.
        for path in [
            "/doc/a\u{0600}b",  // ARABIC NUMBER SIGN
            "/doc/a\u{06dd}b",  // ARABIC END OF AYAH
            "/doc/a\u{070f}b",  // SYRIAC ABBREVIATION MARK
            "/doc/a\u{0890}b",  // ARABIC POUND MARK ABOVE
            "/doc/a\u{110bd}b", // KAITHI NUMBER SIGN
            "/doc/a\u{13430}b", // Egyptian hieroglyph format control
            "/doc/a\u{1bca0}b", // SHORTHAND FORMAT LETTER OVERLAP
        ] {
            assert!(!is_safe_relative_path(path), "accepted {path:?}");
        }
    }

    #[test]
    fn still_accepts_ordinary_percent_free_unicode_slugs() {
        // Guard against over-rejecting: a non-ASCII slug is fine.
        assert!(is_safe_relative_path("/doc/\u{4e2d}\u{6587}-abc123"));
        assert!(is_safe_relative_path("/doc/caf\u{e9}-9f2"));
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

    fn rows_with(cause: Option<TruncationCause>) -> Rows {
        Rows {
            items: Vec::new(),
            truncation: cause.map(|cause| Truncation { fetched: 10, cause }),
        }
    }

    #[test]
    fn a_complete_fetch_is_not_incomplete() {
        assert!(rows_with(None).incomplete().is_none());
    }

    #[test]
    fn a_user_requested_limit_is_not_an_incomplete_result() {
        // `--limit N` asked for exactly this; it must not become exit 9.
        assert!(rows_with(Some(TruncationCause::MaxItems))
            .incomplete()
            .is_none());
    }

    #[test]
    fn every_other_truncation_cause_is_an_incomplete_result() {
        // The CLI gave up before the data ran out. A stderr warning alone
        // would let automation read exit 0 as "complete".
        for cause in [
            TruncationCause::PageLimit,
            TruncationCause::ManualPage,
            TruncationCause::OffsetSpaceExhausted,
        ] {
            assert!(
                rows_with(Some(cause)).incomplete().is_some(),
                "{cause:?} was treated as complete"
            );
        }
    }

    #[test]
    fn the_incomplete_error_carries_exit_code_9() {
        let truncation = Truncation {
            fetched: 10_000,
            cause: TruncationCause::PageLimit,
        };
        let error = incomplete_error("the collection listing", &truncation);
        assert_eq!(error.code, crate::exit::ExitCode::Partial);
        assert!(error.to_string().contains("10000"), "{error}");
        assert!(error.to_string().contains("incomplete"), "{error}");
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
