//! HTTP for the OAuth endpoints - the documented exception to the single
//! request channel.
//!
//! Ordinary API calls MUST go through `engine::Client`. The OAuth metadata,
//! token, registration and revocation endpoints cannot: they are not in the
//! vendored spec, they are not `POST /api/*`, they speak form encoding, and
//! two of them are what produce the credential the channel needs. So they
//! get their own narrow client - and it lives in exactly one module, this
//! one, so "the exception" stays a single auditable place instead of a
//! habit.
//!
//! Everything a server says here is hostile input and goes through
//! [`engine::sanitize::clean_server_text`] with every secret the request
//! carried as a redaction key: an OAuth `error_description` routinely
//! quotes the offending parameter, which is the authorization code, the
//! refresh token or the client secret.

use std::io::Read;
use std::time::Duration;

use engine::sanitize::clean_server_text;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION};
use reqwest::StatusCode;
use serde_json::Value;

use crate::auth::error::{OAuthError, Stage};

/// Total timeout for one OAuth endpoint request.
pub const OAUTH_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum accepted size of an OAuth endpoint response body.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// Maximum characters kept from a server-provided error description.
const MAX_DETAIL_CHARS: usize = 200;

/// Maximum characters kept from a server-provided error code.
const MAX_CODE_CHARS: usize = 64;

/// Fallback origin label when a URL cannot be reduced to one.
const UNKNOWN_ORIGIN: &str = "the authorization server";

/// Build the HTTP client used for every OAuth endpoint call.
pub fn http_client() -> Result<Client, OAuthError> {
    Client::builder()
        .timeout(OAUTH_TIMEOUT)
        .build()
        .map_err(|error| OAuthError::Transport {
            stage: Stage::Discovery,
            origin: UNKNOWN_ORIGIN.to_string(),
            reason: format!("HTTP client could not be built: {error}"),
        })
}

/// One OAuth endpoint call: where it goes, what it is for, and which of
/// its values must never surface in an error message.
#[derive(Clone, Copy)]
pub struct Call<'a> {
    /// Which interaction this is, for messages.
    pub stage: Stage,
    /// Absolute endpoint URL.
    pub url: &'a str,
    /// Values the request carries that must be redacted from server text.
    pub secrets: &'a [&'a str],
}

impl Call<'_> {
    /// Origin of the endpoint, the only URL-derived text ever displayed.
    pub fn origin(&self) -> String {
        origin_of(self.url)
    }

    /// Classify a transport failure.
    fn transport(&self, error: reqwest::Error) -> OAuthError {
        OAuthError::Transport {
            stage: self.stage,
            origin: self.origin(),
            // reqwest errors embed the full request URL in Display AND
            // Debug; never retain one, only a category.
            reason: describe(&error),
        }
    }

    /// Report a response that is not the document the RFC requires.
    pub fn malformed(&self, reason: impl Into<String>) -> OAuthError {
        OAuthError::Malformed {
            stage: self.stage,
            origin: self.origin(),
            reason: reason.into(),
        }
    }
}

/// GET a JSON document from an OAuth endpoint.
pub fn get_json(http: &Client, call: Call<'_>) -> Result<Value, OAuthError> {
    send(http, call, http.get(call.url))
}

/// POST an `application/x-www-form-urlencoded` request, as RFC 6749
/// requires for the token and revocation endpoints.
pub fn post_form(
    http: &Client,
    call: Call<'_>,
    form: &[(&str, &str)],
) -> Result<Value, OAuthError> {
    send(http, call, http.post(call.url).form(form))
}

/// POST a JSON request, as RFC 7591 requires for client registration.
pub fn post_json(http: &Client, call: Call<'_>, body: &Value) -> Result<Value, OAuthError> {
    send(http, call, http.post(call.url).json(body))
}

/// DELETE a resource with a bearer credential, as RFC 7592 requires.
pub fn delete_authorized(http: &Client, call: Call<'_>, bearer: &str) -> Result<(), OAuthError> {
    let request = http
        .delete(call.url)
        .header(AUTHORIZATION, format!("Bearer {bearer}"));
    send(http, call, request).map(|_| ())
}

/// Run one request and turn its outcome into a JSON value or a typed error.
///
/// An empty body is `Value::Null` rather than an error: revocation and
/// registration deletion legitimately answer 200/204 with nothing.
fn send(http: &Client, call: Call<'_>, request: RequestBuilder) -> Result<Value, OAuthError> {
    let _ = http;
    let response = request
        .header(ACCEPT, "application/json")
        .send()
        .map_err(|error| call.transport(error))?;
    let status = response.status();
    let body = read_capped(response, call)?;
    if !status.is_success() {
        return Err(endpoint_error(call, status, &body));
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&body).map_err(|error| {
        // serde_json position messages carry line/column, not content.
        call.malformed(format!("response is not valid JSON ({error})"))
    })
}

