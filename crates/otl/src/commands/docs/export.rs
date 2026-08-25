//! `otl docs export --collection <id> --out <dir>` (story 3.6).
//!
//! The collection is enumerated through the engine's auto-pagination (so a
//! collection larger than one page comes back whole), the document tree is
//! rebuilt from `parentDocumentId`, and each document is written as one
//! markdown file whose name went through [`crate::export`].
//!
//! One document failing does not end the export: failures are collected,
//! summarized at the end, and turned into exit code 9 (partial failure), so
//! a backup of 500 documents is not lost to one unreadable one.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use clap::Args;

use crate::exit::CliError;
use crate::export::Names;
use crate::fields;
use crate::render::{self, OutputMode};
use crate::session::Session;
use crate::stdio;

use super::tree::{self, Plan};

/// Operation that enumerates a collection's documents (auto-paginated).
const LIST_OPERATION: &str = "documents.list";
/// Operation that fetches one document's markdown.
const INFO_OPERATION: &str = "documents.info";
/// Extension of every written file.
const EXTENSION: &str = "md";
/// Maximum number of failures listed individually in the final summary.
const MAX_LISTED_FAILURES: usize = 20;
/// Prefix of the temporary file each document is written through.
const TEMP_PREFIX: &str = "otl-export-";

/// Arguments for `otl docs export`.
#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Collection to export (its id).
    #[arg(long, value_name = "ID")]
    pub collection: String,

    /// Directory to write the markdown files into (created if missing).
    #[arg(long, value_name = "DIR")]
    pub out: PathBuf,

    /// Stop after N documents (a warning says so on stderr).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub limit: Option<u64>,

    /// Write into a directory that already has contents, replacing files.
    #[arg(long)]
    pub overwrite: bool,
}

/// One document that could not be exported.
struct Failure {
    /// Document id, so the user can retry it by hand.
    id: String,
    /// Why it failed (already sanitized: it comes from a `CliError` or an
    /// `io::ErrorKind`, never from raw server text).
    reason: String,
}

/// Run `otl docs export`.
pub fn run(cmd: &ExportArgs, mode: OutputMode) -> Result<(), CliError> {
    // Local checks first: a bad output directory must not cost a request.
    // The canonical path is what everything below joins onto, so an
    // ancestor symlink is resolved once, up front, and reported.
    let root = prepare_out_dir(&cmd.out, cmd.overwrite)?;
    let session = Session::open()?;
    let args = vec![("collectionId".to_string(), cmd.collection.clone())];
    let documents = session.call_rows(LIST_OPERATION, &args, cmd.limit)?;
    let plan = tree::plan(&documents.items);
    if plan.is_empty() {
        stdio::write_diagnostic_line("notice: this collection has no documents to export");
    } else {
        stdio::write_diagnostic_line(&format!("exporting {} document(s)...", plan.len()));
    }
    let mut export = Export {
        session: &session,
        overwrite: cmd.overwrite,
        root: &root,
        written: Vec::new(),
        failures: Vec::new(),
        flattened: false,
        temp_counter: 0,
        #[cfg(unix)]
        written_ids: HashSet::new(),
        // A truncated enumeration means documents exist that this run never
        // even saw, so the export is incomplete no matter how many files
        // were written. Unlike a list command, the caller cannot tell from
        // the output directory that anything is missing.
        truncated: documents.truncation,
    };
    export.write_level(&root, &plan, &plan.roots, 0, &mut Names::new());
    export.finish(mode)
}

/// State carried through the recursive write.
struct Export<'a> {
    session: &'a Session,
    overwrite: bool,
    root: &'a Path,
    written: Vec<String>,
    failures: Vec<Failure>,
    /// Whether the depth-cap warning has already been printed.
    flattened: bool,
    /// Counter making each temporary file name unique within this run.
    temp_counter: u64,
    /// Filesystem identity of every file this run has written.
    ///
    /// The de-duplication in [`Names`] folds Unicode case and normalization,
    /// but a filesystem is free to consider even more names equivalent. This
    /// is the backstop: if a rename lands on a file this run already wrote,
    /// two documents collided and one has been lost, which must be reported
    /// rather than counted as two successful exports.
    #[cfg(unix)]
    written_ids: HashSet<(u64, u64)>,
    /// Why the enumeration stopped short, if it did.
    truncated: Option<engine::Truncation>,
}

