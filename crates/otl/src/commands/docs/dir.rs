//! The output directory of an export, and what can be known about it.
//!
//! A [`Dir`] is a directory PINNED to the entry it named when it was
//! opened, so the export can notice later that the path now leads
//! somewhere else. Pinning is not locking - see [`super::target`] for what
//! that does and does not buy - but it turns a swapped directory from a
//! silent redirect into a reported failure.
//!
//! Filesystem identities live here too, because both this module and the
//! write path need them and they must mean the same thing in both:
//! `None` is "this platform has no identities", never "different".

use std::path::{Path, PathBuf};

/// The filesystem identity of a file or directory.
///
/// `None` means "not available on this platform", never "different":
/// callers must treat an absent identity as no information rather than as a
/// mismatch, and must not report a guarantee they could not check.
pub type FileId = (u64, u64);

/// The identity of whatever `path` names right now.
pub(super) fn identity(path: &Path) -> Option<FileId> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    identity_of(&metadata)
}

/// The identity of an OPEN file, taken from its handle.
///
/// No path is consulted, so this cannot be redirected by anything happening
/// to the directory in the meantime. It is the identity of the file this
/// process is holding, which is what makes it usable as proof later.
pub(super) fn identity_of_handle(file: &std::fs::File) -> Option<FileId> {
    identity_of(&file.metadata().ok()?)
}

/// Pull the identity out of metadata, where the platform has one.
fn identity_of(metadata: &std::fs::Metadata) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        Some((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        // Windows exposes a file index only through an unstable std API.
        let _ = metadata;
        None
    }
}

/// Whether a directory's entries were actually flushed to disk.
///
/// A separate value rather than a bare `Ok` so that "flushed" and "could not
/// be flushed on this platform" cannot be confused for each other by a
/// caller reporting durability to a backup script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// The directory's entries were fsynced.
    Flushed,
    /// This platform offers no way to flush a directory through the
    /// standard library, so nothing can be claimed about whether the names
    /// written survive a crash.
    ///
    /// Only constructed off Unix; the allow keeps the Unix build from
    /// warning about a variant that is real on another target rather than
    /// tempting someone to delete it.
    #[cfg_attr(unix, allow(dead_code))]
    Unconfirmed,
}

/// A directory of the output tree, pinned to the entry it named when it was
/// opened.
#[derive(Debug, Clone)]
pub struct Dir {
    path: PathBuf,
    /// Identity at open time, when the platform exposes one.
    id: Option<(u64, u64)>,
}

impl Dir {
    /// Pin an existing real directory.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect the directory: {}", error.kind()))?;
        if !metadata.file_type().is_dir() {
            return Err("the export directory is not a directory".to_string());
        }
        let id = identity(&path);
        Ok(Self { path, id })
    }

    /// The directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Re-check that this path still names the directory that was pinned.
    ///
    /// Called before every write and again when the directory is flushed,
    /// so that the LAST document written into a directory is covered too -
    /// without the check at flush time, a swap performed after the final
    /// write would never be looked at.
    pub fn verify(&self) -> Result<(), String> {
        let metadata = std::fs::symlink_metadata(&self.path)
            .map_err(|error| format!("the target directory is gone: {}", error.kind()))?;
        if !metadata.file_type().is_dir() {
            return Err("the target directory was replaced by a file or symlink".to_string());
        }
        match (self.id, identity(&self.path)) {
            (Some(pinned), Some(current)) if pinned != current => {
                Err("the target directory was replaced since the export started".to_string())
            }
            _ => Ok(()),
        }
    }

    /// Create (or accept) a subdirectory and pin it.
    ///
    /// `create_dir` is used rather than `create_dir_all` so nothing is ever
    /// created *through* a link, an existing entry is accepted only when it
    /// is a real directory, and the resolved path must still be inside
    /// `root`.
    pub fn child(&self, root: &Path, name: &str) -> Result<Self, String> {
        self.verify()?;
        let path = self.path.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                    format!("cannot inspect the target directory: {}", error.kind())
                })?;
                if !metadata.file_type().is_dir() {
                    return Err(
                        "a symlink or file already occupies the target directory".to_string()
                    );
                }
            }
            Err(error) => return Err(format!("cannot create the directory: {}", error.kind())),
        }
        let resolved = std::fs::canonicalize(&path)
            .map_err(|error| format!("cannot resolve the directory: {}", error.kind()))?;
        if !resolved.starts_with(root) {
            return Err("the directory resolves outside the export directory".to_string());
        }
        Self::open(path)
    }

    /// Flush this directory's entries to disk, re-verifying it first.
    ///
    /// `sync_all` on a file only promises that its CONTENT survives a
    /// crash; the directory entry that gives it a name is separate
    /// metadata. Without this, a power loss right after a successful export
    /// can leave the backup missing files whose data was already durable.
    ///
    /// Returns [`Durability::Unconfirmed`] on platforms that cannot flush a
    /// directory, so the caller reports the gap rather than silently
    /// treating it as a success. The verification happens everywhere.
    pub fn sync(&self) -> Result<Durability, String> {
        self.verify()?;
        flush_directory(&self.path)
    }
}

