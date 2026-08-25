//! The filesystem side of `otl docs export`.
//!
//! Everything that touches the output tree lives here, so the rules below
//! hold for every write rather than at each call site:
//!
//! - a directory is PINNED when it is created or accepted, and re-checked
//!   before each write into it (see [`Dir::verify`]);
//! - a file is never opened by path for writing. Content goes into a fresh
//!   temporary file created with `create_new` and is moved into place with
//!   `rename`, which replaces a symlink at the destination instead of
//!   following it;
//! - without `--overwrite`, the destination name is CLAIMED with
//!   `create_new` before anything is written, so "this file did not exist"
//!   is enforced by the kernel rather than by a check that a second process
//!   could race.
//!
//! ## What this cannot do
//!
//! A path-based write can always be redirected by someone able to rename
//! directories inside the output tree while the export runs: the resolved
//! path is checked, then used, and closing that window needs `openat`-style
//! directory handles, which the standard library does not expose (and which
//! `otl` will not reach for through `unsafe`). Two things bound the damage
//! instead. Such an attacker already has write access to the tree being
//! written, so they can rewrite the exported files anyway; and the identity
//! pin turns the swap from a silent escape into a reported failure, because
//! a replaced directory does not have the inode that was pinned.
//!
//! On Windows the identity pin is inert: the file index is only reachable
//! through an unstable standard-library API. The `create_new`/`rename`
//! rules above are platform-independent and do the load-bearing work.

use std::path::{Path, PathBuf};

/// Prefix of the temporary file each document is written through.
const TEMP_PREFIX: &str = "otl-export-";

/// The filesystem identity of a path, where the platform exposes one.
///
/// `None` means "not available", never "different": callers must treat an
/// absent identity as no information rather than as a mismatch.
fn identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::symlink_metadata(path).ok()?;
        Some((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
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
    /// Called before every write. It does not make a path-based write
    /// atomic (see the module docs), but it does mean a directory swapped
    /// for a link or another directory is reported instead of silently
    /// receiving the export.
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

    /// Flush this directory's entries to disk.
    ///
    /// `sync_all` on a file only promises that its CONTENT survives a
    /// crash; the directory entry that gives it a name is separate metadata.
    /// Without this, a power loss right after a successful export can leave
    /// the backup missing files whose data was already durable.
    ///
    /// Unix only: Windows has no equivalent of opening a directory as a
    /// handle to sync through the standard library.
    pub fn sync(&self) -> Result<(), String> {
        #[cfg(unix)]
        {
            std::fs::File::open(&self.path)
                .and_then(|dir| dir.sync_all())
                .map_err(|error| format!("cannot flush the directory: {}", error.kind()))
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }
}

/// The unique part of one temporary file name.
pub struct TempName {
    /// Distinguishes writes within one run.
    pub counter: u64,
}

impl TempName {
    /// The file name to create in the destination directory.
    ///
    /// Unique per write within a run (the counter) and per run (the pid), so
    /// two concurrent exports into one directory cannot pick the same
    /// temporary file - and `create_new` fails loudly rather than sharing
    /// one if they somehow did.
    fn file_name(&self, extension: &str) -> String {
        format!(
            ".{TEMP_PREFIX}{}-{}.{extension}",
            std::process::id(),
            self.counter
        )
    }
}

/// Write one file into `dir`, atomically, and return the path written.
///
/// The sequence, and what each step is for:
///
/// 1. [`Dir::verify`] - the directory is still the one that was pinned.
/// 2. Without `overwrite`, the destination name is CLAIMED with
///    `create_new`. This is the no-clobber guarantee, and it is the
///    kernel's: two processes exporting the same name into the same
///    directory cannot both succeed, which a "does it exist yet?" test
///    followed by a rename could not prevent.
/// 3. The content goes into a fresh temporary file in the same directory
///    (`create_new`, so it can never follow a link) and is fsynced.
/// 4. `rename` moves it onto the destination: an all-or-nothing replacement
///    that leaves a previous file untouched if anything above failed, and
///    that replaces a symlink at the destination rather than following it.
///
/// Any failure removes the temporary file, and the claimed destination as
/// well, so a failed export leaves no debris and does not block a re-run.
pub fn write_atomically(
    dir: &Dir,
    file_name: &str,
    extension: &str,
    temp: &TempName,
    content: &str,
    overwrite: bool,
) -> Result<PathBuf, String> {
    use std::io::Write;

    dir.verify()?;
    let dest = dir.path().join(file_name);
    let claimed = if overwrite {
        false
    } else {
        claim(&dest)?;
        true
    };
    let cleanup = |temp_path: &Path| {
        let _ = std::fs::remove_file(temp_path);
        if claimed {
            let _ = std::fs::remove_file(&dest);
        }
    };

    let temp_path = dir.path().join(temp.file_name(extension));
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(content.as_bytes())?;
        // fsync before rename: a rename that beats its own data to disk
        // would leave an empty file behind a power loss.
        file.sync_all()
    };
    if let Err(error) = write() {
        cleanup(&temp_path);
        return Err(format!("cannot write the file: {}", error.kind()));
    }
    if let Err(error) = std::fs::rename(&temp_path, &dest) {
        cleanup(&temp_path);
        return Err(format!("cannot place the file: {}", error.kind()));
    }
    Ok(dest)
}

/// Claim a destination name that must not already exist.
fn claim(dest: &Path) -> Result<(), String> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err("a file already exists at this path; pass --overwrite to replace it".to_string())
        }
        Err(error) => Err(format!("cannot create the file: {}", error.kind())),
    }
}

