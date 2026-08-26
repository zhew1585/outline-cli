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
//! Three rules make cleanup safe rather than merely willing:
//!
//! 1. **Every server-side step is anchored to the credential's OWN recorded
//!    origin, never to `OUTLINE_URL`.** A session records the token
//!    endpoint it was issued by; a registration records its instance.
//!    Anchoring on the environment instead would refuse to revoke A's
//!    tokens because the shell happens to point at B - while still
//!    deleting them locally, which turns a revocable token into one that
//!    can never be revoked. Revoking A's tokens at A leaks nothing to B.
//! 2. **This command needs no instance URL at all.** Everything it talks
//!    to comes out of the credential file. `otl auth logout` therefore
//!    works when `OUTLINE_URL` is unset, wrong, or a plaintext value that
//!    predates the transport rule - which is exactly when a user most
//!    needs to clean up, and the only alternative would be deleting the
//!    file by hand and orphaning the DCR registration with it.
//! 3. **Nothing irreversible happens by default.** If a server-side step
//!    could still succeed on a later attempt, the local credentials are
//!    KEPT so that attempt remains possible, and the command exits
//!    non-zero. `--force` is how a user says "I know these cannot be
//!    revoked; discard them anyway".
//!
//! Whatever could not be done is always reported, and the exit code says
//! so: "signed out locally, tokens still live on the server" is not
//! success.

use reqwest::blocking::Client as HttpClient;

use crate::auth::credentials::{
    ClientRegistration, CredentialFile, CredentialStore, OAuthSession, ProfileCredentials,
};
use crate::auth::error::OAuthError;
use crate::auth::oauth::ClientAuth;
use crate::auth::{dcr, endpoint, oauth, transport, AuthError};

/// What `otl auth logout` was asked to do.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Also delete the dynamic client registration from the server.
    pub purge: bool,
    /// Discard the local credentials even when a server-side step that
    /// could still have succeeded did not.
    pub force: bool,
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
    /// Whether a failed server-side step could still succeed later.
    ///
    /// When it could, the local credentials are kept so that attempt stays
    /// possible: discarding the only copy of a token that is still live on
    /// the server turns a recoverable state into a permanent one.
    pub retry_could_succeed: bool,
    /// Whether local credentials were kept because of the above.
    pub kept_for_retry: bool,
    /// Problems worth telling the user about, none of which stopped the
    /// local removal.
    pub warnings: Vec<String>,
}

/// Forget the profile's credentials, revoking what can be revoked.
///
/// Takes no instance URL: see rule 2 in the module docs.
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
        revoke_tokens(&http, session, entry.client.as_ref(), profile, &mut report);
    }
    if options.purge {
        purge_registration(&http, entry.client.as_ref(), &mut report);
    }
    remove_locally(profile, store, &entry, options, &mut report)?;
    Ok(report)
}

/// Take the credentials off this machine, or explain why they were kept.
///
/// Every removal is applied ONLY to the exact credential this run acted on.
/// The decisions above were made from a snapshot taken before the network
/// work; another process can finish a login while a revocation or a DELETE
/// is in flight, and blindly clearing the fields afterwards would delete
/// ITS session, or worse, its `registration_access_token` - stranding on
/// the server an application that nothing can remove.
fn remove_locally(
    profile: &str,
    store: &CredentialStore,
    entry: &ProfileCredentials,
    options: Options,
    report: &mut Report,
) -> Result<(), AuthError> {
    if report.retry_could_succeed && !options.force {
        report.kept_for_retry = true;
        report.warnings.push(
            "the credentials were KEPT on this machine so the failed step \
             can be retried - discarding the only copy of a token that is \
             still live on the server would make it impossible to revoke. \
             Run `otl auth logout` again once the instance is reachable, or \
             `otl auth logout --force` to discard them anyway"
                .to_string(),
        );
        return Ok(());
    }
    let drop_client = drop_registration(options, entry, report);
    store.update(|file: &mut CredentialFile| -> Result<(), AuthError> {
        let profile_entry = file.profile_mut(profile);
        clear_if_unchanged(&mut profile_entry.oauth, entry.oauth.as_ref(), same_session);
        clear_if_unchanged(
            &mut profile_entry.api_key,
            entry.api_key.as_ref(),
            |a, b| a == b,
        );
        if drop_client {
            clear_if_unchanged(
                &mut profile_entry.client,
                entry.client.as_ref(),
                same_registration,
            );
        }
        if profile_entry.is_empty() {
            // Nothing left for the binding to protect, and a stale one
            // would only confuse the next `auth info`.
            profile_entry.origin = None;
        }
        Ok(())
    })?;
    report.file_removed = !store.path().exists();
    Ok(())
}

