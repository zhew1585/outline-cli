//! The `otl auth logout` flow.
//!
//! Plain `logout` forgets everything stored for the profile and asks the
//! server to revoke the tokens. `--purge` additionally deletes the client
//! registration `otl` created for itself.
//!
//! The two are separate because a dynamic registration is REUSABLE: a
//! second `otl auth login` on the same instance reuses it instead of
//! creating another application, so throwing it away on every logout would
//! litter the server with registrations. `--purge` exists for the other
//! case - leaving a machine for good - and is the only way to remove a
//! dynamic client at all, since Outline's admin UI cannot.
//!
//! Every server-side step is best-effort. Local removal is what the user
//! asked for and must happen even when the instance is unreachable;
//! anything that could not be done on the server is reported, never
//! silently swallowed.

use reqwest::blocking::Client as HttpClient;

use crate::auth::credentials::{ClientRegistration, CredentialStore, OAuthSession};
use crate::auth::oauth::ClientAuth;
use crate::auth::{dcr, endpoint, oauth, AuthError};

/// What `otl auth logout` was asked to do.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Also delete the dynamic client registration from the server.
    pub purge: bool,
}

/// What logout actually managed to do.
#[derive(Debug, Default)]
pub struct Report {
    /// Whether anything was stored for the profile at all.
    pub had_credentials: bool,
    /// Whether tokens were revoked on the server.
    pub revoked: bool,
    /// Whether the dynamic registration was deleted from the server.
    pub registration_deleted: bool,
    /// Whether the credential file itself is now gone.
    pub file_removed: bool,
    /// Problems worth telling the user about, none of which stopped the
    /// local removal.
    pub warnings: Vec<String>,
}

/// Forget the profile's credentials, revoking what can be revoked.
pub fn run(profile: &str, store: &CredentialStore, options: Options) -> Result<Report, AuthError> {
    let mut file = store.load()?;
    let Some(entry) = file.profile(profile).cloned() else {
        return Ok(Report::default());
    };
    let mut report = Report {
        had_credentials: !entry.is_empty(),
        ..Report::default()
    };
    let http = endpoint::http_client()?;

    if let Some(session) = &entry.oauth {
        revoke_tokens(&http, session, entry.client.as_ref(), &mut report);
    }
    if options.purge {
        purge_registration(&http, entry.client.as_ref(), &mut report);
    }

    // Local removal happens regardless of what the server said.
    let profile_entry = file.profile_mut(profile);
    profile_entry.oauth = None;
    profile_entry.api_key = None;
    if options.purge || report.registration_deleted {
        profile_entry.client = None;
    }
    store.save(&file)?;
    report.file_removed = !store.path().exists();
    Ok(report)
}

/// Ask the server to revoke the stored tokens.
///
/// Both tokens are offered: revoking the refresh token is what actually
/// ends the session, and revoking the access token closes the remaining
/// window. A server without a revocation endpoint is noted, not failed.
fn revoke_tokens(
    http: &HttpClient,
    session: &OAuthSession,
    registration: Option<&ClientRegistration>,
    report: &mut Report,
) {
    let Some(endpoint_url) = session.revocation_endpoint.as_deref() else {
        report.warnings.push(
            "this instance advertises no token revocation endpoint, so the \
             tokens were only removed locally and stay valid until they expire"
                .to_string(),
        );
        return;
    };
    let client = ClientAuth {
        client_id: &session.client_id,
        client_secret: registration.and_then(|reg| reg.client_secret.as_deref()),
    };
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
            Err(error) => report
                .warnings
                .push(format!("a token could not be revoked ({error})")),
        }
    }
    report.revoked = revoked_any;
}

