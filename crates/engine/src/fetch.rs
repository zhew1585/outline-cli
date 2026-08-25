//! The plain-document channel: unauthenticated GETs of a public document.
//!
//! # Why this is not [`crate::client`]
//!
//! The RPC channel exists for requests that carry the caller's bearer
//! token to the API they are authenticated against. A spec document lives
//! on a third-party host (a CDN, a mirror, a local file server) that the
//! caller has no credentials for, so putting it through that channel would
//! mean either sending the token to that host - a credential leak - or
//! giving the channel a "sometimes omit the credential" mode, which is
//! exactly the kind of conditional that makes a security-critical path
//! unreviewable.
//!
//! # What it still shares
//!
//! Not sending a token is no excuse for behaving differently on the wire.
//! This channel reuses the same primitives as the RPC channel, and it must
//! keep doing so:
//!
//! - [`RetryPolicy`]: HTTP 429 is retried with `Retry-After` (or backoff
//!   with jitter), and exhausting the budget is its own error, not a
//!   generic HTTP failure;
//! - [`Throttle`]: every attempt draws from the process-wide request
//!   budget, so a fetch cannot burst past the rate the rest of the process
//!   is pacing itself to;
//! - one `.send()` in the module, so there is one place where a request is
//!   made, classified, and retried.
//!
//! # What it deliberately does NOT share
//!
//! Its errors are a separate type ([`FetchError`]). A document host is not
//! the API: reporting its 401 as "your API key is invalid" or its DNS
//! failure as "check your instance URL" would be actively misleading. The
//! caller maps [`FetchError`] on its own terms.
//!
//! A fetched body is UNTRUSTED input: size-capped and UTF-8 checked here,
//! and validated by whoever parses it. An error response body is never
//! echoed - a document host's error page is not a diagnostic.

use std::io::Read;
use std::thread;
use std::time::Duration;

use reqwest::header::{ACCEPT, RETRY_AFTER};
use reqwest::{StatusCode, Url};
use thiserror::Error;

use crate::error::TransportKind;
use crate::retry::RetryPolicy;
use crate::throttle::Throttle;

/// Default cap on a fetched document, in bytes.
///
/// The body is read into memory, so an unbounded read would turn a hostile
/// (or merely broken) host into an out-of-memory abort.
pub const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;

/// Accept header sent with every document request.
const ACCEPT_TYPES: &str = "application/json, text/plain, */*";

/// Why a document could not be fetched.
///
/// Every variant is credential-free by construction, like
/// [`crate::EngineError`]: URLs are reduced to their origin (a path or
/// query can carry a token), retained transport errors are stripped of
/// their URL, and no part of a response body is ever kept.
#[derive(Debug, Error)]
pub enum FetchError {
    /// The URL could not be parsed or is not usable.
    ///
    /// Deliberately does not carry the URL: it may embed credentials.
    #[error("invalid document URL: {reason}")]
    InvalidUrl {
        /// Human-readable reason.
        reason: String,
    },

    /// The HTTP client could not be constructed.
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(#[source] reqwest::Error),

    /// A transport-level failure (DNS, TLS, connection, timeout, ...).
    #[error("fetching the document from {origin} failed: {kind}")]
    Transport {
        /// Origin (`scheme://host[:port]`) of the request - no path/query.
        origin: String,
        /// Classified failure category.
        kind: TransportKind,
        /// The underlying error, stored URL-stripped.
        #[source]
        source: reqwest::Error,
    },

    /// The host answered with a non-success status.
    ///
    /// The response body is not read: it is a third party's error page,
    /// not a machine-readable envelope.
    #[error("the document source {origin} answered HTTP {status}")]
    Status {
        /// Origin the request went to.
        origin: String,
        /// HTTP status code.
        status: u16,
    },

    /// The host kept answering HTTP 429 until the retry budget ran out.
    #[error(
        "the document source {origin} is rate limiting this client (HTTP 429): \
         giving up after {retries} retries"
    )]
    RateLimited {
        /// Origin the request went to.
        origin: String,
        /// Retries performed before giving up.
        retries: u32,
    },

    /// The document cannot be used as text: too large, not UTF-8, or the
    /// body could not be read to the end.
    #[error("the document from {origin} is unusable: {reason}")]
    Unusable {
        /// Origin the document came from.
        origin: String,
        /// Authored reason, free of document content.
        reason: String,
    },
}

/// A blocking fetcher for public documents.
///
/// Holds the same pacing state as [`crate::Client`]: a retry policy for
/// 429s and a shared throttle handle.
#[derive(Debug)]
pub struct DocumentFetch {
    http: reqwest::blocking::Client,
    max_bytes: u64,
    retry: RetryPolicy,
    throttle: Throttle,
}

