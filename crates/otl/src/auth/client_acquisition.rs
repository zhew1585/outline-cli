//! Choosing the OAuth client a login speaks as, and binding its callback
//! port.
//!
//! Split out of [`crate::auth::login`], which owns the flow itself. Three
//! sources, in order of preference: a client id an administrator supplied,
//! one already recorded for this profile and instance, or one `otl`
//! registers for itself.
//!
//! The ordering constraint that shapes all of it: a dynamically registered
//! client is pinned to ONE exact redirect URI, so the port is bound BEFORE
//! the registration is created - registering a port that then turns out to
//! be taken would produce a client that can never complete a login - and a
//! registration that can no longer be used must be deleted from the server
//! BEFORE a replacement is created, because creating one overwrites the
//! only credential that could ever delete it.

use reqwest::blocking::Client as HttpClient;

use crate::auth::credentials::{ClientRegistration, CredentialFile};
use crate::auth::error::OAuthError;
use crate::auth::login::{display_client_id, Acquired, ClientSource, Options};
use crate::auth::loopback::{self, CallbackServer};
use crate::auth::metadata::Metadata;
use crate::auth::{dcr, AuthError};
use crate::stdio;

/// Pick the OAuth client to use and bind its callback port.
pub fn acquire_client(
    http: &HttpClient,
    metadata: &Metadata,
    origin: &str,
    file: &CredentialFile,
    profile: &str,
    options: &Options,
) -> Result<Acquired, AuthError> {
    if let Some(client_id) = &options.client_id {
        let server = CallbackServer::bind_fixed()?;
        return Ok(Acquired {
            registration: administered(client_id, server.redirect_uri(), origin),
            server,
            source: ClientSource::Provided,
        });
    }
    if let Some(cached) = cached_for(file, profile, origin) {
        match rebind(&cached) {
            Some(server) => {
                let mut registration = cached;
                registration.redirect_uri = server.redirect_uri().to_string();
                return Ok(Acquired {
                    registration,
                    server,
                    source: ClientSource::Cached,
                });
            }
            // A dynamic client is pinned to its exact redirect URI, so a
            // port we can no longer bind makes the registration unusable.
            // It has to come off the server BEFORE a replacement is
            // created, because creating one overwrites the only credential
            // that could ever delete it.
            None => retire(http, &cached, options.force_new_client)?,
        }
    }
    register_new(http, metadata, origin)
}

/// The registration recorded for this profile and instance, if reusable.
fn cached_for(file: &CredentialFile, profile: &str, origin: &str) -> Option<ClientRegistration> {
    let cached = file.profile(profile)?.client.clone()?;
    match cached.origin.as_deref() {
        Some(recorded) if recorded != origin => {
            stdio::write_diagnostic_line(&format!(
                "notice: the stored client registration for profile {profile:?} \
                 belongs to {recorded}, not {origin}; registering a new one. \
                 Run `otl auth logout --purge` against {recorded} to remove the \
                 old registration there."
            ));
            None
        }
        _ => Some(cached),
    }
}

/// Bind the callback port a cached registration needs.
fn rebind(cached: &ClientRegistration) -> Option<CallbackServer> {
    if !cached.dynamic {
        // An administrator registered every documented port, so any free
        // one will do.
        return CallbackServer::bind_fixed().ok();
    }
    let port = loopback::port_of(&cached.redirect_uri)?;
    CallbackServer::bind_port(port).ok()
}

