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
        .map(|error| error.to_string());
    CredentialHealth {
        path: store.path().to_path_buf(),
        exists,
        usable: permissions.usable()
            && directory_problem.is_none()
            && (!exists || loaded.is_some()),
        permissions,
        directory: store.dir().to_path_buf(),
        directory_mode: crate::auth::secret_file::directory_mode(store.dir()),
        directory_problem,
        profiles: loaded.as_ref().map(profiles_of).unwrap_or_default(),
        env_api_key: crate::auth::source::env_api_key().is_some(),
    }
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
        let mut lines = vec![
            format!("credential file:  {}", self.path.display()),
            format!("exists:           {}", yes_no(self.exists)),
            format!("permissions:      {}", self.permissions.describe()),
            format!("directory:        {}", self.describe_directory()),
            format!("usable:           {}", yes_no(self.usable)),
            format!(
                "OUTLINE_API_KEY:  {}",
                if self.env_api_key {
                    "set (plaintext in the environment)"
                } else {
                    "not set"
                }
            ),
        ];
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
        // The report used to claim it covered the directory and did not.
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
