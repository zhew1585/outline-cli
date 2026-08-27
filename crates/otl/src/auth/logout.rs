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
//!    works when `OUTLINE_URL` is unset, wrong, or a plaintext value -
//!    which is exactly when a user most needs to clean up, and the only
//!    alternative would be deleting the file by hand and orphaning the DCR
//!    registration with it.
//! 3. **Nothing irreversible happens by default.** If a server-side step
//!    could still succeed on a later attempt, the local credentials are
//!    KEPT so that attempt remains possible, and the command exits
//!    non-zero. `--force` is how a user says "I know these cannot be
//!    revoked; discard them anyway".
//!
//! Whatever could not be done is always reported, and the exit code says
//! so: "signed out locally, tokens still live on the server" is not
//! success.

use crate::auth::credentials::{
    ClientRegistration, CredentialFile, CredentialStore, OAuthSession, ProfileCredentials,
};
use crate::auth::logout_remote::{purge_registration, revoke_tokens};
use crate::auth::{endpoint, AuthError};

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
    /// Whether a credential written by ANOTHER process during this logout
    /// is still stored - and therefore was never revoked.
    ///
    /// The revocation requests go out before the lock is taken (they are
    /// slow, and holding the credential lock across them would block every
    /// other `otl`), so a concurrent refresh can land a rotated session in
    /// the file while they are in flight. `clear_if_unchanged` correctly
    /// leaves that session alone - it is not the one this run acted on -
    /// but leaving it alone silently would report "signed out" over a live
    /// bearer session, which is the exact state this command exists to
    /// remove.
    pub survived_concurrent_write: bool,
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
    if keep_for_retry(options, report) {
        return Ok(());
    }
    let drop_client = drop_registration(options, entry, report);
    let mut survivors = Survivors::default();
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
        // Read back INSIDE the transaction: anything still here was written
        // by another process while the revocations were in flight, which
        // means this run never revoked it.
        survivors.session = profile_entry.oauth.is_some();
        survivors.api_key = profile_entry.api_key.is_some();
        if profile_entry.is_empty() {
            // Nothing left for the binding to protect, and a stale one
            // would only confuse the next `auth info`.
            profile_entry.origin = None;
        }
        Ok(())
    })?;
    report.file_removed = !store.path().exists();
    report_survivors(&survivors, report);
    Ok(())
}

/// Whether to leave the credentials in place so a failed step can be tried
/// again, recording why if so.
///
/// Discarding the only copy of a token that is still live on the server
/// makes it permanently unrevocable, so that is never the default.
fn keep_for_retry(options: Options, report: &mut Report) -> bool {
    if !report.retry_could_succeed || options.force {
        return false;
    }
    report.kept_for_retry = true;
    report.warnings.push(
        "the credentials were KEPT on this machine so the failed step can be \
         retried - discarding the only copy of a token that is still live on \
         the server would make it impossible to revoke. Run `otl auth logout` \
         again once the instance is reachable, or `otl auth logout --force` to \
         discard them anyway"
            .to_string(),
    );
    true
}

/// Credentials still stored after the removal transaction.
#[derive(Debug, Default)]
struct Survivors {
    session: bool,
    api_key: bool,
}

/// Turn surviving credentials into warnings and a non-zero exit.
///
/// Never silently accepted: "signed out" printed over a live bearer session
/// is exactly the state this command exists to remove, and a concurrent
/// refresh landing inside the revocation window is just another route to
/// it.
fn report_survivors(survivors: &Survivors, report: &mut Report) {
    if survivors.session {
        report.survived(
            "another process wrote a new OAuth session while this logout was \
             revoking the previous one, so that session is still stored here \
             and has NOT been revoked. Run `otl auth logout` again to revoke \
             and remove it"
                .to_string(),
        );
    }
    if survivors.api_key {
        report.survived(
            "another process stored an API key while this logout was running, \
             so it is still here. Run `otl auth logout` again to remove it"
                .to_string(),
        );
    }
}

impl Report {
    /// Record a server-side step that CANNOT be completed, ever.
    ///
    /// The command still failed to do what was asked, so the exit code says
    /// so - but keeping the credentials would not help, because no retry
    /// can change the answer.
    pub(crate) fn unrevocable(&mut self, warning: String) {
        self.remote_cleanup_failed = true;
        self.warnings.push(warning);
    }

    /// Record a server-side step that a later attempt could still complete.
    ///
    /// Keeps the local credentials by default (see `remove_locally`): they
    /// are the only thing that makes the retry possible.
    pub(crate) fn retryable(&mut self, warning: String) {
        self.remote_cleanup_failed = true;
        self.retry_could_succeed = true;
        self.warnings.push(warning);
    }

    /// Record a credential that outlived this logout because another
    /// process wrote it while the revocations were in flight.
    ///
    /// Not `retryable`: by the time this is known the removal has already
    /// happened, so there is nothing to hold back. It still has to reach
    /// the exit code, because the profile is not signed out.
    pub(crate) fn survived(&mut self, warning: String) {
        self.remote_cleanup_failed = true;
        self.survived_concurrent_write = true;
        self.warnings.push(warning);
    }

    /// Whether this profile's stored session is now revoked AND gone.
    ///
    /// `revoked` on its own only says what this run managed to revoke; a
    /// session another process wrote in the meantime is still sitting
    /// there, unrevoked, and callers must not be told otherwise.
    pub fn signed_out(&self) -> bool {
        self.revoked && !self.survived_concurrent_write
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
