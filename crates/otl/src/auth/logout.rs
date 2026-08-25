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
//! Local removal is what the user asked for and happens even when the
//! instance is unreachable. Server-side steps are attempted first and
//! anything that fails is reported, never silently swallowed - and the exit
//! code says so, because "signed out locally, application still on the
//! server" is not success.
//!
//! One thing is NOT best-effort: the `registration_access_token` is only
//! deleted from disk once the server has confirmed the registration it
//! manages is gone. Dropping it after a failed DELETE would leave an
//! application that nothing can ever remove.

use reqwest::blocking::Client as HttpClient;

use crate::auth::credentials::{
    ClientRegistration, CredentialFile, CredentialStore, OAuthSession, ProfileCredentials,
};
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
    /// Whether something the user asked for could not be done on the
    /// server, so the command must not report plain success.
    pub remote_cleanup_failed: bool,
    /// Problems worth telling the user about, none of which stopped the
    /// local removal.
    pub warnings: Vec<String>,
}

/// Forget the profile's credentials, revoking what can be revoked.
pub fn run(profile: &str, store: &CredentialStore, options: Options) -> Result<Report, AuthError> {
    let Some(entry) = store.load()?.profile(profile).cloned() else {
        return Ok(Report::default());
    };
    let mut report = Report {
        had_credentials: !entry.is_empty(),
        ..Report::default()
    };
    let http = endpoint::http_client()?;

    // Network work happens BEFORE the lock is taken: revocation and
    // deregistration are slow, and holding the credential lock across them
    // would block every other otl process on this machine.
    if let Some(session) = &entry.oauth {
        revoke_tokens(&http, session, entry.client.as_ref(), &mut report);
    }
    if options.purge {
        purge_registration(&http, entry.client.as_ref(), &mut report);
    }

    // Local removal happens regardless of what the server said - the user
    // asked for these credentials to be gone from this machine - but the
    // MANAGEMENT credential only goes once the server confirmed the
    // registration it manages is gone. See `drop_registration`.
    let drop_client = drop_registration(options, &entry, &report);
    store.update(|file: &mut CredentialFile| -> Result<(), AuthError> {
        let profile_entry = file.profile_mut(profile);
        profile_entry.oauth = None;
        profile_entry.api_key = None;
        if drop_client {
            profile_entry.client = None;
        }
        if profile_entry.client.is_none() {
            // Nothing left for the binding to protect, and a stale one
            // would only confuse the next `auth info`.
            profile_entry.origin = None;
        }
        Ok(())
    })?;
    report.file_removed = !store.path().exists();
    Ok(report)
}

/// Whether the local client registration record may be discarded.
///
/// The rule that matters: **a `registration_access_token` is thrown away
/// only once the server has confirmed the registration it manages is
/// gone.** It is the only credential that can delete a dynamically
/// registered client - Outline's admin UI cannot - so dropping it after a
/// failed DELETE would leave an application nobody can ever remove. That is
/// exactly the outcome the persistence rule exists to prevent, so a failed
/// purge KEEPS the record and stays retryable.
///
/// A registration an administrator created carries no management token, and
/// nothing on the server belongs to us, so dropping its cached client id
/// orphans nothing.
fn drop_registration(options: Options, entry: &ProfileCredentials, report: &Report) -> bool {
    if report.registration_deleted {
        return true;
    }
    match &entry.client {
        Some(registration) => options.purge && !registration.dynamic,
        None => false,
    }
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
        Ok(false) => {
            report.remote_cleanup_failed = true;
            report.warnings.push(
                "the stored registration has no management token, so the \
                 application cannot be deleted from the server; ask an admin \
                 to remove it (Settings -> Applications)"
                    .to_string(),
            );
        }
        Err(error) => {
            report.remote_cleanup_failed = true;
            report.warnings.push(format!(
                "the application could not be deleted from the server \
                 ({error}). The credential that manages it has been KEPT on \
                 disk so `otl auth logout --purge` can be retried - deleting \
                 it would leave an application nobody can remove"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

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

        // This registration has no management credentials, so the server
        // never confirmed anything: the local record is KEPT so the orphan
        // stays visible and `--purge` stays retryable.
        let report = run("default", &store, Options { purge: true }).unwrap();
        assert!(!report.registration_deleted);
        assert!(
            report.remote_cleanup_failed,
            "an undeletable registration must not count as a clean purge"
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("management token")),
            "an undeletable registration must be reported: {:?}",
            report.warnings
        );
        assert!(
            store
                .load()
                .unwrap()
                .profile("default")
                .and_then(|entry| entry.client.as_ref())
                .is_some(),
            "the record of an orphan on the server must not be discarded"
        );
    }

    #[test]
    fn purge_keeps_management_credentials_when_the_server_did_not_confirm() {
        // The rule from `drop_registration`: a registration_access_token is
        // the only thing that can delete a dynamic client, so it survives
        // any purge the server did not confirm. Dropping it would leave an
        // application nobody could ever remove.
        let mut registration = dynamic_registration();
        registration.registration_access_token = Some("rat".to_string());
        registration.registration_client_uri =
            Some("https://docs.example.com/oauth/clients/c".to_string());
        let entry = ProfileCredentials {
            origin: Some("https://docs.example.com".to_string()),
            api_key: None,
            oauth: Some(session()),
            client: Some(registration),
        };
        let failed = Report {
            registration_deleted: false,
            remote_cleanup_failed: true,
            ..Report::default()
        };
        assert!(
            !drop_registration(Options { purge: true }, &entry, &failed),
            "a failed purge must keep the credential that manages the orphan"
        );

        let confirmed = Report {
            registration_deleted: true,
            ..Report::default()
        };
        assert!(
            drop_registration(Options { purge: true }, &entry, &confirmed),
            "a confirmed deletion may drop the local record"
        );
    }

    #[test]
    fn a_plain_logout_never_drops_a_registration() {
        let entry = ProfileCredentials {
            origin: Some("https://docs.example.com".to_string()),
            api_key: None,
            oauth: Some(session()),
            client: Some(dynamic_registration()),
        };
        assert!(!drop_registration(
            Options { purge: false },
            &entry,
            &Report::default()
        ));
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
