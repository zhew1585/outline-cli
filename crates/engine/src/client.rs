//! The single request channel.
//!
//! Every HTTP call made on behalf of the engine flows through
//! [`Client::execute`]. Local validation, backoff, error mapping and token
//! renewal will all live here (and only here) as they are implemented.

use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde_json::{Map, Value};

use crate::error::EngineError;
use crate::ir::{OpSpec, ParamType};

/// A blocking RPC client bound to one API base URL and one bearer token.
#[derive(Debug)]
pub struct Client {
    http: reqwest::blocking::Client,
    base_url: String,
    token: String,
}

impl Client {
    /// Create a client for the given base URL and bearer token.
    ///
    /// The base URL must start with `http://` or `https://`. A trailing
    /// slash is tolerated and normalized away.
    pub fn new(base_url: &str, token: &str) -> Result<Self, EngineError> {
        let normalized = base_url.trim_end_matches('/').to_string();
        if !normalized.starts_with("http://") && !normalized.starts_with("https://") {
            return Err(EngineError::InvalidBaseUrl {
                url: base_url.to_string(),
                reason: "expected an http:// or https:// URL".to_string(),
            });
        }
        let http = reqwest::blocking::Client::builder()
            .build()
            .map_err(EngineError::ClientBuild)?;
        Ok(Self {
            http,
            base_url: normalized,
            token: token.to_string(),
        })
    }

    /// Execute one RPC operation with `key=value` arguments.
    ///
    /// Sends `POST {base}/api/{op.name}` with a JSON body assembled from
    /// `args` and returns the parsed JSON response.
    pub fn execute(&self, op: &OpSpec, args: &[(String, String)]) -> Result<Value, EngineError> {
        let url = format!("{}/api/{}", self.base_url, op.name);
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
                message: extract_error_message(response),
            });
        }

        response
            .json()
            .map_err(|source| EngineError::InvalidResponse { url, source })
    }
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
fn extract_error_message(response: reqwest::blocking::Response) -> String {
    let fallback = "no error details in response body".to_string();
    let Ok(body) = response.text() else {
        return fallback;
    };
    match serde_json::from_str::<Value>(&body) {
        Ok(json) => json
            .get("message")
            .or_else(|| json.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(fallback),
        Err(_) if body.trim().is_empty() => fallback,
        Err(_) => body.chars().take(200).collect(),
    }
}
