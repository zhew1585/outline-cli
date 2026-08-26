//! The credential file: the one and only place a credential is stored.
//!
//! Everything else - config, caches, logs, error messages, `doctor`
//! reports - is credential-free by rule. That includes the DCR
//! `registration_access_token`, which is a bearer credential like any
//! other and therefore lives here rather than in a separate registration
//! cache.
//!
//! Every secret-bearing type in this module has a HAND-WRITTEN `Debug` that
//! prints `***`. Do not replace one with `#[derive(Debug)]`: these values
//! end up inside error contexts and `{:?}` formatting during development,
//! and a derived Debug would print them.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auth::error::{StoreError, CREDENTIAL_FORMAT_VERSION};
use crate::auth::lock::CredentialLock;
use crate::auth::paths::{self, CREDENTIALS_FILE_NAME};
use crate::auth::secret_file::{self, Permissions};

/// Placeholder printed instead of any credential in Debug output.
const REDACTED: &str = "***";

/// The whole credential file.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CredentialFile {
    /// Format version, so a future layout change can be detected rather
    /// than silently misread.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Credentials per named profile.
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileCredentials>,
}

fn default_version() -> u32 {
    CREDENTIAL_FORMAT_VERSION
}

impl fmt::Debug for CredentialFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialFile")
            .field("version", &self.version)
            .field("profiles", &self.profiles.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CredentialFile {
    /// Credentials stored for one profile, if any.
    pub fn profile(&self, name: &str) -> Option<&ProfileCredentials> {
        self.profiles.get(name)
    }

    /// Credentials for one profile, created empty if absent.
    pub fn profile_mut(&mut self, name: &str) -> &mut ProfileCredentials {
        self.profiles.entry(name.to_string()).or_default()
    }

    /// Drop profile entries that no longer hold anything.
    ///
    /// Keeps `logout` from leaving `[profiles.work]` headers behind, which
    /// would suggest credentials that are not there.
    pub fn prune(&mut self) {
        self.profiles.retain(|_, profile| !profile.is_empty());
    }

    /// Whether the file holds no credentials at all.
    pub fn is_empty(&self) -> bool {
        self.profiles.values().all(ProfileCredentials::is_empty)
    }
}

/// Everything stored for one profile.
///
/// All three kinds may coexist: an API key for scripts, an OAuth session
/// for interactive use, and the client registration the OAuth session was
/// obtained with.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ProfileCredentials {
    /// Origin (`scheme://host[:port]`) these credentials were issued by.
    ///
    /// A credential is only meaningful to the instance that issued it, and
    /// sending it anywhere else hands a bearer token to a server that was
    /// never supposed to have it. The binding is recorded here and checked
    /// on every use, so re-pointing `OUTLINE_URL` at another host cannot
    /// silently forward this profile's credentials to it.
    ///
    /// `None` means "written before the binding existed, or by hand": that
    /// is treated as unusable rather than as universally valid, so the
    /// failure mode of a missing binding is a refusal, not a leak.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// A personal API key, as stored by `otl auth set-key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// An OAuth session, as stored by `otl auth login`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthSession>,
    /// The OAuth client this profile authenticates as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientRegistration>,
}

impl fmt::Debug for ProfileCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileCredentials")
            .field("origin", &self.origin)
            .field("api_key", &presence(self.api_key.as_deref()))
            .field("oauth", &self.oauth)
            .field("client", &self.client)
            .finish()
    }
}

impl ProfileCredentials {
    /// Whether nothing at all is stored for this profile.
    ///
    /// The origin binding alone is not a credential: a profile that holds
    /// only a leftover binding is empty and gets pruned.
    pub fn is_empty(&self) -> bool {
        self.api_key.is_none() && self.oauth.is_none() && self.client.is_none()
    }

    /// Whether these credentials may be sent to `origin`.
    ///
    /// Fails closed: an unrecorded binding is not usable anywhere.
    pub fn is_bound_to(&self, origin: &str) -> bool {
        self.origin.as_deref() == Some(origin)
    }

    /// Whether this profile holds anything that could AUTHENTICATE a
    /// request.
    ///
    /// A client registration is not such a thing: it names the OAuth client
    /// the login flow speaks as, and cannot be sent to an API. That
    /// distinction matters because a leftover registration must not make a
    /// profile look "bound" to an instance for the purposes of refusing an
    /// environment API key meant for a different one.
    pub fn has_authenticator(&self) -> bool {
        self.api_key.is_some() || self.oauth.is_some()
    }

