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

use std::error::Error as _;
use std::fmt;

use thiserror::Error;

use crate::ir::ParamType;

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

    /// The request could not be assembled locally, so nothing was sent.
    ///
    /// Deliberately carries only an authored reason and no source: a
    /// builder error can embed the offending header value, which may be a
    /// credential.
    #[error("request could not be built: {reason}")]
    InvalidRequest {
        /// Human-readable reason, free of any caller-supplied value.
        reason: String,
    },

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

    /// The server kept answering HTTP 429 until the retry budget ran out.
    ///
    /// Dedicated variant so callers can distinguish "retry later" from
    /// other failures; carries only the origin (credential-free).
    #[error(
        "rate limited by {origin} (HTTP 429): giving up after {retries} retries; \
         the server is throttling this client - try again later"
    )]
    RateLimited {
        /// Origin (`scheme://host[:port]`) of the request - no path/query.
        origin: String,
        /// Number of retries performed before giving up.
        retries: u32,
    },

    /// A paginated fetch could not be completed consistently.
    ///
    /// Raised when a page does not match the caller's pagination
    /// descriptor, or when the server reports an offset that disagrees
    /// with the requested one. Returning the rows fetched so far as a
    /// success would be silent truncation, so this is a hard error.
    #[error("pagination failed: {reason}")]
    Pagination {
        /// Human-readable reason, built from descriptor and counts only.
        reason: String,
    },

    /// A pagination descriptor is not usable.
    ///
    /// Detected before any network request. This is a caller-side
    /// programming error (a malformed JSON pointer, or metadata removal
    /// that would delete the merged rows), never user input.
    #[error("invalid pagination descriptor: {reason}")]
    InvalidPaginationSpec {
        /// Human-readable reason, free of any caller-supplied value.
        reason: String,
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

    /// A `key=value` argument does not name any parameter of the operation.
    ///
    /// Detected locally, before any network request.
    #[error("unknown parameter {name:?} for operation {operation:?}; {valid}")]
    UnknownParam {
        /// Operation name.
        operation: String,
        /// The unrecognized parameter name.
        name: String,
        /// Pre-formatted help text listing the valid parameter names (or
        /// stating that the operation takes none).
        valid: String,
    },

    /// A required parameter was not supplied.
    ///
    /// Detected locally, before any network request.
    #[error("missing required parameter {name:?} (type {ty}) for operation {operation:?}")]
    MissingParam {
        /// Operation name.
        operation: String,
        /// The missing parameter name.
        name: String,
        /// Declared wire type of the missing parameter.
        ty: ParamType,
    },

    /// A complex (object/array/union) parameter was given as `key=value`.
    ///
    /// Complex values cannot be expressed as a scalar CLI argument; the
    /// caller must supply a raw JSON body instead. Detected locally,
    /// before any network request.
    #[error(
        "parameter {name:?} of operation {operation:?} is a complex JSON type \
         (object, array, or union) and cannot be passed as key=value"
    )]
    ComplexParam {
        /// Operation name.
        operation: String,
        /// The complex parameter name.
        name: String,
    },

    /// A `key=value` value does not conform to the parameter's scalar type.
    ///
    /// Detected locally, before any network request. Carries no raw value:
    /// error text must stay credential-free by construction.
    #[error("invalid value for parameter {name:?}: {reason}")]
    InvalidParamValue {
        /// The parameter name.
        name: String,
        /// Human-readable reason (e.g. `expected an integer`).
        reason: String,
    },

    /// A `key=value` value cannot be sent exactly as a JSON number.
    ///
    /// Detected locally, before any network request. Raised instead of
    /// silently rounding the value on the wire.
    #[error("value for parameter {name:?} cannot be sent exactly as a JSON number: {reason}")]
    InexactNumber {
        /// The parameter name.
        name: String,
        /// Human-readable reason.
        reason: String,
    },

    /// The operation's request body cannot be assembled from `key=value`
    /// arguments because a root-level `oneOf`/`anyOf` union constrains it.
    ///
    /// Detected locally, before any network request.
    #[error(
        "operation {operation:?} constrains its request body with a JSON union \
         (oneOf/anyOf), which key=value arguments cannot express"
    )]
    UnionBody {
        /// Operation name.
        operation: String,
    },

    /// The operation requires a request content type this client cannot
    /// assemble (e.g. `multipart/form-data`).
    ///
    /// Detected locally, before any network request.
    #[error(
        "operation {operation:?} requires request content type {content_type}, \
         which this client cannot assemble"
    )]
    UnsupportedBodyType {
        /// Operation name.
        operation: String,
        /// The content type declared by the spec.
        content_type: String,
    },

    /// A document URL could not be parsed or is not usable.
    ///
    /// Deliberately does not carry the offending URL: a document URL may
    /// embed credentials in its userinfo or query components.
    #[error("invalid document URL: {reason}")]
    InvalidDocumentUrl {
        /// Human-readable reason.
        reason: String,
    },

    /// A fetched document cannot be used as text: it exceeded the size cap,
    /// is not UTF-8, or its body could not be read.
    ///
    /// Carries only the origin and an authored reason - never any part of
    /// the document, which is untrusted third-party content.
    #[error("document from {origin} is unusable: {reason}")]
    UnusableDocument {
        /// Origin (`scheme://host[:port]`) the document came from.
        origin: String,
        /// Human-readable reason, free of document content.
        reason: String,
    },

    /// A raw request body is not valid JSON.
    ///
    /// Detected locally, before any network request. The reason is a
    /// serde_json position message (line/column), never body content.
    #[error("request body is not valid JSON: {reason}")]
    InvalidRequestBody {
        /// Parse failure description (position only, no content).
        reason: String,
    },
}