/// The filesystem identity of a file that already exists at `path`.
///
/// Used to notice that two documents are about to land on ONE directory
/// entry even though their names differ - which happens when the filesystem
/// considers more names equivalent than the de-duplication key does. The
/// check has to happen BEFORE the write: a `rename` installs a brand-new
/// inode, so asking afterwards would compare against an identity that never
/// existed before this write.
pub fn existing_identity(path: &Path) -> Option<(u64, u64)> {
    identity(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn temp(counter: u64) -> TempName {
        TempName { counter }
    }

    #[test]
    fn writes_content_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        let path = write_atomically(&target, "a.md", "md", &temp(1), "body\n", false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "body\n");
        assert!(
            !dir.path().join(temp(1).file_name("md")).exists(),
            "the temporary file survived a successful write"
        );
    }

    #[test]
    fn refuses_an_existing_destination_without_overwrite() {
        // Exclusivity comes from `create_new`, not from a prior existence
        // test, so a second writer cannot slip between the two.
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        write_atomically(&target, "a.md", "md", &temp(1), "first", false).unwrap();
        let error = write_atomically(&target, "a.md", "md", &temp(2), "second", false).unwrap_err();
        assert!(error.contains("--overwrite"), "{error}");
        // The first writer's content survived.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "first"
        );
    }

    #[test]
    fn overwrite_replaces_the_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        std::fs::write(dir.path().join("a.md"), "stale").unwrap();
        write_atomically(&target, "a.md", "md", &temp(1), "fresh", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "fresh"
        );
    }

    #[test]
    fn a_failed_write_leaves_no_claimed_destination_behind() {
        // The destination is a directory, so the rename cannot succeed. The
        // claimed name must not survive as an empty file, or a re-run would
        // be blocked by debris from the failed one.
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir(dir.path().join("a.md")).unwrap();
        std::fs::write(dir.path().join("a.md").join("keep"), "keep").unwrap();
        assert!(write_atomically(&target, "a.md", "md", &temp(1), "x", true).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md").join("keep")).unwrap(),
            "keep",
            "the pre-existing directory was damaged"
        );
        assert!(
            !dir.path().join(temp(1).file_name("md")).exists(),
            "the temporary file survived a failed write"
        );
    }

    #[test]
    fn a_failed_write_removes_the_name_it_claimed() {
        // Without `--overwrite` the destination name is created up front to
        // claim it. If the write then fails, that placeholder must go, or a
        // re-run would be refused because of debris from the failed one.
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir(dir.path().join("blocked")).unwrap();
        // The temp file cannot be created because a directory occupies its
        // name, so the write fails after the destination was claimed.
        std::fs::create_dir(dir.path().join(temp(1).file_name("md"))).unwrap();
        assert!(write_atomically(&target, "a.md", "md", &temp(1), "x", false).is_err());
        assert!(
            !dir.path().join("a.md").exists(),
            "the claimed destination survived a failed write"
        );
    }

    #[test]
    fn a_replaced_directory_is_reported_rather_than_written_into() {
        let outer = tempfile::tempdir().unwrap();
        let path = outer.path().join("dir");
        std::fs::create_dir(&path).unwrap();
        let target = Dir::open(path.clone()).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        // On Unix the recreated directory has a different inode, so the pin
        // catches it. Elsewhere there is no identity to compare and the
        // write proceeds - which the module docs state.
        let result = write_atomically(&target, "a.md", "md", &temp(1), "x", false);
        if cfg!(unix) {
            let error = result.unwrap_err();
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
    fn opening_a_file_as_a_directory_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, "x").unwrap();
        assert!(Dir::open(path).is_err());
    }

    #[cfg(unix)]
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
        // Accepting an existing real directory (a re-run) is fine.
        assert!(target.child(&canonical, "sub").is_ok());
    }

    #[test]
    fn temporary_names_are_hidden_and_unique() {
        let first = temp(1).file_name("md");
        let second = temp(2).file_name("md");
        assert_ne!(first, second);
        assert!(first.starts_with('.'), "{first}");
        assert!(first.contains(TEMP_PREFIX), "{first}");
    }

    #[test]
    fn syncing_a_directory_succeeds_or_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        target.sync().expect("syncing the output directory");
    }
}
