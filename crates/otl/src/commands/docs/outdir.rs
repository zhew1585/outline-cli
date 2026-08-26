//! Preparing the directory `otl docs export` writes into.
//!
//! Two jobs, both done before a single network request so that a bad `--out`
//! never costs one:
//!
//! - decide whether this directory may be exported into at all, and say
//!   something useful when it may not;
//! - create whatever part of the path does not exist yet, and remember WHAT
//!   was created. That second half is not bookkeeping for its own sake: a
//!   directory's name is an entry in its parent, so a newly created output
//!   path leaves several directories whose entries have to be flushed for
//!   the export to be durable.

use std::path::{Path, PathBuf};

use anyhow::anyhow;

use crate::exit::CliError;
use crate::text;

use super::target;

/// Hex digits in a temporary file name (see [`super::target::TempNames`]).
const TEMP_HEX_DIGITS: usize = 16;

/// Validate and create the output directory, before any network request,
/// and return its CANONICAL path.
///
/// A directory that already holds something is refused unless `--overwrite`
/// was given: silently mixing a new export into an old one produces a tree
/// that matches neither.
///
/// The canonical path is what the rest of the export joins onto, and it is
/// what the closing "exported N documents to ..." line names. That matters
/// for symlinks in the path the user gave: `--out` itself may not BE a
/// symlink (checked below), but an ancestor of it legitimately can be -
/// `/tmp` and `/var` are symlinks on macOS, and plenty of home directories
/// are. Those are followed, once, before anything is written, and the place
/// they lead to is reported. What is then guaranteed for the rest of the run
/// is the useful part: every directory created under the root is re-resolved
/// and required to still be inside it (see [`super::dir::Dir::child`]), and
/// every file
/// is placed by `create_new` plus `hard_link`/`rename` rather than by
/// opening a path (see [`target::write_atomically`]), so no link inside the
/// tree can redirect a write out of it.
pub(super) fn prepare_out_dir(out: &Path, overwrite: bool) -> Result<Prepared, CliError> {
    let usage = |message: String| CliError::usage(anyhow!(message));
    let mut created = Vec::new();
    match std::fs::symlink_metadata(out) {
        Ok(metadata) if metadata.is_dir() => {
            let contents = inspect_dir(out)?;
            if !overwrite && !contents.is_empty() {
                return Err(not_empty_error(out, &contents));
            }
        }
        // `symlink_metadata` does not follow links, so a symlink lands here
        // rather than in the branch above: an export must not be redirected
        // somewhere else by a link the user may not have noticed.
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(usage(format!(
                "{} is a symlink; point --out at a real directory",
                out.display()
            )))
        }
        Ok(_) => return Err(usage(format!("{} is not a directory", out.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            created = create_dir_recording(out)?
        }
        Err(error) => {
            return Err(usage(format!(
                "cannot use {}: {}",
                out.display(),
                error.kind()
            )))
        }
    }
    let root = std::fs::canonicalize(out).map_err(|error| {
        usage(format!(
            "cannot resolve {}: {}",
            out.display(),
            error.kind()
        ))
    })?;
    Ok(Prepared {
        root,
        new_entries: parents_of(created),
    })
}

/// A validated output directory, plus the directories whose ENTRIES this
/// run created.
pub(super) struct Prepared {
    /// Canonical output directory.
    pub(super) root: PathBuf,
    /// Directories that gained an entry when the output path was created,
    /// deepest first. Each has to be flushed for the newly created
    /// directory NAMES to survive a crash - flushing only the directory
    /// that holds the documents would leave the directory that holds that
    /// directory unflushed.
    pub(super) new_entries: Vec<PathBuf>,
}

/// Create every missing component of `out`, returning the ones created.
///
/// `create_dir_all` does the same thing but does not say what it made, and
/// the export has to know: a directory it created is a new entry in the
/// directory above, and that one needs flushing too.
///
/// Existing components are traversed as they are - an ancestor symlink is
/// legitimate (`/tmp` and `/var` are symlinks on macOS) and is resolved
/// once, here, before anything is written.
pub(super) fn create_dir_recording(out: &Path) -> Result<Vec<PathBuf>, CliError> {
    let usage = |message: String| CliError::usage(anyhow!(message));
    reject_pointless_parent_segment(out)?;
    let mut created = Vec::new();
    let mut path = PathBuf::new();
    for component in out.components() {
        path.push(component);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => continue,
            Ok(_) => return Err(usage(format!("{} is not a directory", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&path).map_err(|error| {
                    usage(format!(
                        "cannot create {}: {}",
                        path.display(),
                        error.kind()
                    ))
                })?;
                created.push(path.clone());
            }
            Err(error) => {
                return Err(usage(format!(
                    "cannot use {}: {}",
                    path.display(),
                    error.kind()
                )))
            }
        }
    }
    Ok(created)
}

/// Refuse a `..` that steps back out of a directory this run would create.
///
/// Components are created literally, one at a time, which is what makes it
/// possible to know afterwards which ones are new. The consequence is that
/// `--out a/../b` with no `a` creates `a`, steps back out of it, creates
/// `b`, and leaves an empty `a` behind for good - a backup command
/// littering the working directory.
///
/// Collapsing `..` lexically instead would be WRONG: when a component is a
/// symlink, `link/..` is the parent of the link's target, not the directory
/// the link sits in, so the collapsed path names somewhere else entirely.
/// Refusing is therefore the honest option, and it costs nothing real -
/// `--out ../backup` and `--out existing/../backup` still work, because
/// their `..` only ever follows a directory that was already there.
fn reject_pointless_parent_segment(out: &Path) -> Result<(), CliError> {
    let mut path = PathBuf::new();
    let mut creating = false;
    for component in out.components() {
        if creating && component == std::path::Component::ParentDir {
            return Err(CliError::usage(anyhow!(
                "{} steps back out of a directory that does not exist yet, \
                 so creating it would leave an empty directory behind. Give \
                 --out a path whose `..` segments only follow directories \
                 that already exist, or write the destination out in full.",
                out.display()
            )));
        }
        path.push(component);
        if !creating && !path.exists() {
            creating = true;
        }
    }
    Ok(())
}

/// The parent directories of newly created ones, deepest first, deduplicated.
///
/// A directory's own name lives in its parent, so these are the directories
/// whose entries changed. Deepest first so a flush happens after everything
/// it records is already durable.
///
/// The empty parent needs care. `Path::new("backup").parent()` is
/// `Some("")`, not `None` - the parent of a bare relative name is the
/// working directory, and `""` is how `Path` spells that. Passing it on
/// verbatim means opening `""`, which always fails, which turned a
/// perfectly good `--out backup` into exit 9. It is mapped to `.`, which
/// names the same directory and can actually be opened.
pub(super) fn parents_of(created: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut parents: Vec<PathBuf> = created
        .iter()
        .filter_map(|path| path.parent())
        .map(|parent| {
            if parent.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                parent.to_path_buf()
            }
        })
        .collect();
    parents.reverse();
    parents.dedup();
    parents
}

