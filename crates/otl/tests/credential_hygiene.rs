//! Story 2.6: the credential file's permissions, atomicity, locking and
//! secrecy, as observed from outside the module that implements them.
//!
//! These checks live here rather than next to the code for two reasons:
//! directory enumeration is forbidden under `crates/*/src` by the startup
//! guard, and a few of them run the real binary, which is what a user
//! actually experiences.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use otl::auth::credentials::{CredentialFile, CredentialStore, OAuthSession};
use otl::auth::lock::CredentialLock;
use otl::auth::report;
use otl::auth::secret_file::{self, Permissions};

/// A distinctive value that must never appear in any output.
const SECRET: &str = "TOKEN-SECRET-9c7a";

/// The instance these test credentials are bound to; the commands below are
/// pointed at the same origin.
const INSTANCE: &str = "http://127.0.0.1:9";

/// `otl` with credential state pinned to `config_dir`.
fn otl(config_dir: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("otl"));
    cmd.env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE")
        .env("OUTLINE_URL", INSTANCE)
        .env("OUTLINE_CONFIG_DIR", config_dir)
        .env("OUTLINE_NO_KEY_WARNING", "1");
    cmd
}

/// Store a credential file holding `SECRET` in every field.
fn seed(dir: &Path) -> CredentialStore {
    let store = CredentialStore::at(dir.to_path_buf());
    let mut file = CredentialFile::default();
    let entry = file.profile_mut("default");
    entry.origin = Some(INSTANCE.to_string());
    entry.api_key = Some(SECRET.to_string());
    entry.oauth = Some(OAuthSession {
        access_token: SECRET.to_string(),
        refresh_token: Some(SECRET.to_string()),
        expires_at: Some(otl::auth::oauth::now_unix() + 3600),
        scope: Some("read write".to_string()),
        client_id: SECRET.to_string(),
        token_endpoint: "https://docs.example.com/oauth/token".to_string(),
        revocation_endpoint: None,
        account: Some("Alice <alice@example.com>".to_string()),
        workspace: Some("Acme".to_string()),
    });
    store.save(&file).unwrap();
    store
}

#[test]
fn an_atomic_write_leaves_no_temporary_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.toml");
    secret_file::write_atomic(&path, "a = 1\n").unwrap();
    secret_file::write_atomic(&path, "a = 2\n").unwrap();
    secret_file::write_atomic(&path, "a = 3\n").unwrap();

    let names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["credentials.toml".to_string()],
        "an interrupted-looking temp file survived: {names:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "a = 3\n");
}

#[test]
fn a_replaced_file_is_never_observed_half_written() {
    // The rename is the only publication point, so any reader either sees
    // the whole old content or the whole new content. Simulated by
    // interleaving readers with writers of different lengths.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.toml");
    let short = "v = 1\n";
    let long = format!("v = 2 # {}\n", "x".repeat(4096));
    secret_file::write_atomic(&path, short).unwrap();

    for round in 0..40 {
        let content = if round % 2 == 0 { long.as_str() } else { short };
        secret_file::write_atomic(&path, content).unwrap();
        let observed = fs::read_to_string(&path).unwrap();
        assert!(
            observed == short || observed == long,
            "observed a partial write of {} bytes",
            observed.len()
        );
    }
}

#[cfg(unix)]
#[test]
fn a_temporary_file_is_created_owner_only_too() {
    // A temp file wide open even briefly would be a readable window; the
    // proof is that the renamed result carries the temp file's own bits.
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.toml");
    // A permissive umask must not widen the file either.
    secret_file::write_atomic(&path, "x = 1\n").unwrap();
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "got {mode:04o}");
}

#[cfg(unix)]
#[test]
fn a_command_needing_credentials_refuses_an_over_wide_file_with_a_fix_command() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let store = seed(dir.path());
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();

    let output = otl(dir.path())
        .args(["api", "documents.info", "id=doc-1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("0644"), "mode not reported: {stderr}");
    assert!(
        stderr.contains(&format!("chmod 600 {}", store.path().display())),
        "no fix command: {stderr}"
    );
    assert!(!stderr.contains(SECRET), "credential leaked: {stderr}");
    // Not silently repaired, and not silently used.
    let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o644,
        "permissions were changed behind the user's back"
    );
}

#[cfg(unix)]
#[test]
fn auth_info_reports_an_over_wide_file_instead_of_refusing_to_run() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let store = seed(dir.path());
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o604)).unwrap();

    let output = otl(dir.path())
        .args(["auth", "info", "--offline"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stdout.contains("0604"), "stdout: {stdout}");
    assert!(
        stdout.contains("\"credential_file_usable\": false"),
        "stdout: {stdout}"
    );
    assert!(!stdout.contains(SECRET), "credential leaked: {stdout}");
}

