//! The single request channel.
//!
//! Every HTTP call made on behalf of the engine flows through the private
//! [`Client::send`]: both the `key=value` path ([`Client::execute`]) and the
//! raw-body passthrough ([`Client::execute_raw`]) funnel into it, so there
//! is exactly one `.send()` in the crate. Local validation, backoff, error
//! mapping and token renewal all live here (and only here).

use std::fmt;
use std::io::Read;
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Url;
use serde_json::Value;

use crate::body::{build_request_body, ensure_dispatchable};
use crate::error::{is_transport_failure, EngineError, TransportKind};
use crate::ir::{OpSpec, ValidationMode};
use crate::sanitize::{clean_server_text, redact_secret, REDACTED};

/// Default total request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum number of bytes read from an error response body.
const MAX_ERROR_BODY_BYTES: u64 = 8 * 1024;
/// Maximum number of characters kept from a server-provided error message.
const MAX_ERROR_MESSAGE_CHARS: usize = 200;
/// Maximum number of characters kept from a server-provided error code, and
/// the maximum length of a string still accepted as a structured code.
const MAX_ERROR_CODE_CHARS: usize = 64;
/// Fallback message when an error response carries no usable text.
const NO_ERROR_DETAILS: &str = "no error details in response body";
/// Reason reported when a request cannot be assembled locally because a
/// header value is not valid HTTP. Deliberately generic and value-free.
const INVALID_HEADER_REASON: &str =
    "a header value contains characters that are not valid in HTTP \
     (for example a newline or a control character)";
/// Explanation used in place of withheld server text.
const SERVER_MESSAGE_WITHHELD: &str =
    "server message withheld: it may quote the request body, which can contain secrets";

