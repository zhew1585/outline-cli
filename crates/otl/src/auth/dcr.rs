//! Dynamic client registration (RFC 7591) and its removal (RFC 7592).
//!
//! Preferred over asking the user to find an administrator: a self-hosted
//! Outline with the MCP preference on lets `otl` register itself as a
//! public client and be usable immediately.
//!
//! The critical part is the CLEANUP contract. A dynamically registered
//! client cannot be deleted from the Outline admin UI - the only way to
//! remove it is RFC 7592 with the `registration_access_token` the
//! registration response carried. That token is therefore persisted with
//! the registration (in the credential file, since it is a credential),
//! and `otl auth logout --purge` uses it. Losing it means leaving an
//! orphan client on the server permanently.

use serde_json::{json, Value};

use reqwest::blocking::Client;

use crate::auth::credentials::ClientRegistration;
use crate::auth::endpoint::{self, Call};
use crate::auth::error::{OAuthError, Stage};
use crate::auth::transport;

/// Client name shown on the consent screen and in the admin UI.
pub const CLIENT_NAME: &str = "outline-cli (otl)";

/// Homepage advertised with the registration, so an administrator looking
/// at the client list can tell what it is.
///
/// This is sent to the instance as the RFC 7591 `client_uri` and shown in
/// Settings -> Applications, so pointing it at the wrong repository is
/// user-visible and stays visible until the client is deleted.
pub const CLIENT_URI: &str = "https://github.com/zhew1585/outline-cli";

/// Token endpoint auth method for a public client: none.
const AUTH_METHOD_NONE: &str = "none";

/// Register `otl` as a public client with the exact redirect URI given.
///
/// The redirect URI must already be bound: registering a port that is then
/// taken by something else would produce a client that can never complete
/// a login.
pub fn register(
    http: &Client,
    registration_endpoint: &str,
    redirect_uri: &str,
    origin: &str,
) -> Result<ClientRegistration, OAuthError> {
    let call = Call {
        stage: Stage::Registration,
        url: registration_endpoint,
        secrets: &[],
    };
    let request = json!({
        "client_name": CLIENT_NAME,
        "client_uri": CLIENT_URI,
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": AUTH_METHOD_NONE,
    });
    let response = endpoint::post_json(http, call, &request)?;
    build(&response, redirect_uri, origin, call)
}

/// Turn a registration response into the record that gets persisted.
fn build(
    response: &Value,
    redirect_uri: &str,
    origin: &str,
    call: Call<'_>,
) -> Result<ClientRegistration, OAuthError> {
    Ok(ClientRegistration {
        client_id: endpoint::require_str(response, "client_id", call)?,
        client_secret: endpoint::optional_str(response, "client_secret"),
        // Not required by the RFC, but without it the client can never be
        // deleted; its absence is worth surfacing rather than hiding.
        registration_access_token: endpoint::optional_str(response, "registration_access_token"),
        registration_client_uri: same_origin_uri(
            endpoint::optional_str(response, "registration_client_uri"),
            origin,
        )?,
        redirect_uri: redirect_uri.to_string(),
        dynamic: true,
        origin: Some(origin.to_string()),
    })
}

/// Require the management URI to stay on the instance's own origin.
///
/// It is stored and later used with a bearer credential attached, so a
/// response that pointed it elsewhere would be a way to harvest the
/// registration access token.
fn same_origin_uri(uri: Option<String>, origin: &str) -> Result<Option<String>, OAuthError> {
    match uri {
        Some(uri) if endpoint::origin_of(&uri) != origin => Err(OAuthError::ForeignEndpoint {
            origin: origin.to_string(),
            endpoint: "registration_client_uri",
        }),
        other => Ok(other),
    }
}

