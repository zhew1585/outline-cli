//! The credential-file check: where credentials live, whether they may be
//! used, and what kinds are stored - never what is in them.
//!
//! Split from [`super::checks`], which asks what a REQUEST would do
//! (configuration, instance, credential, reachability). This file asks what
//! the STORE looks like, and the two questions have different answers about
//! the same directory - which is exactly why the grading rule below is worth
//! its own file.
//!
//! The report this consumes is [`crate::auth::report::credential_health`];
//! nothing here re-derives any of it, and nothing here can reach a
//! credential value.

use serde_json::Value;

use crate::auth::credentials::CredentialStore;
use crate::auth::error::StoreError;
use crate::auth::report::{credential_health, CredentialHealth};
use crate::config;
use crate::exit::ExitCode;

use super::report::{optional, Check, Status};

/// How credentials are protected where there are no POSIX permission bits.
///
/// Stated unconditionally on that platform, not only when a credential file
/// happens to exist: "no file yet" is exactly when a user is deciding
/// whether to trust this machine with one. Never printed on Unix, where it
/// would be false.
pub const WINDOWS_PROTECTION_NOTE: &str =
    "on Windows otl sets no permission bits: this platform has none. Protection \
     relies entirely on the per-user ACL of your profile directory, so keep the \
     credential directory inside your own user profile.";

/// The platform note for the credential check, if this platform needs one.
///
/// A runtime `cfg!` rather than `#[cfg]` so that both arms compile
/// everywhere and the note's text is testable on every platform - the
/// pairing itself is asserted by
/// `the_windows_note_appears_exactly_where_it_is_true`.
pub fn protection_note() -> Option<&'static str> {
    cfg!(windows).then_some(WINDOWS_PROTECTION_NOTE)
}

/// The credential FILE: where it is, whether it may be used, and what it
/// holds - never what is in it.
pub fn check(store: &Result<CredentialStore, StoreError>) -> Check {
    let store = match store {
        Ok(store) => store,
        Err(error) => {
            return Check::new(
                "credentials",
                Status::Problem(ExitCode::Usage),
                "the credential store cannot be used",
            )
            .detailed([error.to_string()])
        }
    };
    file_check(&credential_health(store))
}

/// The check for a credential file that could be inspected.
fn file_check(health: &CredentialHealth) -> Check {
    let status = grade(health);
    Check::new("credentials", status.clone(), summary_of(health, &status))
        .fact("credential_file", health.path.display().to_string())
        .fact("credential_file_exists", health.exists)
        .fact("permissions", health.permissions.describe())
        // The FILE's own verdict, deliberately not the store-wide `usable`
        // from `auth::report`: that one folds the directory in, and this
        // check grades the directory separately below. Publishing the
        // composite here would say "not usable" beside a `connectivity`
        // check that went ahead and used it.
        .fact("file_usable", file_is_usable(health))
        .fact("directory", health.directory.display().to_string())
        .fact("directory_problem", optional(&health.directory_problem))
        .fact(
            "profiles_stored",
            health
                .profiles
                .iter()
                .map(|profile| {
                    Value::from(format!(
                        "{}: {}",
                        config::sanitize_name(&profile.profile),
                        profile.kinds().join(", ")
                    ))
                })
                .collect::<Vec<_>>(),
        )
        .fact("plaintext_key_in_environment", health.env_api_key)
        // WITHOUT the store-wide `usable:` line: this check grades the file
        // and the directory separately, and in the directory-only case it is
        // about to be used.
        .detailed(health.lines_without_verdict())
        .detailed(protection_note().map(str::to_string))
}

/// Whether the credential FILE itself may be used.
///
/// The directory around it is a separate question, graded separately by
/// [`grade`].
fn file_is_usable(health: &CredentialHealth) -> bool {
    health.permissions.usable() && health.file_readable
}

/// How the credential file and the directory around it are graded.
///
/// The two are NOT the same verdict, and this is the one place that
/// decides:
///
/// - **the file itself** unusable - permissions widened, not a regular file,
///   a symlink (dangling or not), owned by someone else, malformed, or
///   written by a newer version - is a PROBLEM (exit 2). No command can use
///   it: `secret_file::read_checked` refuses it on the open descriptor, so
///   `doctor` and every other command give the same answer (this agreement
///   holds because [`crate::auth::file_guard::permissions`] uses
///   `symlink_metadata`, i.e. asks the same question the `O_NOFOLLOW` open
///   asks).
/// - **only the directory** being writable by other users, with a sound
///   owner-only file inside it, is a WARNING, and `credential` and
///   `connectivity` go on to use that file.
///
/// Why the warning is the honest grade rather than a downgrade:
///
/// 1. the file is opened `O_NOFOLLOW` and then checked THROUGH the open
///    descriptor by `require_regular_owned`, which demands the caller's own
///    uid, so another user with write access to the directory cannot plant
///    a file this CLI will read;
/// 2. the file is 0600, so they cannot read the credential either;
/// 3. the CLI does not re-permission an EXISTING directory, so refusing to
///    work in one would contradict the code that created it.
///
/// What is left is deletion, or replacement with a file that then gets
/// refused: nuisance and denial of service, not confidentiality or
/// integrity. That is a warning.
///
/// It is still REPORTED, with the directory's actual mode, and a warning
/// cannot change the exit code, which matches `doctor`'s own rule: a
/// world-writable directory makes no other command fail, so it is not
/// blocking.
fn grade(health: &CredentialHealth) -> Status {
    if !file_is_usable(health) {
        return Status::Problem(ExitCode::Usage);
    }
    if health.directory_problem.is_some() {
        return Status::Warn;
    }
    Status::Ok
}

