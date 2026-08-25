//! The plain-document channel: one GET, no credentials.
//!
//! This is deliberately NOT the RPC request channel in [`crate::client`],
//! and it is the only other place in the crate that performs a request.
//! The distinction is what keeps the "one channel" rule meaningful:
//!
//! - [`crate::client`] talks to the API a caller is authenticated against.
//!   Everything about it - bearer token, retry budget, throttle, error
//!   envelope, credential hygiene of server text - exists because the
//!   requests carry credentials and the answers are trusted input.
//! - this module fetches a public document (a spec) from a host the caller
//!   is NOT authenticated against. Sending the token there would be a
//!   credential leak, retrying or throttling it makes no sense, and its
//!   body is untrusted bytes rather than an API response.
//!
//! So it sends no `Authorization` header, has no retry policy, reads at
//! most [`MAX_DOCUMENT_BYTES`], and returns the body as an opaque string
//! that the caller must validate before use. An error response body is
//! never echoed: a document host is not an API and its error page is not a
//! diagnostic.

use std::io::Read;
use std::time::Duration;

use reqwest::header::ACCEPT;
use reqwest::Url;

use crate::error::{EngineError, TransportKind};

/// Default cap on a fetched document, in bytes.
///
/// The body is read into memory, so an unbounded read would turn a hostile
/// (or merely broken) host into an out-of-memory abort.
pub const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;

/// Message reported for a non-success HTTP status.
///
/// Authored text: the response body is never used, so nothing from a
/// third-party host reaches the caller's terminal.
const FETCH_REJECTED: &str = "the document could not be fetched";

/// Fetch a document over HTTP(S) and return its body as text.
///
/// The URL must be an absolute `http`/`https` URL with a host and without
/// userinfo. No credentials are sent. At most `max_bytes` are read; a
/// larger body, or one that is not UTF-8, is an
/// [`EngineError::UnusableDocument`].
///
/// The returned string is UNTRUSTED input: it has only been checked for
/// size and encoding.
pub fn fetch_document(url: &str, max_bytes: u64, timeout: Duration) -> Result<String, EngineError> {
    let parsed = validate_document_url(url)?;
    let origin = parsed.origin().ascii_serialization();
    let unusable = |reason: String| EngineError::UnusableDocument {
        origin: origin.clone(),
        reason,
    };
    let transport = |source: reqwest::Error| EngineError::Transport {
        origin: origin.clone(),
        kind: TransportKind::classify(&source),
        // reqwest errors embed the full request URL in Display AND Debug;
        // strip it before the error is retained.
        source: source.without_url(),
    };

    let http = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| EngineError::ClientBuild(error.without_url()))?;
    let response = http
        .get(parsed)
        .header(ACCEPT, "application/json, text/plain, */*")
        .send()
        .map_err(transport)?;
    let status = response.status();
    if !status.is_success() {
        return Err(EngineError::Api {
            status: status.as_u16(),
            code: None,
            message: FETCH_REJECTED.to_string(),
        });
    }

    // One extra byte so that hitting the cap is detectable rather than
    // silently truncating the document.
    let mut raw = Vec::new();
    response
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(|error| read_error(&origin, error))?;
    if raw.len() as u64 > max_bytes {
        return Err(unusable(format!(
            "it is larger than the {max_bytes} byte limit"
        )));
    }
    String::from_utf8(raw).map_err(|error| {
        unusable(format!(
            "it is not valid UTF-8 (first bad byte at offset {})",
            error.utf8_error().valid_up_to()
        ))
    })
}

/// Classify a failure while reading the response body.
///
/// A body cut mid-transfer is a transport failure (retrying may help), not
/// a content problem. The wrapped I/O error is not retained: only its kind
/// is reported, so no URL or payload text can come along.
fn read_error(origin: &str, error: std::io::Error) -> EngineError {
    EngineError::UnusableDocument {
        origin: origin.to_string(),
        reason: format!("the response body could not be read ({})", error.kind()),
    }
}

/// Validate a document URL.
///
/// Never place the raw input in the returned error: it may embed
/// credentials in its userinfo or query components. Unlike an API base URL
/// a document URL may carry a path and a query (a mirror or a pinned
/// revision), so only the parts that decide WHERE the request goes are
/// constrained.
fn validate_document_url(url: &str) -> Result<Url, EngineError> {
    let invalid = |reason: &str| EngineError::InvalidDocumentUrl {
        reason: reason.to_string(),
    };
    let parsed = Url::parse(url).map_err(|error| invalid(&format!("not a valid URL ({error})")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(invalid("expected an http:// or https:// URL"));
    }
    if !parsed.has_host() {
        return Err(invalid("URL has no host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid(
            "URL must not contain credentials (user:password@); a document \
             is fetched unauthenticated",
        ));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn accepts_a_url_with_path_and_query() {
        assert!(validate_document_url("https://example.com/a/b.json?ref=v1").is_ok());
    }

    #[test]
    fn rejects_non_http_schemes_and_userinfo() {
        for url in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "https://u:p@example.com/x",
            "https://",
            "nonsense",
        ] {
            assert!(validate_document_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn url_errors_never_echo_the_url() {
        let error = validate_document_url("https://user:hunter2@example.com/x")
            .expect_err("must be rejected");
        let text = format!("{error} {error:?}");
        assert!(!text.contains("hunter2"), "credential leaked: {text}");
    }
}
