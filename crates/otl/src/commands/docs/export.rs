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

use std::collections::VecDeque;
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
    prepare_out_dir(&cmd.out, cmd.overwrite)?;
    let session = Session::open()?;
    let args = vec![("collectionId".to_string(), cmd.collection.clone())];
    let documents = session.call_rows(LIST_OPERATION, &args, cmd.limit)?;
    let plan = tree::plan(&documents);
    if plan.is_empty() {
        stdio::write_diagnostic_line("notice: this collection has no documents to export");
    } else {
        stdio::write_diagnostic_line(&format!("exporting {} document(s)...", plan.len()));
    }
    let mut export = Export {
        session: &session,
        overwrite: cmd.overwrite,
        root: &cmd.out,
        written: Vec::new(),
        failures: Vec::new(),
        flattened: false,
    };
    export.write_level(&cmd.out, &plan, &plan.roots, 0, &mut Names::new());
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
        if let Err(reason) = create_child_dir(&child_dir) {
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
        match write_file(&path, &markdown, self.overwrite) {
            Ok(()) => self.record(&path),
            Err(reason) => self.fail(id, reason),
        }
    }

    /// The markdown for one document, with a title heading.
    ///
    /// Outline keeps the title out of `text`, and the file name is a
    /// sanitized derivative, so the heading is what preserves the real
    /// title. It is not added when the body already opens with one.
    fn fetch_markdown(&self, id: &str) -> Result<String, CliError> {
        let args = [("id".to_string(), id.to_string())];
        let document = self.session.call_data(INFO_OPERATION, &args)?;
        let title = fields::string_at(&document, "/title").unwrap_or_default();
        let text = fields::string_at(&document, "/text").unwrap_or_default();
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
        if self.failures.is_empty() {
            return Ok(());
        }
        Err(CliError::partial(anyhow!("{}", self.failure_summary())))
    }

    /// The written paths, one per line (the scriptable form of the result).
    fn print_paths(&self) -> Result<(), CliError> {
        if self.written.is_empty() {
            return Ok(());
        }
        stdio::write_data(&format!("{}\n", self.written.join("\n")))
    }

    /// A machine-readable summary of the whole export.
    fn print_json(&self) -> Result<(), CliError> {
        let payload = serde_json::json!({
            "out": self.root.display().to_string(),
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

/// Validate and create the output directory, before any network request.
///
/// A directory that already holds something is refused unless `--overwrite`
/// was given: silently mixing a new export into an old one produces a tree
/// that matches neither.
fn prepare_out_dir(out: &Path, overwrite: bool) -> Result<(), CliError> {
    let usage = |message: String| CliError::usage(anyhow!(message));
    match std::fs::symlink_metadata(out) {
        Ok(metadata) if metadata.is_dir() => {
            if !overwrite && !is_empty_dir(out)? {
                return Err(usage(format!(
                    "{} is not empty; pass --overwrite to export into it anyway",
                    out.display()
                )));
            }
            Ok(())
        }
        // `symlink_metadata` does not follow links, so a symlink lands here
        // rather than in the branch above: an export must not be redirected
        // somewhere else by a link the user may not have noticed.
        Ok(metadata) if metadata.file_type().is_symlink() => Err(usage(format!(
            "{} is a symlink; point --out at a real directory",
            out.display()
        ))),
        Ok(_) => Err(usage(format!("{} is not a directory", out.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir_all(out)
            .map_err(|error| usage(format!("cannot create {}: {}", out.display(), error.kind()))),
        Err(error) => Err(usage(format!(
            "cannot use {}: {}",
            out.display(),
            error.kind()
        ))),
    }
}

/// Whether a directory has no entries.
fn is_empty_dir(dir: &Path) -> Result<bool, CliError> {
    let mut entries = std::fs::read_dir(dir).map_err(|error| {
        CliError::usage(anyhow!("cannot read {}: {}", dir.display(), error.kind()))
    })?;
    Ok(entries.next().is_none())
}

/// Create one subdirectory, refusing to follow a symlink.
///
/// An existing real directory is fine (a re-run with `--overwrite`), but a
/// symlink in its place would redirect the export outside the output tree.
fn create_child_dir(path: &Path) -> Result<(), String> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                format!("cannot inspect the target directory: {}", error.kind())
            })?;
            if metadata.file_type().is_dir() {
                Ok(())
            } else {
                Err("a symlink or file already occupies the target directory".to_string())
            }
        }
        Err(error) => Err(format!("cannot create the directory: {}", error.kind())),
    }
}

/// Write one file, never through a symlink.
fn write_file(path: &Path, content: &str, overwrite: bool) -> Result<(), String> {
    use std::io::Write;

    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err("refusing to write through a symlink".to_string());
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        // Belt and braces: the in-memory name set should already have made
        // the name unique, so an existing file means something outside this
        // run owns it.
        options.create_new(true);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot write the file: {}", error.kind()))?;
    file.write_all(content.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|error| format!("cannot write the file: {}", error.kind()))
}

/// `text` with exactly one trailing newline.
fn with_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        return text.to_string();
    }
    format!("{text}\n")
}
