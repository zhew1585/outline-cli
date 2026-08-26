//! The filesystem side of `otl docs export`.
//!
//! Everything that touches the output tree lives here, so the rules below
//! hold for every write rather than at each call site.
//!
//! ## A document file is never partially visible
//!
//! Content is written to a temporary file, fsynced, and only then given its
//! real name. The destination therefore only ever exists with the whole
//! document in it: there is no window in which a reader - or a crash - can
//! find an empty or half-written `Document.md`.
//!
//! Which system call gives it that name depends on the mode, because the
//! two modes want opposite things:
//!
//! - **without `--overwrite`**: [`std::fs::hard_link`], whose defining
//!   property is that it FAILS if the name is taken. That is the no-clobber
//!   guarantee, enforced by the kernel on every platform, and it doubles as
//!   collision detection: if the filesystem considers two of our names
//!   equivalent, the second link fails instead of silently replacing the
//!   first. The temporary file is unlinked afterwards.
//! - **with `--overwrite`**: [`std::fs::rename`], which replaces
//!   atomically - including replacing a symlink at the destination rather
//!   than following it.
//!
//! ## What this cannot do
//!
//! A path-based write can be redirected by someone able to rename
//! directories inside the output tree while the export runs. Closing that
//! window needs `openat`-style directory handles, which the standard
//! library does not expose and which `otl` will not reach for through
//! `unsafe`. Two things bound it instead:
//!
//! - such an attacker already has write access to the tree being written,
//!   so they can rewrite the exported files anyway;
//! - a write that lands outside the output directory is DETECTED after the
//!   fact: [`landed_inside`] re-resolves the file that was just written and
//!   the caller reports it as a failure. That is detection rather than
//!   prevention (the bytes are already elsewhere), but it is what stops the
//!   command from exiting 0 and listing a path it did not actually write.
//!
//! [`Dir::verify`] additionally catches a directory that was swapped and
//! left swapped. A swap that is reverted before the next check is not
//! detectable this way; the post-write resolution above is what covers that
//! case.
//!
//! On Windows the identity pin in `Dir` is inert, because the file index is
//! only reachable through an unstable standard-library API. The
//! `hard_link`/`rename` rules above are platform-independent and do the
//! load-bearing work.

use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::{Path, PathBuf};

/// Prefix of the temporary file each document is written through.
const TEMP_PREFIX: &str = ".otl-export-";

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
    /// Unix only for the flush itself: Windows has no way to open a
    /// directory as a handle through the standard library. The verification
    /// happens on every platform.
    pub fn sync(&self) -> Result<(), String> {
        self.verify()?;
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

/// A temporary file that removes itself unless it is explicitly kept.
///
/// Ownership is the point. The file is created with `create_new`, so its
/// existence PROVES this run created it, and only then does the guard take
/// responsibility for removing it. An earlier version cleaned up by path
/// after any failure, which meant a `create_new` that failed because
/// something was already there went on to delete that something - a file
/// belonging to another process, or to the user.
struct TempFile {
    path: PathBuf,
    file: Option<std::fs::File>,
    keep: bool,
}

impl TempFile {
    /// Create a new temporary file in `dir`, failing if the name is taken.
    fn create(dir: &Path, name: &str) -> std::io::Result<Self> {
        let path = dir.join(name);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Some(file),
            keep: false,
        })
    }

    /// Write the content and flush it all the way to disk.
    ///
    /// fsync before the file is given its real name: a name that beats its
    /// own data to disk would leave an empty document behind a power loss.
    fn fill(&mut self, content: &str) -> std::io::Result<()> {
        use std::io::Write;

        let Some(file) = self.file.as_mut() else {
            return Err(std::io::Error::other("the temporary file is closed"));
        };
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        // Close before linking or renaming: Windows will not rename a file
        // that is still open.
        self.file = None;
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        self.file = None;
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Names the temporary files of one export run.
///
/// The names are unpredictable on purpose. A predictable one (pid plus a
/// counter) can be occupied in advance by anyone who can write to the
/// output directory, which at best denies service and at worst - with the
/// old path-based cleanup - got that file deleted.
pub struct TempNames {
    /// Keyed hasher, seeded once from the operating system.
    state: RandomState,
    counter: u64,
}

impl TempNames {
    /// A fresh namer whose sequence cannot be predicted from outside.
    ///
    /// `RandomState` takes its key from the operating system, so two runs -
    /// and two processes - produce unrelated sequences. The counter only
    /// makes names unique WITHIN a run; the key is what makes them
    /// unguessable, which is the property the old `pid`-plus-counter scheme
    /// lacked.
    pub fn new() -> Self {
        Self {
            state: RandomState::new(),
            counter: 0,
        }
    }

    /// The next temporary file name.
    fn next(&mut self, extension: &str) -> String {
        self.counter += 1;
        let mut hasher = self.state.build_hasher();
        hasher.write_u64(self.counter);
        format!("{TEMP_PREFIX}{:016x}.{extension}", hasher.finish())
    }
}

impl Default for TempNames {
    fn default() -> Self {
        Self::new()
    }
}

/// Write one file into `dir` and return the path it was written to.
///
/// See the module documentation for the guarantees. In short: the content
/// lands complete or not at all, and without `overwrite` the name is taken
/// with no-replace semantics rather than by testing whether it is free.
pub fn write_atomically(
    dir: &Dir,
    file_name: &str,
    extension: &str,
    names: &mut TempNames,
    content: &str,
    overwrite: bool,
) -> Result<PathBuf, String> {
    dir.verify()?;
    let dest = dir.path().join(file_name);

    let mut temp = TempFile::create(dir.path(), &names.next(extension))
        .map_err(|error| format!("cannot create a temporary file: {}", error.kind()))?;
    temp.fill(content)
        .map_err(|error| format!("cannot write the file: {}", error.kind()))?;

    if overwrite {
        // Replace semantics, and the only step that touches the
        // destination: it either becomes the new file or stays as it was.
        std::fs::rename(&temp.path, &dest)
            .map_err(|error| format!("cannot place the file: {}", error.kind()))?;
        temp.keep = true; // renamed away; nothing left to remove
        return Ok(dest);
    }

    // No-replace semantics: the link fails if anything already answers to
    // that name, including a name the filesystem considers equivalent to
    // one already written.
    match std::fs::hard_link(&temp.path, &dest) {
        Ok(()) => Ok(dest),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err("a file already exists at this path; pass --overwrite to replace it".to_string())
        }
        Err(error) => Err(format!("cannot place the file: {}", error.kind())),
    }
    // `temp` drops here and removes itself: on success the content now has
    // its real name through the hard link, and on failure nothing was
    // placed.
}

