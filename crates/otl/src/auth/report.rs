//! Credential health reporting for `otl auth info` - and for `otl doctor`,
//! which is owned elsewhere and is expected to call
//! [`credential_health`] rather than re-derive any of this.
//!
//! The hard rule this module exists to enforce: a health report states
//! WHERE credentials live, WHETHER they are protected, and WHICH KINDS are
//! present - and never a credential value, nor any fragment of one. There
//! is deliberately no code path here that can reach a token: the report is
//! built from paths, permission bits, presence booleans and labels the
//! server gave us for display.

use std::path::PathBuf;

use crate::auth::credentials::{CredentialFile, CredentialStore};
use crate::auth::secret_file::Permissions;
use crate::auth::source::Method;

/// What kinds of credential a profile holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileHealth {
    /// Profile name.
    pub profile: String,
    /// Whether an OAuth session is stored.
    pub oauth: bool,
    /// Whether a refresh token is stored, so renewal is possible.
    pub renewable: bool,
    /// Whether an API key is stored.
    pub api_key: bool,
    /// Whether a client registration is recorded.
    pub client: bool,
    /// Whether that registration was created by `otl` (and can be purged).
    pub dynamic_client: bool,
    /// Whether the registration can still be deleted from the server.
    pub deletable_client: bool,
}

/// The state of the credential file as a whole.
#[derive(Debug, Clone)]
pub struct CredentialHealth {
    /// Absolute path of the credential file.
    pub path: PathBuf,
    /// Whether the file exists.
    pub exists: bool,
    /// Its permission state, as the platform can express it.
    pub permissions: Permissions,
    /// Whether the file may be used as it stands.
    pub usable: bool,
    /// Whether the FILE's own contents could be read as a credential file.
    /// `true` when there is no file yet: nothing is wrong with an absent one.
    ///
    /// Reported separately from [`CredentialHealth::usable`] because the two
    /// answer different questions. `usable` is "may this store be used as it
    /// stands", which folds in the DIRECTORY around the file; this is about
    /// the file itself. `otl doctor` grades the two differently (a file it
    /// cannot read blocks; a directory other users can write to is a
    /// warning), so it needs them apart - and one cannot be derived from the
    /// other, because a bad directory and a malformed file both leave
    /// `usable` false with nothing to say which of them it was.
    ///
    /// One boolean about whether a parse succeeded. Nothing about content.
    pub file_readable: bool,
    /// Absolute path of the directory holding it.
    pub directory: PathBuf,
    /// Octal mode of the directory, where the platform has one.
    pub directory_mode: Option<String>,
    /// Why the directory is unusable, if it is.
    ///
    /// Reported as well as the file's own state: a directory other users
    /// can write to lets them replace the credential file or the refresh
    /// lock, whatever the file's mode says.
    pub directory_problem: Option<String>,
    /// One entry per profile that has anything stored.
    pub profiles: Vec<ProfileHealth>,
    /// Whether `OUTLINE_API_KEY` is set in this environment.
    pub env_api_key: bool,
}

/// Inspect the credential file without revealing anything in it.
///
/// A file that cannot be read - wrong permissions, corrupt, from a newer
/// version - still produces a report: that is exactly when a user needs
/// one. The failure surfaces as `usable: false` plus the permission state,
/// and the profile list is simply empty.
pub fn credential_health(store: &CredentialStore) -> CredentialHealth {
    let permissions = store.permissions();
    let exists = !matches!(permissions, Permissions::Missing);
    let loaded = store.load().ok();
    // Only reported when the directory is already there: a directory that
    // does not exist yet is not a problem, it is a fresh installation.
    let directory_problem = store
        .dir()
        .is_dir()
        .then(|| crate::auth::secret_file::require_private_dir(store.dir()).err())
        .flatten()
        // The CONDITION, not the error's own verdict: nothing is being
        // refused here, and two of this report's consumers go on to read the
        // file. See `StoreError::condition`.
        .map(|error| error.condition());
    // `exists` is symlink-aware (see `file_guard::permissions`), which is
    // what makes this short-circuit safe: a dangling link is something AT
    // the path, so the `loaded.is_none()` refusal is not skipped.
    let file_readable = !exists || loaded.is_some();
    CredentialHealth {
        path: store.path().to_path_buf(),
        exists,
        usable: permissions.usable() && directory_problem.is_none() && file_readable,
        file_readable,
        permissions,
        directory: store.dir().to_path_buf(),
        directory_mode: crate::auth::secret_file::directory_mode(store.dir()),
        directory_problem,
        profiles: loaded.as_ref().map(profiles_of).unwrap_or_default(),
        env_api_key: global_env_key_is_set(),
    }
}