/// Delete a dynamic registration from the server (RFC 7592).
///
/// Returns `Ok(false)` when the registration cannot be deleted because the
/// server never issued the credentials for it - the caller then reports
/// that the client will linger, instead of pretending it is gone.
pub fn delete(http: &Client, registration: &ClientRegistration) -> Result<bool, OAuthError> {
    let (Some(uri), Some(token)) = (
        registration.registration_client_uri.as_deref(),
        registration.registration_access_token.as_deref(),
    ) else {
        return Ok(false);
    };
    // Re-validated at USE time, not trusted because a past registration
    // response contained it: this request carries the management token as a
    // bearer credential, and the URI comes off disk.
    transport::require_secure(uri, "the stored client management URI")?;
    if let Some(origin) = registration.origin.as_deref() {
        if endpoint::origin_of(uri) != origin {
            return Err(OAuthError::ForeignEndpoint {
                origin: origin.to_string(),
                endpoint: "registration_client_uri",
            });
        }
    }
    let call = Call {
        stage: Stage::Deregistration,
        url: uri,
        secrets: &[token, registration.client_id.as_str()],
    };
    match endpoint::delete_authorized(http, call, token) {
        Ok(()) => Ok(true),
        // Already gone on the server: the goal is met either way, and
        // failing here would leave the local record behind for no reason.
        Err(error) if error.is_not_found() => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const ORIGIN: &str = "https://docs.example.com";
    const REDIRECT: &str = "http://127.0.0.1:41234/callback";

    fn call() -> Call<'static> {
        Call {
            stage: Stage::Registration,
            url: "https://docs.example.com/oauth/register",
            secrets: &[],
        }
    }

    #[test]
    fn the_advertised_client_uri_names_this_project() {
        // Sent as RFC 7591 `client_uri` and displayed in the instance's
        // Settings -> Applications list, so a broken edit here is visible
        // to every administrator of every instance otl registers with.
        let uri = reqwest::Url::parse(CLIENT_URI).expect("client_uri must be a valid URL");
        assert_eq!(uri.scheme(), "https");
        assert_eq!(uri.host_str(), Some("github.com"));
        assert_eq!(uri.path(), "/zhew1585/outline-cli");
    }

    #[test]
    fn a_registration_response_keeps_the_management_credentials() {
        let response = json!({
            "client_id": "dcr-client-1",
            "registration_access_token": "rat-1",
            "registration_client_uri": "https://docs.example.com/oauth/clients/dcr-client-1"
        });
        let registration = build(&response, REDIRECT, ORIGIN, call()).expect("valid response");
        assert_eq!(registration.client_id, "dcr-client-1");
        assert_eq!(
            registration.registration_access_token.as_deref(),
            Some("rat-1"),
            "losing this token makes the client undeletable"
        );
        assert!(registration.dynamic);
        assert_eq!(registration.redirect_uri, REDIRECT);
        assert_eq!(registration.origin.as_deref(), Some(ORIGIN));
    }

    #[test]
    fn a_registration_without_a_client_id_is_refused() {
        let error = build(
            &json!({ "registration_access_token": "x" }),
            REDIRECT,
            ORIGIN,
            call(),
        )
        .expect_err("client_id is required");
        assert!(error.to_string().contains("client_id"), "{error}");
    }

    #[test]
    fn an_off_origin_management_uri_is_refused() {
        let response = json!({
            "client_id": "c",
            "registration_access_token": "rat",
            "registration_client_uri": "https://evil.example.net/clients/c"
        });
        let error = build(&response, REDIRECT, ORIGIN, call())
            .expect_err("an off-origin management URI would leak the token");
        assert!(
            error.to_string().contains("registration_client_uri"),
            "{error}"
        );
    }

    #[test]
    fn a_public_client_registration_has_no_secret() {
        let registration =
            build(&json!({ "client_id": "c" }), REDIRECT, ORIGIN, call()).expect("valid");
        assert!(registration.client_secret.is_none());
        assert!(registration.registration_access_token.is_none());
        assert!(registration.registration_client_uri.is_none());
    }

    #[test]
    fn deletion_reports_impossible_when_the_server_issued_no_management_credentials() {
        // No network client is needed: the guard must trip before any
        // request is attempted.
        let http = endpoint::http_client().expect("http client");
        let registration = ClientRegistration {
            client_id: "c".to_string(),
            client_secret: None,
            registration_access_token: None,
            registration_client_uri: None,
            redirect_uri: REDIRECT.to_string(),
            dynamic: true,
            origin: Some(ORIGIN.to_string()),
        };
        assert!(
            !delete(&http, &registration).unwrap(),
            "a registration with no management credentials cannot be deleted"
        );
    }
}
