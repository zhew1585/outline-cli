//! Token endpoint interactions: code exchange, refresh, revocation.
//!
//! `otl` is a PUBLIC client (no secret), so every request here carries
//! `client_id` and, on the exchange, the PKCE verifier. A server that
//! nevertheless issued a secret is honoured: it is sent when present.
//!
//! Everything in this module is a secret in flight. Each call declares the
//! values it sends so that [`crate::auth::endpoint`] can redact them from
//! anything the server says back - OAuth `error_description` text quotes
//! the rejected parameter more often than not.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use serde_json::Value;

use crate::auth::endpoint::{self, Call};
use crate::auth::error::{OAuthError, Stage};
use crate::auth::metadata::CODE_CHALLENGE_METHOD;

/// Grant type for the authorization-code exchange.
const GRANT_AUTHORIZATION_CODE: &str = "authorization_code";

/// Grant type for the refresh exchange.
const GRANT_REFRESH_TOKEN: &str = "refresh_token";

/// Lifetime assumed when a token response omits `expires_in`.
///
/// The test instance answers 3600s; assuming a short life is the safe
/// direction, because the worst case is one unnecessary refresh.
pub const ASSUMED_LIFETIME_SECONDS: i64 = 3600;

/// Which OAuth client the request authenticates as.
#[derive(Clone, Copy)]
pub struct ClientAuth<'a> {
    /// Client identifier.
    pub client_id: &'a str,
    /// Client secret, if the server issued one (public clients have none).
    pub client_secret: Option<&'a str>,
}

impl fmt::Debug for ClientAuth<'_> {
    /// Manual impl: a client id identifies a registration and a secret is
    /// a credential; neither belongs in Debug output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientAuth")
            .field("public", &self.client_secret.is_none())
            .finish()
    }
}

/// Tokens as returned by the token endpoint.
pub struct Tokens {
    /// The new access token.
    pub access_token: String,
    /// The new refresh token. Outline rotates it on every refresh, so a
    /// response that omits it means "keep using the old one".
    pub refresh_token: Option<String>,
    /// Absolute expiry, computed from `expires_in` at the moment the
    /// response was parsed.
    pub expires_at: Option<i64>,
    /// Granted scope, when the server states it.
    pub scope: Option<String>,
}

impl fmt::Debug for Tokens {
    /// Manual impl: tokens must never appear in Debug output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("rotated_refresh_token", &self.refresh_token.is_some())
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Build the authorization request URL the browser is sent to.
///
/// Query assembly goes through a real URL serializer, so a value with an
/// `&` or a `#` in it cannot smuggle extra parameters into the request.
pub fn authorization_url(
    authorization_endpoint: &str,
    client: ClientAuth<'_>,
    redirect_uri: &str,
    scope: &str,
    state: &str,
    code_challenge: &str,
) -> Result<String, OAuthError> {
    let mut url =
        reqwest::Url::parse(authorization_endpoint).map_err(|error| OAuthError::Malformed {
            stage: Stage::Discovery,
            origin: endpoint::origin_of(authorization_endpoint),
            reason: format!("authorization endpoint is not a valid URL ({error})"),
        })?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", scope)
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", CODE_CHALLENGE_METHOD);
    Ok(url.into())
}

/// Exchange an authorization code (plus PKCE verifier) for tokens.
pub fn exchange_code(
    http: &Client,
    token_endpoint: &str,
    client: ClientAuth<'_>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Tokens, OAuthError> {
    let secrets = secrets_of(client, &[code, verifier]);
    let call = Call {
        stage: Stage::TokenExchange,
        url: token_endpoint,
        secrets: &secrets,
    };
    let mut form = vec![
        ("grant_type", GRANT_AUTHORIZATION_CODE),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client.client_id),
        ("code_verifier", verifier),
    ];
    if let Some(secret) = client.client_secret {
        form.push(("client_secret", secret));
    }
    let response = endpoint::post_form(http, call, &form)?;
    parse_tokens(&response, call)
}