/// Remove a dynamic registration that can no longer be used.
///
/// Registering a replacement overwrites the stored
/// `registration_access_token`, which is the ONLY way to delete the old
/// registration - Outline's admin UI cannot. So this must succeed before a
/// replacement is created, and a failure stops the flow instead of trading
/// a working login for a permanent orphan.
///
/// `forced` is the user saying, explicitly, that they accept the orphan.
fn retire(
    http: &HttpClient,
    registration: &ClientRegistration,
    forced: bool,
) -> Result<(), AuthError> {
    if !registration.dynamic {
        // Nothing on the server belongs to us; the local record is just a
        // cached client id.
        return Ok(());
    }
    let port = loopback::port_of(&registration.redirect_uri)
        .map(|port| port.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let failure = match dcr::delete(http, registration) {
        Ok(true) => {
            stdio::write_diagnostic_line(
                "notice: the stored client registration's callback port is no \
                 longer available; it has been removed from the server and \
                 will be replaced.",
            );
            return Ok(());
        }
        Ok(false) => "the server issued no management token for it".to_string(),
        Err(error) => error.to_string(),
    };
    if !forced {
        return Err(AuthError::OAuth(OAuthError::RetireFailed {
            port,
            reason: failure,
        }));
    }
    stdio::write_diagnostic_line(&format!(
        "warning: --force-new-client was given, so the old registration \
         (client {client}) is being abandoned rather than deleted \
         ({failure}). Ask an admin to remove it under Settings -> \
         Applications.",
        client = display_client_id(&registration.client_id)
    ));
    Ok(())
}

/// Register `otl` as a new public client.
fn register_new(
    http: &HttpClient,
    metadata: &Metadata,
    origin: &str,
) -> Result<Acquired, AuthError> {
    let Some(endpoint) = metadata.registration_endpoint.as_deref() else {
        return Err(AuthError::OAuth(unavailable()));
    };
    // Bind first, register the exact port second.
    let server = CallbackServer::bind_ephemeral()?;
    let registration = match dcr::register(http, endpoint, server.redirect_uri(), origin) {
        Ok(registration) => registration,
        Err(error) if error.is_not_found() => return Err(AuthError::OAuth(unavailable())),
        Err(error) => return Err(AuthError::OAuth(error)),
    };
    Ok(Acquired {
        registration,
        server,
        source: ClientSource::Registered,
    })
}

/// The fallback guidance when dynamic registration is not on offer.
fn unavailable() -> OAuthError {
    OAuthError::RegistrationUnavailable {
        redirect_uri: loopback::documented_redirect_uris().join("\n\x20 "),
    }
}

/// A client id an administrator created, recorded so a later login can
/// reuse it without the flag.
fn administered(client_id: &str, redirect_uri: &str, origin: &str) -> ClientRegistration {
    ClientRegistration {
        client_id: client_id.to_string(),
        client_secret: None,
        registration_access_token: None,
        registration_client_uri: None,
        redirect_uri: redirect_uri.to_string(),
        dynamic: false,
        origin: Some(origin.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_provided_client_id_is_recorded_as_not_dynamic() {
        let registration = administered(
            "admin-client",
            "http://127.0.0.1:8586/callback",
            "https://docs.example.com",
        );
        assert!(
            !registration.dynamic,
            "an administrator's client must never be deleted by --purge"
        );
        assert!(registration.registration_access_token.is_none());
        assert_eq!(
            registration.origin.as_deref(),
            Some("https://docs.example.com")
        );
    }

    #[test]
    fn a_cached_registration_for_another_instance_is_not_reused() {
        let mut file = CredentialFile::default();
        file.profile_mut("default").client = Some(administered(
            "c",
            "http://127.0.0.1:8586/callback",
            "https://other.example.com",
        ));
        assert!(
            cached_for(&file, "default", "https://docs.example.com").is_none(),
            "a client id from another instance must not be reused"
        );
        assert!(cached_for(&file, "default", "https://other.example.com").is_some());
    }

    #[test]
    fn the_dcr_fallback_lists_every_documented_redirect_uri() {
        let text = unavailable().to_string();
        for port in loopback::CALLBACK_PORTS {
            assert!(
                text.contains(&format!("127.0.0.1:{port}/callback")),
                "port {port} missing from the admin instructions: {text}"
            );
        }
    }
}
