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

use crate::auth::client_acquisition::acquire_client;
use crate::auth::credentials::{ClientRegistration, CredentialFile, CredentialStore, OAuthSession};
use crate::auth::error::{OAuthError, Stage};
use crate::auth::loopback::{self, CallbackServer};
use crate::auth::metadata::{self, Metadata, CODE_CHALLENGE_METHOD, DEFAULT_SCOPE};
use crate::auth::oauth::{self, ClientAuth};
use crate::auth::source::CredentialProvider;
use crate::auth::{dcr, endpoint, AuthError, Identity};
use crate::browser;
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
pub struct Acquired {
    pub registration: ClientRegistration,
    pub server: CallbackServer,
    pub source: ClientSource,
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
    let access_token = session.access_token.clone();
    if let Err(error) = persist_session(store, profile, origin, &acquired, session) {
        return Err(abandon(&http, &acquired, error));
    }

    let identity = capture_identity(base_url, profile, origin, store, &access_token);
    Ok(Outcome {
        identity,
        scope,
        client_source: acquired.source,
        credential_path: store.path().to_path_buf(),
    })
}

/// Store the session these tokens belong to, if the profile is still ours.
fn persist_session(
    store: &CredentialStore,
    profile: &str,
    origin: &str,
    acquired: &Acquired,
    session: OAuthSession,
) -> Result<(), AuthError> {
    let registration = acquired.registration.clone();
    store.update(|file: &mut CredentialFile| -> Result<(), AuthError> {
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
pub fn display_client_id(client_id: &str) -> String {
    engine::sanitize::clean_server_text(client_id, "", false, MAX_CLIENT_ID_CHARS)
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
        if let Err(error) = browser::spawn(url) {
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
    access_token: &str,
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
    if let Err(error) = record_identity(profile, store, &identity, access_token) {
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
    // The SESSION this login just wrote, specifically. Not "whatever
    // credential this profile resolves to": a stored API key or an
    // environment key would answer as a different principal, and this call
    // exists to label the session with the account it belongs to.
    let provider = CredentialProvider::for_session(store.clone(), profile, &file, origin)?
        .ok_or_else(|| AuthError::NoCredentials {
            profile: profile.to_string(),
        })?;
    let client = engine::Client::with_credentials(base_url, Arc::new(provider))?;
    crate::auth::fetch_identity(&client)
}

/// Store the identity labels alongside the session.
fn record_identity(
    profile: &str,
    store: &CredentialStore,
    identity: &Identity,
    access_token: &str,
) -> Result<(), AuthError> {
    store.update(|file: &mut CredentialFile| -> Result<(), AuthError> {
        // Only label the session THIS login wrote. A full `auth.info` round
        // trip separates the session write from this one, and a concurrent
        // login can land in between - labelling its session with our
        // identity would make `otl auth info` confidently report the wrong
        // account for the token actually in use. Same rule as
        // `claim_client` here and `clear_if_unchanged` in logout.
        match file.profile_mut(profile).oauth.as_mut() {
            Some(session) if session.access_token == access_token => {
                session.account = identity.account();
                session.workspace = identity.workspace.clone();
            }
            _ => {}
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    #[test]
    fn identity_labels_only_land_on_the_session_this_login_wrote() {
        // `record_identity` runs after a full `auth.info` round trip, so
        // a concurrent login can replace the session in between;
        // labelling whatever is on disk would make `otl auth info` report
        // the wrong account for the token actually in use.
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::at(dir.path().to_path_buf());
        let mut file = CredentialFile::default();
        let entry = file.profile_mut("default");
        entry.origin = Some("https://docs.example.com".to_string());
        entry.oauth = Some(OAuthSession {
            access_token: "written-by-another-login".to_string(),
            refresh_token: None,
            expires_at: None,
            scope: None,
            client_id: "c".to_string(),
            token_endpoint: "https://docs.example.com/oauth/token".to_string(),
            revocation_endpoint: None,
            account: None,
            workspace: None,
        });
        store.save(&file).unwrap();

        let identity = Identity {
            user: Some("Ours".to_string()),
            email: None,
            workspace: Some("Our workspace".to_string()),
        };
        record_identity("default", &store, &identity, "the-token-we-wrote").unwrap();

        let after = store.load().unwrap();
        let session = after.profile("default").unwrap().oauth.as_ref().unwrap();
        assert!(
            session.account.is_none(),
            "another login's session was labelled with our identity"
        );

        // And it does label its own.
        record_identity("default", &store, &identity, "written-by-another-login").unwrap();
        let after = store.load().unwrap();
        assert_eq!(
            after
                .profile("default")
                .unwrap()
                .oauth
                .as_ref()
                .unwrap()
                .account
                .as_deref(),
            Some("Ours")
        );
    }

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
    fn a_session_captures_the_endpoints_a_later_refresh_needs() {
        let registration = ClientRegistration {
            client_id: "c".to_string(),
            client_secret: None,
            registration_access_token: None,
            registration_client_uri: None,
            redirect_uri: "http://127.0.0.1:8586/callback".to_string(),
            dynamic: false,
            origin: Some("https://d.example".to_string()),
        };
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
