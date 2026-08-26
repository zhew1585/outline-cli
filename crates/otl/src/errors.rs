//! Outline-flavoured mapping from engine errors to exit codes and
//! human-readable messages.
//!
//! The engine stays generic: it exposes typed error info (HTTP status,
//! machine-readable code, sanitized message). Everything Outline-specific
//! about wording, hints, and exit-code classes lives here.
//!
//! Credential hygiene: every message below is composed exclusively from
//! engine error Displays and fields, which are credential-free by
//! construction (see `engine::error`). Never interpolate raw URLs,
//! tokens, or unsanitized server text here.

use engine::fetch::FetchError;
use engine::EngineError;

use crate::config::ENV_API_KEY;
use crate::exit::{CliError, ExitCode};

/// Hint appended to server errors (HTTP 5xx).
const SERVER_RETRY_HINT: &str =
    "The server failed to process the request; it may help to retry later.";
/// Advice for a request that could not even be assembled locally. The
/// offending value is never echoed - it is the credential itself.
const INVALID_REQUEST_HINT: &str = "This usually means the API key contains characters that are \
     not allowed in an HTTP header (for example a trailing newline). \
     Re-copy the key without surrounding whitespace.";
/// Hint appended when rate-limit retries are exhausted.
const RATE_LIMIT_HINT: &str =
    "Wait for the rate limit to reset and retry, or fetch fewer items per run.";
/// Hint appended to network/transport failures.
const NETWORK_RETRY_HINT: &str =
    "Check your network connection and the OUTLINE_URL host, then retry.";
/// Hints for the spec-source domain. None of them mention the API key or
/// the instance URL: neither is involved in fetching a spec.
const FETCH_NETWORK_HINT: &str =
    "Check your network connection and that the spec source above is reachable, then retry. \
     `otl` keeps working on the spec built into the binary meanwhile.";
const FETCH_AUTH_HINT: &str =
    "The spec source requires authentication. `otl spec sync` fetches the document \
     anonymously and never sends your API key to a spec host; point --url at a \
     publicly readable copy, or pass --spec with a local file.";
const FETCH_NOT_FOUND_HINT: &str =
    "Check the --url value, or pass --spec with a local copy of the document.";
const FETCH_SERVER_HINT: &str =
    "The spec source failed to serve the document; it may help to retry later.";
/// Hint appended when spec-fetch rate-limit retries are exhausted.
const FETCH_RATE_LIMIT_HINT: &str =
    "Wait for the spec source's rate limit to reset and run `otl spec sync` again.";

/// Map an engine error to a `CliError` with a documented exit code
/// (see `docs/exit-codes.md`) and a polished stderr message.
pub fn map_engine_error(error: EngineError) -> CliError {
    map_engine_error_with_hint(error, None)
}

/// Same as [`map_engine_error`], plus a caller-supplied actionable hint.
///
/// Exit-code classification stays here (one table, one place); the hint is
/// the calling command's own contribution - which flag or file would get
/// past this particular failure. It is authored text, never server- or
/// user-supplied, so appending it cannot leak anything.
pub fn map_engine_error_with_hint(error: EngineError, hint: Option<&str>) -> CliError {
    let (code, message) = classify(&error);
    let message = match hint {
        Some(hint) => format!("{message}; {hint}"),
        None => message,
    };
    CliError::new(code, anyhow::Error::new(error).context(message))
}