impl Report {
    /// Record a server-side step that CANNOT be completed, ever.
    ///
    /// The command still failed to do what was asked, so the exit code says
    /// so - but keeping the credentials would not help, because no retry
    /// can change the answer.
    fn unrevocable(&mut self, warning: String) {
        self.remote_cleanup_failed = true;
        self.warnings.push(warning);
    }

    /// Record a server-side step that a later attempt could still complete.
    ///
    /// Keeps the local credentials by default (see `remove_locally`): they
    /// are the only thing that makes the retry possible.
    fn retryable(&mut self, warning: String) {
        self.remote_cleanup_failed = true;
        self.retry_could_succeed = true;
        self.warnings.push(warning);
    }
}

/// Clear `current` only if it still holds the value this run acted on.
///
/// `None` for `acted_on` means there was nothing to remove, so whatever is
/// there now arrived afterwards and is not ours to delete.
fn clear_if_unchanged<T>(
    current: &mut Option<T>,
    acted_on: Option<&T>,
    same: impl Fn(&T, &T) -> bool,
) {
    let Some(acted_on) = acted_on else {
        return;
    };
    if current.as_ref().is_some_and(|now| same(now, acted_on)) {
        *current = None;
    }
}

/// Whether two session records are the same stored session.
///
/// Compared on the access token: a refresh rotates it, and a rotated
/// session is still the same login, but it is also a session another
/// process just wrote and is entitled to keep using. Erring towards
/// "different" leaves a usable credential in place, which is the safe
/// direction - a leftover session can be removed by running logout again,
/// while one deleted by mistake cannot be recovered.
fn same_session(current: &OAuthSession, acted_on: &OAuthSession) -> bool {
    current.access_token == acted_on.access_token
}

