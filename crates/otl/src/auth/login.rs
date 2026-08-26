//! The `otl auth login` flow: discovery, client acquisition, browser
//! consent, code exchange, storage.
//!
//! Ordering is load-bearing in two places:
//!
//! - the callback port is BOUND BEFORE a client is registered, because a
//!   dynamically registered client is pinned to one exact redirect URI and
//!   registering a port that then turns out to be taken would produce a
//!   client that can never complete a login;
//! - a new registration is PERSISTED BEFORE the browser step, because its
//!   `registration_access_token` is the only way to delete it again and a
//!   login abandoned at the consent screen must not leave an
//!   undeletable client behind on the server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use reqwest::blocking::Client as HttpClient;

use crate::auth::credentials::{ClientRegistration, CredentialFile, CredentialStore, OAuthSession};
use crate::auth::error::{OAuthError, Stage};
use crate::auth::loopback::{self, CallbackServer};
use crate::auth::metadata::{self, Metadata, CODE_CHALLENGE_METHOD, DEFAULT_SCOPE};
use crate::auth::oauth::{self, ClientAuth};
use crate::auth::source::CredentialProvider;
use crate::auth::{browser, dcr, endpoint, AuthError, Identity};
use crate::stdio;

/// Maximum characters kept from a server-supplied client id when it is
/// printed. Long enough for any real identifier, short enough that a
/// hostile one cannot flood the terminal.
const MAX_CLIENT_ID_CHARS: usize = 80;

/// What `otl auth login` was asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    /// Client id supplied by an administrator, if any.
    pub client_id: Option<String>,
    /// Scope to request.
    pub scope: String,
    /// How long to wait for the browser redirect.
    pub timeout: Duration,
    /// Whether to launch a browser (`--no-browser` prints the URL only).
    pub open_browser: bool,
    /// Abandon a stored registration that cannot be removed, accepting the
    /// orphan it leaves on the server.
    pub force_new_client: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            client_id: None,
            scope: DEFAULT_SCOPE.to_string(),
            timeout: loopback::AUTH_TIMEOUT,
            open_browser: true,
            force_new_client: false,
        }
    }
}

/// Where the OAuth client used for this login came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientSource {
    /// A client id given on the command line.
    Provided,
    /// A client already recorded for this profile and instance.
    Cached,
    /// A client `otl` just registered for itself.
    Registered,
}

impl ClientSource {
    /// Label for the success message.
    pub fn label(self) -> &'static str {
        match self {
            Self::Provided => "using the client id you supplied",
            Self::Cached => "reusing the client registration already stored for this instance",
            Self::Registered => "registered otl as a new application on this instance",
        }
    }
}

/// What a successful login produced.
#[derive(Debug)]
pub struct Outcome {
    /// Who the new tokens belong to, when the instance could be asked.
    pub identity: Option<Identity>,
    /// Scope the server actually granted.
    pub scope: Option<String>,
    /// Where the client came from.
    pub client_source: ClientSource,
    /// Path of the credential file the session was written to.
    pub credential_path: PathBuf,
}

/// The bound callback listener plus the client it belongs to.
struct Acquired {
    registration: ClientRegistration,
    server: CallbackServer,
    source: ClientSource,
}

/// Run the whole login flow.
pub fn run(
    base_url: &str,
    profile: &str,
    origin: &str,
    store: &CredentialStore,
    options: &Options,
) -> Result<Outcome, AuthError> {
    // Before ANY network work: if this profile already belongs to another
    // instance there is nothing to discover here, and asking would tell a
    // server we are not going to use that this user exists.
    let existing = store.load()?;
    crate::auth::ensure_bindable(existing.profile(profile), profile, origin)?;

    let http = endpoint::http_client()?;
    let metadata = metadata::discover(&http, base_url)?;
    require_s256(&metadata, base_url)?;
    warn_about_unsupported_scopes(&metadata, &options.scope);

    let acquired = acquire_client(&http, &metadata, origin, &existing, profile, options)?;
    if acquired.source == ClientSource::Registered {
        // Persist before the browser step: see the module docs. Under the
        // credential lock and against a freshly read file, so a concurrent
        // refresh cannot be clobbered by a stale snapshot.
        persist_registration(store, profile, origin, &acquired, &http)?;
    }

    let tokens = authorize(&http, &metadata, &acquired, options)?;
    let session = build_session(&metadata, &acquired.registration, tokens);
    let scope = session.scope.clone();
    let registration = acquired.registration.clone();
    let claimed = store.update(|file: &mut CredentialFile| -> Result<(), AuthError> {
        crate::auth::ensure_bindable(file.profile(profile), profile, origin)?;
        // The client on disk must still be the one these tokens were issued
        // for. A concurrent login that finished first owns the profile now,
        // and overwriting its registration would drop the only credential
        // that can ever delete the application it created.
        claim_client(file.profile(profile), &registration, profile)?;
        let entry = file.profile_mut(profile);
        // The binding is what stops these tokens being sent to another
        // instance later; it is written together with them, never after.
        entry.origin = Some(origin.to_string());
        entry.client = Some(registration.clone());
        entry.oauth = Some(session);
        Ok(())
    });
    if let Err(error) = claimed {
        return Err(abandon(&http, &acquired, error));
    }

    let identity = capture_identity(base_url, profile, origin, store);
    Ok(Outcome {
        identity,
        scope,
        client_source: acquired.source,
        credential_path: store.path().to_path_buf(),
    })
}