/// Exchange a refresh token for a new access token (and, on Outline, a
/// rotated refresh token).
pub fn refresh(
    http: &Client,
    token_endpoint: &str,
    client: ClientAuth<'_>,
    refresh_token: &str,
) -> Result<Tokens, OAuthError> {
    let secrets = secrets_of(client, &[refresh_token]);
    let call = Call {
        stage: Stage::Refresh,
        url: token_endpoint,
        secrets: &secrets,
    };
    let mut form = vec![
        ("grant_type", GRANT_REFRESH_TOKEN),
        ("refresh_token", refresh_token),
        ("client_id", client.client_id),
    ];
    if let Some(secret) = client.client_secret {
        form.push(("client_secret", secret));
    }
    let response = endpoint::post_form(http, call, &form)?;
    parse_tokens(&response, call)
}

/// Revoke one token (RFC 7009).
pub fn revoke(
    http: &Client,
    revocation_endpoint: &str,
    client: ClientAuth<'_>,
    token: &str,
) -> Result<(), OAuthError> {
    let secrets = secrets_of(client, &[token]);
    let call = Call {
        stage: Stage::Revocation,
        url: revocation_endpoint,
        secrets: &secrets,
    };
    let mut form = vec![("token", token), ("client_id", client.client_id)];
    if let Some(secret) = client.client_secret {
        form.push(("client_secret", secret));
    }
    endpoint::post_form(http, call, &form).map(|_| ())
}