/// Flush one directory's entries to disk.
///
/// Used for the output directory itself and for the ancestors that had to
/// be created to reach it - the name of a directory lives in its PARENT, so
/// flushing only the directory that holds the documents would leave the
/// directory holding that directory unflushed.
pub fn flush_directory(path: &Path) -> Result<Durability, String> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)
            .and_then(|dir| dir.sync_all())
            .map(|()| Durability::Flushed)
            .map_err(|error| format!("cannot flush the directory: {}", error.kind()))
    }
    #[cfg(not(unix))]
    {
        // No way to obtain a directory handle through the standard library,
        // so nothing can be flushed and nothing may be claimed. Reported
        // rather than returned as a success: a Windows branch that pretends
        // to have done the work is exactly the failure mode this project
        // forbids.
        let _ = path;
        Ok(Durability::Unconfirmed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_recreated_directory_no_longer_verifies() {
        // Same path, different inode: the pin is what notices.
        let outer = tempfile::tempdir().unwrap();
        let path = outer.path().join("dir");
        std::fs::create_dir(&path).unwrap();
        let target = Dir::open(path.clone()).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        if cfg!(unix) {
            let error = target.verify().unwrap_err();
            assert!(error.contains("replaced"), "{error}");
        }
    }

    #[test]
    fn a_directory_replaced_by_a_file_is_reported() {
        let outer = tempfile::tempdir().unwrap();
        let path = outer.path().join("dir");
        std::fs::create_dir(&path).unwrap();
        let target = Dir::open(path.clone()).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, "not a directory").unwrap();
        let error = target.verify().unwrap_err();
        assert!(error.contains("replaced"), "{error}");
    }

    #[test]
    fn syncing_verifies_the_directory_too() {
        // Without this, a directory swapped after the LAST write into it
        // would never be looked at again.
        let outer = tempfile::tempdir().unwrap();
        let path = outer.path().join("dir");
        std::fs::create_dir(&path).unwrap();
        let target = Dir::open(path.clone()).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, "swapped").unwrap();
        assert!(target.sync().is_err(), "sync accepted a swapped directory");
    }

    #[test]
    fn opening_a_file_as_a_directory_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, "x").unwrap();
        assert!(Dir::open(path).is_err());
    }

    #[test]
    fn a_child_directory_that_resolves_outside_the_root_is_refused() {
        use std::os::unix::fs::symlink;

        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let outside = outer.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("escape")).unwrap();

        let target = Dir::open(root.clone()).unwrap();
        let error = target.child(&root, "escape").unwrap_err();
        assert!(
            error.contains("symlink") || error.contains("outside"),
            "{error}"
        );
    }

    #[test]
    fn a_child_directory_inside_the_root_is_accepted_and_reusable() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let canonical = std::fs::canonicalize(&root).unwrap();
        let target = Dir::open(canonical.clone()).unwrap();
        let child = target.child(&canonical, "sub").unwrap();
        assert!(child.path().ends_with("sub"));
        assert!(target.child(&canonical, "sub").is_ok());
    }

    #[test]
    fn syncing_a_directory_succeeds_or_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        target.sync().expect("syncing the output directory");
    }
}
