//! The filesystem primitives every credential write and read goes through.
//!
//! Four rules, none of them negotiable (see `project-context.md`):
//!
//! 1. A file holding credentials is CREATED with owner-only permissions.
//!    There is no create-then-`chmod` window, because the permission bits
//!    are part of the `open(2)` call.
//! 2. Reads validate permissions first. A file group- or world-accessible
//!    is refused with a fix command, never silently used and never
//!    silently tightened.
//! 3. Writes are atomic: a temporary sibling in the same directory (also
//!    created owner-only) is written, fsynced, then renamed over the
//!    target. A killed process leaves either the old file or the new one,
//!    never a half-written one. `rename` also carries the temp file's own
//!    permission bits onto the destination, so an atomic write can never
//!    widen access.
//! 4. Windows has no POSIX permission bits. Rather than pretend, this
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

/// Read a credential file after validating its permissions.
///
/// Returns `Ok(None)` when the file does not exist - an absent credential
/// file is the normal state of a fresh installation, not an error.
///
/// The permission check runs against the OPENED file (`fstat`, not `stat`),
/// so a path swapped between check and read cannot get past it.
pub fn read_checked(path: &Path) -> Result<Option<String>, StoreError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(StoreError::Read {
                path: display(path),
                reason: error.to_string(),
            })
        }
    };
    let metadata = file.metadata().map_err(|error| StoreError::Read {
        path: display(path),
        reason: error.to_string(),
    })?;
    if let Permissions::TooOpen { mode } = classify(&metadata) {
        return Err(StoreError::Permissions {
            path: display(path),
            mode,
        });
    }
    read_capped(file, path)
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

/// Create `dir` (and any missing parents) if it is not there yet.
///
/// Only the final component is created with owner-only permissions: the
/// parents are ordinary configuration directories (`~/.config`), and
/// forcing 0700 onto them would be a surprising side effect. An existing
/// directory is left exactly as the user has it - reporting is the job of
/// `auth info`, silently re-permissioning someone's home directory is not.
pub fn ensure_dir(dir: &Path) -> Result<(), StoreError> {
    if dir.is_dir() {
        return Ok(());
    }
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

/// Write `contents` to `path` atomically, owner-only.
///
/// Sequence: create an owner-only temporary sibling, write, fsync, rename
/// over the target, then fsync the directory so the rename itself is
/// durable. Any failure removes the temporary file; the target is either
/// untouched or fully replaced.
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
    sync_dir(dir);
    Ok(())
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
/// The lock file carries no content, but it lives next to the credentials
/// and is created with the same permissions so a directory listing cannot
/// be used to learn which profiles exist.
pub fn open_or_create_owner_only(path: &Path) -> Result<File, StoreError> {
    if let Some(dir) = path.parent() {
        ensure_dir(dir)?;
    }
    match open_owner_only(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| StoreError::Lock {
                path: display(path),
                reason: error.to_string(),
            }),
        Err(error) => Err(StoreError::Lock {
            path: display(path),
            reason: error.to_string(),
        }),
    }
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

/// Best-effort directory fsync, so a completed rename survives a crash.
///
/// Advisory: a platform that cannot open a directory as a file (Windows)
/// simply skips it, and the rename is still atomic.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
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