/// Whether `OUTLINE_API_KEY` is set in this environment.
///
/// PRESENCE only, and never the value. This is a hygiene observation - the
/// report says "there is a plaintext key in your environment", which is worth
/// saying whether or not it is the one in use - and not a credential source:
/// choosing and releasing a key belongs to the config gate, which scopes it
/// to the selected profile. Reading it here to DECIDE anything is the bug
/// this comment exists to prevent.
fn global_env_key_is_set() -> bool {
    std::env::var(crate::config::ENV_API_KEY).is_ok_and(|value| !value.trim().is_empty())
}

/// Summarize every profile in a loaded credential file.
fn profiles_of(file: &CredentialFile) -> Vec<ProfileHealth> {
    file.profiles
        .iter()
        .filter(|(_, entry)| !entry.is_empty())
        .map(|(name, entry)| ProfileHealth {
            profile: name.clone(),
            oauth: entry.oauth.is_some(),
            renewable: entry
                .oauth
                .as_ref()
                .is_some_and(|session| session.refresh_token.is_some()),
            api_key: entry.api_key.is_some(),
            client: entry.client.is_some(),
            dynamic_client: entry
                .client
                .as_ref()
                .is_some_and(|registration| registration.dynamic),
            deletable_client: entry.client.as_ref().is_some_and(|registration| {
                registration.dynamic
                    && registration.registration_access_token.is_some()
                    && registration.registration_client_uri.is_some()
            }),
        })
        .collect()
}

impl ProfileHealth {
    /// The credential kinds this profile holds, for display.
    pub fn kinds(&self) -> Vec<&'static str> {
        let mut kinds = Vec::new();
        if self.oauth {
            kinds.push(if self.renewable {
                "oauth session (renewable)"
            } else {
                "oauth session (not renewable, no refresh token)"
            });
        }
        if self.api_key {
            kinds.push("api key");
        }
        if self.client {
            kinds.push(if self.dynamic_client {
                "client registration (created by otl)"
            } else {
                "client registration (from an administrator)"
            });
        }
        kinds
    }
}

impl CredentialHealth {
    /// The health report as lines suitable for stdout.
    ///
    /// Composed exclusively from paths, booleans and authored labels: there
    /// is no value in scope here that could be a credential.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = self.where_lines();
        lines.push(format!("usable:           {}", yes_no(self.usable)));
        lines.extend(self.what_lines());
        lines
    }

    /// The same lines WITHOUT the store-wide `usable:` verdict.
    ///
    /// For a surface that grades the file and the directory separately and
    /// states its own verdict for each: `otl doctor` warns about a directory
    /// other users can write to and then reads the file anyway, so a
    /// composite "usable: no" in its own detail block would contradict the
    /// connectivity check two lines below it.
    ///
    /// The two renderings share every other line rather than being written
    /// twice, so a field added to this report cannot appear in one and be
    /// forgotten in the other.
    pub fn lines_without_verdict(&self) -> Vec<String> {
        let mut lines = self.where_lines();
        lines.extend(self.what_lines());
        lines
    }

    /// Where the credentials live, and what state that location is in.
    fn where_lines(&self) -> Vec<String> {
        vec![
            format!("credential file:  {}", self.path.display()),
            format!("exists:           {}", yes_no(self.exists)),
            format!("permissions:      {}", self.permissions.describe()),
            format!("directory:        {}", self.describe_directory()),
        ]
    }

    /// What is stored, and what else is lying around in the environment.
    fn what_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "OUTLINE_API_KEY:  {}",
            if self.env_api_key {
                "set (plaintext in the environment)"
            } else {
                "not set"
            }
        )];
        if self.profiles.is_empty() {
            lines.push("stored profiles:  none".to_string());
            return lines;
        }
        for profile in &self.profiles {
            lines.push(format!(
                "profile {}: {}",
                profile.profile,
                profile.kinds().join(", ")
            ));
            if profile.dynamic_client && !profile.deletable_client {
                lines.push(
                    "\x20 warning: this registration has no management token, so \
                     `otl auth logout --purge` cannot delete it from the server"
                        .to_string(),
                );
            }
        }
        lines
    }
}