#[test]
fn auth_info_never_prints_a_credential_or_a_fragment_of_one() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let output = otl(dir.path())
        .args(["auth", "info", "--offline"])
        .output()
        .unwrap();
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(0), "{printed}");
    for fragment in [SECRET, "TOKEN-", "SECRET", "9c7a"] {
        assert!(
            !printed.contains(fragment),
            "{fragment:?} appears in auth info output: {printed}"
        );
    }
    // It does say where things are and which kinds exist.
    assert!(printed.contains("credentials.toml"), "{printed}");
    assert!(printed.contains("oauth"), "{printed}");
}

#[test]
fn the_credential_health_report_is_reusable_by_doctor() {
    // `otl doctor` belongs to another epic; this is the seam it must call
    // rather than re-deriving any of it.
    let dir = tempfile::tempdir().unwrap();
    let store = seed(dir.path());
    let health = report::credential_health(&store);
    let rendered = health.lines().join("\n");

    assert!(rendered.contains(&store.path().display().to_string()));
    assert!(health.exists && health.usable);
    assert_eq!(health.profiles.len(), 1);
    assert!(!rendered.contains(SECRET), "{rendered}");
}

#[cfg(not(unix))]
#[test]
fn on_windows_the_report_states_that_protection_rests_on_the_acl() {
    let dir = tempfile::tempdir().unwrap();
    let store = seed(dir.path());
    let rendered = report::credential_health(&store).lines().join("\n");
    assert!(
        rendered.contains("no POSIX permission bits"),
        "Windows must not claim permissions it did not set: {rendered}"
    );
    assert!(rendered.contains("ACL"), "{rendered}");
}

#[cfg(unix)]
#[test]
fn on_unix_the_report_states_the_actual_mode() {
    let dir = tempfile::tempdir().unwrap();
    let store = seed(dir.path());
    let rendered = report::credential_health(&store).lines().join("\n");
    assert!(rendered.contains("0600"), "{rendered}");
    assert!(
        !rendered.contains("no POSIX permission bits"),
        "the Windows caveat must not appear on Unix: {rendered}"
    );
}

#[test]
fn the_platform_permission_description_matches_the_platform() {
    // One assertion that holds on every target: the description never
    // claims a mode the platform cannot express.
    let describe_missing = Permissions::Missing.describe();
    assert!(describe_missing.contains("does not exist"));

    let describe_na = Permissions::NotApplicable.describe();
    assert!(!describe_na.contains("0600"));
}

#[test]
fn concurrent_refreshes_are_serialized_by_the_lock_file() {
    // The property the refresh path depends on: while one holder has the
    // lock, no other can take it, and it becomes free again afterwards.
    let dir = tempfile::tempdir().unwrap();
    let held = CredentialLock::acquire(dir.path()).unwrap();

    let path = Arc::new(dir.path().to_path_buf());
    let contenders: Vec<_> = (0..4)
        .map(|_| {
            let path = Arc::clone(&path);
            std::thread::spawn(move || {
                CredentialLock::acquire_within(&path, Duration::from_millis(80)).is_ok()
            })
        })
        .collect();
    for contender in contenders {
        assert!(
            !contender.join().unwrap(),
            "a second refresh acquired the lock while it was held"
        );
    }

    drop(held);
    CredentialLock::acquire_within(dir.path(), Duration::from_secs(2))
        .expect("the lock must be free once the holder is gone");
}

#[cfg(unix)]
#[test]
fn set_key_stores_a_key_owner_only_and_reports_where() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let mut child = otl(dir.path())
        .args(["auth", "set-key"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"ol_api_TESTKEY_9c7a\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

    let path = dir.path().join("credentials.toml");
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "got {mode:04o}");
    assert!(stdout.contains("credentials.toml"), "stdout: {stdout}");
    assert!(
        !stdout.contains("TESTKEY") && !stderr.contains("TESTKEY"),
        "the key was echoed back: {stdout} {stderr}"
    );
    // And it is the key that got stored.
    let stored = CredentialStore::at(dir.path().to_path_buf())
        .load()
        .unwrap();
    assert_eq!(
        stored.profile("default").unwrap().api_key.as_deref(),
        Some("ol_api_TESTKEY_9c7a")
    );
}

