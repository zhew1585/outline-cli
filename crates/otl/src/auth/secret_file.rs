//! The filesystem primitives every credential write and read goes through.
//!
//! Six rules, none of them negotiable (see `project-context.md`):
//!
//! 1. A file holding credentials is CREATED with owner-only permissions.
//!    There is no create-then-`chmod` window, because the permission bits
//!    are part of the `open(2)` call.
//! 2. Reads validate permissions first. A file group- or world-accessible
//!    is refused with a fix command, never silently used and never
//!    silently tightened.
//! 3. Writes are atomic: a temporary sibling in the same directory (also
//!    created owner-only) is written, fsynced, then renamed over the
//!    target, and the directory entry is fsynced too. A killed process
//!    leaves either the old file or the new one, never a half-written one.
//!    `rename` also carries the temp file's own permission bits onto the
//!    destination, so an atomic write can never widen access.
//! 4. Nothing is trusted by PATHNAME. A credential path is opened with
//!    `O_NOFOLLOW | O_NONBLOCK` and then validated on the resulting
//!    DESCRIPTOR: regular file, owned by us, owner-only. That is what makes
//!    a symlinked credential path a refusal instead of a redirect, and a
//!    FIFO a refusal instead of a hang - `open` on a blocking FIFO would
//!    otherwise wait forever, before any check could run.
//! 5. The containing directory is validated the same way, because a
//!    directory another user can write to lets them swap any file inside
//!    it - including the lock file that makes token refresh single-flight.
//! 6. Windows has no POSIX permission bits. Rather than pretend, this
//!    module reports [`Permissions::NotApplicable`] there and the CLI says
//!    out loud that protection rests on the per-user ACL of the profile
//!    directory.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::auth::error::StoreError;
#[cfg(unix)]
use crate::auth::paths::{DIR_MODE, FILE_MODE, FORBIDDEN_MODE_BITS};

/// Maximum accepted size of a credential file.
///
/// The file holds a handful of tokens; anything larger is corruption or an
/// attempt to make the CLI allocate without bound.
pub const MAX_CREDENTIAL_FILE_BYTES: u64 = 1024 * 1024;

/// How many temporary names are tried before giving up.
///
/// The name embeds process id plus randomness, so a collision means either
/// exceptional bad luck or a hostile directory; either way, bounded.
const TEMP_NAME_ATTEMPTS: u32 = 8;

/// Bytes of randomness in a temporary file name.
const TEMP_NAME_BYTES: usize = 8;

/// Directory permission bits that make a credential directory unusable:
/// group- or other-WRITE. Write access is what lets another user swap the
/// credential file or the refresh lock; read access is governed by the
/// files' own 0600, so a conventional 0755 `~/.config` is fine.
#[cfg(unix)]
const DIR_FORBIDDEN_WRITE_BITS: u32 = 0o022;

/// What the platform can say about a credential file's permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permissions {
    /// Unix: only the owner can read or write (no group/other bits).
    OwnerOnly {
        /// Rendered octal mode, e.g. `0600`.
        mode: String,
    },
    /// Unix: someone other than the owner has access.
    TooOpen {
        /// Rendered octal mode, e.g. `0644`.
        mode: String,
    },
    /// The file does not exist yet.
    Missing,
    /// Windows: POSIX permission bits do not exist on this platform.
    NotApplicable,
    /// Metadata could not be read.
    Unknown {
        /// I/O failure description.
        reason: String,
    },
}

impl Permissions {
    /// One line for `auth info` / `doctor`, stating the platform truth
    /// rather than a comfortable fiction.
    pub fn describe(&self) -> String {
        match self {
            Self::OwnerOnly { mode } => format!("{mode} (owner read/write only)"),
            Self::TooOpen { mode } => {
                format!("{mode} - TOO OPEN, other users can read it; run `chmod 600` on it")
            }
            Self::Missing => "file does not exist yet".to_string(),
            Self::NotApplicable => "not applicable on Windows: this platform has no POSIX \
                 permission bits, so otl does not set any. Protection relies \
                 entirely on the per-user ACL of your profile directory."
                .to_string(),
            Self::Unknown { reason } => format!("could not be determined ({reason})"),
        }
    }