/// How much of a server error response may be surfaced to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorDetail {
    /// Report the server's free-form message (sanitized, token-redacted).
    ///
    /// Safe when every value in the request came from the caller's own
    /// arguments, and available as an explicit opt-in otherwise.
    #[default]
    Full,
    /// Report only the structured error code, never free-form text.
    CodeOnly,
}

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
        Self::with_timeout(base_url, token, DEFAULT_TIMEOUT)
    }

    /// Create a client with an explicit total request timeout.
    ///
    /// Same contract as [`Client::new`], which uses [`DEFAULT_TIMEOUT`].
    pub fn with_timeout(
        base_url: &str,
        token: &str,
        timeout: Duration,
    ) -> Result<Self, EngineError> {
        let parsed = validate_base_url(base_url)?;
        let http = reqwest::blocking::Client::builder()
            .timeout(timeout)
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
    pub fn execute(
        &self,
        op: &OpSpec,
        args: &[(String, String)],
        validation: ValidationMode,
    ) -> Result<Value, EngineError> {
        let body = build_request_body(op, args, validation)?;
        let bytes = serde_json::to_vec(&body).map_err(|error| EngineError::InvalidRequestBody {
            reason: error.to_string(),
        })?;
        // Every value came from the caller's own command line, so server
        // error text may be surfaced in full (sanitized and token-free).
        self.send(&op.path, bytes, ErrorDetail::Full)
    }

    /// Execute one RPC operation with a caller-supplied raw JSON body.
    ///
    /// The body must be valid JSON (checked locally, before any network
    /// request) and is sent byte-for-byte verbatim, bypassing `key=value`
    /// assembly and parameter validation entirely.
    ///
    /// A raw body may carry credentials this client knows nothing about,
    /// and a server error response may quote the request it rejected.
    /// There is no way to recognize such a quote after the fact - a secret
    /// can be short, escaped differently, or overlap another value - so
    /// the decision is categorical: with [`ErrorDetail::CodeOnly`] the
    /// server's free-form text is withheld and only its structured error
    /// code is reported. [`ErrorDetail::Full`] is the caller's explicit
    /// opt-in to seeing text that may echo the body.
    pub fn execute_raw(
        &self,
        op: &OpSpec,
        body: &str,
        detail: ErrorDetail,
    ) -> Result<Value, EngineError> {
        ensure_dispatchable(op)?;
        // Well-formedness only: no value tree is built, so a large body
        // costs one pass and no per-value work.
        if let Err(error) = serde_json::from_str::<serde::de::IgnoredAny>(body) {
            return Err(EngineError::InvalidRequestBody {
                reason: error.to_string(),
            });
        }
        self.send(&op.path, body.as_bytes().to_vec(), detail)
    }

    /// The single wire path: POST a JSON payload and parse the response.
    ///
    /// This is the only `.send()` in the engine. It carries both bodies
    /// serialized from `key=value` arguments and raw caller-supplied bytes,
    /// so every request shares one set of headers, one error
    /// classification, and one credential-hygiene pipeline.
    ///
    /// `detail` decides how much of a server error response may be
    /// surfaced (see [`Client::execute_raw`]).
    fn send(
        &self,
        op_path: &str,
        body: Vec<u8>,
        detail: ErrorDetail,
    ) -> Result<Value, EngineError> {
        let url = format!("{}{}", self.base_url, op_path);
        let response = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .map_err(|source| self.send_error(source))?;

        let status = response.status();
        if !status.is_success() {
            let parts = extract_error_parts(response, &self.token, detail);
            return Err(EngineError::Api {
                status: status.as_u16(),
                code: parts.code,
                message: parts.message,
            });
        }

        response.json().map_err(|source| self.body_error(source))
    }

    /// Classify a failure of `send()`.
    ///
    /// A builder failure never reached the network: the request could not
    /// be assembled locally (an invalid header value, e.g. a credential
    /// containing a newline). It must not be reported as a network problem,
    /// and the underlying error is NOT retained - a builder error may embed
    /// the offending header value.
    fn send_error(&self, source: reqwest::Error) -> EngineError {
        if source.is_builder() {
            return EngineError::InvalidRequest {
                reason: INVALID_HEADER_REASON.to_string(),
            };
        }
        EngineError::Transport {
            origin: self.display_origin(),
            kind: TransportKind::classify(&source),
            // reqwest errors embed the full request URL in their Display
            // AND Debug output (reqwest docs warn about this explicitly);
            // strip it before the error is retained so the stored source is
            // credential-free by construction.
            source: source.without_url(),
        }
    }

    /// Classify a failure of reading/decoding a success response body.
    ///
    /// A body that times out or is cut mid-transfer is a TRANSPORT failure,
    /// not malformed JSON: callers must be able to tell "retry may help"
    /// from "the server sent something unparseable".
    fn body_error(&self, source: reqwest::Error) -> EngineError {
        if is_transport_failure(&source) {
            return EngineError::Transport {
                origin: self.display_origin(),
                kind: TransportKind::classify(&source),
                source: source.without_url(),
            };
        }
        EngineError::InvalidResponse {
            origin: self.display_origin(),
            // See the Transport arm: strip the URL before retention.
            source: source.without_url(),
        }
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

/// Typed error info extracted from an error response body.
struct ApiErrorParts {
    /// Machine-readable error code (e.g. the envelope's `error` field).
    code: Option<String>,
    /// Human-readable message.
    message: String,
}

/// Pull best-effort typed error info out of an error response.
///
/// The body read is capped at [`MAX_ERROR_BODY_BYTES`]; one extra byte is
/// requested so a cap hit is detectable, which makes the trailing fragment
/// of a cut body droppable as a unit.
///
/// With [`ErrorDetail::Full`] every extracted field goes through
/// [`clean_server_text`], which owns the whole credential-hygiene pipeline
/// (redaction before AND after normalization, smuggling check, length cap).
///
/// With [`ErrorDetail::CodeOnly`] the free-form text is dropped entirely
/// and only a code-shaped [`is_error_code`] value is reported: server text
/// may quote the request body, and no filter can reliably recognize a
/// caller's own secret inside it after the fact.
fn extract_error_parts(
    response: reqwest::blocking::Response,
    secret: &str,
    detail: ErrorDetail,
) -> ApiErrorParts {
    let no_details = |detail: ErrorDetail| match detail {
        ErrorDetail::CodeOnly => withheld_parts(None, secret),
        ErrorDetail::Full => ApiErrorParts {
            code: None,
            message: NO_ERROR_DETAILS.to_string(),
        },
    };
    let fallback = |mut parts: ApiErrorParts| {
        if parts.message.is_empty() {
            parts.message = NO_ERROR_DETAILS.to_string();
        }
        parts
    };

    let mut raw = Vec::new();
    if response
        .take(MAX_ERROR_BODY_BYTES + 1)
        .read_to_end(&mut raw)
        .is_err()
    {
        return no_details(detail);
    }
    let capped = raw.len() as u64 > MAX_ERROR_BODY_BYTES;
    raw.truncate(MAX_ERROR_BODY_BYTES as usize);
    let body = String::from_utf8_lossy(&raw);
    let parsed = serde_json::from_str::<Value>(&body).ok();
    if detail == ErrorDetail::CodeOnly {
        return withheld_parts(parsed.as_ref(), secret);
    }
    // Whether a piece of text may itself be cut mid-way governs the
    // cap-tail treatment. A body that PARSED is complete no matter how it
    // was capped - JSON tolerates unlimited trailing whitespace, so a
    // complete envelope can sit inside a capped body - and dropping the
    // last word of a complete field would corrupt a legitimate diagnostic
    // for no security gain. The skeleton smuggling check still applies to
    // every field either way.
    let parts = match parsed {
        Some(json) => {
            let clean = |text: &str, cap: usize| clean_server_text(text, secret, false, cap);
            ApiErrorParts {
                code: json
                    .get("error")
                    .and_then(Value::as_str)
                    .map(|code| clean(code, MAX_ERROR_CODE_CHARS))
                    .filter(|code| !code.is_empty()),
                message: json
                    .get("message")
                    .or_else(|| json.get("error"))
                    .and_then(Value::as_str)
                    .map(|message| clean(message, MAX_ERROR_MESSAGE_CHARS))
                    .unwrap_or_default(),
            }
        }
        // Raw text straight out of the body: this is the only text that a
        // read cap can have cut mid-token.
        None => ApiErrorParts {
            code: None,
            message: clean_server_text(&body, secret, capped, MAX_ERROR_MESSAGE_CHARS),
        },
    };
    fallback(parts)
}

/// Describe an error response without repeating any free-form text.
///
/// The structured error code is reported when the response carries one in
/// a code-shaped field ([`is_error_code`]); a server that puts prose - or
/// a quoted request body - there is treated as having sent no code. The
/// surviving code still goes through [`clean_server_text`] (and is
/// re-checked afterwards) so that a code that smuggles our own bearer token
/// is discarded rather than printed.
fn withheld_parts(parsed: Option<&Value>, secret: &str) -> ApiErrorParts {
    let code = parsed
        .and_then(|json| json.get("error").or_else(|| json.get("code")))
        .and_then(Value::as_str)
        .filter(|code| is_error_code(code))
        .map(|code| clean_server_text(code, secret, false, MAX_ERROR_CODE_CHARS))
        .filter(|code| is_error_code(code));
    ApiErrorParts {
        code,
        message: SERVER_MESSAGE_WITHHELD.to_string(),
    }
}

/// Whether `text` has the shape of a machine-readable error code: a short
/// run of ASCII alphanumerics and `_`, `-`, `.` separators.
///
/// Deliberately strict: it is what separates a stable code from arbitrary
/// server text that might embed the request body. Being ASCII-only, a
/// string that passes can carry no invisible characters either.
fn is_error_code(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_ERROR_CODE_CHARS
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}
