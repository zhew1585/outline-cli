//! Refresh single-flight: an advisory lock on a file next to the
//! credentials.
//!
//! Why a FILE lock and not an in-process mutex: refresh tokens rotate on
//! every use, so two `otl` PROCESSES refreshing at the same time would each
//! spend the same refresh token and one of them would be told
//! `invalid_grant` - after which the credential file holds a token the
//! server has already retired. A process-local mutex cannot prevent that;
//! an advisory lock in the credential directory can.
//!
//! The lock is only ever held around a refresh, never around ordinary
//! requests, so a stuck process cannot block reads.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs4::{FileExt, TryLockError};

use crate::auth::error::StoreError;
use crate::auth::paths::LOCK_FILE_NAME;
use crate::auth::secret_file;

/// How long to wait for another process to finish its refresh.
///
/// Generous compared with a token request (a second or two) but bounded, so
/// a crashed process holding a stale lock cannot hang the CLI forever.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to retry while waiting for the lock.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// An acquired advisory lock. Released when dropped.
#[derive(Debug)]
pub struct RefreshLock {
    file: File,
    path: PathBuf,
}

impl RefreshLock {
    /// Take the refresh lock for the credential directory, waiting up to
    /// [`LOCK_TIMEOUT`] for another process to finish.
    ///
    /// Polls `try_lock` rather than blocking forever: a blocking `flock`
    /// has no timeout, and inheriting one process's crash as a permanent
    /// hang is worse than reporting it.
    pub fn acquire(dir: &Path) -> Result<Self, StoreError> {
        Self::acquire_within(dir, LOCK_TIMEOUT)
    }

    /// [`RefreshLock::acquire`] with an explicit budget (tests).
    pub fn acquire_within(dir: &Path, budget: Duration) -> Result<Self, StoreError> {
        let path = dir.join(LOCK_FILE_NAME);
        let file = secret_file::open_or_create_owner_only(&path)?;
        let deadline = Instant::now() + budget;
        loop {
            // Fully qualified on purpose: `std::fs::File` grew its own
            // inherent `try_lock` in Rust 1.89 and would shadow the trait
            // method. The workspace MSRV is 1.85, so the crate stays the
            // portable choice - and this spelling makes sure it is the one
            // being called.
            match FileExt::try_lock(&file) {
                Ok(()) => return Ok(Self { file, path }),
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
                        "another otl process has held it for more than {}s; \
                         if no other otl is running, delete the file",
                        budget.as_secs()
                    ),
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Path of the lock file, for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RefreshLock {
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
        let held = RefreshLock::acquire(dir.path()).unwrap();
        assert!(held.path().exists());

        // A second attempt inside the same process must not sneak past the
        // lock either: fs4 locks are per-file-handle, so a fresh handle
        // sees the same contention another process would.
        let contended = RefreshLock::acquire_within(dir.path(), Duration::from_millis(120));
        assert!(
            contended.is_err(),
            "a second holder acquired the lock at the same time"
        );

        drop(held);
        RefreshLock::acquire_within(dir.path(), Duration::from_millis(500))
            .expect("the lock must be free again after the holder is dropped");
    }

    #[test]
    fn a_contended_lock_reports_how_to_recover() {
        let dir = tempfile::tempdir().unwrap();
        let _held = RefreshLock::acquire(dir.path()).unwrap();
        let error = RefreshLock::acquire_within(dir.path(), Duration::from_millis(60))
            .expect_err("expected contention");
        let text = error.to_string();
        assert!(text.contains(LOCK_FILE_NAME), "{text}");
        assert!(text.contains("delete the file"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_file_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let held = RefreshLock::acquire(dir.path()).unwrap();
        let mode = std::fs::metadata(held.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, crate::auth::paths::FILE_MODE, "got {mode:04o}");
    }
}