    /// Whether the file may be used as-is.
    pub fn usable(&self) -> bool {
        !matches!(self, Self::TooOpen { .. })
    }
}

/// Inspect the permissions of a credential file without reading it.
pub fn permissions(path: &Path) -> Permissions {
    match fs::metadata(path) {
        Ok(metadata) => classify(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Permissions::Missing,
        Err(error) => Permissions::Unknown {
            reason: error.to_string(),
        },
    }
}

/// Classify already-fetched metadata.
#[cfg(unix)]
fn classify(metadata: &fs::Metadata) -> Permissions {
    use std::os::unix::fs::MetadataExt;

    let mode = metadata.mode() & 0o777;
    let rendered = format!("{mode:04o}");
    if mode & FORBIDDEN_MODE_BITS == 0 {
        Permissions::OwnerOnly { mode: rendered }
    } else {
        Permissions::TooOpen { mode: rendered }
    }
}

/// Classify already-fetched metadata (Windows has no bits to classify).
#[cfg(not(unix))]
fn classify(_metadata: &fs::Metadata) -> Permissions {
    Permissions::NotApplicable
}

/// Read a credential file after validating what was actually opened.
///
/// Returns `Ok(None)` when the file does not exist - an absent credential
/// file is the normal state of a fresh installation, not an error.
///
/// Every check runs against the OPENED DESCRIPTOR, never the pathname: the
/// open itself refuses to traverse a symlink and refuses to block on a
/// FIFO, and the descriptor is then required to be a regular file owned by
/// this user with owner-only permissions. A path swapped between check and
/// read therefore cannot get past any of it.
pub fn read_checked(path: &Path) -> Result<Option<String>, StoreError> {
    let file = match open_secret_read(path) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata().map_err(|error| StoreError::Read {
        path: display(path),
        reason: error.to_string(),
    })?;
    require_regular_owned(&metadata, path)?;
    if let Permissions::TooOpen { mode } = classify(&metadata) {
        return Err(StoreError::Permissions {
            path: display(path),
            mode,
        });
    }
    read_capped(file, path)
}

/// Open a credential file for reading, refusing symlinks and blocking opens.
///
/// `Ok(None)` means the file is simply not there.
///
/// Unix uses `O_NOFOLLOW | O_NONBLOCK`:
///
/// - `O_NOFOLLOW` makes a symlinked credential path an error (`ELOOP`)
///   instead of a silent redirect to whatever it points at. Checking with
///   `symlink_metadata` first would leave a window; refusing in the open
///   itself leaves none.
/// - `O_NONBLOCK` matters because `open` on a FIFO with no writer BLOCKS
///   FOREVER, before any permission or type check can run. A 0600 FIFO left
///   at the credential path would hang every command that needs
///   credentials. With `O_NONBLOCK` the open returns and the type check
///   rejects it. For a regular file the flag has no effect on reads.
#[cfg(unix)]
fn open_secret_read(path: &Path) -> Result<Option<File>, StoreError> {
    use rustix::fs::{Mode, OFlags};

    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(fd) => Ok(Some(File::from(fd))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(StoreError::Read {
            path: display(path),
            reason: describe_open_failure(error),
        }),
    }
}

/// Explain an open failure in terms of what it means for a credential path.
#[cfg(unix)]
fn describe_open_failure(error: rustix::io::Errno) -> String {
    match error {
        rustix::io::Errno::LOOP => "it is a symbolic link, and a credential file must be a \
             real file at that exact path (a link could point anywhere)"
            .to_string(),
        rustix::io::Errno::NXIO => "it is a pipe with nothing writing to it, not a credential \
             file"
            .to_string(),
        other => other.to_string(),
    }
}

/// Open a credential file for reading (no Unix open flags available).
///
/// Windows has neither `O_NOFOLLOW` nor FIFOs at a filesystem path, and
/// reparse-point traversal is governed by the directory ACL instead. The
/// regular-file check on the opened handle still applies.
#[cfg(not(unix))]
fn open_secret_read(path: &Path) -> Result<Option<File>, StoreError> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StoreError::Read {
            path: display(path),
            reason: error.to_string(),
        }),
    }
}