/// Delete the dynamic client registration from the server.
fn purge_registration(
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
        Ok(false) => report.warnings.push(
            "the stored registration has no management token, so the \
             application cannot be deleted from the server; ask an admin to \
             remove it (Settings -> Applications)"
                .to_string(),
        ),
        Err(error) => report.warnings.push(format!(
            "the application could not be deleted from the server ({error}); \
             the local record was kept so `otl auth logout --purge` can be \
             retried"
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::auth::credentials::CredentialFile;

    fn scratch() -> (tempfile::TempDir, CredentialStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::at(dir.path().to_path_buf());
        (dir, store)
    }

    fn session() -> OAuthSession {
        OAuthSession {
            access_token: "at".to_string(),
            refresh_token: Some("rt".to_string()),
            expires_at: None,
            scope: Some("read write".to_string()),
            client_id: "c".to_string(),
            token_endpoint: "https://docs.example.com/oauth/token".to_string(),
            // No revocation endpoint: nothing is sent over the network, so
            // these tests stay offline.
            revocation_endpoint: None,
            account: None,
            workspace: None,
        }
    }

    fn dynamic_registration() -> ClientRegistration {
        ClientRegistration {
            client_id: "c".to_string(),
            client_secret: None,
            registration_access_token: None,
            registration_client_uri: None,
            redirect_uri: "http://127.0.0.1:41234/callback".to_string(),
            dynamic: true,
            origin: Some("https://docs.example.com".to_string()),
        }
    }

    #[test]
    fn logout_removes_every_credential_and_then_the_file() {
        let (_dir, store) = scratch();
        let mut file = CredentialFile::default();
        let entry = file.profile_mut("default");
        entry.oauth = Some(session());
        entry.api_key = Some("key".to_string());
        store.save(&file).unwrap();

        let report = run("default", &store, Options::default()).unwrap();
        assert!(report.had_credentials);
        assert!(
            report.file_removed,
            "the file should be gone: no credentials left"
        );
        assert!(!store.path().exists());
        // Revocation was impossible, and that is stated rather than hidden.
        assert!(
            report.warnings.iter().any(|w| w.contains("revocation")),
            "{:?}",
            report.warnings
        );
    }

    #[test]
    fn logout_keeps_a_reusable_registration_but_purge_drops_it() {
        let (_dir, store) = scratch();
        let mut file = CredentialFile::default();
        let entry = file.profile_mut("default");
        entry.oauth = Some(session());
        entry.client = Some(dynamic_registration());
        store.save(&file).unwrap();

        run("default", &store, Options::default()).unwrap();
        let after = store.load().unwrap();
        assert!(
            after.profile("default").unwrap().client.is_some(),
            "a reusable registration must survive a plain logout"
        );
        assert!(after.profile("default").unwrap().oauth.is_none());

        let report = run("default", &store, Options { purge: true }).unwrap();
        assert!(!store.path().exists(), "purge must leave nothing behind");
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("management token")),
            "an undeletable registration must be reported: {:?}",
            report.warnings
        );
    }

    #[test]
    fn purge_refuses_to_delete_an_administrators_client() {
        let (_dir, store) = scratch();
        let mut file = CredentialFile::default();
        let mut registration = dynamic_registration();
        registration.dynamic = false;
        file.profile_mut("default").client = Some(registration);
        file.profile_mut("default").oauth = Some(session());
        store.save(&file).unwrap();

        let report = run("default", &store, Options { purge: true }).unwrap();
        assert!(!report.registration_deleted);
        assert!(
            report.warnings.iter().any(|w| w.contains("administrator")),
            "{:?}",
            report.warnings
        );
    }

    #[test]
    fn logging_out_of_one_profile_leaves_the_other_alone() {
        let (_dir, store) = scratch();
        let mut file = CredentialFile::default();
        file.profile_mut("default").api_key = Some("key-a".to_string());
        file.profile_mut("work").api_key = Some("key-b".to_string());
        store.save(&file).unwrap();

        let report = run("default", &store, Options::default()).unwrap();
        assert!(
            !report.file_removed,
            "another profile still has credentials"
        );
        let after = store.load().unwrap();
        assert!(after.profile("default").is_none());
        assert_eq!(
            after.profile("work").unwrap().api_key.as_deref(),
            Some("key-b")
        );
    }

    #[test]
    fn logging_out_with_nothing_stored_is_not_an_error() {
        let (_dir, store) = scratch();
        let report = run("default", &store, Options { purge: true }).unwrap();
        assert!(!report.had_credentials);
        assert!(report.warnings.is_empty());
    }
}