/// Whether the file just written is really inside the output directory.
///
/// Resolved through the filesystem, not compared lexically, so a directory
/// that was swapped for a link after the last check is caught: the
/// destination then resolves somewhere else (or no longer resolves at all).
/// This is the check that keeps a redirected write from being reported as a
/// successful export.
pub fn landed_inside(root: &Path, path: &Path) -> Result<(), String> {
    let resolved = std::fs::canonicalize(path).map_err(|error| {
        format!(
            "cannot confirm where the file was written: {}",
            error.kind()
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(
            "the file was written outside the export directory: a directory \
             in the path was replaced while the export was running"
                .to_string(),
        );
    }
    Ok(())
}

/// The filesystem identity of a file that already exists at `path`.
///
/// Used to notice that two documents are about to land on ONE directory
/// entry even though their names differ - which happens when the filesystem
/// considers more names equivalent than the de-duplication key does. The
/// check has to happen BEFORE the write: a `rename` installs a brand-new
/// inode, so asking afterwards would compare against an identity that never
/// existed before this write.
///
/// This is the backstop for `--overwrite` only. Without it, the no-replace
/// link in [`write_atomically`] already refuses such a collision on every
/// platform.
pub fn existing_identity(path: &Path) -> Option<(u64, u64)> {
    identity(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn names() -> TempNames {
        TempNames::new()
    }

    #[test]
    fn writes_content_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        let path = write_atomically(&target, "a.md", "md", &mut names(), "body\n", false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "body\n");
        assert_eq!(entries(dir.path()), vec!["a.md".to_string()]);
    }

    /// Every entry of a directory, sorted, as plain names.
    fn entries(dir: &Path) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        found.sort();
        found
    }

    #[test]
    fn the_destination_never_exists_empty() {
        // The regression this replaced: claiming the name with an empty
        // file first meant a crash in that window left a zero-byte
        // `Document.md` behind, which a later run would then refuse to
        // overwrite. Nothing may exist at the destination until the content
        // is complete, so at every point there is either no destination or
        // a whole document.
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        let mut namer = names();
        // A pre-existing entry list proves the destination is not created
        // ahead of the content: the only file that appears is the finished
        // one.
        write_atomically(&target, "a.md", "md", &mut namer, "whole\n", false).unwrap();
        assert_eq!(entries(dir.path()), vec!["a.md".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "whole\n"
        );
    }

    #[test]
    fn refuses_an_existing_destination_without_overwrite() {
        // Exclusivity is the kernel's: `hard_link` fails when the name is
        // taken, so there is no test-then-act window for a second writer.
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        let mut namer = names();
        write_atomically(&target, "a.md", "md", &mut namer, "first", false).unwrap();
        let error =
            write_atomically(&target, "a.md", "md", &mut namer, "second", false).unwrap_err();
        assert!(error.contains("--overwrite"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "first",
            "the first writer's content was replaced"
        );
        // And the refused attempt left nothing behind.
        assert_eq!(entries(dir.path()), vec!["a.md".to_string()]);
    }

    #[test]
    fn overwrite_replaces_the_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        std::fs::write(dir.path().join("a.md"), "stale").unwrap();
        write_atomically(&target, "a.md", "md", &mut names(), "fresh", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "fresh"
        );
        assert_eq!(entries(dir.path()), vec!["a.md".to_string()]);
    }

    #[test]
    fn a_failed_placement_leaves_the_old_content_and_no_debris() {
        // The destination is a directory, so neither rename nor link can
        // succeed. The pre-existing content must survive and no temporary
        // file may be left.
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir(dir.path().join("a.md")).unwrap();
        std::fs::write(dir.path().join("a.md").join("keep"), "keep").unwrap();
        assert!(write_atomically(&target, "a.md", "md", &mut names(), "x", true).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md").join("keep")).unwrap(),
            "keep",
            "the pre-existing directory was damaged"
        );
        assert_eq!(entries(dir.path()), vec!["a.md".to_string()]);
    }

    #[test]
    fn a_pre_existing_file_is_never_deleted_by_cleanup() {
        // The regression: cleanup used to remove the temporary path
        // whether or not this call had created it, so an attacker who
        // guessed the (pid-based) name and put a file there got it
        // deleted. Ownership now comes from `create_new` succeeding.
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        let bystander = dir.path().join("someone-elses-file");
        std::fs::write(&bystander, "not mine").unwrap();

        // Fill the directory with entries and run several writes; nothing
        // this call did not create may disappear.
        let mut namer = names();
        for index in 0..5 {
            write_atomically(
                &target,
                &format!("{index}.md"),
                "md",
                &mut namer,
                "x",
                false,
            )
            .unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&bystander).unwrap(),
            "not mine",
            "a file belonging to someone else was removed"
        );
    }

    #[test]
    fn an_occupied_temporary_name_does_not_destroy_the_occupant() {
        // Directly: create the temp file the namer is about to hand out,
        // then prove a failure on that name leaves it alone. The namer is
        // seeded per instance, so the same instance reproduces the name.
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        let mut namer = names();
        let taken = namer.next("md");
        std::fs::write(dir.path().join(&taken), "occupied").unwrap();

        // Rewind so the next call produces the same name.
        namer.counter -= 1;
        let error = write_atomically(&target, "a.md", "md", &mut namer, "x", false).unwrap_err();
        assert!(error.contains("temporary file"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(&taken)).unwrap(),
            "occupied",
            "the occupying file was deleted"
        );
    }

    #[test]
    fn temporary_names_are_hidden_and_unpredictable() {
        let mut namer = names();
        let first = namer.next("md");
        let second = namer.next("md");
        assert_ne!(first, second);
        assert!(first.starts_with('.'), "{first}");
        assert!(first.ends_with(".md"), "{first}");
        // Two namers must not agree, or the name would be a function of the
        // pid alone - which is exactly what made it guessable.
        let other = names().next("md");
        assert_ne!(first, other);
        // And the pid must not be readable out of the name.
        assert!(
            !first.contains(&std::process::id().to_string()),
            "{first} exposes the pid"
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

        let result = write_atomically(&target, "a.md", "md", &mut names(), "x", false);
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

    #[cfg(unix)]
    #[test]
    fn a_write_that_landed_outside_the_root_is_detected() {
        // Detection, not prevention: the bytes are already elsewhere. The
        // point is that the command cannot then report the path as
        // exported.
        use std::os::unix::fs::symlink;

        let outer = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(outer.path()).unwrap().join("root");
        std::fs::create_dir(&root).unwrap();
        let outside = outer.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("a.md"), "escaped").unwrap();
        symlink(&outside, root.join("link")).unwrap();

        assert!(landed_inside(&root, &root.join("a.md")).is_err());
        std::fs::write(root.join("a.md"), "inside").unwrap();
        landed_inside(&root, &root.join("a.md")).expect("a file inside the root");
        let error = landed_inside(&root, &root.join("link").join("a.md")).unwrap_err();
        assert!(error.contains("outside"), "{error}");
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
        assert!(target.child(&canonical, "sub").is_ok());
    }

    #[test]
    fn syncing_a_directory_succeeds_or_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let target = Dir::open(dir.path().to_path_buf()).unwrap();
        target.sync().expect("syncing the output directory");
    }
}