/// Require an opened credential file to be a regular file owned by us.
///
/// A directory, device, socket or FIFO at the credential path is never
/// something this program wrote, and neither is a file belonging to another
/// user. Both mean the path is not what it claims to be.
#[cfg(unix)]
fn require_regular_owned(metadata: &fs::Metadata, path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::MetadataExt;

    if !metadata.is_file() {
        return Err(StoreError::NotARegularFile {
            path: display(path),
            kind: describe_kind(metadata),
        });
    }
    let us = rustix::process::geteuid().as_raw();
    if metadata.uid() != us {
        return Err(StoreError::ForeignOwner {
            path: display(path),
            owner: metadata.uid(),
            us,
        });
    }
    Ok(())
}

/// Name the file type for an error message.
#[cfg(unix)]
fn describe_kind(metadata: &fs::Metadata) -> &'static str {
    use std::os::unix::fs::FileTypeExt;

    let kind = metadata.file_type();
    if kind.is_dir() {
        "a directory"
    } else if kind.is_symlink() {
        "a symbolic link"
    } else if kind.is_fifo() {
        "a named pipe"
    } else if kind.is_socket() {
        "a socket"
    } else if kind.is_block_device() || kind.is_char_device() {
        "a device"
    } else {
        "not a regular file"
    }
}

/// Require an opened credential file to be a regular file (Windows).
#[cfg(not(unix))]
fn require_regular_owned(metadata: &fs::Metadata, path: &Path) -> Result<(), StoreError> {
    if metadata.is_file() {
        return Ok(());
    }
    Err(StoreError::NotARegularFile {
        path: display(path),
        kind: if metadata.is_dir() {
            "a directory"
        } else {
            "not a regular file"
        },
    })
}

/// Read at most [`MAX_CREDENTIAL_FILE_BYTES`] from an open file.
fn read_capped(file: File, path: &Path) -> Result<Option<String>, StoreError> {
    let mut text = String::new();
    let read = file
        .take(MAX_CREDENTIAL_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| StoreError::Read {
            path: display(path),
            reason: error.to_string(),
        })?;
    if read as u64 > MAX_CREDENTIAL_FILE_BYTES {
        return Err(StoreError::Read {
            path: display(path),
            reason: format!(
                "the file is larger than {MAX_CREDENTIAL_FILE_BYTES} bytes, \
                 which a credential file never is"
            ),
        });
    }
    Ok(Some(text))
}

/// Create `dir` if needed, then require it to be safe to keep secrets in.
///
/// Only the final component is CREATED with owner-only permissions: the
/// parents are ordinary configuration directories (`~/.config`), and forcing
/// 0700 onto them would be a surprising side effect.
///
/// An existing directory is not re-permissioned - silently changing
/// someone's home directory is overreach - but it IS validated, and the
/// line is drawn at WRITE access. A directory another user can write to
/// lets them replace anything inside it: the credential file, and the lock
/// file that makes token refresh single-flight. Read access to the
/// directory is harmless (the credential file's own 0600 governs that), so
/// the common `~/.config` at 0755 passes untouched while a 0777 or
/// foreign-owned directory is refused.
pub fn ensure_dir(dir: &Path) -> Result<(), StoreError> {
    if !dir.is_dir() {
        create_dir(dir)?;
    }
    require_private_dir(dir)
}

/// Create the directory chain, with owner-only bits on the last component.
fn create_dir(dir: &Path) -> Result<(), StoreError> {
    let failed = |error: std::io::Error| StoreError::Directory {
        path: display(dir),
        reason: error.to_string(),
    };
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            fs::create_dir_all(parent).map_err(failed)?;
        }
    }
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(DIR_MODE);
    }
    match builder.create(dir) {
        Ok(()) => Ok(()),
        // Another process won the race; the directory is what matters.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(failed(error)),
    }
}