    /// The origin an OAuth session provably belongs to.
    ///
    /// Derived from the token endpoint captured at login, which discovery
    /// had already proved same-origin with the instance. That makes the
    /// session SELF-DESCRIBING: even if the profile-level binding were
    /// rewritten by a later write, the session still names the server that
    /// issued it, so the check cannot be defeated by editing one field.
    pub fn session_origin(&self) -> Option<String> {
        let session = self.oauth.as_ref()?;
        Some(
            engine::base_url_origin(&session.token_endpoint)
                .unwrap_or_else(|| UNKNOWN_ORIGIN.to_string()),
        )
    }

    /// Discard everything that does not belong to `origin`, then bind to it.
    ///
    /// Returns whether anything had to be discarded, so a caller can say so
    /// out loud. Used only where discarding is what the user asked for.
    pub fn rebind_to(&mut self, origin: &str) -> bool {
        if self.is_bound_to(origin) || self.is_empty() {
            self.origin = Some(origin.to_string());
            return false;
        }
        *self = Self {
            origin: Some(origin.to_string()),
            ..Self::default()
        };
        true
    }
}

/// Stand-in when a stored endpoint has no recoverable origin. Never equal
/// to a real origin, so it fails closed.
const UNKNOWN_ORIGIN: &str = "an unrecognizable instance";

/// An OAuth session: the tokens plus what is needed to renew them.
#[derive(Serialize, Deserialize, Clone)]
pub struct OAuthSession {
    /// Current access token.
    pub access_token: String,
    /// Current refresh token. Rotated on every refresh, so the value here
    /// is single-use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds at which the access token expires.
    ///
    /// Stored absolute rather than as the `expires_in` the server sent, so
    /// reading it back needs no memory of when the response arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Scope the tokens were granted with, as returned by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Client id the session belongs to; a refresh needs it.
    pub client_id: String,
    /// Token endpoint used to refresh, captured at login so a refresh
    /// needs no second discovery round trip.
    pub token_endpoint: String,
    /// Revocation endpoint, when the server advertised one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_endpoint: Option<String>,
    /// Human-readable account label captured at login, so `auth info` can
    /// name the identity without a network call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Workspace label captured at login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl fmt::Debug for OAuthSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthSession")
            .field("access_token", &REDACTED)
            .field("refresh_token", &presence(self.refresh_token.as_deref()))
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("client_id", &REDACTED)
            .field("account", &self.account)
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

/// The OAuth client `otl` authenticates as for one profile.
#[derive(Serialize, Deserialize, Clone)]
pub struct ClientRegistration {
    /// Client id, from dynamic registration or from an administrator.
    pub client_id: String,
    /// Client secret, if the server insisted on issuing one. `otl`
    /// registers as a public client, so this is normally absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// RFC 7592 credential for managing this registration.
    ///
    /// MUST be persisted: a dynamically registered client cannot be
    /// removed from the Outline admin UI, so losing this token leaves an
    /// orphan client on the server forever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_access_token: Option<String>,
    /// RFC 7592 management URI for this registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_client_uri: Option<String>,
    /// The exact redirect URI this client was registered with. A later
    /// login must bind that same port or register anew.
    pub redirect_uri: String,
    /// Whether the registration was obtained dynamically (and can
    /// therefore be deleted again) or supplied by an administrator.
    #[serde(default)]
    pub dynamic: bool,
    /// Instance origin the registration belongs to, so pointing a profile
    /// at another instance does not silently reuse a foreign client id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl fmt::Debug for ClientRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientRegistration")
            .field("client_id", &REDACTED)
            .field("client_secret", &presence(self.client_secret.as_deref()))
            .field(
                "registration_access_token",
                &presence(self.registration_access_token.as_deref()),
            )
            .field("redirect_uri", &self.redirect_uri)
            .field("dynamic", &self.dynamic)
            .field("origin", &self.origin)
            .finish()
    }
}

/// Describe whether an optional secret is set, without revealing it.
fn presence(value: Option<&str>) -> &'static str {
    match value {
        Some(_) => REDACTED,
        None => "none",
    }
}

/// The credential file on disk, and the operations allowed on it.
#[derive(Debug, Clone)]
pub struct CredentialStore {
    dir: PathBuf,
    path: PathBuf,
}

impl CredentialStore {
    /// The store in the platform configuration directory.
    pub fn discover() -> Result<Self, StoreError> {
        Ok(Self::at(paths::config_dir()?))
    }