impl CredentialHealth {
    /// One line about the directory holding the credential file.
    ///
    /// States the ACTUAL mode rather than a reassuring label. The security
    /// criterion is "only the owner can write", which `0755` satisfies -
    /// but calling `0755` "owner-only" would claim a stronger permission
    /// state than the directory has, and a health report that overstates
    /// protection is worse than one that says nothing.
    fn describe_directory(&self) -> String {
        let path = self.directory.display();
        match (&self.directory_problem, &self.directory_mode) {
            (Some(problem), _) => format!("{path} - PROBLEM: {problem}"),
            (None, Some(mode)) => format!("{path} ({mode}, not writable by other users)"),
            (None, None) => format!("{path} (present)"),
        }
    }
}

/// Render a boolean for a report line.
fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

/// Label for the credential a request would actually use.
pub fn method_line(method: Method) -> String {
    format!("method:           {}", method.label())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::auth::credentials::{ClientRegistration, OAuthSession};

    const SECRET: &str = "TOKEN-SECRET-9c7a";

    fn scratch() -> (tempfile::TempDir, CredentialStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::at(dir.path().to_path_buf());
        (dir, store)
    }

    fn populated(store: &CredentialStore) {
        let mut file = CredentialFile::default();
        let entry = file.profile_mut("default");
        entry.api_key = Some(SECRET.to_string());
        entry.oauth = Some(OAuthSession {
            access_token: SECRET.to_string(),
            refresh_token: Some(SECRET.to_string()),
            expires_at: Some(1_900_000_000),
            scope: Some("read write".to_string()),
            client_id: SECRET.to_string(),
            token_endpoint: "https://docs.example.com/oauth/token".to_string(),
            revocation_endpoint: None,
            account: Some("Alice <alice@example.com>".to_string()),
            workspace: Some("Acme".to_string()),
        });
        entry.client = Some(ClientRegistration {
            client_id: SECRET.to_string(),
            client_secret: Some(SECRET.to_string()),
            registration_access_token: Some(SECRET.to_string()),
            registration_client_uri: Some("https://docs.example.com/oauth/clients/1".to_string()),
            redirect_uri: "http://127.0.0.1:41234/callback".to_string(),
            dynamic: true,
            origin: Some("https://docs.example.com".to_string()),
        });
        store.save(&file).unwrap();
    }

    #[test]
    fn the_report_never_contains_a_credential_or_a_fragment_of_one() {
        let (_dir, store) = scratch();
        populated(&store);
        let health = credential_health(&store);
        let rendered = format!("{}\n{health:?}", health.lines().join("\n"));
        assert!(
            !rendered.contains("TOKEN-SECRET"),
            "credential leaked into the health report: {rendered}"
        );
        // Not even a prefix of it.
        assert!(!rendered.contains("TOKEN-"), "{rendered}");
        assert!(!rendered.contains("9c7a"), "{rendered}");
    }

    #[test]
    fn the_report_states_where_credentials_live_and_which_kinds_exist() {
        let (_dir, store) = scratch();
        populated(&store);
        let health = credential_health(&store);
        let rendered = health.lines().join("\n");
        assert!(
            rendered.contains(&store.path().display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("oauth session (renewable)"), "{rendered}");
        assert!(rendered.contains("api key"), "{rendered}");
        assert!(rendered.contains("created by otl"), "{rendered}");
        assert!(health.usable);
        assert!(health.exists);
    }

    #[test]
    fn a_fresh_installation_reports_an_absent_file_rather_than_a_problem() {
        let (_dir, store) = scratch();
        let health = credential_health(&store);
        assert!(!health.exists);
        assert!(health.usable, "a missing file is not a permissions problem");
        assert_eq!(health.permissions, Permissions::Missing);
        assert!(health.lines().iter().any(|line| line.contains("none")));
    }

    #[cfg(unix)]
    #[test]
    fn an_over_wide_file_is_reported_as_unusable_with_its_mode() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, store) = scratch();
        populated(&store);
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let health = credential_health(&store);
        assert!(!health.usable, "a world-readable file must not be usable");
        let rendered = health.lines().join("\n");
        assert!(rendered.contains("0644"), "{rendered}");
        assert!(rendered.contains("TOO OPEN"), "{rendered}");
        assert!(!rendered.contains("TOKEN-SECRET"), "{rendered}");
        // Profiles cannot be listed from a file that must not be read.
        assert!(health.profiles.is_empty());
    }

    #[test]
    fn a_registration_that_cannot_be_deleted_is_called_out() {
        let (_dir, store) = scratch();
        let mut file = CredentialFile::default();
        file.profile_mut("default").client = Some(ClientRegistration {
            client_id: "c".to_string(),
            client_secret: None,
            registration_access_token: None,
            registration_client_uri: None,
            redirect_uri: "http://127.0.0.1:41234/callback".to_string(),
            dynamic: true,
            origin: None,
        });
        store.save(&file).unwrap();

        let health = credential_health(&store);
        assert!(!health.profiles[0].deletable_client);
        let rendered = health.lines().join("\n");
        assert!(
            rendered.contains("cannot delete it from the server"),
            "{rendered}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_other_users_can_write_is_reported_as_a_problem() {
        // A directory others can write to lets them swap the credential
        // file or the refresh lock, whatever the file's own mode says.
        use std::os::unix::fs::PermissionsExt;

        let (dir, store) = scratch();
        populated(&store);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

        let health = credential_health(&store);
        assert!(health.directory_problem.is_some(), "{health:?}");
        assert!(
            !health.usable,
            "a directory anyone can write to must make the store unusable"
        );
        let rendered = health.lines().join("\n");
        assert!(rendered.contains("directory:"), "{rendered}");
        assert!(rendered.contains("PROBLEM"), "{rendered}");
        assert!(rendered.contains("0777"), "{rendered}");
        assert!(!rendered.contains(SECRET), "{rendered}");

        let _ = std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700));
    }

    #[cfg(unix)]
    #[test]
    fn a_0755_directory_is_reported_with_its_real_mode() {
        // Allowed, because others cannot WRITE it - but it is not
        // owner-only, and a health report that overstates protection is
        // worse than one that says nothing.
        use std::os::unix::fs::PermissionsExt;

        let (dir, store) = scratch();
        populated(&store);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let health = credential_health(&store);
        assert!(health.usable, "0755 must remain usable");
        let rendered = health.lines().join("\n");
        assert!(rendered.contains("0755"), "{rendered}");
        assert!(
            !rendered.contains("owner-only"),
            "0755 was described as owner-only: {rendered}"
        );
        assert!(
            rendered.contains("not writable by other users"),
            "{rendered}"
        );

        let _ = std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700));
    }

    #[test]
    fn a_healthy_directory_is_named_without_a_problem() {
        let (_dir, store) = scratch();
        populated(&store);
        let health = credential_health(&store);
        assert!(health.directory_problem.is_none(), "{health:?}");
        let rendered = health.lines().join("\n");
        assert!(
            rendered.contains(&store.dir().display().to_string()),
            "the directory must be named: {rendered}"
        );
        assert!(!rendered.contains("PROBLEM"), "{rendered}");
    }

    /// A file that cannot be parsed is distinguishable from a directory
    /// problem, which is what lets `otl doctor` grade the two differently.
    /// Both leave `usable` false, so `usable` alone cannot tell them apart.
    #[test]
    fn an_unparsable_file_is_reported_as_unreadable_rather_than_as_a_directory_problem() {
        let (_dir, store) = scratch();
        populated(&store);
        // Overwrites the CONTENT and keeps the 0600 mode the store created,
        // so this is a file whose permissions are fine and whose contents
        // are not.
        std::fs::write(store.path(), b"this is not a credential file").unwrap();

        let health = credential_health(&store);
        assert!(!health.file_readable, "{health:?}");
        assert!(!health.usable);
        assert!(health.permissions.usable(), "the mode is still 0600");
        assert!(health.directory_problem.is_none(), "{health:?}");
        assert!(health.profiles.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_sound_file_in_a_writable_directory_is_readable_but_not_usable() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, store) = scratch();
        populated(&store);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

        let health = credential_health(&store);
        // The other half of the pair above: the FILE is fine, the directory
        // is not, and the report says which.
        assert!(health.file_readable, "{health:?}");
        assert!(health.directory_problem.is_some());
        assert!(!health.usable);

        let _ = std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700));
    }

    #[test]
    fn a_missing_file_counts_as_readable() {
        let (_dir, store) = scratch();
        let health = credential_health(&store);
        assert!(
            health.file_readable,
            "there is nothing wrong with a file that does not exist yet"
        );
    }

    /// A dangling symlink is something AT the path that every read refuses,
    /// so the report must not call the path empty and healthy:
    /// `read_checked`'s `O_NOFOLLOW` open fails on such a path.
    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_a_file_problem_and_not_an_absent_file() {
        let (dir, store) = scratch();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), store.path()).unwrap();

        let health = credential_health(&store);
        assert!(health.exists, "something IS at that path: {health:?}");
        assert!(!health.file_readable, "every read of it fails: {health:?}");
        assert!(!health.usable);
        let rendered = health.lines().join("\n");
        assert!(rendered.contains("symbolic link"), "{rendered}");
        assert!(
            !rendered.contains("does not exist yet"),
            "the path is not empty: {rendered}"
        );
    }

    /// A symlink to a perfectly good file is refused too, and for the same
    /// reason: the read path opens `O_NOFOLLOW`. The permission state
    /// describes the LINK, not the target it declines to follow.
    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_sound_file_is_also_a_file_problem() {
        let (dir, store) = scratch();
        let real = dir.path().join("real.toml");
        let inner = CredentialStore::at(dir.path().to_path_buf());
        populated(&inner);
        std::fs::rename(inner.path(), &real).unwrap();
        std::os::unix::fs::symlink(&real, store.path()).unwrap();

        let health = credential_health(&store);
        assert!(health.exists);
        assert!(!health.file_readable, "{health:?}");
        assert!(!health.usable);
        assert!(health.profiles.is_empty(), "nothing may be read from it");
    }

    /// The two renderings differ in exactly one line, and it is the
    /// store-wide verdict: `doctor` grades the file and the directory
    /// separately, so it must not print a composite one.
    #[test]
    fn the_verdict_free_rendering_drops_only_the_usable_line() {
        let (_dir, store) = scratch();
        populated(&store);
        let health = credential_health(&store);

        let full = health.lines();
        let without = health.lines_without_verdict();
        assert_eq!(
            full.len(),
            without.len() + 1,
            "exactly one line differs:\n{full:?}\n{without:?}"
        );
        assert!(full.iter().any(|line| line.starts_with("usable:")));
        assert!(
            !without.iter().any(|line| line.starts_with("usable:")),
            "the composite verdict must be gone: {without:?}"
        );
        // Everything else survives, in order.
        let kept: Vec<&String> = full
            .iter()
            .filter(|line| !line.starts_with("usable:"))
            .collect();
        assert_eq!(kept, without.iter().collect::<Vec<&String>>());
    }

    // --- what a health report may print about a store error ----------
    //
    // These live here rather than beside `StoreError` because the property
    // they protect is THIS module's: `directory_problem` is a description,
    // and a description must not carry the verdict of a path that refuses.
    // `Display` keeps that verdict for the write path, which is where it is
    // true.

    fn too_open() -> crate::auth::error::StoreError {
        crate::auth::error::StoreError::DirectoryTooOpen {
            path: "/home/u/.config/outline-cli".to_string(),
            mode: "0777".to_string(),
        }
    }

    /// The two phrasings of one error, side by side, because the difference
    /// is the whole point: `Display` is the WRITE path refusing, `condition`
    /// is a report describing. A health report that says "refusing to use
    /// it" and then reads the file contradicts itself.
    #[test]
    fn the_display_refuses_and_the_condition_only_describes() {
        let displayed = too_open().to_string();
        assert!(displayed.contains("refusing to use it"), "{displayed}");
        assert!(displayed.contains("chmod 700"), "{displayed}");

        let condition = too_open().condition();
        assert!(
            !condition.contains("refusing"),
            "a description must not carry the write path's verdict: {condition}"
        );
        // The FACTS survive: which directory, what mode, what it means.
        assert!(
            condition.contains("/home/u/.config/outline-cli"),
            "{condition}"
        );
        assert!(condition.contains("0777"), "{condition}");
        assert!(condition.contains("writable by other users"), "{condition}");
    }

    #[test]
    fn the_other_two_directory_conditions_also_drop_the_verdict() {
        let foreign = crate::auth::error::StoreError::ForeignOwner {
            path: "/d".to_string(),
            owner: 501,
            us: 502,
        };
        assert!(!foreign.condition().contains("refusing"), "{foreign}");
        assert!(foreign.condition().contains("501"), "{foreign}");

        let wrong_type = crate::auth::error::StoreError::NotARegularFile {
            path: "/d".to_string(),
            kind: "a directory",
        };
        assert!(!wrong_type.condition().contains("refusing"), "{wrong_type}");
        assert!(
            wrong_type.condition().contains("a directory"),
            "{wrong_type}"
        );
    }

    /// A variant that carries no verdict keeps its own message, so nothing
    /// is lost by describing it.
    #[test]
    fn a_verdict_free_variant_describes_itself() {
        let error = crate::auth::error::StoreError::Parse {
            path: "/d/credentials.toml".to_string(),
            reason: "line 3".to_string(),
        };
        assert_eq!(error.condition(), error.to_string());
    }

    #[test]
    fn profiles_with_nothing_stored_are_not_listed() {
        let (_dir, store) = scratch();
        let mut file = CredentialFile::default();
        file.profile_mut("empty");
        file.profile_mut("real").api_key = Some("k".to_string());
        store.save(&file).unwrap();

        let health = credential_health(&store);
        let names: Vec<_> = health.profiles.iter().map(|p| p.profile.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }
}