impl Export<'_> {
    /// Write every node of one directory level, recursing into children.
    ///
    /// `names` is the claimed-name set of `dir`: uniqueness is per
    /// directory, so two documents with the same title in different parts
    /// of the tree keep their natural names.
    ///
    /// The flattening case (past [`tree::MAX_DEPTH`]) is handled with a
    /// QUEUE, not recursion: a pathologically deep chain would otherwise
    /// recurse once per level at the same depth and overflow the stack.
    /// Recursion depth here is therefore bounded by `MAX_DEPTH`.
    fn write_level(
        &mut self,
        dir: &Path,
        plan: &Plan<'_>,
        nodes: &[usize],
        depth: usize,
        names: &mut Names,
    ) {
        let mut pending: VecDeque<usize> = nodes.iter().copied().collect();
        while let Some(node) = pending.pop_front() {
            let children = plan.children(node);
            let stem = names.claim(plan.title(node));
            if children.is_empty() {
                self.write_document(dir, plan, node, &stem);
                continue;
            }
            if depth >= tree::MAX_DEPTH {
                // Children are flattened into this directory rather than
                // dropped; `names` keeps them unique.
                self.write_document(dir, plan, node, &stem);
                self.warn_flattened();
                pending.extend(children.iter().copied());
                continue;
            }
            self.write_branch(dir, plan, node, &stem, children, depth);
        }
    }

    /// Write a document that has children: its own file plus a directory of
    /// the same name holding it and its descendants.
    fn write_branch(
        &mut self,
        dir: &Path,
        plan: &Plan<'_>,
        node: usize,
        stem: &str,
        children: &[usize],
        depth: usize,
    ) {
        let child_dir = dir.join(stem);
        if let Err(reason) = create_child_dir(self.root, &child_dir) {
            // The whole subtree lives in that directory, so every document
            // under it failed too. Reporting only the parent would let the
            // descendants vanish from both stdout and the summary - exactly
            // the silent loss this command exists to avoid.
            self.fail(plan.id(node), reason.clone());
            for descendant in plan.descendants(node) {
                self.fail(plan.id(descendant), reason.clone());
            }
            return;
        }
        let mut child_names = Names::new();
        // The parent's own file lives inside its directory and shares its
        // name, claimed there before any child can take it.
        let own = child_names.claim_exact(stem);
        self.write_document(&child_dir, plan, node, &own);
        self.write_level(&child_dir, plan, children, depth + 1, &mut child_names);
    }

    /// Fetch and write one document.
    fn write_document(&mut self, dir: &Path, plan: &Plan<'_>, node: usize, stem: &str) {
        let id = plan.id(node);
        let path = dir.join(format!("{stem}.{EXTENSION}"));
        let markdown = match self.fetch_markdown(id) {
            Ok(markdown) => markdown,
            Err(error) => {
                self.fail(id, error.to_string());
                return;
            }
        };
        self.temp_counter += 1;
        let temp = TempName {
            counter: self.temp_counter,
        };
        match write_file_atomically(dir, &path, &temp, &markdown, self.overwrite) {
            Ok(()) => self.claim_written(id, &path),
            Err(reason) => self.fail(id, reason),
        }
    }

    /// Record a written file, or report the collision if two documents just
    /// landed on the same one.
    fn claim_written(&mut self, id: &str, path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            // A failing stat is ignored on purpose: the write itself
            // succeeded, so refusing to record it because the follow-up
            // check could not run would be worse than not checking.
            if let Ok(metadata) = std::fs::symlink_metadata(path) {
                if !self.written_ids.insert((metadata.dev(), metadata.ino())) {
                    self.fail(
                        id,
                        "another document in this export already wrote this \
                         file: the filesystem treats their names as the same \
                         entry, so only one of the two survived"
                            .to_string(),
                    );
                    return;
                }
            }
        }
        self.record(path);
    }

    /// The markdown for one document, with a title heading.
    ///
    /// Outline keeps the title out of `text`, and the file name is a
    /// sanitized derivative, so the heading is what preserves the real
    /// title. It is not added when the body already opens with one.
    ///
    /// A response with NO `text` field (or a null one) is an error, not an
    /// empty document: writing a file holding only the title would record
    /// data loss as a successful export. `docs view` only warns about the
    /// same response because it has nothing to corrupt - here the file on
    /// disk is the artifact. An empty STRING is honoured as written: an
    /// empty document is a real thing.
    fn fetch_markdown(&self, id: &str) -> Result<String, CliError> {
        let args = [("id".to_string(), id.to_string())];
        let document = self.session.call_data(INFO_OPERATION, &args)?;
        let title = fields::string_at(&document, "/title").unwrap_or_default();
        let text = fields::string_at(&document, "/text").ok_or_else(|| {
            CliError::failure(anyhow!(
                "the server returned no markdown body for this document; \
                 refusing to write a file that would look like an empty one"
            ))
        })?;
        if text.trim_start().starts_with("# ") || title.is_empty() {
            return Ok(with_trailing_newline(text));
        }
        Ok(with_trailing_newline(&format!("# {title}\n\n{text}")))
    }

    /// Record a written file, by its path relative to the output directory.
    fn record(&mut self, path: &Path) {
        let shown = path
            .strip_prefix(self.root)
            .unwrap_or(path)
            .display()
            .to_string();
        self.written.push(shown);
    }

    /// Record one failed document.
    fn fail(&mut self, id: &str, reason: String) {
        self.failures.push(Failure {
            id: id.to_string(),
            reason,
        });
    }

    /// Warn once that the hierarchy was deeper than the export mirrors.
    fn warn_flattened(&mut self) {
        if std::mem::replace(&mut self.flattened, true) {
            return;
        }
        stdio::write_diagnostic_line(&format!(
            "warning: the document tree is deeper than {} levels; documents \
             below that were written next to their parent instead of under it",
            tree::MAX_DEPTH
        ));
    }

    /// Print the result and pick the exit status.
    ///
    /// Exit 9 (partial failure) covers BOTH ways an export can come out
    /// short: individual documents that could not be written, and an
    /// enumeration that never listed every document in the first place. The
    /// second one is the more dangerous of the two, because the output
    /// directory looks perfectly healthy - nothing in it says that
    /// documents 10,001 and beyond were never requested.
    fn finish(self, mode: OutputMode) -> Result<(), CliError> {
        match mode {
            OutputMode::Json => self.print_json()?,
            OutputMode::Table => self.print_paths()?,
        }
        stdio::write_diagnostic_line(&format!(
            "exported {} document(s) to {}",
            self.written.len(),
            self.root.display()
        ));
        let mut reasons: Vec<String> = Vec::new();
        if let Some(truncation) = &self.truncated {
            reasons.push(format!(
                "the collection listing stopped after {} document(s) before \
                 the collection was exhausted, so documents beyond that were \
                 never fetched; this export is NOT a complete copy of the \
                 collection",
                truncation.fetched
            ));
        }
        if !self.failures.is_empty() {
            reasons.push(self.failure_summary());
        }
        if reasons.is_empty() {
            return Ok(());
        }
        Err(CliError::partial(anyhow!("{}", reasons.join("\n"))))
    }

    /// The written paths, one per line (the scriptable form of the result).
    fn print_paths(&self) -> Result<(), CliError> {
        if self.written.is_empty() {
            return Ok(());
        }
        stdio::write_data(&format!("{}\n", self.written.join("\n")))
    }

    /// A machine-readable summary of the whole export.
    ///
    /// `complete` is the field a backup script should branch on: it is false
    /// whenever a document failed OR the enumeration itself was cut short,
    /// which is the one case the file tree cannot reveal on its own.
    fn print_json(&self) -> Result<(), CliError> {
        let payload = serde_json::json!({
            "out": self.root.display().to_string(),
            "complete": self.failures.is_empty() && self.truncated.is_none(),
            "enumeration_truncated": self.truncated.is_some(),
            "exported": self.written,
            "failed": self
                .failures
                .iter()
                .map(|failure| serde_json::json!({
                    "id": failure.id,
                    "reason": failure.reason,
                }))
                .collect::<Vec<_>>(),
        });
        let rendered = render::render(&payload, OutputMode::Json)
            .map_err(|error| CliError::failure(anyhow!("failed to render summary: {error}")))?;
        stdio::write_data_line(&rendered)
    }

    /// The end-of-run failure report.
    fn failure_summary(&self) -> String {
        let mut lines = vec![format!(
            "{} of {} document(s) could not be exported:",
            self.failures.len(),
            self.failures.len() + self.written.len()
        )];
        lines.extend(
            self.failures
                .iter()
                .take(MAX_LISTED_FAILURES)
                .map(|failure| format!("  {}: {}", failure.id, failure.reason)),
        );
        if self.failures.len() > MAX_LISTED_FAILURES {
            lines.push(format!(
                "  ... and {} more",
                self.failures.len() - MAX_LISTED_FAILURES
            ));
        }
        lines.join("\n")
    }
}

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
/// and required to still be inside it (see [`create_child_dir`]), and every
/// file is placed by `create_new` + `rename` rather than by opening a path
/// (see [`write_file_atomically`]), so no link inside the tree can redirect
/// a write out of it.
fn prepare_out_dir(out: &Path, overwrite: bool) -> Result<PathBuf, CliError> {
    let usage = |message: String| CliError::usage(anyhow!(message));
    match std::fs::symlink_metadata(out) {
        Ok(metadata) if metadata.is_dir() => {
            if !overwrite && !is_empty_dir(out)? {
                return Err(usage(format!(
                    "{} is not empty; pass --overwrite to export into it anyway",
                    out.display()
                )));
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir_all(out)
            .map_err(|error| usage(format!("cannot create {}: {}", out.display(), error.kind())))?,
        Err(error) => {
            return Err(usage(format!(
                "cannot use {}: {}",
                out.display(),
                error.kind()
            )))
        }
    }
    std::fs::canonicalize(out).map_err(|error| {
        usage(format!(
            "cannot resolve {}: {}",
            out.display(),
            error.kind()
        ))
    })
}

/// Whether a directory has no entries.
fn is_empty_dir(dir: &Path) -> Result<bool, CliError> {
    let mut entries = std::fs::read_dir(dir).map_err(|error| {
        CliError::usage(anyhow!("cannot read {}: {}", dir.display(), error.kind()))
    })?;
    Ok(entries.next().is_none())
}

/// Create one subdirectory and confirm it is still inside `root`.
///
/// `create_dir` never follows a link to create something elsewhere (unlike
/// `create_dir_all`), and an entry that already exists is accepted only when
/// `symlink_metadata` says it is a real directory. The canonical-path check
/// afterwards is what catches the remaining case - a directory swapped for a
/// link between the check and the writes that follow - because it re-asks
/// the OS where this path actually leads instead of trusting the earlier
/// answer.
fn create_child_dir(root: &Path, path: &Path) -> Result<(), String> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                format!("cannot inspect the target directory: {}", error.kind())
            })?;
            if !metadata.file_type().is_dir() {
                return Err("a symlink or file already occupies the target directory".to_string());
            }
        }
        Err(error) => return Err(format!("cannot create the directory: {}", error.kind())),
    }
    let resolved = std::fs::canonicalize(path)
        .map_err(|error| format!("cannot resolve the directory: {}", error.kind()))?;
    if !resolved.starts_with(root) {
        return Err("the directory resolves outside the export directory".to_string());
    }
    Ok(())
}