    /// The store in an explicit directory (tests, unusual layouts).
    pub fn at(dir: PathBuf) -> Self {
        let path = dir.join(CREDENTIALS_FILE_NAME);
        Self { dir, path }
    }

    /// Directory holding the credential file and its lock.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of the credential file itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Permission state of the credential file, for `auth info`/`doctor`.
    pub fn permissions(&self) -> Permissions {
        secret_file::permissions(&self.path)
    }

    /// Read the credential file.
    ///
    /// An absent file yields an empty set: a fresh installation is not an
    /// error. Over-wide permissions are, and so is a version this build
    /// does not understand.
    pub fn load(&self) -> Result<CredentialFile, StoreError> {
        let Some(text) = secret_file::read_checked(&self.path)? else {
            return Ok(CredentialFile {
                version: CREDENTIAL_FORMAT_VERSION,
                profiles: BTreeMap::new(),
            });
        };
        let parsed: CredentialFile = toml::from_str(&text).map_err(|error| StoreError::Parse {
            path: secret_file::display(&self.path),
            // Never the crate's own Display: it quotes the source line.
            reason: parse_position(&text, error.span()),
        })?;
        if parsed.version > CREDENTIAL_FORMAT_VERSION {
            return Err(StoreError::Version {
                path: secret_file::display(&self.path),
                found: parsed.version,
                supported: CREDENTIAL_FORMAT_VERSION,
            });
        }
        Ok(parsed)
    }

    /// Write the credential file atomically, owner-only.
    ///
    /// A set with nothing left in it removes the file instead of leaving an
    /// empty husk on disk, so `logout` really does leave no trace.
    pub fn save(&self, file: &CredentialFile) -> Result<(), StoreError> {
        let mut file = file.clone();
        file.prune();
        file.version = CREDENTIAL_FORMAT_VERSION;
        if file.is_empty() {
            return secret_file::remove(&self.path);
        }
        let text = toml::to_string_pretty(&file).map_err(|error| StoreError::Write {
            path: secret_file::display(&self.path),
            // A serialization failure names a field, never a value.
            reason: error.to_string(),
        })?;
        secret_file::write_atomic(&self.path, &text)
    }

    /// Remove the credential file entirely.
    pub fn delete(&self) -> Result<(), StoreError> {
        secret_file::remove(&self.path)
    }

    /// Take the credential lock, so a caller can run a longer transaction
    /// (a refresh) with the file to itself.
    pub fn lock(&self) -> Result<CredentialLock, StoreError> {
        CredentialLock::acquire(&self.dir)
    }

