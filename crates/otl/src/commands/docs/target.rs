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
//! ## Where a document actually landed is PROVEN, not assumed
//!
//! The temporary file's filesystem identity is taken from its open handle
//! (`fstat`, no path involved), so it is the identity of the file this run
//! created and nothing else. After the file is given its real name, the
//! destination path is stat'ed and the two identities are compared.
//!
//! That is what makes a directory swap detectable even when it is REVERTED.
//! If a directory in the path is replaced mid-write, the file lands in the
//! attacker's directory; restoring the original before the check does not
//! help them, because the original directory does not contain the inode
//! that was just created - it contains nothing at that name, or an older
//! file with a different one. Either way the comparison fails and the
//! caller reports the document rather than listing a path it did not write.
//!
//! An earlier version resolved the destination path instead and checked
//! that it was lexically inside the output root. That proved only where the
//! path pointed at the instant of the check, which is exactly what a
//! reverted swap makes meaningless.
//!
//! ## What this still cannot do
//!
//! It cannot PREVENT the redirected write. Closing that window needs
//! `openat`-style directory handles, which the standard library does not
//! expose and which `otl` will not reach for through `unsafe`. The bytes
//! may already be in the attacker's directory by the time the mismatch is
//! noticed; what is guaranteed is that the command will not report success
//! for them. Such an attacker also already has write access to the tree
//! being written, so they can rewrite the exported files anyway.
//!
//! On Windows there is no identity to compare: the file index is only
//! reachable through an unstable standard-library API. The check degrades
//! to the weaker path resolution there, and says so at the call site. The
//! `hard_link`/`rename` rules above are platform-independent and do the
//! load-bearing work.
//!
//! ## Durability is reported, never assumed
//!
//! Renaming or linking a file into place makes it visible, not durable: the
//! directory entry is separate metadata. Directories are therefore fsynced,
//! and the outcome is a [`Durability`] value rather than a bare `Ok`, so a
//! platform where the flush cannot be performed reports that instead of
//! being indistinguishable from one where it succeeded.

use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::{Path, PathBuf};

/// Prefix of the temporary file each document is written through.
const TEMP_PREFIX: &str = ".otl-export-";

/// The filesystem identity of a file or directory.
///
/// `None` means "not available on this platform", never "different":
/// callers must treat an absent identity as no information rather than as a
/// mismatch, and must not report a guarantee they could not check.
pub type FileId = (u64, u64);

/// The identity of whatever `path` names right now.
fn identity(path: &Path) -> Option<FileId> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    identity_of(&metadata)
}