/// Read a response body, refusing anything over [`MAX_RESPONSE_BYTES`].
fn read_capped(response: Response, call: Call<'_>) -> Result<String, OAuthError> {
    let mut raw = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|error| OAuthError::Transport {
            stage: call.stage,
            origin: call.origin(),
            reason: format!("response body could not be read: {error}"),
        })?;
    if raw.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(call.malformed(format!(
            "response is larger than {MAX_RESPONSE_BYTES} bytes"
        )));
    }
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// Build the error for a non-success OAuth response.
///
/// RFC 6749 section 5.2 puts a machine-readable `error` and a free-form
/// `error_description` in the body; both are sanitized, and the free-form
/// one is what routinely quotes the parameter that was rejected.
fn endpoint_error(call: Call<'_>, status: StatusCode, body: &str) -> OAuthError {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|json| json.get("error"))
        .and_then(Value::as_str)
        .map(|text| sanitize(text, call.secrets, MAX_CODE_CHARS))
        .filter(|text| !text.is_empty());
    let description = parsed
        .as_ref()
        .and_then(|json| {
            json.get("error_description")
                .or_else(|| json.get("message"))
        })
        .and_then(Value::as_str)
        .map(|text| sanitize(text, call.secrets, MAX_DETAIL_CHARS))
        .filter(|text| !text.is_empty());
    OAuthError::Endpoint {
        stage: call.stage,
        origin: call.origin(),
        status: status.as_u16(),
        detail: join_detail(code.as_deref(), description.as_deref()),
    }
}

/// Compose the `: code - description` suffix of an endpoint error.
fn join_detail(code: Option<&str>, description: Option<&str>) -> String {
    match (code, description) {
        (Some(code), Some(text)) if code != text => format!(": {code} - {text}"),
        (Some(code), _) => format!(": {code}"),
        (None, Some(text)) => format!(": {text}"),
        (None, None) => String::new(),
    }
}

/// Run server text through the hygiene pipeline once per secret it may
/// have echoed back.
///
/// Applying the pipeline repeatedly is what makes a multi-secret request
/// safe: each pass redacts exact occurrences and discards the text
/// wholesale if that secret is recoverable from it.
pub fn sanitize(text: &str, secrets: &[&str], cap: usize) -> String {
    secrets.iter().fold(text.to_string(), |acc, secret| {
        clean_server_text(&acc, secret, false, cap)
    })
}

/// Categorize a transport failure without retaining the URL.
fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "request timed out".to_string()
    } else if error.is_connect() {
        "connection failed (DNS, refused, or TLS)".to_string()
    } else if error.is_redirect() {
        "redirect policy violated".to_string()
    } else if error.is_body() || error.is_decode() {
        "body transfer failed".to_string()
    } else {
        "transport error".to_string()
    }
}

/// A required string field of a response document.
pub fn require_str(value: &Value, field: &str, call: Call<'_>) -> Result<String, OAuthError> {
    optional_str(value, field).ok_or_else(|| {
        call.malformed(format!(
            "required field {field:?} is missing or not a string"
        ))
    })
}

/// An optional string field of a response document, blanks treated as
/// absent.
pub fn optional_str(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// The origin (`scheme://host[:port]`) of a URL, or a neutral label.
///
/// The only URL-derived form safe to display: paths and queries of an
/// endpoint URL can carry per-tenant identifiers.
pub fn origin_of(url: &str) -> String {
    engine::base_url_origin(url).unwrap_or_else(|| UNKNOWN_ORIGIN.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_error_redacts_every_secret_the_request_carried() {
        let call = Call {
            stage: Stage::TokenExchange,
            url: "https://docs.example.com/oauth/token",
            secrets: &["auth-code-SECRET", "verifier-SECRET"],
        };
        let body = r#"{"error":"invalid_grant",
            "error_description":"code auth-code-SECRET does not match verifier-SECRET"}"#;
        let rendered = endpoint_error(call, StatusCode::BAD_REQUEST, body).to_string();
        assert!(
            !rendered.contains("SECRET"),
            "secret leaked through an OAuth error: {rendered}"
        );
        assert!(rendered.contains("invalid_grant"), "{rendered}");
        assert!(rendered.contains("HTTP 400"), "{rendered}");
    }

    #[test]
    fn endpoint_error_survives_a_non_json_body() {
        let call = Call {
            stage: Stage::Refresh,
            url: "https://docs.example.com/oauth/token",
            secrets: &["refresh-SECRET"],
        };
        let rendered =
            endpoint_error(call, StatusCode::BAD_GATEWAY, "<html>bad gateway</html>").to_string();
        assert!(rendered.contains("HTTP 502"), "{rendered}");
        assert!(rendered.contains("https://docs.example.com"), "{rendered}");
    }

    #[test]
    fn endpoint_error_does_not_repeat_a_code_that_is_also_the_description() {
        let call = Call {
            stage: Stage::Revocation,
            url: "https://docs.example.com/oauth/revoke",
            secrets: &[],
        };
        let body = r#"{"error":"invalid_request","error_description":"invalid_request"}"#;
        let rendered = endpoint_error(call, StatusCode::BAD_REQUEST, body).to_string();
        assert_eq!(rendered.matches("invalid_request").count(), 1, "{rendered}");
    }

    #[test]
    fn origin_of_hides_the_endpoint_path() {
        assert_eq!(
            origin_of("https://docs.example.com/oauth/token"),
            "https://docs.example.com"
        );
        assert_eq!(origin_of("not a url"), UNKNOWN_ORIGIN);
    }

    #[test]
    fn optional_str_treats_blank_as_absent() {
        let value = serde_json::json!({ "a": "  ", "b": " x ", "c": 1 });
        assert_eq!(optional_str(&value, "a"), None);
        assert_eq!(optional_str(&value, "b").as_deref(), Some("x"));
        assert_eq!(optional_str(&value, "c"), None);
        assert_eq!(optional_str(&value, "missing"), None);
    }
}