impl EngineError {
    /// Whether this error is a local validation/usage error, raised before
    /// any network request (as opposed to a transport or server failure).
    pub fn is_validation(&self) -> bool {
        matches!(
            self,
            Self::InvalidBaseUrl { .. }
                | Self::InvalidDocumentUrl { .. }
                | Self::UnknownParam { .. }
                | Self::MissingParam { .. }
                | Self::ComplexParam { .. }
                | Self::InvalidParamValue { .. }
                | Self::InexactNumber { .. }
                | Self::UnionBody { .. }
                | Self::UnsupportedBodyType { .. }
                | Self::InvalidRequestBody { .. }
        )
    }

    /// Whether the caller should be pointed at supplying a raw JSON body
    /// instead: the value or shape at fault cannot be expressed as
    /// `key=value` arguments at all.
    pub fn suggests_raw_body(&self) -> bool {
        matches!(
            self,
            Self::ComplexParam { .. }
                | Self::UnionBody { .. }
                | Self::InexactNumber { .. }
                | Self::MissingParam {
                    ty: ParamType::Json,
                    ..
                }
        )
    }
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
        } else if error.is_body() || error.is_decode() || has_io_source(error) {
            Self::Body
        } else {
            Self::Other
        }
    }
}

/// Whether a reqwest error that surfaced AFTER a response was received is a
/// transport failure rather than a content problem.
///
/// A body that times out or is cut mid-transfer must be reported as a
/// (retryable) transport failure, not as malformed JSON: only a genuine
/// syntax error means retrying cannot help. A truncated or aborted body
/// surfaces as a body/timeout error, or as a decode error carrying an I/O
/// error in its source chain; a genuine JSON syntax error carries only a
/// serde error.
pub fn is_transport_failure(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || error.is_connect()
        || error.is_request()
        || error.is_body()
        || has_io_source(error)
}

/// Whether an I/O error appears anywhere in the error's source chain.
fn has_io_source(error: &reqwest::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = error.source();
    while let Some(inner) = source {
        if inner.is::<std::io::Error>() {
            return true;
        }
        source = inner.source();
    }
    false
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