/// Pick the exit code and compose the top-level message for an engine error.
fn classify(error: &EngineError) -> (ExitCode, String) {
    match error {
        EngineError::InvalidBaseUrl { .. } => (ExitCode::Usage, error.to_string()),
        // Nothing was sent, so this is a configuration problem, not a
        // network one: no retry hint, and exit code 2.
        EngineError::InvalidRequest { .. } => {
            (ExitCode::Usage, format!("{error}.\n{INVALID_REQUEST_HINT}"))
        }
        EngineError::Transport { .. } => (
            ExitCode::Network,
            format!("network error: {error}.\n{NETWORK_RETRY_HINT}"),
        ),
        EngineError::Api {
            status,
            code,
            message,
        } => classify_api(*status, code.as_deref(), message),
        // Local validation: rejected before a single byte went on the wire,
        // so these are usage errors (exit code 2) like a bad flag would be.
        // Listed one by one on purpose: a new engine variant must fail to
        // compile here rather than silently inherit a class.
        EngineError::UnknownParam { .. }
        | EngineError::MissingParam { .. }
        | EngineError::ComplexParam { .. }
        | EngineError::InvalidParamValue { .. }
        | EngineError::InexactNumber { .. }
        | EngineError::UnionBody { .. }
        | EngineError::UnsupportedBodyType { .. }
        | EngineError::InvalidRequestBody { .. } => (ExitCode::Usage, error.to_string()),
        // The server throttled this client until the retry budget ran out:
        // its own exit code, so scripts can tell "try later" from a real
        // failure.
        EngineError::RateLimited { .. } => (
            ExitCode::RateLimited,
            format!("{error}.\n{RATE_LIMIT_HINT}"),
        ),
        // A server that breaks its own pagination contract mid-fetch, or a
        // descriptor that cannot be used: both are "the result is not
        // trustworthy", never a partial success.
        EngineError::Pagination { .. } | EngineError::InvalidPaginationSpec { .. } => {
            (ExitCode::Failure, error.to_string())
        }
        EngineError::ClientBuild(_) | EngineError::InvalidResponse { .. } => {
            (ExitCode::Failure, error.to_string())
        }
    }
}

/// Map a document-fetch error to a `CliError`.
///
/// A separate function from [`map_engine_error`] on purpose, and not a
/// delegation to it: the two describe different servers. The Outline
/// instance is reached with the user's API key, so its 401 means "your key
/// is wrong" and its DNS failure means "check `OUTLINE_URL`". A spec host
/// is reached anonymously and has nothing to do with either, so every hint
/// here talks about the spec SOURCE. Exit codes keep their documented
/// meaning (a 404 is still "not found", an exhausted 429 is still 8) - it
/// is the wording, and only the wording, that differs.
pub fn map_fetch_error(error: FetchError) -> CliError {
    let (code, message) = classify_fetch(&error);
    CliError::new(code, anyhow::Error::new(error).context(message))
}

/// Pick the exit code and message for a document-fetch error.
fn classify_fetch(error: &FetchError) -> (ExitCode, String) {
    match error {
        // Nothing was sent: the URL never passed local checks.
        FetchError::InvalidUrl { .. } => (ExitCode::Usage, error.to_string()),
        FetchError::ClientBuild(_) => (ExitCode::Failure, error.to_string()),
        FetchError::Transport { .. } => {
            (ExitCode::Network, format!("{error}.\n{FETCH_NETWORK_HINT}"))
        }
        FetchError::Status { status, .. } => classify_fetch_status(*status, error),
        FetchError::RateLimited { .. } => (
            ExitCode::RateLimited,
            format!("{error}.\n{FETCH_RATE_LIMIT_HINT}"),
        ),
        FetchError::Unusable { .. } => (ExitCode::Failure, error.to_string()),
    }
}

/// Class of a non-success status from a document host.
fn classify_fetch_status(status: u16, error: &FetchError) -> (ExitCode, String) {
    let (exit, hint) = match status {
        401 | 403 => (ExitCode::Auth, Some(FETCH_AUTH_HINT)),
        404 => (ExitCode::NotFound, Some(FETCH_NOT_FOUND_HINT)),
        400..=499 => (ExitCode::ApiRequest, None),
        500..=599 => (ExitCode::Server, Some(FETCH_SERVER_HINT)),
        _ => (ExitCode::Failure, None),
    };
    let suffix = hint.map(|hint| format!("\n{hint}")).unwrap_or_default();
    (exit, format!("{error}{suffix}"))
}

