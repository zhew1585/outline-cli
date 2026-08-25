//! The credential-file transaction lock.
//!
//! Why a FILE lock and not an in-process mutex: refresh tokens rotate on
//! every use, so two `otl` PROCESSES touching the credential file at the
//! same time can each spend the same refresh token, or one can write back a
//! stale snapshot over the other's rotation. Either way the file ends up
//! holding a token the server has already retired. A process-local mutex
//! cannot prevent that; an advisory lock in the credential directory can.
//!
//! **Every read-modify-write of the credential file takes this lock**, not
//! just token refresh. `set-key`, `login`, `logout` and refresh all mutate
//! the same file, and a lock that only one of them respects is not a lock.
//! [`crate::auth::credentials::CredentialStore::update`] is the way to do
//! it; this module is the primitive underneath.
//!
//! The lock is taken around the read-modify-write, never around waiting for
//! a human or a browser. Holding it across interactive input would let one
//! forgotten terminal block every other `otl` process.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs4::{FileExt, TryLockError};

use crate::auth::error::StoreError;
use crate::auth::paths::LOCK_FILE_NAME;
use crate::auth::secret_file;

/// How long to wait for another process to finish its transaction.
///
/// Generous compared with a token request (a second or two) but bounded, so
/// a crashed process holding a stale lock cannot hang the CLI forever.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to retry while waiting for the lock.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// An acquired advisory lock. Released when dropped.
#[derive(Debug)]
pub struct CredentialLock {
    file: File,
    path: PathBuf,
}

impl CredentialLock {
    /// Take the credential lock for `dir`, waiting up to [`LOCK_TIMEOUT`]
    /// for another process to finish.
    ///
    /// Polls `try_lock` rather than blocking forever: a blocking `flock`
    /// has no timeout, and inheriting one process's crash as a permanent
    /// hang is worse than reporting it.
    pub fn acquire(dir: &Path) -> Result<Self, StoreError> {
        Self::acquire_within(dir, LOCK_TIMEOUT)
    }

    /// [`CredentialLock::acquire`] with an explicit budget (tests).
    pub fn acquire_within(dir: &Path, budget: Duration) -> Result<Self, StoreError> {
        let path = dir.join(LOCK_FILE_NAME);
        // Validated on the opened descriptor: regular file, owned by us,
        // owner-only, in a directory no other user can write to. Without
        // that, a planted lock file makes the lock meaningless.
        let file = secret_file::open_or_create_owner_only(&path)?;
        let deadline = Instant::now() + budget;
        loop {
            match FileExt::try_lock(&file) {
                Ok(()) => return Self::verify(file, path),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => {
                    return Err(StoreError::Lock {
                        path: secret_file::display(&path),
                        reason: error.to_string(),
                    })
                }
            }
            if Instant::now() >= deadline {
                return Err(StoreError::Lock {
                    path: secret_file::display(&path),
                    reason: format!(
                        "another otl process has held it for more than {}s. \
                         Wait for it to finish, or check for a stuck otl \
                         process and stop it. Do NOT delete the lock file \
                         while another otl is running: a new process would \
                         then lock a different file and both would believe \
                         they hold the lock",
                        budget.as_secs()
                    ),
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Confirm the locked descriptor is still what the path names.
    ///
    /// The lock lives on an inode; other processes find it by pathname. If
    /// the path was unlinked or replaced between opening and locking, the
    /// next process would create a fresh inode, lock that, and both would
    /// think they hold the lock - the exact split the lock exists to
    /// prevent. This turns that race into a reported error.
    fn verify(file: File, path: PathBuf) -> Result<Self, StoreError> {
        if secret_file::still_same_file(&file, &path) {
            return Ok(Self { file, path });
        }
        Err(StoreError::Lock {
            path: secret_file::display(&path),
            reason: "it was replaced while being locked, so the lock cannot \
                     be trusted to be exclusive; retry"
                .to_string(),
        })
    }

    /// Path of the lock file, for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CredentialLock {
    /// Release explicitly rather than relying on the descriptor closing:
    /// the intent is clearer, and a failure here has nothing left to
    /// report to.
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let held = CredentialLock::acquire(dir.path()).unwrap();
        assert!(held.path().exists());

        // A second attempt inside the same process must not sneak past the
        // lock either: fs4 locks are per-file-handle, so a fresh handle
        // sees the same contention another process would.
        let contended = CredentialLock::acquire_within(dir.path(), Duration::from_millis(120));
        assert!(
            contended.is_err(),
            "a second holder acquired the lock at the same time"
        );

        drop(held);
        CredentialLock::acquire_within(dir.path(), Duration::from_millis(500))
            .expect("the lock must be free again after the holder is dropped");
    }

    #[test]
    fn a_contended_lock_never_advises_deleting_the_lock_file() {
        // Deleting a held lock file splits the lock domain: the next
        // process locks a new inode and both proceed. The message must not
        // suggest it.
        let dir = tempfile::tempdir().unwrap();
        let _held = CredentialLock::acquire(dir.path()).unwrap();
        let error = CredentialLock::acquire_within(dir.path(), Duration::from_millis(60))
            .expect_err("expected contention");
        let text = error.to_string();
        assert!(text.contains(LOCK_FILE_NAME), "{text}");
        assert!(
            !text.contains("delete the file"),
            "still advising deletion: {text}"
        );
        assert!(text.contains("Do NOT delete the lock file"), "{text}");
        assert!(text.contains("stuck otl process"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_file_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let held = CredentialLock::acquire(dir.path()).unwrap();
        let mode = std::fs::metadata(held.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, crate::auth::paths::FILE_MODE, "got {mode:04o}");
    }

    #[cfg(unix)]
    #[test]
    fn a_pre_planted_over_wide_lock_file_is_refused() {
        // Another user who can write the directory could leave a lock file
        // they also hold; using it would make the lock a no-op.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE_NAME);
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();

        let error = CredentialLock::acquire(dir.path())
            .expect_err("a world-writable lock file must be refused");
        assert!(error.to_string().contains("0666"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_lock_path_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("decoy");
        std::fs::write(&elsewhere, b"").unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.path().join(LOCK_FILE_NAME)).unwrap();

        let error =
            CredentialLock::acquire(dir.path()).expect_err("a symlinked lock must be refused");
        assert!(error.to_string().contains("symbolic link"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_world_writable_credential_directory_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = CredentialLock::acquire(dir.path())
            .expect_err("a directory others can write must be refused");
        let text = error.to_string();
        assert!(text.contains("0777"), "{text}");
        assert!(text.contains("chmod 700"), "{text}");
        // Restore so the temp dir can be cleaned up.
        let _ = std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700));
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_replaced_underneath_us_is_detected() {
        // Simulates the substitution attack: the pathname is repointed at a
        // different inode after the lock was taken.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE_NAME);
        let held = CredentialLock::acquire(dir.path()).unwrap();
        assert!(secret_file::still_same_file(&held.file, &path));

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert!(
            !secret_file::still_same_file(&held.file, &path),
            "a replaced lock path went undetected"
        );
    }
}
