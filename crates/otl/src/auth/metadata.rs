//! Authorization-server metadata discovery (RFC 8414).
//!
//! Every endpoint the login flow uses comes from
//! `/.well-known/oauth-authorization-server` on the instance itself - none
//! is hard-coded, so a self-hosted Outline that moves its routes keeps
//! working.
//!
//! Discovered endpoints are required to sit on the SAME ORIGIN as the
//! instance. That is a deliberate restriction, not an oversight: the
//! authorization code, the PKCE verifier and the refresh token are all
//! posted to the token endpoint, so a metadata document that pointed it at
//! another host would hand those to a server the user never named. Outline
//! serves its own OAuth endpoints, so the restriction costs nothing and
//! turns a tampered metadata document from a credential leak into a clear
//! refusal.

use reqwest::blocking::Client;
use serde_json::Value;

use crate::auth::endpoint::{self, Call};
use crate::auth::error::{OAuthError, Stage};

/// Well-known path of the authorization-server metadata document.
pub const METADATA_PATH: &str = "/.well-known/oauth-authorization-server";

/// PKCE code challenge method this CLI uses. `plain` is never acceptable.
pub const CODE_CHALLENGE_METHOD: &str = "S256";

/// Scopes requested by default. The test instance advertises exactly these
/// two global scopes.
pub const DEFAULT_SCOPE: &str = "read write";

/// What the instance advertises about its OAuth endpoints.
#[derive(Debug, Clone)]
pub struct Metadata {
    /// Issuer identifier, as advertised.
    pub issuer: Option<String>,
    /// Where the browser is sent for consent.
    pub authorization_endpoint: String,
    /// Where codes and refresh tokens are exchanged.
    pub token_endpoint: String,
    /// RFC 7591 dynamic client registration, when offered.
    pub registration_endpoint: Option<String>,
    /// RFC 7009 token revocation, when offered.
    pub revocation_endpoint: Option<String>,
    /// Scopes the server says it supports, when advertised.
    pub scopes_supported: Vec<String>,
    /// Whether the server advertises PKCE `S256`.
    pub supports_s256: bool,
}

/// Fetch and validate the metadata document for `base_url`.
pub fn discover(http: &Client, base_url: &str) -> Result<Metadata, OAuthError> {
    let origin = endpoint::origin_of(base_url);
    let url = format!("{}{METADATA_PATH}", base_url.trim_end_matches('/'));
    let call = Call {
        stage: Stage::Discovery,
        url: &url,
        secrets: &[],
    };
    let document = endpoint::get_json(http, call)?;
    build(&document, &origin, call)
}

/// Turn a fetched document into validated [`Metadata`].
fn build(document: &Value, origin: &str, call: Call<'_>) -> Result<Metadata, OAuthError> {
    let authorization_endpoint = same_origin(
        endpoint::require_str(document, "authorization_endpoint", call)?,
        origin,
        "authorization_endpoint",
    )?;
    let token_endpoint = same_origin(
        endpoint::require_str(document, "token_endpoint", call)?,
        origin,
        "token_endpoint",
    )?;
    let registration_endpoint = optional_same_origin(
        endpoint::optional_str(document, "registration_endpoint"),
        origin,
        "registration_endpoint",
    )?;
    let revocation_endpoint = optional_same_origin(
        endpoint::optional_str(document, "revocation_endpoint"),
        origin,
        "revocation_endpoint",
    )?;
    Ok(Metadata {
        issuer: endpoint::optional_str(document, "issuer"),
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        revocation_endpoint,
        scopes_supported: string_list(document, "scopes_supported"),
        supports_s256: string_list(document, "code_challenge_methods_supported")
            .iter()
            .any(|method| method == CODE_CHALLENGE_METHOD),
    })
}

/// Require an advertised endpoint to live on the instance's own origin.
fn same_origin(url: String, origin: &str, endpoint: &'static str) -> Result<String, OAuthError> {
    if endpoint::origin_of(&url) == origin {
        return Ok(url);
    }
    Err(OAuthError::ForeignEndpoint {
        origin: origin.to_string(),
        endpoint,
    })
}

