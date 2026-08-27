//! Decoding for operations that return a signed download redirect.

use reqwest::header::LOCATION;
use reqwest::Url;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponseKind {
    Json,
    RedirectLocation,
}

pub(crate) enum ResponseData {
    Json(Value),
    RedirectLocation(String),
}

/// Extract a credential-bearing target without exposing a rejected header.
pub(crate) fn location(response: &reqwest::blocking::Response) -> Option<String> {
    let raw = response.headers().get(LOCATION)?.to_str().ok()?;
    let parsed = Url::parse(raw).ok()?;
    let safe = matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    safe.then(|| parsed.to_string())
}