/// Refuse a credential directory anyone but its owner can write to.
///
/// Checked on the OPENED directory, so the answer describes the directory
/// the subsequent operations will actually use.
#[cfg(unix)]
pub fn require_private_dir(dir: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::MetadataExt;

    let handle = File::open(dir).map_err(|error| StoreError::Directory {
        path: display(dir),
        reason: error.to_string(),
    })?;
    let metadata = handle.metadata().map_err(|error| StoreError::Directory {
        path: display(dir),
        reason: error.to_string(),
    })?;
    if !metadata.is_dir() {
        return Err(StoreError::NotARegularFile {
            path: display(dir),
            kind: "not a directory",
        });
    }
    let us = rustix::process::geteuid().as_raw();
    if metadata.uid() != us {
        return Err(StoreError::ForeignOwner {
            path: display(dir),
            owner: metadata.uid(),
            us,
        });
    }
    let mode = metadata.mode() & 0o777;
    if mode & DIR_FORBIDDEN_WRITE_BITS != 0 {
        return Err(StoreError::DirectoryTooOpen {
            path: display(dir),
            mode: format!("{mode:04o}"),
        });
    }
    Ok(())
}

/// Windows has no POSIX bits; the per-user profile ACL governs this.
#[cfg(not(unix))]
pub fn require_private_dir(_dir: &Path) -> Result<(), StoreError> {
    Ok(())
}

/// Write `contents` to `path` atomically, owner-only.
///
/// Sequence: create an owner-only temporary sibling, write, fsync, rename
/// over the target, then fsync the directory so the rename itself is
/// durable. Any failure removes the temporary file; the target is either
/// untouched or fully replaced.
///
/// The final directory fsync is NOT best-effort. A refresh token rotates on
/// every use, so the server has already retired the old one by the time
/// this runs: reporting success while the rename may still be lost to a
/// power failure would leave a credential file that reverts to a token the
/// server rejects. If durability cannot be confirmed, the caller has to
/// hear about it.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    ensure_dir(dir)?;
    let (mut file, temp_path) = create_temp(dir, path)?;
    let outcome = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = outcome {
        remove_quietly(&temp_path);
        return Err(StoreError::Write {
            path: display(&temp_path),
            reason: error.to_string(),
        });
    }
    if let Err(error) = fs::rename(&temp_path, path) {
        remove_quietly(&temp_path);
        return Err(StoreError::Write {
            path: display(path),
            reason: error.to_string(),
        });
    }
    sync_dir(dir)
}

/// Create the owner-only temporary sibling an atomic write goes through.
///
/// `create_new` is what makes this safe: the file must not already exist,
/// so a pre-planted symlink or a stale temp file cannot be written through.
fn create_temp(dir: &Path, target: &Path) -> Result<(File, PathBuf), StoreError> {
    let stem = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials");
    let mut last: Option<std::io::Error> = None;
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let candidate = dir.join(format!(".{stem}.tmp.{}.{}", std::process::id(), nonce()?));
        match open_owner_only(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => {
                return Err(StoreError::Write {
                    path: display(&candidate),
                    reason: error.to_string(),
                })
            }
        }
    }
    Err(StoreError::Write {
        path: display(dir),
        reason: match last {
            Some(error) => {
                format!("no free temporary file name after {TEMP_NAME_ATTEMPTS} tries ({error})")
            }
            None => format!("no free temporary file name after {TEMP_NAME_ATTEMPTS} tries"),
        },
    })
}

/// `open(2)` a brand-new file with owner-only permissions from the start.
fn open_owner_only(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Permission bits belong to the open() call: creating the file and
        // then chmod-ing it would leave a window in which it is readable.
        options.mode(FILE_MODE);
    }
    options.open(path)
}

