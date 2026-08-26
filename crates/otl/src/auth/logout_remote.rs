//! What `otl auth logout` asks the SERVER to do: revoke the tokens, and
//! with `--purge` delete the application `otl` registered for itself.
//!
//! Split out of [`crate::auth::logout`], which owns the flow and the local
//! removal. Both halves obey the same two rules:
//!
//! - Every request is anchored to the origin the CREDENTIAL recorded for
//!   itself - a session's token endpoint, a registration's instance - never
//!   to `OUTLINE_URL`. Anchoring on the environment refused to revoke A's
//!   tokens because the shell pointed at B, while the local half deleted
//!   them anyway.
//! - A failure is classified by whether a retry could ever succeed
//!   ([`OAuthError::is_permanent`]), because that decides both the wording
//!   and whether the caller keeps the credentials for another attempt.

use reqwest::blocking::Client as HttpClient;

use crate::auth::credentials::{ClientRegistration, OAuthSession};
use crate::auth::error::OAuthError;
use crate::auth::logout::Report;
use crate::auth::oauth::ClientAuth;
use crate::auth::{dcr, oauth, transport};

/// Ask the server to revoke the stored tokens.
///
/// Anchored to the session's OWN origin - the token endpoint it recorded at
/// login - not to `OUTLINE_URL`. That is the same anchor `dcr::delete` uses
/// for a registration, and it is the correct one: what the same-origin
/// check defends against is a tampered credential file, and the baseline
/// for that is the credential's self-recorded issuer, not an environment
/// variable the user can point anywhere.
///
/// Both tokens are offered: revoking the refresh token is what actually
/// ends the session, and revoking the access token closes the remaining
/// window.
pub fn revoke_tokens(
    http: &HttpClient,
    session: &OAuthSession,
    registration: Option<&ClientRegistration>,
    profile: &str,
    report: &mut Report,
) {
    let endpoint_url = match usable_revocation_endpoint(session, profile) {
        Ok(Some(url)) => url,
        // Nothing to retry: this instance cannot revoke, or the endpoint it
        // recorded may not be used. Say so and let removal proceed - waiting
        // would never turn into a successful revocation.
        Ok(None) => return report.unrevocable(NO_REVOCATION_ENDPOINT.to_string()),
        Err(error) => return report.unrevocable(error.to_string()),
    };
    let client = ClientAuth {
        client_id: &session.client_id,
        client_secret: registration.and_then(|reg| reg.client_secret.as_deref()),
    };
    let (revoked_any, failures) = revoke_each(http, session, client, endpoint_url);
    report.revoked = revoked_any && failures.is_empty();
    for (failure, permanent) in failures {
        if permanent {
            // The server rejected the credential itself, so no retry can
            // revoke these tokens; keeping them buys nothing.
            report.unrevocable(failure);
        } else {
            // The endpoint exists and answered badly: a later attempt can
            // still work, so this run must not destroy the only copy.
            report.retryable(failure);
        }
    }
}

/// Revoke both tokens, collecting each failure and whether it is permanent.
///
/// The refresh token goes first: it is the root of the session, so if only
/// one revocation gets through, that is the one worth having.
fn revoke_each(
    http: &HttpClient,
    session: &OAuthSession,
    client: ClientAuth<'_>,
    endpoint_url: &str,
) -> (bool, Vec<(String, bool)>) {
    let mut failures = Vec::new();
    let mut revoked_any = false;
    for token in [
        session.refresh_token.as_deref(),
        Some(session.access_token.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        match oauth::revoke(http, endpoint_url, client, token) {
            Ok(()) => revoked_any = true,
            Err(error) => {
                let permanent = error.is_permanent();
                let remedy = if permanent {
                    "; retrying will not help, so run `otl auth login` to \
                     re-authenticate"
                } else {
                    ""
                };
                failures.push((
                    format!("a token could not be revoked ({error}){remedy}"),
                    permanent,
                ));
            }
        }
    }
    (revoked_any, failures)
}

/// Notice printed when the instance offers no way to revoke at all.
const NO_REVOCATION_ENDPOINT: &str =
    "this instance advertises no token revocation endpoint, so the \
     tokens cannot be revoked and stay valid until they expire";

/// The revocation endpoint, if there is a usable one.
///
/// `Ok(None)` means the instance never advertised one. `Err` means it
/// advertised one that may not be used - a plaintext URL, or one that does
/// not belong to the instance that issued this session.
fn usable_revocation_endpoint<'a>(
    session: &'a OAuthSession,
    profile: &str,
) -> Result<Option<&'a str>, OAuthError> {
    let Some(url) = session.revocation_endpoint.as_deref() else {
        return Ok(None);
    };
    transport::require_stored_secure(url, profile, "OAuth revocation endpoint")?;
    let issuer = session_origin(session);
    transport::require_same_origin(url, &issuer, profile, "OAuth revocation endpoint")?;
    Ok(Some(url))
}

