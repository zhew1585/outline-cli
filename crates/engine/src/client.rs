//! The single request channel.
//!
//! Every HTTP call made on behalf of the engine flows through
//! [`Client::execute`]. Local validation, backoff, error mapping and token
//! renewal will all live here (and only here) as they are implemented.

use std::fmt;
use std::io::Read;

use reqwest::header::{ACCEPT, AUTHORIZATION};
use reqwest::Url;
use serde_json::{Map, Value};

use crate::error::EngineError;
use crate::ir::{OpSpec, ParamType};

/// Placeholder shown instead of secrets in Debug output.
const REDACTED: &str = "***";
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
    token: String,
}

impl fmt::Debug for Client {
    /// Manual impl: the bearer token must never appear in Debug output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
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
            .map_err(EngineError::ClientBuild)?;
        Ok(Self {
            http,
            base_url: parsed.as_str().trim_end_matches('/').to_string(),
            token: token.to_string(),
        })
    }

    /// Execute one RPC operation with `key=value` arguments.
    ///
    /// Sends `POST {base}{op.path}` with a JSON body assembled from `args`
    /// and returns the parsed JSON response. The operation path comes from
    /// the IR verbatim; the engine imposes no URL convention of its own.
    pub fn execute(&self, op: &OpSpec, args: &[(String, String)]) -> Result<Value, EngineError> {
        let url = format!("{}{}", self.base_url, op.path);
        let body = build_body(op, args);

        let response = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(ACCEPT, "application/json")
            .json(&body)
            .send()
            .map_err(|source| EngineError::Transport {
                url: url.clone(),
                source,
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
            .map_err(|source| EngineError::InvalidResponse { url, source })
    }
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

/// Assemble the JSON request body from `key=value` argument pairs.
///
/// Dispatch is structured by [`ParamType`]; in this milestone every type is
/// passed through as a JSON string (typed conversion is a later story).
/// Arguments without a matching parameter spec are passed through as strings.
fn build_body(op: &OpSpec, args: &[(String, String)]) -> Value {
    let entries = args.iter().map(|(key, raw)| {
        let ty = op.param(key).map_or(ParamType::Json, |p| p.ty);
        (key.clone(), encode_scalar(ty, raw))
    });
    Value::Object(entries.collect::<Map<String, Value>>())
}

/// Encode one raw CLI value according to its declared wire type.
fn encode_scalar(ty: ParamType, raw: &str) -> Value {
    match ty {
        ParamType::String
        | ParamType::Integer
        | ParamType::Boolean
        | ParamType::Number
        | ParamType::Json => Value::String(raw.to_string()),
    }
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
/// the marker between every character).
fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, REDACTED)
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