/// Open (creating if needed) an owner-only file for locking purposes.
///
/// The lock file carries no content, but the LOCK IS THE SAFETY PROPERTY:
/// it is what stops two processes from spending the same single-use refresh
/// token. So the same checks a credential file gets apply here too - the
/// opened descriptor must be a regular file owned by this user, with no
/// group or other access - and the containing directory must not be
/// writable by anyone else, which is what `ensure_dir` enforces.
///
/// Without those checks another local user could plant a lock file, or
/// replace one, and two processes would each believe they hold the lock.
pub fn open_or_create_owner_only(path: &Path) -> Result<File, StoreError> {
    if let Some(dir) = path.parent() {
        ensure_dir(dir)?;
    }
    let lock_error = |reason: String| StoreError::Lock {
        path: display(path),
        reason,
    };
    let file = match open_owner_only(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            reopen_existing_lock(path)?
        }
        Err(error) => return Err(lock_error(error.to_string())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| lock_error(error.to_string()))?;
    require_regular_owned(&metadata, path)?;
    if let Permissions::TooOpen { mode } = classify(&metadata) {
        return Err(StoreError::Permissions {
            path: display(path),
            mode,
        });
    }
    Ok(file)
}

/// Reopen a lock file that already exists, refusing a symlinked path.
#[cfg(unix)]
fn reopen_existing_lock(path: &Path) -> Result<File, StoreError> {
    use rustix::fs::{Mode, OFlags};

    let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    rustix::fs::open(path, flags, Mode::empty())
        .map(File::from)
        .map_err(|error| StoreError::Lock {
            path: display(path),
            reason: describe_open_failure(error),
        })
}

/// Reopen a lock file that already exists (no Unix open flags available).
#[cfg(not(unix))]
fn reopen_existing_lock(path: &Path) -> Result<File, StoreError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| StoreError::Lock {
            path: display(path),
            reason: error.to_string(),
        })
}

/// Whether `path` still names the same file as `file`.
///
/// Locks live on an inode, but the pathname is what other processes resolve.
/// If someone unlinks or renames the lock path after we locked it, the next
/// process creates a NEW inode, locks that, and both believe they hold the
/// lock - which is exactly the state the refresh single-flight exists to
/// prevent. Comparing device and inode after acquiring the lock turns that
/// split into a detected error. The directory being owner-writable-only
/// means only this user (or root) can even attempt it.
#[cfg(unix)]
pub fn still_same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(held) = file.metadata() else {
        return false;
    };
    match fs::metadata(path) {
        Ok(named) => named.dev() == held.dev() && named.ino() == held.ino(),
        Err(_) => false,
    }
}

/// Whether `path` still names the same file as `file` (Windows).
///
/// A locked file cannot be deleted or renamed while a handle is open, so the
/// substitution this guards against on Unix is not reachable.
#[cfg(not(unix))]
pub fn still_same_file(_file: &File, _path: &Path) -> bool {
    true
}

/// Delete a file, treating "already gone" as success.
pub fn remove(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::Write {
            path: display(path),
            reason: error.to_string(),
        }),
    }
}

/// Best-effort cleanup of a temporary file on the error path.
fn remove_quietly(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Flush the directory entry so a completed rename survives a crash.
///
/// Unix: failures propagate. `fsync` on a directory descriptor is what
/// makes the rename durable, and a caller that just rotated a single-use
/// refresh token must not be told the write succeeded when it might not
/// have landed.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    let failed = |reason: String| StoreError::Write {
        path: display(dir),
        reason: format!(
            "the file was replaced but the change could not be flushed to \
             disk ({reason}), so it may not survive a crash"
        ),
    };
    let handle = File::open(dir).map_err(|error| failed(error.to_string()))?;
    handle.sync_all().map_err(|error| failed(error.to_string()))
}

/// Flush the directory entry (no-op off Unix).
///
/// Windows has no directory descriptor to fsync: `File::open` on a
/// directory fails outright, so there is nothing to propagate. Its
/// `MoveFileEx` replace is atomic with respect to readers regardless, which
/// is the property the credential file depends on.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), StoreError> {
    Ok(())
}

