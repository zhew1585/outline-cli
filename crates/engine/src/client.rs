//! The single request channel.
//!
//! Every HTTP call made on behalf of the engine flows through
//! [`Client::execute`]. Local validation, backoff, error mapping and token
//! renewal will all live here (and only here) as they are implemented.

use std::fmt;
use std::io::Read;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Url;
use serde_json::Value;

use crate::body::build_request_body;
use crate::error::{EngineError, TransportKind};
use crate::ir::OpSpec;

/// Placeholder shown instead of secrets in Debug output.
const REDACTED: &str = "***";
/// Minimum length (in chars) of a trailing secret fragment worth redacting.
const MIN_SECRET_FRAGMENT_CHARS: usize = 4;
/// Maximum number of bytes read from an error response body.
const MAX_ERROR_BODY_BYTES: u64 = 8 * 1024;
/// Maximum number of characters kept from a server-provided error message.
const MAX_ERROR_MESSAGE_CHARS: usize = 200;
/// Fallback message when an error response carries no usable text.
const NO_ERROR_DETAILS: &str = "no error details in response body";

/// A blocking RPC client bound to one API base URL and one bearer token.
pub struct Client {
    http: reqwest::blocking::Client,
    base_url: String,
    /// `scheme://host[:port]` of the base URL - the only URL-derived text
    /// this client ever puts into user-visible output (base URL paths can
    /// carry secrets, e.g. token-in-path auth schemes).
    origin: String,
    token: String,
}

impl fmt::Debug for Client {
    /// Manual impl: the bearer token must never appear in Debug output,
    /// and the base URL is reduced to its origin (a path can carry
    /// secrets).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("origin", &self.origin)
            .field("token", &REDACTED)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Create a client for the given base URL and bearer token.
    ///
    /// The base URL must be a valid absolute `http`/`https` URL with a host
    /// and without userinfo (credentials), query, or fragment. A trailing
    /// slash is tolerated and normalized away.
    pub fn new(base_url: &str, token: &str) -> Result<Self, EngineError> {
        let parsed = validate_base_url(base_url)?;
        let http = reqwest::blocking::Client::builder()
            .build()
            .map_err(|error| EngineError::ClientBuild(error.without_url()))?;
        Ok(Self {
            http,
            base_url: parsed.as_str().trim_end_matches('/').to_string(),
            origin: parsed.origin().ascii_serialization(),
            token: token.to_string(),
        })
    }

    /// Execute one RPC operation with `key=value` arguments.
    ///
    /// Arguments are validated against the operation's parameter specs and
    /// coerced to their declared JSON types locally - any validation error
    /// is returned before a single byte goes on the wire. Then sends
    /// `POST {base}{op.path}` and returns the parsed JSON response. The
    /// operation path comes from the IR verbatim; the engine imposes no
    /// URL convention of its own.
    pub fn execute(&self, op: &OpSpec, args: &[(String, String)]) -> Result<Value, EngineError> {
        let body = build_request_body(op, args)?;
        let bytes = serde_json::to_vec(&body).map_err(|error| EngineError::InvalidRequestBody {
            reason: error.to_string(),
        })?;
        self.send(&op.path, bytes)
    }

    /// Execute one RPC operation with a caller-supplied raw JSON body.
    ///
    /// The body must be valid JSON (checked locally, before any network
    /// request) and is sent byte-for-byte verbatim, bypassing `key=value`
    /// assembly and parameter validation entirely.
    pub fn execute_raw(&self, op: &OpSpec, body: &str) -> Result<Value, EngineError> {
        if let Err(error) = serde_json::from_str::<serde::de::IgnoredAny>(body) {
            return Err(EngineError::InvalidRequestBody {
                reason: error.to_string(),
            });
        }
        self.send(&op.path, body.as_bytes().to_vec())
    }

