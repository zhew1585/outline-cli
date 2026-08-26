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
use crate::text;

use super::dir::{self, Dir, Durability};
use super::outdir::{self, Prepared};
use super::target::{self, TempNames};
use super::tree::{self, Plan};

/// Operation that enumerates a collection's documents (auto-paginated).
const LIST_OPERATION: &str = "documents.list";
/// Operation that fetches one document's markdown.
const INFO_OPERATION: &str = "documents.info";
/// Extension of every written file.
const EXTENSION: &str = "md";
/// Maximum number of failures listed individually in the final summary.
const MAX_LISTED_FAILURES: usize = 20;

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
    /// The document's id, when it has one.
    ///
    /// `None` for a listing row that carried no usable id - which is the
    /// whole reason that row failed. Distinguishing the two matters to a
    /// script: `id` is what `documents.info` takes, so handing it a
    /// stand-in label would produce a confusing local validation error
    /// instead of "this row never had an id".
    ///
    /// Kept RAW where it exists: it is what the caller needs to feed back
    /// to the API, and the JSON summary carries it verbatim for that
    /// reason.
    id: Option<String>,
    /// How this failure is NAMED to a human - the id, or the position of
    /// the unusable row in the listing.
    ///
    /// Server-controlled text, so the summary quotes it through
    /// [`text::quote`] rather than printing it as it came.
    label: String,
    /// Why it failed (already sanitized: it comes from a `CliError` or an
    /// `io::ErrorKind`, never from raw server text).
    reason: String,
}

/// Run `otl docs export`.
pub fn run(cmd: &ExportArgs, mode: OutputMode) -> Result<(), CliError> {
    // Local checks first: a bad output directory must not cost a request.
    // The canonical path is what everything below joins onto, so an
    // ancestor symlink is resolved once, up front, and reported.
    let prepared: Prepared = outdir::prepare_out_dir(&cmd.out, cmd.overwrite)?;
    let root = prepared.root;
    let root_dir = Dir::open(root.clone())
        .map_err(|reason| CliError::usage(anyhow!("{}: {reason}", root.display())))?;
    let session = Session::open()?;
    let args = vec![("collectionId".to_string(), cmd.collection.clone())];
    let documents = session.call_rows(LIST_OPERATION, &args, cmd.limit)?;
    let plan = tree::plan(&documents.items);
    if plan.is_empty() && plan.unusable().is_empty() {
        stdio::write_diagnostic_line("notice: this collection has no documents to export");
    } else if !plan.is_empty() {
        stdio::write_diagnostic_line(&format!("exporting {} document(s)...", plan.len()));
    }
    let mut export = Export::new(&session, cmd, &root, &documents);
    // A row the listing could not identify is a document the server said
    // exists and this run never fetched. It has to land in the accounting
    // like any other missing document.
    for unusable in plan.unusable() {
        export.fail_unusable(unusable.label(), unusable.reason.to_string());
    }
    export.write_level(&root_dir, &plan, &plan.roots, 0, &mut Names::new());
    export.flush(&root_dir);
    // The output directory's own NAME is an entry in the directory above
    // it. When this run created that name, flushing only the directory that
    // holds the documents would leave the name itself unflushed, and a
    // crash could take the whole export with it.
    for parent in &prepared.new_entries {
        let outcome = dir::flush_directory(parent);
        export.record_durability(parent, outcome);
    }
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
    /// Unpredictable names for the temporary files documents are written
    /// through.
    temp_names: TempNames,
    /// Filesystem identity of every file this run has written.
    ///
    /// The de-duplication in [`Names`] folds Unicode case and
    /// normalization, but a filesystem is free to consider even more names
    /// equivalent. This is the backstop, and it is consulted BEFORE each
    /// write: if the destination already IS a file this run wrote, the two
    /// names are one directory entry on this filesystem, and overwriting
    /// would lose a document that was reported as exported.
    written_ids: HashSet<(u64, u64)>,
    /// Why the enumeration stopped short in a way the caller did not ask
    /// for, if it did.
    truncated: Option<engine::Truncation>,
    /// Whether `--limit` is what stopped the enumeration.
    limited: bool,
    /// Temporary files that survived a successful publish, each a hidden
    /// copy of a document.
    stray: Vec<String>,
    /// True when a directory could not be flushed because this platform
    /// offers no way to do it, as opposed to because the flush failed.
    durability_unconfirmed: bool,
    /// Directories whose entries could not be flushed to disk.
    ///
    /// The files are written and readable; what is unknown is whether they
    /// survive a crash. A backup that cannot confirm durability must not
    /// report itself as a clean success, so this reaches the exit code and
    /// the JSON summary rather than only stderr.
    undurable: Vec<String>,
}