/// Hex nonce for a temporary file name.
fn nonce() -> Result<String, StoreError> {
    let mut bytes = [0_u8; TEMP_NAME_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| StoreError::Write {
        path: String::new(),
        reason: format!("no randomness available for a temporary file name: {error}"),
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Render a path for an error message.
///
/// Lossy on purpose: a path that is not valid UTF-8 must still produce a
/// usable message rather than a different, worse error.
pub fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A scratch directory that cleans itself up.
    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn write_atomic_creates_the_file_with_owner_only_permissions() {
        let dir = scratch();
        let path = dir.path().join("credentials.toml");
        write_atomic(&path, "version = 1\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "version = 1\n");
        assert!(
            permissions(&path).usable(),
            "fresh credential file is not owner-only: {:?}",
            permissions(&path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_creates_the_file_at_exactly_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        let path = dir.path().join("credentials.toml");
        write_atomic(&path, "x = 1\n").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE, "expected 0600, got {mode:04o}");
    }

    // "an atomic write leaves no temporary file behind" needs directory
    // enumeration, which the startup guard forbids anywhere under
    // `crates/*/src` (a runtime source has no data files to discover). The
    // check therefore lives in `tests/credential_hygiene.rs`.

    #[cfg(unix)]
    #[test]
    fn rewriting_a_wide_open_file_narrows_it_back_to_0600() {
        // rename(2) carries the temp file's own bits, so an atomic write
        // can never leave the destination wider than it created it.
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        let path = dir.path().join("credentials.toml");
        write_atomic(&path, "a = 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        write_atomic(&path, "a = 2\n").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE, "expected 0600, got {mode:04o}");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dir_creates_the_final_component_at_0700() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        let nested = dir.path().join("parent").join("outline-cli");
        ensure_dir(&nested).unwrap();
        let mode = fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, DIR_MODE, "expected 0700, got {mode:04o}");
    }

    #[test]
    fn ensure_dir_is_idempotent() {
        let dir = scratch();
        ensure_dir(dir.path()).unwrap();
        ensure_dir(dir.path()).unwrap();
    }

    #[test]
    fn read_checked_reports_a_missing_file_as_absent_not_as_an_error() {
        let dir = scratch();
        let path = dir.path().join("credentials.toml");
        assert_eq!(read_checked(&path).unwrap(), None);
        assert_eq!(permissions(&path), Permissions::Missing);
    }

    #[cfg(unix)]
    #[test]
    fn read_checked_refuses_a_group_readable_file_with_a_fix_command() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        let path = dir.path().join("credentials.toml");
        write_atomic(&path, "secret = \"tok\"\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        let error = read_checked(&path).expect_err("a group-readable file must be refused");
        let text = error.to_string();
        assert!(text.contains("0640"), "mode missing: {text}");
        assert!(text.contains("chmod 600"), "fix command missing: {text}");
        assert!(!text.contains("tok"), "content leaked: {text}");
        // And it is NOT auto-repaired.
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "permissions were silently changed");
    }

    #[cfg(unix)]
    #[test]
    fn read_checked_refuses_every_flavour_of_over_wide_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        for mode in [0o604, 0o660, 0o644, 0o666, 0o700 | 0o001] {
            let path = dir.path().join(format!("credentials-{mode:o}.toml"));
            write_atomic(&path, "x = 1\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(read_checked(&path).is_err(), "mode {mode:04o} was accepted");
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_checked_accepts_a_file_stricter_than_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        let path = dir.path().join("credentials.toml");
        write_atomic(&path, "x = 1\n").unwrap();
        // Read-only for the owner is stricter, not wider: still usable.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(read_checked(&path).unwrap().is_some());
    }

    #[test]
    fn read_checked_refuses_an_oversized_file() {
        let dir = scratch();
        let path = dir.path().join("credentials.toml");
        let huge = "#".repeat(MAX_CREDENTIAL_FILE_BYTES as usize + 1);
        write_atomic(&path, &huge).unwrap();
        let error = read_checked(&path).expect_err("an oversized file must be refused");
        assert!(error.to_string().contains("larger than"), "{error}");
    }

    // --- finding [15]: what is actually at the path --------------------

    #[cfg(unix)]
    #[test]
    fn a_symlinked_credential_path_is_refused_not_followed() {
        // A link is not a credential file. Following it would read - and,
        // on the next write, replace by rename - whatever it points at.
        let dir = scratch();
        let decoy = dir.path().join("decoy.toml");
        write_atomic(&decoy, "api_key = \"decoy\"\n").unwrap();
        let path = dir.path().join("credentials.toml");
        std::os::unix::fs::symlink(&decoy, &path).unwrap();

        let error = read_checked(&path).expect_err("a symlinked path must be refused");
        let text = error.to_string();
        assert!(text.contains("symbolic link"), "{text}");
        assert!(!text.contains("decoy"), "target content leaked: {text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_at_the_credential_path_is_refused_without_hanging() {
        // Without O_NONBLOCK, `open` on a FIFO with no writer blocks
        // forever - before any permission or type check could run - and
        // every command needing credentials would hang. The test itself
        // would hang too if that regressed.
        let dir = scratch();
        let path = dir.path().join("credentials.toml");
        // Owner-only, so the permission check alone would wave it through:
        // only the file-TYPE check can reject it. Created with mkfifo(1)
        // because this is a test-only need.
        let made = std::process::Command::new("mkfifo")
            .arg("-m")
            .arg("600")
            .arg(&path)
            .status();
        match made {
            Ok(status) if status.success() => {}
            // No mkfifo on this platform: nothing to assert about fifos.
            _ => return,
        }

        let error = read_checked(&path).expect_err("a fifo must be refused");
        assert!(
            error.to_string().contains("pipe"),
            "expected the type to be named: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_at_the_credential_path_is_refused() {
        let dir = scratch();
        let path = dir.path().join("credentials.toml");
        fs::create_dir(&path).unwrap();
        let error = read_checked(&path).expect_err("a directory must be refused");
        assert!(error.to_string().contains("directory"), "{error}");
    }

    // --- finding [11]: the containing directory ------------------------

    #[cfg(unix)]
    #[test]
    fn a_world_writable_credential_directory_is_refused() {
        // Write access to the directory is enough to swap the credential
        // file or the refresh lock, whatever the files' own modes say.
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let error = ensure_dir(dir.path()).expect_err("a world-writable directory is unsafe");
        let text = error.to_string();
        assert!(text.contains("0777"), "{text}");
        assert!(text.contains("chmod 700"), "{text}");
        let _ = fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700));
    }

    #[cfg(unix)]
    #[test]
    fn a_group_writable_credential_directory_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o770)).unwrap();
        assert!(ensure_dir(dir.path()).is_err());
        let _ = fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700));
    }

    #[cfg(unix)]
    #[test]
    fn a_conventional_world_readable_config_directory_is_fine() {
        // `~/.config` is 0755 nearly everywhere. Read access to the
        // directory reveals no credential - the files are 0600 - so
        // refusing it would break the tool for no security gain.
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            ensure_dir(dir.path()).is_ok(),
            "a 0755 directory must remain usable"
        );
        let _ = fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700));
    }

    // --- finding [10]: durability is reported --------------------------

    #[cfg(unix)]
    #[test]
    fn a_write_into_an_unwritable_directory_reports_failure() {
        // Whatever goes wrong, `write_atomic` must not return Ok: a caller
        // that just rotated a single-use refresh token relies on that, and
        // silence would leave a credential file holding a dead token.
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch();
        let nested = dir.path().join("locked");
        fs::create_dir(&nested).unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o500)).unwrap();

        let outcome = write_atomic(&nested.join("credentials.toml"), "x = 1\n");
        let _ = fs::set_permissions(&nested, fs::Permissions::from_mode(0o700));
        assert!(outcome.is_err(), "an unwritable directory reported success");
    }

    #[test]
    fn remove_treats_an_absent_file_as_removed() {
        let dir = scratch();
        remove(&dir.path().join("nope.toml")).unwrap();
    }

    #[test]
    fn permission_description_for_windows_does_not_claim_bits_were_set() {
        let text = Permissions::NotApplicable.describe();
        assert!(text.contains("no POSIX permission bits"), "{text}");
        assert!(text.contains("ACL"), "{text}");
        assert!(
            !text.contains("0600"),
            "claims a mode it did not set: {text}"
        );
    }
}