    /// The single wire path: POST a JSON payload and parse the response.
    fn send(&self, op_path: &str, body: Vec<u8>) -> Result<Value, EngineError> {
        let url = format!("{}{}", self.base_url, op_path);
        let response = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .map_err(|source| EngineError::Transport {
                origin: self.display_origin(),
                kind: TransportKind::classify(&source),
                // reqwest errors embed the full request URL in their
                // Display AND Debug output (reqwest docs warn about this
                // explicitly); strip it before the error is retained so
                // the stored source is credential-free by construction.
                source: source.without_url(),
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(EngineError::Api {
                status: status.as_u16(),
                message: extract_error_message(response, &self.token),
            });
        }

        response
            .json()
            .map_err(|source| EngineError::InvalidResponse {
                origin: self.display_origin(),
                // See the Transport arm: strip the URL before retention.
                source: source.without_url(),
            })
    }

    /// The origin for error messages, passed through secret redaction as
    /// defense in depth (an origin should never contain the token, but no
    /// URL-derived text reaches output without going through the pipeline).
    fn display_origin(&self) -> String {
        redact_secret(&self.origin, &self.token)
    }
}

/// Check whether a string is a well-formed base URL: parses as absolute
/// `http`/`https`, has a host, and carries no userinfo, query, or fragment
/// - the same shape [`Client::new`] enforces.
///
/// Validity is NOT a credential-safety guarantee: a valid base URL may
/// still carry secrets in its path. Never display a full base URL; use
/// [`base_url_origin`] for anything user-visible.
pub fn is_valid_base_url(base_url: &str) -> bool {
    validate_base_url(base_url).is_ok()
}

/// The origin (`scheme://host[:port]`) of a base URL that passes
/// [`Client::new`]'s shape checks, or `None` if it does not.
///
/// This is the only URL-derived form safe to display: it can never carry
/// userinfo, query, fragment, or path components, all of which may embed
/// credentials.
pub fn base_url_origin(base_url: &str) -> Option<String> {
    validate_base_url(base_url)
        .ok()
        .map(|parsed| parsed.origin().ascii_serialization())
}

/// Validate the base URL with a real URL parser.
///
/// Never place the raw input in the returned error: it may embed
/// credentials in its userinfo component.
fn validate_base_url(base_url: &str) -> Result<Url, EngineError> {
    let invalid = |reason: &str| EngineError::InvalidBaseUrl {
        reason: reason.to_string(),
    };
    let parsed =
        Url::parse(base_url).map_err(|error| invalid(&format!("not a valid URL ({error})")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(invalid("expected an http:// or https:// URL"));
    }
    if !parsed.has_host() {
        return Err(invalid("URL has no host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid(
            "URL must not contain credentials (user:password@); \
             pass the API key separately",
        ));
    }
    if parsed.query().is_some() {
        return Err(invalid("URL must not contain a query string"));
    }
    if parsed.fragment().is_some() {
        return Err(invalid("URL must not contain a fragment"));
    }
    Ok(parsed)
}

/// Pull a best-effort human-readable message out of an error response.
///
/// The body read is capped at [`MAX_ERROR_BODY_BYTES`]. Any occurrence of
/// `secret` (the client's own bearer token, which a server or proxy may
/// reflect back) is redacted BEFORE sanitization and truncation, so not
/// even a token prefix can survive the length cap. The result is then
/// sanitized (control characters stripped, whitespace collapsed) and
/// capped at [`MAX_ERROR_MESSAGE_CHARS`] before it can reach stderr.
fn extract_error_message(response: reqwest::blocking::Response, secret: &str) -> String {
    let mut raw = Vec::new();
    if response
        .take(MAX_ERROR_BODY_BYTES)
        .read_to_end(&mut raw)
        .is_err()
    {
        return NO_ERROR_DETAILS.to_string();
    }
    let body = String::from_utf8_lossy(&raw);
    let candidate = match serde_json::from_str::<Value>(&body) {
        Ok(json) => json
            .get("message")
            .or_else(|| json.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        Err(_) => body.into_owned(),
    };
    let sanitized = sanitize_message(&redact_secret(&candidate, secret));
    if sanitized.is_empty() {
        NO_ERROR_DETAILS.to_string()
    } else {
        sanitized
    }
}

/// Replace every occurrence of `secret` in `text` with [`REDACTED`].
///
/// An empty secret is left alone (a plain `str::replace("")` would insert
/// the marker between every character). After the exact replacement, any
/// trailing fragment that is a prefix of the secret is also redacted: the
/// [`MAX_ERROR_BODY_BYTES`] read cap can cut a reflected secret mid-way,
/// and such a cut fragment can only appear at the very end of the capped
/// text, where an exact match cannot find it.
fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    redact_cut_secret_tail(&text.replace(secret, REDACTED), secret)
}

/// Redact a trailing prefix of `secret` (at least
/// [`MIN_SECRET_FRAGMENT_CHARS`] chars long) left behind by the body cap.
///
/// A cap cut mid-character leaves a U+FFFD from lossy decoding after the
/// fragment, so trailing replacement characters are ignored when matching.
fn redact_cut_secret_tail(text: &str, secret: &str) -> String {
    let trimmed = text.trim_end_matches(char::REPLACEMENT_CHARACTER);
    let fragment = secret
        .char_indices()
        .map(|(index, c)| &secret[..index + c.len_utf8()])
        .rev()
        .filter(|prefix| prefix.chars().count() >= MIN_SECRET_FRAGMENT_CHARS)
        .find(|prefix| trimmed.ends_with(prefix));
    match fragment {
        Some(prefix) => {
            let kept = &trimmed[..trimmed.len() - prefix.len()];
            format!("{kept}{REDACTED}")
        }
        None => text.to_string(),
    }
}

/// Strip control characters (including ANSI/OSC escapes), collapse
/// whitespace runs, trim, and cap length.
fn sanitize_message(raw: &str) -> String {
    let collapsed = raw
        .chars()
        .map(|c| {
            if c.is_control() || c.is_whitespace() {
                ' '
            } else {
                c
            }
        })
        .fold(String::new(), |mut acc, c| {
            if c != ' ' || !acc.ends_with(' ') {
                acc.push(c);
            }
            acc
        });
    collapsed
        .trim()
        .chars()
        .take(MAX_ERROR_MESSAGE_CHARS)
        .collect()
}