/// Map an API error envelope (status + code + message) to its class.
fn classify_api(status: u16, code: Option<&str>, message: &str) -> (ExitCode, String) {
    let auth_hint = || format!("Check that {ENV_API_KEY} holds a valid API key for this instance.");
    let (exit, label, hint) = match status {
        401 => (ExitCode::Auth, "authentication failed", Some(auth_hint())),
        403 => (ExitCode::Auth, "permission denied", None),
        404 => (ExitCode::NotFound, "not found", None),
        400..=499 => (ExitCode::ApiRequest, "request rejected", None),
        500..=599 => (
            ExitCode::Server,
            "server error",
            Some(SERVER_RETRY_HINT.to_string()),
        ),
        _ => (ExitCode::Failure, "unexpected HTTP status", None),
    };
    let code_suffix = match code {
        // Skip the suffix when it would only repeat the message.
        Some(code) if code != message => format!(" [{code}]"),
        _ => String::new(),
    };
    let hint_suffix = hint.map(|hint| format!("\n{hint}")).unwrap_or_default();
    let text = format!("{label} (HTTP {status}): {message}{code_suffix}{hint_suffix}");
    (exit, text)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn api(status: u16, code: Option<&str>, message: &str) -> EngineError {
        EngineError::Api {
            status,
            code: code.map(str::to_string),
            message: message.to_string(),
        }
    }

    #[test]
    fn maps_401_to_auth_with_key_hint() {
        let mapped = map_engine_error(api(401, Some("authentication_required"), "bad token"));
        assert_eq!(mapped.code, ExitCode::Auth);
        let text = mapped.to_string();
        assert!(text.contains("authentication failed (HTTP 401): bad token"));
        assert!(text.contains(ENV_API_KEY), "hint missing: {text}");
    }

    #[test]
    fn maps_403_to_auth_without_key_hint() {
        let mapped = map_engine_error(api(403, None, "forbidden"));
        assert_eq!(mapped.code, ExitCode::Auth);
        assert!(mapped.to_string().contains("permission denied (HTTP 403)"));
    }

    #[test]
    fn maps_404_to_not_found() {
        let mapped = map_engine_error(api(404, Some("not_found"), "document not found"));
        assert_eq!(mapped.code, ExitCode::NotFound);
        let text = mapped.to_string();
        assert!(text.contains("not found (HTTP 404): document not found [not_found]"));
    }

    #[test]
    fn maps_other_4xx_to_api_request() {
        let mapped = map_engine_error(api(400, Some("validation_error"), "id: Invalid uuid"));
        assert_eq!(mapped.code, ExitCode::ApiRequest);
        assert!(mapped.to_string().contains("[validation_error]"));
    }

    #[test]
    fn maps_5xx_to_server_with_retry_hint() {
        let mapped = map_engine_error(api(503, None, "unavailable"));
        assert_eq!(mapped.code, ExitCode::Server);
        let text = mapped.to_string();
        assert!(text.contains("server error (HTTP 503)"));
        assert!(text.contains("retry"), "retry hint missing: {text}");
    }

    #[test]
    fn skips_code_suffix_when_it_repeats_the_message() {
        // When the envelope has `error` but no `message`, the engine uses
        // the code as the message; do not print it twice.
        let mapped = map_engine_error(api(404, Some("not_found"), "not_found"));
        assert!(!mapped.to_string().contains("[not_found]"));
    }

    #[test]
    fn maps_invalid_request_to_usage_without_retry_hint() {
        // A request that never left the machine is a configuration error,
        // not a network error: exit 2, and no "retry" advice.
        let mapped = map_engine_error(EngineError::InvalidRequest {
            reason: "a header value contains characters that are not valid in HTTP".to_string(),
        });
        assert_eq!(mapped.code, ExitCode::Usage);
        let text = mapped.to_string();
        assert!(
            !text.contains("retry"),
            "retry hint on a local error: {text}"
        );
        assert!(text.contains("HTTP header"), "hint missing: {text}");
    }

    #[test]
    fn maps_invalid_base_url_to_usage() {
        let mapped = map_engine_error(EngineError::InvalidBaseUrl {
            reason: "no host".to_string(),
        });
        assert_eq!(mapped.code, ExitCode::Usage);
    }
}