    /// Run one read-modify-write of the credential file as a transaction.
    ///
    /// **This is the only correct way to change the credential file.** The
    /// file is loaded INSIDE the lock and saved before the lock is
    /// released, so a caller can never write back a snapshot taken before
    /// another process rotated the tokens - which would resurrect a refresh
    /// token the server has already retired and force a fresh login.
    ///
    /// The closure must not wait for a human, a browser, or the network:
    /// the lock is held for its whole duration and every other `otl`
    /// process blocks behind it. Gather input first, then call this.
    pub fn update<T, E>(
        &self,
        edit: impl FnOnce(&mut CredentialFile) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<StoreError>,
    {
        let _lock = self.lock()?;
        let mut file = self.load()?;
        let outcome = edit(&mut file)?;
        self.save(&file)?;
        Ok(outcome)
    }
}

/// Reduce a TOML syntax error to a position in the file.
///
/// The `toml` crate renders the offending source line in its own error
/// Display. In a credential file that line is a token, so only the
/// coordinates may be reported.
fn parse_position(text: &str, span: Option<std::ops::Range<usize>>) -> String {
    let Some(span) = span else {
        return "syntax error".to_string();
    };
    let cut = span.start.min(text.len());
    let prefix = &text[..cut];
    let line = prefix.matches('\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.len(), |(_, last)| last.len())
        + 1;
    format!("syntax error at line {line}, column {column}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn store() -> (tempfile::TempDir, CredentialStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::at(dir.path().to_path_buf());
        (dir, store)
    }

    fn session() -> OAuthSession {
        OAuthSession {
            access_token: "access-SECRET".to_string(),
            refresh_token: Some("refresh-SECRET".to_string()),
            expires_at: Some(1_800_000_000),
            scope: Some("read write".to_string()),
            client_id: "client-SECRET".to_string(),
            token_endpoint: "https://docs.example.com/oauth/token".to_string(),
            revocation_endpoint: Some("https://docs.example.com/oauth/revoke".to_string()),
            account: Some("Alice <alice@example.com>".to_string()),
            workspace: Some("Acme".to_string()),
        }
    }

    #[test]
    fn an_absent_file_loads_as_an_empty_set() {
        let (_dir, store) = store();
        let file = store.load().unwrap();
        assert!(file.is_empty());
        assert_eq!(file.version, CREDENTIAL_FORMAT_VERSION);
    }

    #[test]
    fn a_saved_session_round_trips() {
        let (_dir, store) = store();
        let mut file = store.load().unwrap();
        file.profile_mut("default").oauth = Some(session());
        store.save(&file).unwrap();

        let reloaded = store.load().unwrap();
        let stored = reloaded.profile("default").unwrap().oauth.as_ref().unwrap();
        assert_eq!(stored.access_token, "access-SECRET");
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-SECRET"));
        assert_eq!(stored.expires_at, Some(1_800_000_000));
        assert_eq!(stored.scope.as_deref(), Some("read write"));
    }

    #[test]
    fn saving_an_emptied_set_removes_the_file() {
        let (_dir, store) = store();
        let mut file = store.load().unwrap();
        file.profile_mut("default").api_key = Some("key".to_string());
        store.save(&file).unwrap();
        assert!(store.path().exists());

        file.profile_mut("default").api_key = None;
        store.save(&file).unwrap();
        assert!(
            !store.path().exists(),
            "an empty credential set left a husk file behind"
        );
    }

    #[test]
    fn removing_one_profile_keeps_the_other() {
        let (_dir, store) = store();
        let mut file = store.load().unwrap();
        file.profile_mut("default").api_key = Some("key-a".to_string());
        file.profile_mut("work").api_key = Some("key-b".to_string());
        store.save(&file).unwrap();

        let mut file = store.load().unwrap();
        file.profiles.remove("default");
        store.save(&file).unwrap();

        let reloaded = store.load().unwrap();
        assert!(reloaded.profile("default").is_none());
        assert_eq!(
            reloaded.profile("work").unwrap().api_key.as_deref(),
            Some("key-b")
        );
    }

    #[test]
    fn a_future_format_version_is_refused_rather_than_misread() {
        let (_dir, store) = store();
        secret_file::write_atomic(store.path(), "version = 99\n").unwrap();
        let error = store.load().expect_err("a newer format must be refused");
        assert!(error.to_string().contains("upgrade otl"), "{error}");
    }

    #[test]
    fn a_malformed_file_reports_a_position_and_never_its_content() {
        let (_dir, store) = store();
        secret_file::write_atomic(
            store.path(),
            "version = 1\n[profiles.default]\napi_key = \"leaked-SECRET-9c7a\n",
        )
        .unwrap();
        let error = store.load().expect_err("a malformed file must be refused");
        let text = format!("{error} / {error:?}");
        assert!(
            !text.contains("leaked-SECRET-9c7a"),
            "credential leaked through a parse error: {text}"
        );
        assert!(text.contains("line"), "no position reported: {text}");
    }

    #[test]
    fn debug_output_never_shows_a_credential() {
        let mut file = CredentialFile::default();
        let profile = file.profile_mut("default");
        profile.api_key = Some("api-key-SECRET".to_string());
        profile.oauth = Some(session());
        profile.client = Some(ClientRegistration {
            client_id: "client-SECRET".to_string(),
            client_secret: Some("client-secret-SECRET".to_string()),
            registration_access_token: Some("rat-SECRET".to_string()),
            registration_client_uri: Some("https://docs.example.com/oauth/clients/1".to_string()),
            redirect_uri: "http://127.0.0.1:8586/callback".to_string(),
            dynamic: true,
            origin: Some("https://docs.example.com".to_string()),
        });

        let rendered = format!("{file:?} {:?}", file.profile("default").unwrap());
        assert!(
            !rendered.contains("SECRET"),
            "credential leaked in Debug output: {rendered}"
        );
        assert!(rendered.contains(REDACTED));
    }

    #[test]
    fn the_serialized_file_is_toml_with_the_version_pinned() {
        let (_dir, store) = store();
        let mut file = store.load().unwrap();
        file.version = 0; // whatever the caller left here is corrected
        file.profile_mut("default").api_key = Some("k".to_string());
        store.save(&file).unwrap();
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("version = 1"), "{text}");
        assert!(text.contains("[profiles.default]"), "{text}");
    }
}