/// Refuse to continue without PKCE `S256`.
///
/// `plain` would put the verifier in the authorization request, which
/// travels through the browser - and this is a public client, so PKCE is
/// the only thing binding the code to this process.
fn require_s256(metadata: &Metadata, base_url: &str) -> Result<(), AuthError> {
    if metadata.supports_s256 {
        return Ok(());
    }
    Err(AuthError::OAuth(OAuthError::Malformed {
        stage: Stage::Discovery,
        origin: endpoint::origin_of(base_url),
        reason: format!(
            "the instance does not advertise PKCE {CODE_CHALLENGE_METHOD}, which \
             otl requires as a public client"
        ),
    }))
}

/// Warn when the instance does not list a requested scope.
fn warn_about_unsupported_scopes(metadata: &Metadata, scope: &str) {
    if metadata.scopes_supported.is_empty() {
        return;
    }
    let unknown: Vec<&str> = scope
        .split_whitespace()
        .filter(|wanted| {
            !metadata
                .scopes_supported
                .iter()
                .any(|known| known == wanted)
        })
        .collect();
    if unknown.is_empty() {
        return;
    }
    stdio::write_diagnostic_line(&format!(
        "warning: this instance does not list the requested scope(s) {}; \
         it advertises: {}",
        unknown.join(", "),
        metadata.scopes_supported.join(", ")
    ));
}

/// Record a brand-new dynamic registration, undoing it if that fails.
///
/// A registration that exists on the server but not on disk is the worst
/// outcome available: its `registration_access_token` is the only thing
/// that can ever delete it, and losing that leaves an application no one
/// can remove. So a failed save triggers a compensating RFC 7592 delete,
/// and if THAT fails too the orphan is reported loudly with the client id
/// an administrator needs to find it.
fn persist_registration(
    store: &CredentialStore,
    profile: &str,
    origin: &str,
    acquired: &Acquired,
    http: &HttpClient,
) -> Result<(), AuthError> {
    let registration = acquired.registration.clone();
    let saved = store.update(|file: &mut CredentialFile| -> Result<(), AuthError> {
        crate::auth::ensure_bindable(file.profile(profile), profile, origin)?;
        // A concurrent login registered its own client while this one was
        // talking to the server. Both registrations now exist there, but
        // only one management token fits on disk - so the loser gives up
        // and deletes its own, rather than overwriting the winner's and
        // stranding an application nobody can remove.
        claim_client(file.profile(profile), &registration, profile)?;
        let entry = file.profile_mut(profile);
        entry.origin = Some(origin.to_string());
        entry.client = Some(registration.clone());
        Ok(())
    });
    match saved {
        Ok(()) => Ok(()),
        Err(error) => Err(abandon(http, acquired, error)),
    }
}

/// Whether the client recorded on disk is still the one this login owns.
///
/// Absent is fine (we are about to write it). A DIFFERENT one means another
/// login won the race for this profile.
fn claim_client(
    entry: Option<&crate::auth::credentials::ProfileCredentials>,
    ours: &ClientRegistration,
    profile: &str,
) -> Result<(), AuthError> {
    let stored = entry.and_then(|entry| entry.client.as_ref());
    match stored {
        Some(other) if other.client_id != ours.client_id => {
            Err(AuthError::OAuth(OAuthError::ConcurrentLogin {
                profile: profile.to_string(),
                cleanup: String::new(),
            }))
        }
        _ => Ok(()),
    }
}