/// [`same_origin`] for an endpoint the server may legitimately omit.
fn optional_same_origin(
    url: Option<String>,
    origin: &str,
    endpoint: &'static str,
) -> Result<Option<String>, OAuthError> {
    url.map(|url| same_origin(url, origin, endpoint))
        .transpose()
}

/// A JSON array of strings, or an empty list.
fn string_list(document: &Value, field: &str) -> Vec<String> {
    document
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    const ORIGIN: &str = "https://docs.example.com";

    fn call() -> Call<'static> {
        Call {
            stage: Stage::Discovery,
            url: "https://docs.example.com/.well-known/oauth-authorization-server",
            secrets: &[],
        }
    }

    fn full_document() -> Value {
        json!({
            "issuer": ORIGIN,
            "authorization_endpoint": "https://docs.example.com/oauth/authorize",
            "token_endpoint": "https://docs.example.com/oauth/token",
            "registration_endpoint": "https://docs.example.com/oauth/register",
            "revocation_endpoint": "https://docs.example.com/oauth/revoke",
            "scopes_supported": ["read", "write"],
            "code_challenge_methods_supported": ["S256"]
        })
    }

    #[test]
    fn a_complete_document_is_accepted() {
        let metadata = build(&full_document(), ORIGIN, call()).expect("valid metadata");
        assert!(metadata.supports_s256);
        assert_eq!(metadata.scopes_supported, vec!["read", "write"]);
        assert_eq!(
            metadata.registration_endpoint.as_deref(),
            Some("https://docs.example.com/oauth/register")
        );
    }

    #[test]
    fn a_missing_token_endpoint_is_refused() {
        let mut document = full_document();
        document
            .as_object_mut()
            .map(|map| map.remove("token_endpoint"));
        let error = build(&document, ORIGIN, call()).expect_err("token_endpoint is required");
        assert!(error.to_string().contains("token_endpoint"), "{error}");
    }

    #[test]
    fn an_off_origin_token_endpoint_is_refused_rather_than_followed() {
        let mut document = full_document();
        document["token_endpoint"] = json!("https://evil.example.net/oauth/token");
        let error = build(&document, ORIGIN, call()).expect_err("an off-origin endpoint is unsafe");
        let text = error.to_string();
        assert!(text.contains("token_endpoint"), "{text}");
        assert!(text.contains("different host"), "{text}");
    }

    #[test]
    fn an_off_origin_optional_endpoint_is_refused_too() {
        for field in ["registration_endpoint", "revocation_endpoint"] {
            let mut document = full_document();
            document[field] = json!("https://evil.example.net/x");
            assert!(
                build(&document, ORIGIN, call()).is_err(),
                "{field} was accepted off-origin"
            );
        }
    }

    #[test]
    fn a_different_port_counts_as_a_different_origin() {
        let mut document = full_document();
        document["token_endpoint"] = json!("https://docs.example.com:8443/oauth/token");
        assert!(build(&document, ORIGIN, call()).is_err());
    }

    #[test]
    fn a_server_without_s256_is_reported_as_such() {
        let mut document = full_document();
        document["code_challenge_methods_supported"] = json!(["plain"]);
        let metadata = build(&document, ORIGIN, call()).expect("still parseable");
        assert!(!metadata.supports_s256);
    }

    #[test]
    fn optional_endpoints_may_be_absent() {
        let document = json!({
            "authorization_endpoint": "https://docs.example.com/oauth/authorize",
            "token_endpoint": "https://docs.example.com/oauth/token"
        });
        let metadata = build(&document, ORIGIN, call()).expect("minimal metadata is valid");
        assert!(metadata.registration_endpoint.is_none());
        assert!(metadata.revocation_endpoint.is_none());
        assert!(!metadata.supports_s256);
    }
}