/// The one-line verdict for the credential check.
///
/// Says something the detail block does not repeat: how many profiles hold
/// anything, plus the permission state. The detail is `auth::report`'s own
/// rendering, which this check must not paraphrase.
fn summary_of(health: &CredentialHealth, status: &Status) -> String {
    match status {
        Status::Problem(_) => "the credential file cannot be used as it stands".to_string(),
        // A file that does not exist yet is not "sound": that sentence
        // would read "the file is sound (file does not exist yet)", which
        // says two contradictory things about the same file.
        Status::Warn if !health.exists => {
            "there is no credential file yet, and the directory that would hold one is \
             not private"
                .to_string()
        }
        Status::Warn => format!(
            "the file is sound ({}), but the directory holding it is not",
            health.permissions.describe()
        ),
        _ if health.profiles.is_empty() => {
            format!("nothing stored yet ({})", health.permissions.describe())
        }
        _ => format!(
            "{} profile(s) with stored credentials ({})",
            health.profiles.len(),
            health.permissions.describe()
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::auth::credentials::CredentialFile;
    use crate::auth::secret_file::Permissions;

    /// The platform note is present exactly where it is true. A note that
    /// claimed permission bits on Windows - or promised an ACL on Unix -
    /// would be a health report that lies about protection, which is the one
    /// thing `auth::report` exists to prevent.
    #[test]
    fn the_windows_note_appears_exactly_where_it_is_true() {
        assert_eq!(protection_note().is_some(), cfg!(windows));
        assert!(WINDOWS_PROTECTION_NOTE.contains("ACL"));
        assert!(WINDOWS_PROTECTION_NOTE.contains("sets no permission bits"));
    }

    /// A synthetic health report, so the grading rule can be exercised over
    /// combinations that are awkward to arrange on a real filesystem (a
    /// world-writable directory around a malformed file, say).
    fn health(
        permissions: Permissions,
        file_readable: bool,
        directory: Option<&str>,
    ) -> CredentialHealth {
        CredentialHealth {
            path: std::path::PathBuf::from("/home/u/.config/outline-cli/credentials.toml"),
            exists: true,
            permissions,
            usable: false,
            file_readable,
            directory: std::path::PathBuf::from("/home/u/.config/outline-cli"),
            directory_mode: Some("0700".to_string()),
            directory_problem: directory.map(str::to_string),
            profiles: Vec::new(),
            env_api_key: false,
        }
    }

    fn owner_only() -> Permissions {
        Permissions::OwnerOnly {
            mode: "0600".to_string(),
        }
    }

    fn too_open() -> Permissions {
        Permissions::TooOpen {
            mode: "0644".to_string(),
        }
    }

    /// The grading rule in one place.
    ///
    /// The FILE blocks; the DIRECTORY around a sound file warns. The reasons
    /// are on [`grade`]; this pins the outcomes so that neither half can be
    /// "tidied" into the other.
    #[test]
    fn the_file_blocks_and_a_writable_directory_only_warns() {
        let writable = Some("directory 0777 is writable by other users");

        // A file others can read: no command can use it, so it blocks.
        assert_eq!(
            grade(&health(too_open(), true, None)),
            Status::Problem(ExitCode::Usage)
        );
        // A file that cannot be parsed: same, and this is the case `usable`
        // alone could not distinguish from a directory problem.
        assert_eq!(
            grade(&health(owner_only(), false, None)),
            Status::Problem(ExitCode::Usage)
        );
        // A sound file in a directory others can write: reported, not
        // blocking. `credential` and `connectivity` go on to use it.
        assert_eq!(grade(&health(owner_only(), true, writable)), Status::Warn);
        // Both wrong: the file's verdict wins, because it is the blocking one.
        assert_eq!(
            grade(&health(too_open(), true, writable)),
            Status::Problem(ExitCode::Usage)
        );
        // Nothing wrong at all.
        assert_eq!(grade(&health(owner_only(), true, None)), Status::Ok);
    }

    /// The warning has to SAY what is wrong with the directory, and it must
    /// not describe the file as unusable - the run is about to use it.
    #[test]
    fn a_directory_only_warning_names_the_directory_and_not_the_file() {
        let health = health(owner_only(), true, Some("directory 0777 is writable"));
        let check = file_check(&health);
        assert_eq!(check.status, Status::Warn);
        assert!(
            check.summary.contains("the file is sound"),
            "{}",
            check.summary
        );
        assert!(
            !check.summary.contains("cannot be used"),
            "a file that is about to be used must not be called unusable: {}",
            check.summary
        );
        let facts: Vec<&(&str, serde_json::Value)> = check.facts.iter().collect();
        let fact = |name: &str| {
            facts
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or(Value::Null)
        };
        // The FILE's own verdict, not the composite one: publishing "not
        // usable" here would contradict the connectivity check below it.
        assert_eq!(fact("file_usable"), Value::from(true));
        assert!(fact("directory_problem").is_string(), "{check:?}");
    }

    #[test]
    fn a_credential_store_that_cannot_be_opened_is_a_local_problem() {
        let check = check(&Err(StoreError::NoConfigDir));
        assert_eq!(check.status, Status::Problem(ExitCode::Usage));
        assert!(!check.detail.is_empty(), "the reason must be reported");
    }

    #[test]
    fn a_healthy_credential_file_reports_its_path_and_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::at(dir.path().to_path_buf());
        let mut file = CredentialFile::default();
        file.profile_mut("default").api_key = Some("KEY-SECRET-9c7a".to_string());
        store.save(&file).unwrap();

        let check = check(&Ok(store.clone()));
        assert_eq!(check.status, Status::Ok);
        let rendered = format!("{:?}\n{}", check.facts, check.detail.join("\n"));
        assert!(
            rendered.contains(&store.path().display().to_string()),
            "the credential file must be named: {rendered}"
        );
        assert!(rendered.contains("api key"), "{rendered}");
        assert!(
            !rendered.contains("SECRET-9c7a"),
            "credential leaked: {rendered}"
        );
        // The note about Windows ACLs, and nothing else, differs per
        // platform; on Unix the mode is stated instead.
        assert_eq!(
            check
                .detail
                .iter()
                .any(|line| line.contains("per-user ACL")),
            cfg!(windows),
            "{:?}",
            check.detail
        );
    }

    /// An over-wide credential file must be a PROBLEM that names the file and
    /// its mode, and it must be exit 2 (fix a bit locally), never 4.
    #[cfg(unix)]
    #[test]
    fn an_over_wide_credential_file_is_a_problem_that_names_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::at(dir.path().to_path_buf());
        let mut file = CredentialFile::default();
        file.profile_mut("default").api_key = Some("KEY-SECRET-9c7a".to_string());
        store.save(&file).unwrap();
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();

        let check = check(&Ok(store.clone()));
        assert_eq!(check.status, Status::Problem(ExitCode::Usage));
        let rendered = check.detail.join("\n");
        assert!(
            rendered.contains(&store.path().display().to_string()),
            "the offending file must be named: {rendered}"
        );
        assert!(rendered.contains("0644"), "{rendered}");
        assert!(rendered.contains("chmod 600"), "{rendered}");
        assert!(!rendered.contains("SECRET-9c7a"), "{rendered}");
    }

    /// The remedy is the canonical message, not a copy of it.
    ///
    /// Asserted by EQUALITY against `AuthError::NoCredentials`: that error
    /// already lists every way in, and `auth`'s own
    /// `the_no_credentials_message_names_every_way_in` is what pins the
    /// content. The detail of a check that is ABOUT TO USE the file must not
    /// carry a store-wide "usable: no", and the directory fact must not
    /// claim a refusal that only the write path performs.
    #[test]
    fn a_directory_only_warning_never_claims_the_store_was_refused() {
        let condition = crate::auth::error::StoreError::DirectoryTooOpen {
            path: "/home/u/.config/outline-cli".to_string(),
            mode: "0777".to_string(),
        }
        .condition();
        let check = file_check(&health(owner_only(), true, Some(&condition)));
        assert_eq!(check.status, Status::Warn);

        let detail = check.detail.join("\n");
        assert!(
            !detail.lines().any(|line| line.starts_with("usable:")),
            "a composite verdict in the detail of a check that then uses the \
             file is a contradiction: {detail}"
        );
        assert!(
            !detail.contains("refusing"),
            "the read path refuses nothing here: {detail}"
        );
        // The facts a user needs are all still there.
        assert!(detail.contains("0777"), "{detail}");
        assert!(detail.contains("writable by other users"), "{detail}");
        assert!(detail.contains("credential file:"), "{detail}");
        let problem = check
            .facts
            .iter()
            .find(|(key, _)| *key == "directory_problem")
            .map(|(_, value)| value.to_string())
            .unwrap_or_default();
        assert!(!problem.contains("refusing"), "{problem}");
        assert!(problem.contains("0777"), "{problem}");
    }

    /// A file that does not exist yet must not be called sound.
    #[test]
    fn a_warning_about_the_directory_of_an_absent_file_says_the_file_is_absent() {
        let mut absent = health(Permissions::Missing, true, Some("0777, writable"));
        absent.exists = false;
        let check = file_check(&absent);
        assert_eq!(check.status, Status::Warn);
        assert!(
            !check.summary.contains("sound"),
            "an absent file is not sound: {}",
            check.summary
        );
        assert!(
            check.summary.contains("no credential file yet"),
            "{}",
            check.summary
        );
        assert!(
            !check.summary.contains("does not exist yet)"),
            "the permission phrase does not belong in this sentence: {}",
            check.summary
        );
    }
}