/// Everything a request carries that must be redacted from server text.
fn secrets_of<'a>(client: ClientAuth<'a>, extra: &[&'a str]) -> Vec<&'a str> {
    let mut secrets = vec![client.client_id];
    secrets.extend(client.client_secret);
    secrets.extend_from_slice(extra);
    secrets
}

/// Parse a token endpoint response.
fn parse_tokens(response: &Value, call: Call<'_>) -> Result<Tokens, OAuthError> {
    Ok(Tokens {
        access_token: endpoint::require_str(response, "access_token", call)?,
        refresh_token: endpoint::optional_str(response, "refresh_token"),
        expires_at: expires_in(response).map(|seconds| now_unix() + seconds),
        scope: endpoint::optional_str(response, "scope"),
    })
}

/// The `expires_in` of a token response, in seconds.
///
/// Accepts a JSON number or a numeric string: both appear in the wild, and
/// a missing value falls back to [`ASSUMED_LIFETIME_SECONDS`] rather than
/// "never expires", so an absent field cannot turn into a token that is
/// never refreshed.
fn expires_in(response: &Value) -> Option<i64> {
    let raw = response.get("expires_in")?;
    let seconds = raw
        .as_i64()
        .or_else(|| raw.as_str().and_then(|text| text.trim().parse().ok()))
        .unwrap_or(ASSUMED_LIFETIME_SECONDS);
    Some(seconds.max(0))
}

/// Current time in Unix seconds, clamped at the epoch.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    fn call() -> Call<'static> {
        Call {
            stage: Stage::TokenExchange,
            url: "https://docs.example.com/oauth/token",
            secrets: &[],
        }
    }

    #[test]
    fn an_authorization_url_carries_every_required_parameter() {
        let url = authorization_url(
            "https://docs.example.com/oauth/authorize",
            ClientAuth {
                client_id: "client-1",
                client_secret: None,
            },
            "http://127.0.0.1:8586/callback",
            "read write",
            "state-1",
            "challenge-1",
        )
        .expect("valid endpoint");
        let parsed = reqwest::Url::parse(&url).unwrap();
        let pairs: std::collections::HashMap<_, _> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["client_id"], "client-1");
        assert_eq!(pairs["redirect_uri"], "http://127.0.0.1:8586/callback");
        assert_eq!(pairs["scope"], "read write");
        assert_eq!(pairs["state"], "state-1");
        assert_eq!(pairs["code_challenge"], "challenge-1");
        assert_eq!(pairs["code_challenge_method"], "S256");
    }

    #[test]
    fn authorization_url_parameters_cannot_smuggle_extra_query_keys() {
        let url = authorization_url(
            "https://docs.example.com/oauth/authorize",
            ClientAuth {
                client_id: "id&scope=admin",
                client_secret: None,
            },
            "http://127.0.0.1:8586/callback",
            "read",
            "s",
            "c",
        )
        .expect("valid endpoint");
        let parsed = reqwest::Url::parse(&url).unwrap();
        let scopes: Vec<_> = parsed
            .query_pairs()
            .filter(|(key, _)| key == "scope")
            .map(|(_, value)| value.into_owned())
            .collect();
        assert_eq!(scopes, vec!["read"], "an extra scope parameter got in");
    }

    #[test]
    fn a_token_response_becomes_an_absolute_expiry() {
        let before = now_unix();
        let tokens = parse_tokens(
            &json!({
                "access_token": "at",
                "refresh_token": "rt",
                "expires_in": 3600,
                "scope": "read write",
                "token_type": "Bearer"
            }),
            call(),
        )
        .expect("a complete token response");
        let expires_at = tokens.expires_at.expect("expiry");
        assert!(
            (before + 3600..=now_unix() + 3600).contains(&expires_at),
            "expiry {expires_at} is not now+3600"
        );
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt"));
        assert_eq!(tokens.scope.as_deref(), Some("read write"));
    }

    #[test]
    fn a_string_expires_in_is_accepted() {
        let tokens = parse_tokens(
            &json!({ "access_token": "at", "expires_in": "1800" }),
            call(),
        )
        .unwrap();
        let expires_at = tokens.expires_at.expect("expiry");
        assert!(expires_at >= now_unix() + 1700);
    }

    #[test]
    fn a_nonsense_expires_in_falls_back_to_the_assumed_lifetime() {
        let tokens = parse_tokens(
            &json!({ "access_token": "at", "expires_in": "soon" }),
            call(),
        )
        .unwrap();
        let expires_at = tokens.expires_at.expect("expiry");
        assert!(expires_at >= now_unix() + ASSUMED_LIFETIME_SECONDS - 5);
    }

    #[test]
    fn a_response_without_an_access_token_is_refused() {
        let error = parse_tokens(&json!({ "refresh_token": "rt" }), call())
            .expect_err("access_token is required");
        assert!(error.to_string().contains("access_token"), "{error}");
    }

    #[test]
    fn a_response_without_a_refresh_token_keeps_the_old_one_absent() {
        let tokens = parse_tokens(&json!({ "access_token": "at" }), call()).unwrap();
        assert!(tokens.refresh_token.is_none());
        assert!(tokens.expires_at.is_none());
    }

    #[test]
    fn debug_output_never_shows_tokens_or_client_credentials() {
        let tokens = parse_tokens(
            &json!({ "access_token": "at-SECRET", "refresh_token": "rt-SECRET" }),
            call(),
        )
        .unwrap();
        let client = ClientAuth {
            client_id: "client-SECRET",
            client_secret: Some("secret-SECRET"),
        };
        let rendered = format!("{tokens:?} {client:?}");
        assert!(!rendered.contains("SECRET"), "leak: {rendered}");
    }

    #[test]
    fn every_secret_a_request_carries_is_listed_for_redaction() {
        let client = ClientAuth {
            client_id: "cid",
            client_secret: Some("csecret"),
        };
        let secrets = secrets_of(client, &["code", "verifier"]);
        assert_eq!(secrets, vec!["cid", "csecret", "code", "verifier"]);

        let public = ClientAuth {
            client_id: "cid",
            client_secret: None,
        };
        assert_eq!(secrets_of(public, &["rt"]), vec!["cid", "rt"]);
    }
}