/// The origin that issued this session, from the token endpoint it recorded.
fn session_origin(session: &OAuthSession) -> String {
    crate::auth::endpoint::origin_of(&session.token_endpoint)
}

/// Delete the dynamic client registration from the server.
pub fn purge_registration(
    http: &HttpClient,
    registration: Option<&ClientRegistration>,
    report: &mut Report,
) {
    let Some(registration) = registration else {
        return;
    };
    if !registration.dynamic {
        report.warnings.push(
            "the client id for this profile was created by an administrator, \
             so --purge left it alone; only otl's own registrations are deleted"
                .to_string(),
        );
        return;
    }
    match dcr::delete(http, registration) {
        Ok(true) => report.registration_deleted = true,
        // No management credential was ever issued: no retry can change
        // that, so keeping the local record buys nothing.
        Ok(false) => report.unrevocable(
            "the stored registration has no management token, so the \
             application cannot be deleted from the server; ask an admin to \
             remove it (Settings -> Applications)"
                .to_string(),
        ),
        // Permanently impossible: the stored management URI is plaintext or
        // points at another instance, or the server rejected the management
        // token outright. Saying "retry" here would be false - the same
        // local rule refuses the same stored value every time.
        Err(error) if error.is_permanent() => report.unrevocable(format!(
            "the application cannot be deleted from the server ({error}). \
             Retrying will not help; run `otl auth login` to re-discover \
             this instance's endpoints, or `otl auth logout --purge --force` \
             to discard the local record and ask an admin to remove the \
             application"
        )),
        // The server refused transiently or was unreachable: a later
        // attempt can work, and the management token is what makes it
        // possible.
        Err(error) => report.retryable(format!(
            "the application could not be deleted from the server ({error}); \
             `otl auth logout --purge` can be retried"
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const ORIGIN: &str = "https://docs.example.com";

    fn session() -> OAuthSession {
        OAuthSession {
            access_token: "at".to_string(),
            refresh_token: Some("rt".to_string()),
            expires_at: None,
            scope: Some("read write".to_string()),
            client_id: "c".to_string(),
            token_endpoint: format!("{ORIGIN}/oauth/token"),
            revocation_endpoint: None,
            account: None,
            workspace: None,
        }
    }
    #[test]
    fn a_plaintext_revocation_endpoint_is_refused_at_use_time() {
        // The endpoint comes off disk and is about to receive both tokens.
        let mut stored = session();
        stored.revocation_endpoint = Some("http://docs.example.com/oauth/revoke".to_string());
        let error = usable_revocation_endpoint(&stored, "default")
            .expect_err("a plaintext stored endpoint must be refused");
        let text = error.to_string();
        assert!(text.contains("revocation endpoint"), "{text}");
        assert!(text.contains("otl auth login"), "{text}");
    }
    #[test]
    fn a_revocation_endpoint_on_another_host_is_refused_at_use_time() {
        let mut stored = session();
        stored.revocation_endpoint = Some("https://evil.example.net/oauth/revoke".to_string());
        let error = usable_revocation_endpoint(&stored, "default")
            .expect_err("an off-origin stored endpoint must be refused");
        // Anchored to the SESSION's own issuer, not to any ambient URL.
        assert!(error.to_string().contains(ORIGIN), "{error}");
    }
    #[test]
    fn revocation_is_anchored_to_the_session_not_to_the_environment() {
        // The R3 finding: anchoring on OUTLINE_URL refused to revoke A's
        // tokens because the shell pointed at B - while still deleting them
        // locally. The session records its own issuer; that is the anchor.
        let stored = OAuthSession {
            revocation_endpoint: Some(format!("{ORIGIN}/oauth/revoke")),
            ..session()
        };
        assert_eq!(session_origin(&stored), ORIGIN);
        let usable = usable_revocation_endpoint(&stored, "default")
            .expect("the session's own endpoint must be usable");
        assert_eq!(usable, Some(&*format!("{ORIGIN}/oauth/revoke")));
    }
}
