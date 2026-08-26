//! What is allowed to hold credentials: permissions, file type, ownership,
//! and the directory around them.
//!
//! Split out of [`crate::auth::secret_file`], which owns the read/write
//! primitives. The rule these checks exist to enforce is that NOTHING is
//! trusted by pathname: every judgement here is made about an already-open
//! descriptor, so a path swapped between the check and the use cannot get
//! past it.

use std::fs::{self, DirBuilder, File};
use std::path::Path;

use crate::auth::error::StoreError;
#[cfg(unix)]
use crate::auth::paths::{DIR_MODE, FORBIDDEN_MODE_BITS};
use crate::auth::secret_file::display;

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
pub(crate) fn classify(metadata: &fs::Metadata) -> Permissions {
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
pub(crate) fn classify(_metadata: &fs::Metadata) -> Permissions {
    Permissions::NotApplicable
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

/// The directory's octal mode, where the platform has one.
///
/// Reported rather than judged: [`require_private_dir`] decides usability,
/// this exists so a health report can state the real mode instead of a
/// reassuring label.
#[cfg(unix)]
pub fn directory_mode(dir: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(dir).ok()?;
    Some(format!("{:04o}", metadata.mode() & 0o777))
}

/// The directory's octal mode (Windows has none).
#[cfg(not(unix))]
pub fn directory_mode(_dir: &Path) -> Option<String> {
    None
}