/// Give up on a login, removing anything it created on the server first.
///
/// Only a registration THIS login created is removed: a reused or
/// administrator-supplied client belongs to someone else. Exactly one
/// deletion is attempted, and whatever became of it is folded into the
/// reported error - so a server-side leftover is never silent.
fn abandon(http: &HttpClient, acquired: &Acquired, cause: AuthError) -> AuthError {
    if acquired.source != ClientSource::Registered {
        return cause;
    }
    let client_id = display_client_id(&acquired.registration.client_id);
    let removed = dcr::delete(http, &acquired.registration);
    let cleanup = match &removed {
        Ok(true) => "the registration it created has been removed from the server".to_string(),
        Ok(false) => format!(
            "client {client_id} cannot be removed automatically because the \
             server issued no management token for it"
        ),
        Err(error) => format!("client {client_id} could not be removed: {error}"),
    };
    // A concurrent login is a normal race, not a fault: report it as such.
    if let AuthError::OAuth(OAuthError::ConcurrentLogin { profile, .. }) = cause {
        return AuthError::OAuth(OAuthError::ConcurrentLogin {
            profile,
            cleanup: format!(" ({cleanup})"),
        });
    }
    // Anything else: if the registration really is gone, the original
    // failure is the useful message. If it is not, an application is
    // stranded on the server and that outranks everything else.
    if matches!(removed, Ok(true)) {
        return cause;
    }
    AuthError::OAuth(OAuthError::OrphanedRegistration {
        origin: acquired
            .registration
            .origin
            .clone()
            .unwrap_or_else(|| "the instance".to_string()),
        client_id,
        reason: cause.to_string(),
        cleanup,
    })
}

/// Make a server-supplied client id safe to print.
///
/// A client id is not a secret - it travels through the browser in the
/// authorization URL, and an administrator needs it to find a stranded
/// application - but it IS server-controlled text arriving on a terminal.
/// Without this, a hostile registration endpoint could return a client id
/// containing newlines or escape sequences and forge diagnostic lines.
fn display_client_id(client_id: &str) -> String {
    engine::sanitize::clean_server_text(client_id, "", false, MAX_CLIENT_ID_CHARS)
}

/// Pick the OAuth client to use and bind its callback port.
fn acquire_client(
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

/// Send the user through the browser and exchange the resulting code.
fn authorize(
    http: &HttpClient,
    metadata: &Metadata,
    acquired: &Acquired,
    options: &Options,
) -> Result<oauth::Tokens, AuthError> {
    let pkce = crate::auth::pkce::Pkce::generate()?;
    let state = crate::auth::pkce::random_state()?;
    let client = client_auth(&acquired.registration);
    let url = oauth::authorization_url(
        &metadata.authorization_endpoint,
        client,
        acquired.server.redirect_uri(),
        &options.scope,
        &state,
        pkce.challenge(),
    )?;
    announce(&url, options.open_browser);
    let code = acquired.server.wait_for_code(&state, options.timeout)?;
    let tokens = oauth::exchange_code(
        http,
        &metadata.token_endpoint,
        client,
        &code,
        pkce.verifier(),
        acquired.server.redirect_uri(),
    )?;
    Ok(tokens)
}

/// Tell the user what is happening. Diagnostics and prompts are stderr;
/// stdout stays reserved for data.
fn announce(url: &str, open_browser: bool) {
    if open_browser {
        if let Err(error) = browser::open(url) {
            stdio::write_diagnostic_line(&format!("notice: {error}"));
        } else {
            stdio::write_diagnostic_line("Opening your browser to sign in to Outline.");
        }
    }
    stdio::write_diagnostic_line(&format!(
        "If the browser does not open, visit this URL:\n\x20 {url}\nWaiting for \
         the redirect..."
    ));
}

/// The client credentials to authenticate token requests with.
fn client_auth(registration: &ClientRegistration) -> ClientAuth<'_> {
    ClientAuth {
        client_id: &registration.client_id,
        client_secret: registration.client_secret.as_deref(),
    }
}