/// The unique part of one temporary file name.
struct TempName {
    counter: u64,
}

impl TempName {
    /// The file name to create in the destination directory.
    ///
    /// Unique per write within a run (counter) and per run (pid), so two
    /// concurrent exports into one directory cannot pick the same temporary
    /// file - and `create_new` below fails loudly rather than silently
    /// sharing one if they somehow did.
    fn file_name(&self) -> String {
        format!(
            ".{TEMP_PREFIX}{}-{}.{EXTENSION}",
            std::process::id(),
            self.counter
        )
    }
}

/// Write one file atomically: fresh temp file in the same directory, fsync,
/// then rename into place.
///
/// This is what makes the two dangerous outcomes impossible rather than
/// unlikely:
///
/// - **A half-written file.** The destination is only ever touched by
///   `rename`, which either replaces it completely or not at all. An I/O
///   error while writing leaves the destination untouched - crucially, a
///   previous good backup survives a failed `--overwrite`, where truncating
///   the destination first would have destroyed it.
/// - **Writing through a symlink.** The temp file is created with
///   `create_new` (`O_CREAT|O_EXCL`), which fails outright on an existing
///   entry and therefore can never follow a link; and `rename` REPLACES a
///   symlink at the destination instead of following it. There is no
///   check-then-open window left to lose, unlike a `symlink_metadata` test
///   followed by `open`.
///
/// Without `overwrite`, an existing destination is refused up front. That
/// check is advisory (the file could appear afterwards), but its job is
/// only to keep a re-run from replacing files, not to defend the tree - the
/// `rename` semantics above do that.
fn write_file_atomically(
    dir: &Path,
    path: &Path,
    temp: &TempName,
    content: &str,
    overwrite: bool,
) -> Result<(), String> {
    use std::io::Write;

    if !overwrite && std::fs::symlink_metadata(path).is_ok() {
        return Err("a file already exists at this path; pass --overwrite to \
                    replace it"
            .to_string());
    }
    let temp_path = dir.join(temp.file_name());
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
        // Nothing was renamed, so the destination is untouched; drop the
        // partial temp file so a failed export leaves no debris.
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!("cannot write the file: {}", error.kind()));
    }
    if let Err(error) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!("cannot place the file: {}", error.kind()));
    }
    Ok(())
}

/// `text` with exactly one trailing newline.
fn with_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        return text.to_string();
    }
    format!("{text}\n")
}