impl<'a> Export<'a> {
    /// Start an export run.
    fn new(
        session: &'a Session,
        cmd: &ExportArgs,
        root: &'a Path,
        documents: &crate::session::Rows,
    ) -> Self {
        Self {
            session,
            overwrite: cmd.overwrite,
            root,
            written: Vec::new(),
            failures: Vec::new(),
            flattened: false,
            temp_names: TempNames::new(),
            written_ids: HashSet::new(),
            undurable: Vec::new(),
            stray: Vec::new(),
            durability_unconfirmed: false,
            // Only truncation the caller did NOT ask for makes the export
            // incomplete. `--limit N` stopping at N documents is the
            // requested outcome and stays exit 0 - the same boundary the
            // other curated list commands honour, and the one registered
            // in docs/exit-codes.md.
            truncated: documents.incomplete().copied(),
            limited: documents.truncation.is_some() && documents.incomplete().is_none(),
        }
    }

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
        dir: &Dir,
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
        dir: &Dir,
        plan: &Plan<'_>,
        node: usize,
        stem: &str,
        children: &[usize],
        depth: usize,
    ) {
        let child_dir = match dir.child(self.root, stem) {
            Ok(child_dir) => child_dir,
            Err(reason) => {
                // The whole subtree lives in that directory, so every
                // document under it failed too. Reporting only the parent
                // would let the descendants vanish from both stdout and the
                // summary - exactly the silent loss this command exists to
                // avoid.
                self.fail(plan.id(node), reason.clone());
                for descendant in plan.descendants(node) {
                    self.fail(plan.id(descendant), reason.clone());
                }
                return;
            }
        };
        let mut child_names = Names::new();
        // The parent's own file lives inside its directory and shares its
        // name, claimed there before any child can take it.
        let own = child_names.claim_exact(stem);
        self.write_document(&child_dir, plan, node, &own);
        self.write_level(&child_dir, plan, children, depth + 1, &mut child_names);
        self.flush(&child_dir);
    }

    /// Flush one directory, recording a failure to do so.
    ///
    /// Also the last time this directory is verified, which is what covers
    /// a swap performed after the final write into it.
    fn flush(&mut self, dir: &Dir) {
        self.record_durability(dir.path(), dir.sync());
    }

    /// Fold one flush outcome into the run's durability verdict.
    ///
    /// Three outcomes, kept apart on purpose: flushed, could not be flushed
    /// (a real failure), and cannot be flushed on this platform (not a
    /// failure, but not a confirmation either - claiming durability there
    /// would be a Windows branch pretending to have done the work).
    fn record_durability(&mut self, path: &Path, outcome: Result<Durability, String>) {
        match outcome {
            Ok(Durability::Flushed) => {}
            Ok(Durability::Unconfirmed) => self.durability_unconfirmed = true,
            Err(reason) => {
                stdio::write_diagnostic_line(&format!(
                    "warning: could not flush {} to disk: {reason}",
                    path.display()
                ));
                self.undurable.push(format!("{}: {reason}", path.display()));
            }
        }
    }

    /// Refuse a destination that IS a file this run already wrote.
    ///
    /// Checked before writing, not after: a `rename` installs a new inode,
    /// so asking afterwards would compare against an identity that did not
    /// exist before this write and could never match. Reached when the
    /// filesystem considers two of our names equivalent even though the
    /// de-duplication key does not - which is also why `--overwrite` is the
    /// only mode that needs it, the no-replace link used otherwise refusing
    /// such a collision outright.
    fn check_not_already_taken(&self, dest: &Path) -> Result<(), String> {
        let Some(existing) = target::existing_identity(dest) else {
            return Ok(());
        };
        if !self.written_ids.contains(&existing) {
            return Ok(());
        }
        Err(
            "another document in this export already occupies this file: \
             the filesystem treats their two names as one directory entry, \
             so writing this one would lose the other"
                .to_string(),
        )
    }

    /// Fetch and write one document.
    fn write_document(&mut self, dir: &Dir, plan: &Plan<'_>, node: usize, stem: &str) {
        let id = plan.id(node);
        let file_name = format!("{stem}.{EXTENSION}");
        let markdown = match self.fetch_markdown(id) {
            Ok(markdown) => markdown,
            Err(error) => {
                self.fail(id, error.to_string());
                return;
            }
        };
        if let Err(reason) = self.check_not_already_taken(&dir.path().join(&file_name)) {
            return self.fail(id, reason);
        }
        let written = target::write_atomically(
            dir,
            &file_name,
            EXTENSION,
            &mut self.temp_names,
            &markdown,
            self.overwrite,
        );
        let written = match written {
            Ok(written) => written,
            Err(reason) => return self.fail(id, reason),
        };
        // Did the destination end up naming the file that was just written?
        // Compared by identity, not by resolving the path: a directory
        // swapped during the write and restored afterwards would satisfy a
        // path check while the document sat somewhere else entirely.
        if let Err(reason) = target::confirm_landing(self.root, &written.path, written.id) {
            return self.fail(id, reason);
        }
        self.record_stray(written.stray.as_deref());
        if let Some(identity) = written
            .id
            .or_else(|| target::existing_identity(&written.path))
        {
            self.written_ids.insert(identity);
        }
        self.record(&written.path);
    }

    /// Report a temporary file that outlived a successful publish.
    ///
    /// Not a failed export - the document is exactly where it belongs - but
    /// the leftover name is a second link to it, so the output tree holds a
    /// hidden full copy of the document. Worth saying out loud and worth
    /// putting in the JSON, without turning a correct export into a failed
    /// one.
    fn record_stray(&mut self, stray: Option<&Path>) {
        let Some(stray) = stray else {
            return;
        };
        stdio::write_diagnostic_line(&format!(
            "warning: could not remove the temporary file {}; it is a \
             complete copy of this document and should be deleted",
            stray.display()
        ));
        self.stray.push(stray.display().to_string());
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
        self.record_failure(Some(id.to_string()), id.to_string(), reason);
    }

    /// Record a listing row that never became a document.
    ///
    /// No id, because not having one is why it failed; the label carries
    /// the row's position instead.
    fn fail_unusable(&mut self, label: String, reason: String) {
        self.record_failure(None, label, reason);
    }

    /// Record one failure.
    fn record_failure(&mut self, id: Option<String>, label: String, reason: String) {
        self.failures.push(Failure { id, label, reason });
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
    /// Exit 9 (partial failure) covers every way an export can come out
    /// short of what it promised: individual documents that could not be
    /// written, an enumeration that never listed every document in the
    /// first place, and directories whose entries could not be flushed to
    /// disk. The last two are the dangerous ones, because the output
    /// directory looks perfectly healthy - nothing in it says that
    /// documents 10,001 and beyond were never requested, or that the names
    /// of the files in it may not survive a power loss.
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
        if self.durability_unconfirmed {
            // Said once, plainly: the files are written, and this platform
            // gives no way to confirm that their names survive a crash.
            // Silence here would read as a confirmation.
            stdio::write_diagnostic_line(
                "notice: this platform cannot flush a directory through the \
                 standard library, so whether the exported file names survive \
                 a crash could not be confirmed (the JSON summary reports \
                 \"durable\": null)",
            );
        }
        let reasons = self.shortfalls();
        if reasons.is_empty() {
            return Ok(());
        }
        Err(CliError::partial(anyhow!("{}", reasons.join("\n"))))
    }

    /// The durability verdict, as a JSON tri-state.
    ///
    /// `None` becomes `null`: this platform cannot flush a directory, so
    /// neither `true` nor `false` would be a statement anything checked.
    fn durable(&self) -> Option<bool> {
        if !self.undurable.is_empty() {
            return Some(false);
        }
        if self.durability_unconfirmed {
            return None;
        }
        Some(true)
    }

    /// Whether the export delivered everything it was asked for, durably.
    fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.truncated.is_none() && self.undurable.is_empty()
    }

    /// Every way this run fell short of what it promised, in words.
    ///
    /// Empty means the export delivered everything it was asked for, and
    /// durably.
    fn shortfalls(&self) -> Vec<String> {
        let mut reasons: Vec<String> = Vec::new();
        if !self.undurable.is_empty() {
            reasons.push(format!(
                "{} director{} could not be flushed to disk, so the files \
                 written there are readable now but are not known to survive \
                 a crash:\n  {}",
                self.undurable.len(),
                if self.undurable.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                self.undurable.join("\n  ")
            ));
        }
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
        reasons
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
    /// Three separate facts, because collapsing them would mislead:
    ///
    /// - `complete`: the export delivered everything it was asked for. This
    ///   is the field a backup script branches on.
    /// - `enumeration_truncated`: the listing stopped before the collection
    ///   was exhausted for a reason the caller did NOT ask for. The one
    ///   shortfall the file tree cannot reveal on its own.
    /// - `limit_reached`: `--limit` stopped the listing. Not a failure - it
    ///   is what was requested - but a script asking "is this the whole
    ///   collection?" needs `complete && !limit_reached`.
    /// - `durable`: `true` when every directory written was flushed to
    ///   disk, `false` when a flush was attempted and failed, and `null`
    ///   when this platform cannot flush a directory at all. `null` is not
    ///   a formality: reporting `true` there would be claiming a guarantee
    ///   nothing checked.
    /// - `stray`: temporary files that survived a successful publish. Each
    ///   is a complete copy of a document sitting in the output tree under
    ///   a hidden name.
    /// - `failed[]`: `id` is the document id and is `null` for a listing
    ///   row that never had one - so a script can retry exactly the
    ///   entries where `id != null` and report the rest. `label` is the
    ///   human-readable name of the same failure (the id, or the row's
    ///   position in the listing).
    fn print_json(&self) -> Result<(), CliError> {
        let payload = serde_json::json!({
            "out": self.root.display().to_string(),
            "complete": self.is_complete(),
            "enumeration_truncated": self.truncated.is_some(),
            "limit_reached": self.limited,
            "durable": self.durable(),
            "stray": self.stray,
            "exported": self.written,
            "failed": self
                .failures
                .iter()
                .map(|failure| serde_json::json!({
                    "id": failure.id,
                    "label": failure.label,
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
                .map(|failure| format!("  {}: {}", text::quote(&failure.label), failure.reason)),
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

/// `text` with exactly one trailing newline.
fn with_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        return text.to_string();
    }
    format!("{text}\n")
}