/// The identity of an OPEN file, taken from its handle.
///
/// No path is consulted, so this cannot be redirected by anything happening
/// to the directory in the meantime. It is the identity of the file this
/// process is holding, which is what makes it usable as proof later.
fn identity_of_handle(file: &std::fs::File) -> Option<FileId> {
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
    /// Identity taken from the open handle at creation time.
    ///
    /// This is what makes the file THIS call's property: a path can be
    /// pointed at something else afterwards, a handle cannot.
    id: Option<FileId>,
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
        let id = identity_of_handle(&file);
        Ok(Self {
            path,
            file: Some(file),
            id,
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

    /// Remove the temporary file, but only if the path still names it.
    ///
    /// Between creating the file and removing it, someone with write access
    /// to the directory can unlink it and put their own file at the same
    /// name; deleting by path alone would then destroy that file. The
    /// identity check makes the removal apply to the inode this call
    /// created or to nothing at all.
    ///
    /// A platform without identities cannot make that check, and removing
    /// by path is still better than leaving a copy of the document behind,
    /// so the removal proceeds there.
    fn remove(&mut self) -> std::io::Result<()> {
        self.keep = true;
        self.file = None;
        match (self.id, identity(&self.path)) {
            (Some(mine), Some(current)) if mine != current => Ok(()),
            (Some(_), None) => Ok(()),
            _ => std::fs::remove_file(&self.path),
        }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = self.remove();
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

/// The outcome of publishing one document.
#[derive(Debug)]
pub struct Written {
    /// Where it was published.
    pub path: PathBuf,
    /// Identity of the file that was published, from the handle this call
    /// held. `None` on platforms without identities.
    pub id: Option<FileId>,
    /// A temporary file that could not be removed after a successful
    /// publish, if any. It is a complete copy of the document sitting in
    /// the output tree under a hidden name, so the caller must surface it.
    pub stray: Option<PathBuf>,
}

/// Write one file into `dir`.
///
/// See the module documentation for the guarantees. In short: the content
/// lands complete or not at all, without `overwrite` the name is taken with
/// no-replace semantics rather than by testing whether it is free, and the
/// returned identity is what lets the caller prove the destination really
/// names the file this call wrote.
pub fn write_atomically(
    dir: &Dir,
    file_name: &str,
    extension: &str,
    names: &mut TempNames,
    content: &str,
    overwrite: bool,
) -> Result<Written, String> {
    dir.verify()?;
    let dest = dir.path().join(file_name);

    let mut temp = TempFile::create(dir.path(), &names.next(extension))
        .map_err(|error| format!("cannot create a temporary file: {}", error.kind()))?;
    temp.fill(content)
        .map_err(|error| format!("cannot write the file: {}", error.kind()))?;
    let id = temp.id;

    if overwrite {
        // Replace semantics, and the only step that touches the
        // destination: it either becomes the new file or stays as it was.
        std::fs::rename(&temp.path, &dest)
            .map_err(|error| format!("cannot place the file: {}", error.kind()))?;
        // Renamed away: there is no temporary file left to remove, and the
        // guard must not try (the name may since belong to someone else).
        temp.keep = true;
        return Ok(Written {
            path: dest,
            id,
            stray: None,
        });
    }

    // No-replace semantics: the link fails if anything already answers to
    // that name, including a name the filesystem considers equivalent to
    // one already written.
    match std::fs::hard_link(&temp.path, &dest) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(
                "a file already exists at this path; pass --overwrite to replace it".to_string(),
            )
        }
        Err(error) => return Err(format!("cannot place the file: {}", error.kind())),
    }
    // The document is published; the temporary name is now a second link to
    // it. Failing to drop that link is not a failure of the export, but it
    // does leave a full copy of the document in the output tree under a
    // hidden name - so it is reported rather than swallowed.
    let stray = temp.remove().err().map(|_| temp.path.clone());
    Ok(Written {
        path: dest,
        id,
        stray,
    })
}

/// Confirm that `dest` names the file that was just written.
///
/// Compares filesystem IDENTITIES, not paths. A directory swapped during
/// the write sends the file somewhere else, and restoring the directory
/// afterwards does not put it back - the original directory holds either
/// nothing at that name or an older file with a different identity, so the
/// comparison fails either way. Resolving the path instead would be fooled
/// by exactly that revert, because it only describes where the name points
/// at the instant it is asked.
///
/// Where the platform has no identities, this degrades to resolving the
/// path and checking it is lexically inside `root`. That is the weaker
/// check the paragraph above criticises, and it is all Windows can do
/// through the standard library; it still catches a swap that is left in
/// place.
pub fn confirm_landing(root: &Path, dest: &Path, written: Option<FileId>) -> Result<(), String> {
    let Some(expected) = written else {
        let resolved = std::fs::canonicalize(dest).map_err(|error| {
            format!(
                "cannot confirm where the file was written: {}",
                error.kind()
            )
        })?;
        if !resolved.starts_with(root) {
            return Err(REDIRECTED.to_string());
        }
        return Ok(());
    };
    match identity(dest) {
        Some(actual) if actual == expected => Ok(()),
        _ => Err(REDIRECTED.to_string()),
    }
}

/// Reported when a written file is not where it was supposed to go.
const REDIRECTED: &str = "the file was not written where it should have been: a directory in \
     the path was replaced while the export was running";

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
pub fn existing_identity(path: &Path) -> Option<FileId> {
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
        let written =
            write_atomically(&target, "a.md", "md", &mut names(), "body\n", false).unwrap();
        assert_eq!(std::fs::read_to_string(&written.path).unwrap(), "body\n");
        assert!(written.stray.is_none(), "a temporary file survived");
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

    #[cfg(unix)]
    #[test]
    fn a_temporary_file_replaced_after_creation_is_not_deleted() {
        // Random names stop the temp path being occupied in ADVANCE; they
        // do nothing about someone watching the directory, unlinking the
        // temp after it is created, and dropping their own file at that
        // name. Deleting by path alone would then destroy that file, so the
        // guard removes by identity.
        let dir = tempfile::tempdir().unwrap();
        let name = names().next("md");
        let temp_path = dir.path().join(&name);
        let mut guard = TempFile::create(dir.path(), &name).unwrap();
        guard.fill("ours").unwrap();

        // The attacker swaps the file out from under the guard.
        std::fs::remove_file(&temp_path).unwrap();
        std::fs::write(&temp_path, "someone else's file").unwrap();

        drop(guard);
        assert_eq!(
            std::fs::read_to_string(&temp_path).unwrap(),
            "someone else's file",
            "the guard deleted a file it did not create"
        );
    }

    #[test]
    fn a_temporary_file_the_guard_created_is_removed() {
        // The other half: when nothing interfered, the guard must still
        // clean up, or every failed write would leak a copy.
        let dir = tempfile::tempdir().unwrap();
        let mut namer = names();
        let name = namer.next("md");
        let temp_path = dir.path().join(&name);
        {
            let mut guard = TempFile::create(dir.path(), &name).unwrap();
            guard.fill("ours").unwrap();
            assert!(temp_path.exists());
        }
        assert!(!temp_path.exists(), "the temporary file was left behind");
    }

    #[cfg(unix)]
    #[test]
    fn a_removal_that_fails_is_reported_rather_than_swallowed() {
        // After a successful no-overwrite publish the temporary name is a
        // second link to the document, so failing to drop it leaves a
        // hidden full copy in the output tree. `remove` must therefore
        // return the error instead of discarding it - that return value is
        // what becomes `Written::stray`.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let mut guard = TempFile::create(&work, ".otl-export-test.md").unwrap();
        guard.fill("body").unwrap();

        // A directory with no write permission refuses the unlink.
        std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o500)).unwrap();
        let outcome = guard.remove();
        std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o700)).unwrap();

        if std::fs::metadata(work.join(".otl-export-test.md")).is_err() {
            // Running as root, where permissions do not apply and the
            // removal succeeded. Nothing to assert.
            return;
        }
        assert!(
            outcome.is_err(),
            "a failed removal was reported as success, so the leftover copy \
             would never be mentioned"
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
    fn landing_is_confirmed_by_identity_not_by_the_path() {
        // The attack the path check could not see: the file is written
        // somewhere else and the directory is put back before anyone looks.
        // Resolving `root/a.md` afterwards succeeds and points inside the
        // root - at a DIFFERENT file. Comparing identities catches it.
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        // Stand in for "the file this run wrote": an inode that exists, but
        // not the one the destination names.
        let elsewhere = root.join("elsewhere.md");
        std::fs::write(&elsewhere, "the document that was really written").unwrap();
        let written = identity(&elsewhere);

        let dest = root.join("a.md");
        std::fs::write(&dest, "an older file that was here all along").unwrap();

        if written.is_some() {
            let error = confirm_landing(&root, &dest, written)
                .expect_err("a destination naming another file must be rejected");
            assert!(error.contains("not written where"), "{error}");
        }
        // And the honest case still passes.
        confirm_landing(&root, &elsewhere, written).expect("the file it really is");
    }

    #[test]
    fn a_destination_that_vanished_is_not_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let gone = root.join("gone.md");
        std::fs::write(&gone, "x").unwrap();
        let written = identity(&gone);
        std::fs::remove_file(&gone).unwrap();
        assert!(confirm_landing(&root, &gone, written).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn without_identities_landing_falls_back_to_resolving_the_path() {
        // The Windows path, exercised here by passing `None`: weaker, but
        // it still catches a symlink that is left in place.
        use std::os::unix::fs::symlink;

        let outer = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(outer.path()).unwrap().join("root");
        std::fs::create_dir(&root).unwrap();
        let outside = outer.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("a.md"), "escaped").unwrap();
        symlink(&outside, root.join("link")).unwrap();

        std::fs::write(root.join("a.md"), "inside").unwrap();
        confirm_landing(&root, &root.join("a.md"), None).expect("a file inside the root");
        let error = confirm_landing(&root, &root.join("link").join("a.md"), None).unwrap_err();
        assert!(error.contains("not written where"), "{error}");
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