/// What an existing output directory contains, as far as this command cares.
#[derive(Debug, Default, PartialEq, Eq)]
struct Contents {
    /// Entries that are not this command's leftovers.
    others: usize,
    /// Leftover temporary files from an interrupted or unlucky earlier run.
    leftovers: Vec<String>,
}

impl Contents {
    /// Whether the directory holds nothing at all.
    fn is_empty(&self) -> bool {
        self.others == 0 && self.leftovers.is_empty()
    }
}

/// Classify the entries of an existing output directory.
///
/// Leftover temporary files are counted separately because they are the one
/// kind of content the user cannot see: they start with a dot, so `ls` hides
/// them, and the resulting "this directory is not empty" would be about a
/// directory that looks empty. They can be left behind by a run whose
/// cleanup failed (reported at the time as `stray`) or by one that was
/// killed before its guards could run.
fn inspect_dir(dir: &Path) -> Result<Contents, CliError> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        CliError::usage(anyhow!("cannot read {}: {}", dir.display(), error.kind()))
    })?;
    let mut contents = Contents::default();
    for entry in entries {
        let entry = entry.map_err(|error| {
            CliError::usage(anyhow!("cannot read {}: {}", dir.display(), error.kind()))
        })?;
        let name = entry.file_name().to_string_lossy().to_string();
        if is_our_leftover(&name) && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            contents.leftovers.push(name);
        } else {
            contents.others += 1;
        }
    }
    Ok(contents)
}