/// Assemble the session record from a token response.
fn build_session(
    metadata: &Metadata,
    registration: &ClientRegistration,
    tokens: oauth::Tokens,
) -> OAuthSession {
    OAuthSession {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_at,
        scope: tokens.scope,
        client_id: registration.client_id.clone(),
        // Captured now so a refresh needs no second discovery round trip.
        token_endpoint: metadata.token_endpoint.clone(),
        revocation_endpoint: metadata.revocation_endpoint.clone(),
        account: None,
        workspace: None,
    }
}

/// Ask the instance who just signed in, and record it for `auth info`.
///
/// Best effort: the tokens are already stored and valid, so a failure here
/// is a notice, not a failed login.
fn capture_identity(
    base_url: &str,
    profile: &str,
    origin: &str,
    store: &CredentialStore,
) -> Option<Identity> {
    let identity = match query_identity(base_url, profile, origin, store) {
        Ok(identity) => identity,
        Err(error) => {
            stdio::write_diagnostic_line(&format!(
                "notice: signed in, but the instance could not be asked who you \
                 are ({error})"
            ));
            return None;
        }
    };
    if let Err(error) = record_identity(profile, store, &identity) {
        stdio::write_diagnostic_line(&format!(
            "notice: signed in, but your name could not be cached for \
             `otl auth info` ({error})"
        ));
    }
    Some(identity)
}

/// One `auth.info` call through the ordinary request channel.
fn query_identity(
    base_url: &str,
    profile: &str,
    origin: &str,
    store: &CredentialStore,
) -> Result<Identity, AuthError> {
    let file = store.load()?;
    let provider =
        CredentialProvider::resolve(store.clone(), profile, &file, origin)?.ok_or_else(|| {
            AuthError::NoCredentials {
                profile: profile.to_string(),
            }
        })?;
    let client = engine::Client::with_credentials(base_url, Arc::new(provider))?;
    crate::auth::fetch_identity(&client)
}

/// Store the identity labels alongside the session.
fn record_identity(
    profile: &str,
    store: &CredentialStore,
    identity: &Identity,
) -> Result<(), AuthError> {
    store.update(|file: &mut CredentialFile| -> Result<(), AuthError> {
        if let Some(session) = file.profile_mut(profile).oauth.as_mut() {
            session.account = identity.account();
            session.workspace = identity.workspace.clone();
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn metadata(scopes: &[&str], s256: bool) -> Metadata {
        Metadata {
            issuer: "https://docs.example.com".to_string(),
            authorization_endpoint: "https://docs.example.com/oauth/authorize".to_string(),
            token_endpoint: "https://docs.example.com/oauth/token".to_string(),
            registration_endpoint: None,
            revocation_endpoint: None,
            scopes_supported: scopes.iter().map(|s| s.to_string()).collect(),
            supports_s256: s256,
        }
    }

    #[test]
    fn an_instance_without_s256_is_refused() {
        let error = require_s256(&metadata(&[], false), "https://docs.example.com")
            .expect_err("plain PKCE is not acceptable for a public client");
        assert!(error.to_string().contains("S256"), "{error}");
    }

    #[test]
    fn an_instance_with_s256_passes() {
        assert!(require_s256(&metadata(&[], true), "https://docs.example.com").is_ok());
    }

    #[test]
    fn the_default_options_request_read_and_write_over_a_browser() {
        let options = Options::default();
        assert_eq!(options.scope, "read write");
        assert!(options.open_browser);
        assert_eq!(options.timeout, loopback::AUTH_TIMEOUT);
        assert!(options.client_id.is_none());
    }

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

    #[test]
    fn a_session_captures_the_endpoints_a_later_refresh_needs() {
        let registration = administered("c", "http://127.0.0.1:8586/callback", "https://d.example");
        let mut meta = metadata(&[], true);
        meta.revocation_endpoint = Some("https://docs.example.com/oauth/revoke".to_string());
        let session = build_session(
            &meta,
            &registration,
            oauth::Tokens {
                access_token: "at".to_string(),
                refresh_token: Some("rt".to_string()),
                expires_at: Some(1_800_000_000),
                scope: Some("read".to_string()),
            },
        );
        assert_eq!(session.token_endpoint, meta.token_endpoint);
        assert_eq!(
            session.revocation_endpoint.as_deref(),
            Some("https://docs.example.com/oauth/revoke")
        );
        assert_eq!(session.client_id, "c");
        assert!(session.account.is_none(), "identity is captured separately");
    }
}
