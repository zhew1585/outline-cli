//! Opening a file without the risk of never coming back.
//!
//! `File::open` is not guaranteed to return: opening a FIFO blocks until a
//! writer appears, and an unresponsive network filesystem can block for
//! just as long. Checking the file type first prevents the common case but
//! not the race - the path can change between the check and the open - so
//! both callers that open a path from outside this process go through
//! here.
//!
//! This is deliberately the ONLY place in the runtime that opens a file by
//! path (`tests/startup_guard.rs` enforces that), which keeps the answer to
//! "what can this process open?" a single short list.
//!
//! # The limit of this approach
//!
//! The open runs on another thread and the caller waits on a channel. If
//! the open never returns, the caller gets an error but the WORKER THREAD
//! STAYS BLOCKED until the process exits (or a writer shows up). For a
//! command-line tool that is about to exit with an error, that costs one
//! thread and no file descriptor - the `File`, if it ever materializes, is
//! dropped when the send fails. It would not be acceptable in a
//! long-running process that could accumulate them, and a real fix needs a
//! non-blocking open (`O_NONBLOCK`), whose flag value is platform-specific
//! and would mean a `libc` dependency for one call.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Why a path could not be opened.
#[derive(Debug)]
pub(crate) enum OpenError {
    /// The open failed outright.
    Io(std::io::Error),
    /// The open did not return within the timeout, so the path is not a
    /// plain readable file whatever it claimed to be a moment ago.
    Blocked(Duration),
}

impl OpenError {
    /// A message that names the situation without echoing the path (the
    /// caller has it and decides how to display it).
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Io(error) => error.kind().to_string(),
            Self::Blocked(timeout) => format!(
                "opening it did not complete within {} seconds; it is not a plain \
                 readable file (a pipe with no writer blocks forever, and so can an \
                 unresponsive network filesystem)",
                timeout.as_secs()
            ),
        }
    }
}

/// Open a file, giving up if the open itself does not return in time.
pub(crate) fn open_with_timeout(path: &Path, timeout: Duration) -> Result<File, OpenError> {
    let (sender, receiver) = mpsc::channel();
    let owned: PathBuf = path.to_path_buf();
    thread::spawn(move || {
        // The receiver may already be gone (we timed out); dropping the
        // result here closes the descriptor with it.
        let _ = sender.send(File::open(owned));
    });
    match receiver.recv_timeout(timeout) {
        Ok(Ok(file)) => Ok(file),
        Ok(Err(error)) => Err(OpenError::Io(error)),
        Err(_) => Err(OpenError::Blocked(timeout)),
    }
}

/// Whether two metadata readings describe the same filesystem object.
///
/// Compared by identity (device and inode), not by type: a path swapped
/// between the check and the open may well point at another REGULAR file,
/// which a type comparison would happily accept.
///
/// Windows has no stable equivalent in the standard library, so the check
/// degrades to the type comparison the caller already does. That is a
/// weaker guarantee, and it is stated here rather than pretended away.
#[cfg(unix)]
pub(crate) fn is_same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
pub(crate) fn is_same_file(_left: &std::fs::Metadata, _right: &std::fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn opens_a_regular_file() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        assert!(open_with_timeout(file.path(), Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn reports_a_missing_file_as_io() {
        let error = open_with_timeout(Path::new("/nonexistent/otl/x"), Duration::from_secs(5))
            .expect_err("must fail");
        assert!(matches!(error, OpenError::Io(_)), "{error:?}");
    }

    /// The watchdog is what closes the window between a file-type check and
    /// the open itself.
    #[cfg(unix)]
    #[test]
    fn gives_up_on_an_open_that_blocks() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let fifo = dir.path().join("blocking.pipe");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success());

        let started = std::time::Instant::now();
        let error = open_with_timeout(&fifo, Duration::from_millis(200))
            .expect_err("a pipe with no writer must not open");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "did not give up"
        );
        assert!(matches!(error, OpenError::Blocked(_)), "{error:?}");
        assert!(error.describe().contains("did not complete"));

        // Release the worker thread so this test leaves nothing blocked
        // behind: opening the write end lets its `File::open` return.
        let _writer = std::fs::OpenOptions::new().write(true).open(&fifo);
    }

    #[cfg(unix)]
    #[test]
    fn identifies_the_same_file_by_device_and_inode() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let by_path = std::fs::symlink_metadata(file.path()).unwrap();
        let by_handle = file.as_file().metadata().unwrap();
        assert!(is_same_file(&by_path, &by_handle));

        let other = tempfile::NamedTempFile::new().expect("temp file");
        let other_meta = std::fs::symlink_metadata(other.path()).unwrap();
        assert!(
            !is_same_file(&by_path, &other_meta),
            "two different regular files must not compare equal"
        );
    }
}