impl DocumentFetch {
    /// A fetcher with the given request timeout, the default retry policy
    /// and the process-wide throttle.
    pub fn new(timeout: Duration) -> Result<Self, FetchError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| FetchError::ClientBuild(error.without_url()))?;
        Ok(Self {
            http,
            max_bytes: MAX_DOCUMENT_BYTES,
            retry: RetryPolicy::default(),
            throttle: Throttle::process_wide(),
        })
    }

    /// Replace the 429 retry policy.
    #[must_use]
    pub fn with_retry_policy(self, retry: RetryPolicy) -> Self {
        Self { retry, ..self }
    }

    /// Share a specific throttle handle instead of the process-wide one.
    #[must_use]
    pub fn with_throttle(self, throttle: Throttle) -> Self {
        Self { throttle, ..self }
    }

    /// Cap the document size at `max_bytes` instead of
    /// [`MAX_DOCUMENT_BYTES`].
    #[must_use]
    pub fn with_max_bytes(self, max_bytes: u64) -> Self {
        Self { max_bytes, ..self }
    }

    /// Fetch a document and return its body as text.
    ///
    /// The URL must be an absolute `http`/`https` URL with a host and
    /// without userinfo. No credentials are sent. HTTP 429 is retried per
    /// the retry policy; every attempt is paced by the throttle.
    ///
    /// The returned string is UNTRUSTED: only its size and encoding have
    /// been checked.
    pub fn get_text(&self, url: &str) -> Result<String, FetchError> {
        let parsed = validate_document_url(url)?;
        let origin = parsed.origin().ascii_serialization();
        let mut attempt: u32 = 0;
        loop {
            let response = self.send(&parsed, &origin)?;
            let status = response.status();
            if status == StatusCode::TOO_MANY_REQUESTS {
                attempt = self.wait_for_retry(&response, &origin, attempt)?;
                continue;
            }
            if !status.is_success() {
                return Err(FetchError::Status {
                    origin,
                    status: status.as_u16(),
                });
            }
            return self.read_text(response, &origin);
        }
    }

    /// The single wire path of this module: pace, then GET once.
    fn send(&self, url: &Url, origin: &str) -> Result<reqwest::blocking::Response, FetchError> {
        self.pace();
        self.http
            .get(url.clone())
            .header(ACCEPT, ACCEPT_TYPES)
            .send()
            .map_err(|source| FetchError::Transport {
                origin: origin.to_string(),
                kind: TransportKind::classify(&source),
                // reqwest errors embed the full request URL in Display AND
                // Debug; strip it before the error is retained.
                source: source.without_url(),
            })
    }

    /// Draw one token from the shared throttle, sleeping out any delay.
    fn pace(&self) {
        let delay = self.throttle.acquire_delay();
        if delay.is_zero() {
            return;
        }
        if delay >= Duration::from_secs(1) {
            eprintln!(
                "throttling: waiting {:.1}s to respect the request rate limit",
                delay.as_secs_f64()
            );
        }
        thread::sleep(delay);
    }

    /// Sleep out one 429 and report the next attempt number, or give up.
    ///
    /// Same policy as the RPC channel: honour `Retry-After` when the
    /// server sends a usable one, exponential backoff with jitter
    /// otherwise, and a dedicated error once the budget is spent.
    fn wait_for_retry(
        &self,
        response: &reqwest::blocking::Response,
        origin: &str,
        attempt: u32,
    ) -> Result<u32, FetchError> {
        if attempt >= self.retry.max_retries {
            return Err(FetchError::RateLimited {
                origin: origin.to_string(),
                retries: attempt,
            });
        }
        let wait = self
            .retry
            .retry_wait(retry_after_header(response).as_deref(), attempt);
        // Diagnostics only ever go to stderr; stdout is data.
        eprintln!(
            "rate limited by {origin} (HTTP 429); waiting {:.1}s before retry {}/{}",
            wait.as_secs_f64(),
            attempt + 1,
            self.retry.max_retries
        );
        thread::sleep(wait);
        Ok(attempt + 1)
    }

    /// Read the body, bounded, and decode it as UTF-8.
    fn read_text(
        &self,
        response: reqwest::blocking::Response,
        origin: &str,
    ) -> Result<String, FetchError> {
        let unusable = |reason: String| FetchError::Unusable {
            origin: origin.to_string(),
            reason,
        };
        // One extra byte so hitting the cap is detectable rather than
        // silently truncating the document.
        let mut raw = Vec::new();
        response
            .take(self.max_bytes.saturating_add(1))
            .read_to_end(&mut raw)
            .map_err(|error| {
                unusable(format!(
                    "the response body could not be read ({})",
                    error.kind()
                ))
            })?;
        if raw.len() as u64 > self.max_bytes {
            return Err(unusable(format!(
                "it is larger than the {} byte limit",
                self.max_bytes
            )));
        }
        String::from_utf8(raw).map_err(|error| {
            unusable(format!(
                "it is not valid UTF-8 (first bad byte at offset {})",
                error.utf8_error().valid_up_to()
            ))
        })
    }
}

/// Fetch one document with the default policies.
///
/// Convenience wrapper over [`DocumentFetch`] for the one-shot case.
pub fn fetch_document(url: &str, max_bytes: u64, timeout: Duration) -> Result<String, FetchError> {
    DocumentFetch::new(timeout)?
        .with_max_bytes(max_bytes)
        .get_text(url)
}

/// The `Retry-After` header of a response, if present and readable.
fn retry_after_header(response: &reqwest::blocking::Response) -> Option<String> {
    response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// The origin (`scheme://host[:port]`) of a document URL, or `None` when
/// the URL is not one [`DocumentFetch::get_text`] would accept.
///
/// The only URL-derived form safe to display or persist: a path or query
/// can carry a token, an origin cannot.
pub fn document_origin(url: &str) -> Option<String> {
    validate_document_url(url)
        .ok()
        .map(|parsed| parsed.origin().ascii_serialization())
}

/// Validate a document URL.
///
/// Never place the raw input in the returned error: it may embed
/// credentials in its userinfo or query components. Unlike an API base URL
/// a document URL may carry a path and a query (a mirror or a pinned
/// revision), so only the parts that decide WHERE the request goes are
/// constrained.
fn validate_document_url(url: &str) -> Result<Url, FetchError> {
    let invalid = |reason: &str| FetchError::InvalidUrl {
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