/// Whether two registration records name the same server-side application
/// AND carry the same management credential.
///
/// Client id and management URI identify what the DELETE was aimed at; the
/// management token is included because it is the thing whose loss is
/// unrecoverable. No current code path rotates the token while keeping the
/// id and URI, so this is defence in depth rather than a fix - but every
/// extra field can only make the comparison more conservative, and
/// "conservative" here means keeping a credential rather than destroying
/// one that cannot be recreated.
fn same_registration(current: &ClientRegistration, acted_on: &ClientRegistration) -> bool {
    current.client_id == acted_on.client_id
        && current.registration_client_uri == acted_on.registration_client_uri
        && current.registration_access_token == acted_on.registration_access_token
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
fn drop_registration(options: Options, entry: &ProfileCredentials, report: &mut Report) -> bool {
    if report.registration_deleted {
        return true;
    }
    let Some(registration) = &entry.client else {
        return false;
    };
    if !options.purge {
        return false;
    }
    // Nothing on the server belongs to us: the local record is a cached
    // client id, and dropping it orphans nothing.
    if !registration.dynamic {
        return true;
    }
    // `--force` is the user accepting the orphan explicitly. Leaving them
    // no way to finish the cleanup would be worse: they would delete the
    // file by hand, which loses the same token with no warning at all.
    if options.force {
        report.warnings.push(format!(
            "--force was given, so the record of client {} is being \
             discarded even though the server still has that application. \
             Nothing can delete it now; ask an admin to remove it under \
             Settings -> Applications",
            crate::auth::endpoint::sanitize(&registration.client_id, &[], MAX_CLIENT_ID_CHARS)
        ));
        return true;
    }
    false
}

/// Maximum characters kept from a server-supplied client id when printed.
const MAX_CLIENT_ID_CHARS: usize = 80;

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
fn revoke_tokens(
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
            Err(error) => failures.push(format!("a token could not be revoked ({error})")),
        }
    }
    report.revoked = revoked_any && failures.is_empty();
    for failure in failures {
        // The endpoint exists and answered badly: a later attempt can still
        // work, so this one must not destroy the only copy of the tokens.
        report.retryable(failure);
    }
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
        // No management credential was ever issued: no retry can change
        // that, so keeping the local record buys nothing.
        Ok(false) => report.unrevocable(
            "the stored registration has no management token, so the \
             application cannot be deleted from the server; ask an admin to \
             remove it (Settings -> Applications)"
                .to_string(),
        ),
        // The server refused or was unreachable: a later attempt can still
        // work, and the management token is what makes it possible.
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
        let report = run(
            "default",
            &store,
            Options {
                purge: true,
                force: false,
            },
        )
        .unwrap();
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
        let mut failed = Report {
            registration_deleted: false,
            remote_cleanup_failed: true,
            ..Report::default()
        };
        assert!(
            !drop_registration(
                Options {
                    purge: true,
                    force: false
                },
                &entry,
                &mut failed
            ),
            "a failed purge must keep the credential that manages the orphan"
        );

        let mut confirmed = Report {
            registration_deleted: true,
            ..Report::default()
        };
        assert!(
            drop_registration(
                Options {
                    purge: true,
                    force: false
                },
                &entry,
                &mut confirmed
            ),
            "a confirmed deletion may drop the local record"
        );

        // `--force` discards it anyway, and says the orphan is now
        // permanent so the user can act on it.
        let mut forced = Report {
            remote_cleanup_failed: true,
            ..Report::default()
        };
        assert!(drop_registration(
            Options {
                purge: true,
                force: true
            },
            &entry,
            &mut forced
        ));
        assert!(
            forced.warnings.iter().any(|w| w.contains("ask an admin")),
            "the permanent orphan must be named: {:?}",
            forced.warnings
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
            Options {
                purge: false,
                force: false
            },
            &entry,
            &mut Report::default()
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

        let report = run(
            "default",
            &store,
            Options {
                purge: true,
                force: false,
            },
        )
        .unwrap();
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

    // --- finding [20]: only remove what this run acted on ---------------

    #[test]
    fn a_session_written_after_the_snapshot_survives_logout() {
        // P1 snapshots the profile, then spends time on the network. P2
        // completes a login in the meantime. P1 must not delete P2's
        // session just because its own snapshot had one.
        let acted_on = session();
        let mut current = Some(OAuthSession {
            access_token: "written-by-another-login".to_string(),
            ..session()
        });
        clear_if_unchanged(&mut current, Some(&acted_on), same_session);
        assert!(
            current.is_some(),
            "logout deleted a session another process had just written"
        );

        // The session it did act on is removed as normal.
        let mut current = Some(acted_on.clone());
        clear_if_unchanged(&mut current, Some(&acted_on), same_session);
        assert!(current.is_none());
    }

    #[test]
    fn a_registration_written_after_the_snapshot_keeps_its_management_token() {
        // The dangerous one: P1 deletes C1 on the server, P2 registers C2
        // while P1 waits. Clearing the field afterwards would drop RAT2 -
        // the only credential that can ever delete C2 - leaving an
        // application on the server that nothing can remove.
        let acted_on = dynamic_registration();
        let mut current = Some(ClientRegistration {
            client_id: "registered-by-another-login".to_string(),
            registration_access_token: Some("rat-2".to_string()),
            ..dynamic_registration()
        });
        clear_if_unchanged(&mut current, Some(&acted_on), same_registration);
        assert_eq!(
            current
                .as_ref()
                .and_then(|reg| reg.registration_access_token.as_deref()),
            Some("rat-2"),
            "logout discarded the management token of a newer registration"
        );
    }

    #[test]
    fn a_registration_whose_management_uri_changed_is_not_ours_to_remove() {
        let acted_on = ClientRegistration {
            registration_client_uri: Some("https://docs.example.com/oauth/clients/c".to_string()),
            ..dynamic_registration()
        };
        let mut current = Some(ClientRegistration {
            registration_client_uri: Some("https://docs.example.com/oauth/clients/z".to_string()),
            ..dynamic_registration()
        });
        clear_if_unchanged(&mut current, Some(&acted_on), same_registration);
        assert!(current.is_some());
    }

    #[test]
    fn nothing_is_removed_when_there_was_nothing_to_act_on() {
        // A credential that appeared after an empty snapshot belongs to
        // whoever wrote it.
        let mut current = Some("written-later".to_string());
        clear_if_unchanged(&mut current, None, |a, b| a == b);
        assert_eq!(current.as_deref(), Some("written-later"));
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

    #[test]
    fn logging_out_with_nothing_stored_is_not_an_error() {
        let (_dir, store) = scratch();
        let report = run(
            "default",
            &store,
            Options {
                purge: true,
                force: false,
            },
        )
        .unwrap();
        assert!(!report.had_credentials);
        assert!(report.warnings.is_empty());
    }
}