#[test]
fn set_key_with_empty_input_stores_nothing() {
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let output = otl(dir.path())
        .args(["auth", "set-key"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("nothing was stored"), "stderr: {stderr}");
    assert!(
        !dir.path().join("credentials.toml").exists(),
        "a rejected key must not create a credential file"
    );
}

// --- finding [7]: every writer takes the same lock -------------------------

#[test]
fn a_read_modify_write_cannot_clobber_a_concurrent_rotation() {
    // The reported lost update: a writer that snapshots the file, waits,
    // and then saves would put back the token another process has already
    // rotated - and the server has already retired the old one, so the next
    // refresh fails and the user has to log in again.
    //
    // `update` closes that by loading INSIDE the lock: the closure only
    // ever sees current state.
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::at(dir.path().to_path_buf());

    let mut initial = CredentialFile::default();
    let entry = initial.profile_mut("default");
    entry.origin = Some(INSTANCE.to_string());
    entry.oauth = Some(rotating_session("access-1", "refresh-1"));
    store.save(&initial).unwrap();

    // A stale snapshot, taken before the rotation below.
    let stale = store.load().unwrap();
    assert_eq!(
        stale
            .profile("default")
            .unwrap()
            .oauth
            .as_ref()
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("refresh-1")
    );

    // Another process rotates the tokens.
    store
        .update(
            |file: &mut CredentialFile| -> Result<(), otl::auth::error::StoreError> {
                file.profile_mut("default").oauth = Some(rotating_session("access-2", "refresh-2"));
                Ok(())
            },
        )
        .unwrap();

    // Now the first writer adds an API key. It must NOT write `stale` back.
    store
        .update(
            |file: &mut CredentialFile| -> Result<(), otl::auth::error::StoreError> {
                file.profile_mut("default").api_key = Some("added-later".to_string());
                Ok(())
            },
        )
        .unwrap();

    let after = store.load().unwrap();
    let session = after.profile("default").unwrap().oauth.as_ref().unwrap();
    assert_eq!(
        session.refresh_token.as_deref(),
        Some("refresh-2"),
        "a stale snapshot resurrected a refresh token the server retired"
    );
    assert_eq!(session.access_token, "access-2");
    assert_eq!(
        after.profile("default").unwrap().api_key.as_deref(),
        Some("added-later"),
        "the concurrent edit was lost instead"
    );
}

#[test]
fn an_update_runs_under_the_credential_lock() {
    // If `update` did not hold the lock, this nested acquisition would
    // succeed and the transaction guarantee would be fictional.
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::at(dir.path().to_path_buf());
    let observed = store
        .update(
            |_file: &mut CredentialFile| -> Result<bool, otl::auth::error::StoreError> {
                Ok(CredentialLock::acquire_within(dir.path(), Duration::from_millis(60)).is_err())
            },
        )
        .unwrap();
    assert!(observed, "update did not hold the credential lock");
}

/// A session with the given token pair, bound to the test instance.
fn rotating_session(access: &str, refresh: &str) -> OAuthSession {
    OAuthSession {
        access_token: access.to_string(),
        refresh_token: Some(refresh.to_string()),
        expires_at: Some(otl::auth::oauth::now_unix() + 3600),
        scope: Some("read write".to_string()),
        client_id: "client-1".to_string(),
        token_endpoint: "https://docs.example.com/oauth/token".to_string(),
        revocation_endpoint: None,
        account: None,
        workspace: None,
    }
}

#[cfg(unix)]
#[test]
fn set_key_does_not_lose_a_rotation_that_happened_while_it_waited() {
    // End-to-end version of the same race, through the real binary: the key
    // is read from stdin first and the file is re-read under the lock, so a
    // rotation that lands in between survives.
    use std::io::Write;
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::at(dir.path().to_path_buf());
    let mut initial = CredentialFile::default();
    let entry = initial.profile_mut("default");
    entry.origin = Some(INSTANCE.to_string());
    entry.oauth = Some(rotating_session("access-9", "refresh-9"));
    store.save(&initial).unwrap();

    let mut child = otl(dir.path())
        .args(["auth", "set-key"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"ol_api_LATER\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let after = store.load().unwrap();
    let profile = after.profile("default").unwrap();
    assert_eq!(profile.api_key.as_deref(), Some("ol_api_LATER"));
    assert_eq!(
        profile.oauth.as_ref().unwrap().refresh_token.as_deref(),
        Some("refresh-9"),
        "set-key clobbered the stored session"
    );
    // And it recorded which instance the key is for.
    assert_eq!(profile.origin.as_deref(), Some(INSTANCE));
}