/// Whether a name is one of THIS command's temporary files.
///
/// Matched exactly - prefix, sixteen lowercase hex digits, `.md` - rather
/// than by prefix alone. The message this feeds says "an earlier export
/// left this behind", and saying that about a file the user created and
/// happened to name `.otl-export-notes.md` would be a confident lie. It
/// also has to be a regular FILE: a directory by that name is not
/// something this command ever creates.
fn is_our_leftover(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(target::TEMP_PREFIX) else {
        return false;
    };
    let Some(hex) = rest.strip_suffix(".md") else {
        return false;
    };
    hex.len() == TEMP_HEX_DIGITS && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The error for an output directory that already holds something.
///
/// Two different situations, and they need different advice: content the
/// user put there, and leftovers this command itself failed to clean up.
fn not_empty_error(dir: &Path, contents: &Contents) -> CliError {
    if contents.others > 0 {
        return CliError::usage(anyhow!(
            "{} is not empty; pass --overwrite to export into it anyway",
            dir.display()
        ));
    }
    // The names came off the filesystem, which allows any byte but NUL and
    // `/`. `stdio` scrubs the terminal-control classes on the way out, and
    // `text::quote` here additionally keeps each name on one line so it
    // cannot pose as a second sentence of the message.
    let named: Vec<String> = contents
        .leftovers
        .iter()
        .map(|name| text::quote(name))
        .collect();
    CliError::usage(anyhow!(
        "{} holds {} leftover temporary file(s) from an earlier export that \
         did not finish cleaning up: {}. They are hidden files, so the \
         directory looks empty. Delete them and run again; --overwrite \
         would leave them in place rather than replace them.",
        dir.display(),
        named.len(),
        named.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The directories `--out <path>` would flush, for a given set of
    /// components it had to create.
    fn flushed(created: &[&str]) -> Vec<String> {
        parents_of(created.iter().map(PathBuf::from).collect())
            .iter()
            .map(|path| path.display().to_string())
            .collect()
    }

    #[test]
    fn a_bare_relative_out_flushes_the_working_directory() {
        // `Path::new("backup").parent()` is `Some("")`, not `None`. Passing
        // that on meant opening `""`, which always fails - so `--out backup`
        // reported exit 9 on a completely successful export while
        // `--out ./backup`, the same directory, reported exit 0.
        assert_eq!(flushed(&["backup"]), vec![".".to_string()]);
    }

    #[test]
    fn a_nested_relative_out_flushes_every_created_parent_deepest_first() {
        assert_eq!(
            flushed(&["a", "a/b", "a/b/c"]),
            vec!["a/b".to_string(), "a".to_string(), ".".to_string()],
            "each created directory's name lives in the one above it"
        );
    }

    #[test]
    fn a_dot_relative_out_names_the_working_directory_the_same_way() {
        // `./backup` and `backup` are the same directory, so they must
        // produce the same flush set.
        assert_eq!(flushed(&["./backup"]), vec![".".to_string()]);
    }

    #[test]
    fn an_absolute_out_flushes_up_to_the_first_directory_that_existed() {
        assert_eq!(
            flushed(&["/tmp/x", "/tmp/x/y"]),
            vec!["/tmp/x".to_string(), "/tmp".to_string()],
            "the pre-existing directory holds the name of the first new one"
        );
    }

    #[test]
    fn an_out_that_already_existed_flushes_nothing_extra() {
        assert!(flushed(&[]).is_empty());
    }

    #[test]
    fn repeated_parents_are_collapsed() {
        // Two siblings created in one directory need it flushed once.
        assert_eq!(
            flushed(&["a", "a/b", "a/c"]),
            vec!["a".to_string(), ".".to_string()]
        );
    }

    #[test]
    fn no_flush_target_is_ever_the_empty_path() {
        // The property behind the bug, stated directly: an empty path can
        // never be opened, so producing one guarantees a false alarm.
        for created in [
            vec!["backup"],
            vec!["a", "a/b"],
            vec!["./x", "./x/y"],
            vec!["/abs", "/abs/deep"],
        ] {
            for target in flushed(&created) {
                assert!(
                    !target.is_empty(),
                    "empty flush target from {created:?}: it can only fail"
                );
            }
        }
    }

    #[test]
    fn only_exact_temporary_names_count_as_our_leftovers() {
        // The message these feed says "an earlier export left this behind".
        // Saying that about a file the user made and happened to name
        // `.otl-export-notes.md` would be a confident lie.
        assert!(is_our_leftover(".otl-export-0123456789abcdef.md"));
        assert!(is_our_leftover(".otl-export-ffffffffffffffff.md"));
        for name in [
            ".otl-export-notes.md",             // not hex
            ".otl-export-0123456789abcdef.txt", // not .md
            ".otl-export-0123456789abcde.md",   // 15 digits
            ".otl-export-0123456789abcdef0.md", // 17 digits
            ".otl-export-.md",                  // no digits
            "otl-export-0123456789abcdef.md",   // no leading dot
            "Alpha.md",
            "",
        ] {
            assert!(!is_our_leftover(name), "{name:?} was claimed as ours");
        }
    }

    #[test]
    fn a_hostile_leftover_name_cannot_carry_control_characters_into_the_message() {
        // A filename may contain any byte but NUL and `/`, and this one goes
        // into a message that reaches a terminal.
        let contents = Contents {
            others: 0,
            leftovers: vec![
                ".otl-export-\u{1b}]52;c;cGF5bG9hZA==\u{7}.md".to_string(),
                "second\nforged: line".to_string(),
            ],
        };
        let rendered = not_empty_error(Path::new("out"), &contents).to_string();
        assert!(!rendered.contains('\u{1b}'), "ESC survived: {rendered:?}");
        assert!(!rendered.contains('\u{7}'), "BEL survived: {rendered:?}");
        assert!(
            !rendered.contains('\n'),
            "a name forged a line break: {rendered:?}"
        );
    }

    #[test]
    fn ordinary_content_keeps_the_overwrite_advice() {
        let contents = Contents {
            others: 1,
            leftovers: Vec::new(),
        };
        let rendered = not_empty_error(Path::new("out"), &contents).to_string();
        assert!(rendered.contains("--overwrite"), "{rendered}");
        assert!(!rendered.contains("leftover"), "{rendered}");
    }

    #[test]
    fn a_parent_segment_after_a_missing_directory_is_refused() {
        // `--out a/../b` with no `a` would create `a`, step out of it and
        // leave it behind empty.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("a").join("..").join("b");
        let error =
            create_dir_recording(&out).expect_err("a pointless parent segment must be refused");
        assert_eq!(error.code, crate::exit::ExitCode::Usage);
        assert!(error.to_string().contains("steps back out"), "{error}");
        // Nothing was created: the check runs before any mkdir.
        assert!(
            !dir.path().join("a").exists(),
            "an empty directory was left"
        );
        assert!(!dir.path().join("b").exists());
    }

    #[test]
    fn parent_segments_after_existing_directories_are_fine() {
        // `..` is only a problem when it follows something this run would
        // have to create. These shapes leave nothing behind.
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("existing");
        std::fs::create_dir(&existing).unwrap();

        let out = existing.join("..").join("target");
        create_dir_recording(&out).expect("a `..` after an existing directory");
        assert!(dir.path().join("target").is_dir());

        // And a plain relative `..` at the front.
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let up = nested.join("..").join("sibling");
        create_dir_recording(&up).expect("a leading `..`");
        assert!(dir.path().join("sibling").is_dir());
    }
}
