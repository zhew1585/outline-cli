//! `otl spec sync` and `otl spec reset` - the spec lifecycle commands.
//!
//! `sync` is the ONLY code path in the CLI that fetches or parses an
//! OpenAPI document at run time. It compiles the document once and stores
//! the resulting IR as a bincode cache; every later command deserializes
//! that cache instead of parsing anything, so new endpoints are usable
//! immediately without a CLI release and without a startup cost.
//!
//! Nothing here ever runs on its own: the CLI performs no update check and
//! no background fetch (NFR4, no phone home). A sync happens when, and
//! only when, the user types it.

use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::anyhow;
use clap::{Args, Subcommand};
use engine::fetch::{self, MAX_DOCUMENT_BYTES};
use engine::ir::OpSpec;
use serde_json::{json, Value};

use crate::errors::map_fetch_error;
use crate::exit::CliError;
use crate::ops;
use crate::render::OutputMode;
use crate::spec::{self, cache, openfile};
use crate::stdio;

/// Total timeout for fetching a spec document.
///
/// Longer than an API call: this is a several-megabyte document from a
/// CDN, on a command the user is watching.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a `--spec` open may take before it is treated as a file that
/// cannot be read. Generous: it only has to outlast a slow disk.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest list of changed operation names printed in human output.
const MAX_LISTED_CHANGES: usize = 8;

/// Source label recorded in the cache for a local document.
///
/// A filesystem path is deliberately NOT stored: it can name a home
/// directory or a private checkout, and the cache is a plain file.
const LOCAL_SOURCE: &str = "local file";

/// Source label for a URL whose origin could not be determined. Cannot
/// happen for a URL the fetch accepted, but the record must say something.
const UNKNOWN_SOURCE: &str = "unknown origin";

/// Arguments for `otl spec`.
#[derive(Debug, Args)]
pub struct SpecArgs {
    #[command(subcommand)]
    command: SpecCommand,
}

#[derive(Debug, Subcommand)]
enum SpecCommand {
    /// Fetch the upstream spec, compile it, and make it effective now.
    Sync(SyncArgs),
    /// Delete the synced spec and go back to the built-in one.
    Reset,
}

/// Arguments for `otl spec sync`.
#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Fetch the document from this URL instead of the upstream one.
    #[arg(long, value_name = "URL", conflicts_with = "spec_file")]
    url: Option<String>,

    /// Compile a local OpenAPI document instead of fetching one.
    ///
    /// The development override: point the CLI at a document you are
    /// editing, then every command uses it until `otl spec reset`.
    ///
    /// (The field is `spec_file` so the clap id stays distinct from the
    /// `spec` subcommand's own.)
    #[arg(long = "spec", value_name = "PATH")]
    spec_file: Option<PathBuf>,

    /// Rewrite the cache even when the document has not changed.
    #[arg(long)]
    force: bool,
}

/// Where a document came from, which decides how a bad document is
/// classified: a file the user named is a usage error, a fetched document
/// is a failure of the remote source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Local,
    Remote,
}

/// Run the `spec` subcommand.
pub fn run(args: &SpecArgs, mode: OutputMode) -> Result<(), CliError> {
    match &args.command {
        SpecCommand::Sync(sync) => run_sync(sync, mode),
        SpecCommand::Reset => run_reset(mode),
    }
}

/// Fetch (or read), compile, and cache a spec.
fn run_sync(args: &SyncArgs, mode: OutputMode) -> Result<(), CliError> {
    let before: Vec<String> = ops::table().iter().map(|op| op.name.to_string()).collect();
    let (document, source, label) = load_document(args)?;
    let hash = cache::spec_hash(&document);

    if !args.force {
        if let Some(unchanged) = unchanged_report(&hash, before.len()) {
            return emit(&unchanged, mode);
        }
    }

    let ops = compile(&document, source)?;
    let meta = cache::CacheMeta::new(hash.clone(), label);
    let path = cache::store(&ops, &meta).map_err(|error| store_error(error, source))?;
    emit(&sync_report(&ops, &before, &meta, &path), mode)
}

/// Delete the cache, if there is one.
fn run_reset(mode: OutputMode) -> Result<(), CliError> {
    let removed = cache::reset().map_err(cache_write_error)?;
    let report = match &removed {
        Some(path) => Report {
            json: json!({"removed": true, "cache_path": path.display().to_string()}),
            human: format!(
                "removed the synced spec at {}; the built-in spec is in effect again",
                path.display()
            ),
        },
        None => Report {
            json: json!({"removed": false}),
            human: "no synced spec to remove; the built-in spec is already in effect".to_string(),
        },
    };
    emit(&report, mode)
}

/// Read the document to compile, and say where it came from.
///
/// The returned label is what goes into the cache: an origin for a URL
/// (never the full URL, which may carry a token in its query) and a fixed
/// placeholder for a file (never its path).
///
/// For a fetched document the origin is the one that ANSWERED, not the one
/// that was asked. A redirect can move the answer to another host, and
/// `source` is the only signal a user has for "who wrote these endpoint
/// definitions" - recording the host that merely pointed elsewhere would
/// make that signal a lie.
fn load_document(args: &SyncArgs) -> Result<(String, Source, String), CliError> {
    if let Some(path) = &args.spec_file {
        let document = read_local(path)?;
        return Ok((document, Source::Local, LOCAL_SOURCE.to_string()));
    }
    let url = args.url.as_deref().unwrap_or(spec::UPSTREAM_SPEC_URL);
    // Announced on stderr, not stdout: stdout is data.
    stdio::write_diagnostic_line("fetching the OpenAPI document...");
    let fetched =
        fetch::fetch_document(url, MAX_DOCUMENT_BYTES, FETCH_TIMEOUT).map_err(map_fetch_error)?;
    let label = if fetched.origin.is_empty() {
        UNKNOWN_SOURCE.to_string()
    } else {
        fetched.origin
    };
    Ok((fetched.text, Source::Remote, label))
}

/// Read a local document, refusing anything that is not a plain file of
/// plausible size.
///
/// The file type is checked BEFORE opening: opening a FIFO blocks until a
/// writer appears, which no read cap can interrupt, and a directory or
/// device node has no useful content either. Then the size is checked
/// (cheap rejection) and the read itself is bounded too, so a file that
/// grows in between cannot exhaust memory.
fn read_local(path: &Path) -> Result<String, CliError> {
    let file = open_regular(path)?;
    let mut raw = String::new();
    let read = file
        .take(MAX_DOCUMENT_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|error| read_error(path, error))?;
    if read as u64 > MAX_DOCUMENT_BYTES {
        return Err(too_large(path));
    }
    Ok(raw)
}

/// Open a path only if it is a regular file of plausible size.
///
/// The type is checked before AND after opening. Before, because opening a
/// FIFO blocks until a writer appears and no read cap can interrupt that;
/// after, through the open handle, so a path swapped between the two calls
/// cannot slip something else past the first check.
///
/// The open itself runs under a watchdog, because those two checks leave a
/// window: a path that is a regular file when it is examined and a FIFO
/// when it is opened would block forever, and the handle check would never
/// get to run. A blocking open is reported as an error instead. (The
/// alternative, `O_NONBLOCK`, needs a platform-specific flag value and
/// therefore a `libc` dependency for one call.)
fn open_regular(path: &Path) -> Result<fs::File, CliError> {
    let not_regular = || {
        CliError::usage(anyhow!(
            "the spec path {} is not the regular file it was a moment ago; \
             pass a saved OpenAPI document (a pipe, socket, device or \
             directory cannot be read safely: opening one can block forever)",
            path.display()
        ))
    };
    // Follows symlinks (a symlink to a regular file is fine) but reports
    // the TYPE of the target, which is what keeps the open below from
    // blocking on a pipe.
    let expected = fs::metadata(path).map_err(|error| read_error(path, error))?;
    if !expected.is_file() {
        return Err(not_regular());
    }
    if expected.len() > MAX_DOCUMENT_BYTES {
        return Err(too_large(path));
    }
    let file = openfile::open_with_timeout(path, OPEN_TIMEOUT)
        .map_err(|error| open_error(path, &error))?;
    // Re-checked through the open handle by IDENTITY, the same way the
    // cache path does it. A type comparison would accept a path swapped
    // for a DIFFERENT regular file between the two calls, which is exactly
    // what this claims to catch.
    let opened = file.metadata().map_err(|error| read_error(path, error))?;
    if !opened.is_file() || !openfile::is_same_file(&expected, &opened) {
        return Err(not_regular());
    }
    if opened.len() > MAX_DOCUMENT_BYTES {
        return Err(too_large(path));
    }
    Ok(file)
}

/// A failed open of the `--spec` path.
fn open_error(path: &Path, error: &openfile::OpenError) -> CliError {
    CliError::usage(anyhow!(
        "cannot read the spec file {}: {}",
        path.display(),
        error.describe()
    ))
}

/// A filesystem failure on the `--spec` path, reduced to its error kind.
fn read_error(path: &Path, error: std::io::Error) -> CliError {
    CliError::usage(anyhow!(
        "cannot read the spec file {}: {}",
        path.display(),
        error.kind()
    ))
}

/// The `--spec` document exceeds the document size cap.
fn too_large(path: &Path) -> CliError {
    CliError::usage(anyhow!(
        "the spec file {} is too large: the limit is {MAX_DOCUMENT_BYTES} bytes",
        path.display()
    ))
}

/// Compile a document into IR, treating it as untrusted input.
fn compile(document: &str, source: Source) -> Result<Vec<OpSpec>, CliError> {
    let compiled = spec_compile::compile_json(document, &spec::compile_options())
        .map_err(|error| compile_error(&error.to_string(), source))?;
    let ops = spec::to_ir(&compiled);
    // Belt and braces: the compiler already enforces these rules, and the
    // cache loader enforces them again on the way back in.
    spec::validate_ops(&ops).map_err(|reason| compile_error(&reason, source))?;
    Ok(ops)
}

/// Classify an unusable document.
///
/// A file the user named is a usage error (exit 2), like an invalid
/// `--body` file. A fetched document is the remote source's fault, not the
/// invocation's, so it is a generic failure (exit 1).
fn compile_error(reason: &str, source: Source) -> CliError {
    let message = anyhow!("the OpenAPI document cannot be used: {reason}");
    match source {
        Source::Local => CliError::usage(message),
        Source::Remote => CliError::failure(message),
    }
}

/// A cache that cannot be written or deleted is a generic failure: the
/// command did not do what it said, and no fallback applies.
fn cache_write_error(error: cache::CacheError) -> CliError {
    let remedy = error.remedy();
    CliError::failure(anyhow!("{error}.\n{remedy}"))
}

/// A failed store during `spec sync`.
///
/// Two of these are really verdicts on the DOCUMENT rather than on the
/// filesystem - it declares more than the cache format can hold - so they
/// are classified like any other unusable document: the user's own file is
/// a usage error, a fetched one is the source's fault. Everything else is
/// a plain failure to write.
fn store_error(error: cache::CacheError, source: Source) -> CliError {
    let about_the_document = matches!(
        error,
        cache::CacheError::TooLarge { .. } | cache::CacheError::Unsupportable(_)
    );
    if !about_the_document {
        return cache_write_error(error);
    }
    let remedy = error.remedy();
    let message = anyhow!("the OpenAPI document cannot be used: {error}.\n{remedy}");
    match source {
        Source::Local => CliError::usage(message),
        Source::Remote => CliError::failure(message),
    }
}

/// One command result, in both output states.
struct Report {
    json: Value,
    human: String,
}

/// Print a report in the resolved output mode.
fn emit(report: &Report, mode: OutputMode) -> Result<(), CliError> {
    match mode {
        OutputMode::Json => {
            let text = serde_json::to_string_pretty(&report.json).map_err(|error| {
                CliError::failure(anyhow!("failed to render the report: {error}"))
            })?;
            stdio::write_data_line(&text)
        }
        OutputMode::Table => stdio::write_data_line(&report.human),
    }
}

/// Report for a document whose hash already matches the cache.
///
/// Returns `None` when there is no usable cache to compare against - a
/// damaged or outdated one must be rewritten, not treated as up to date.
fn unchanged_report(hash: &str, op_count: usize) -> Option<Report> {
    let cached = cache::load().ok().flatten()?;
    if cached.meta.spec_hash != hash {
        return None;
    }
    Some(Report {
        json: json!({
            "changed": false,
            "operations": op_count,
            "spec_hash": hash,
            "source": cached.meta.source,
        }),
        human: format!(
            "already up to date: {op_count} operations, spec {}; \
             pass --force to rewrite the cache",
            short_hash(hash)
        ),
    })
}

/// Report for a completed sync, including what changed.
fn sync_report(ops: &[OpSpec], before: &[String], meta: &cache::CacheMeta, path: &Path) -> Report {
    let after: Vec<String> = ops.iter().map(|op| op.name.to_string()).collect();
    let added = difference(&after, before);
    let removed = difference(before, &after);
    let human = format!(
        "synced {} operations from {}\n  new: {}\n  gone: {}\n  spec {}\n  cache: {}",
        after.len(),
        meta.source,
        summarize(&added),
        summarize(&removed),
        short_hash(&meta.spec_hash),
        path.display()
    );
    Report {
        json: json!({
            "changed": true,
            "operations": after.len(),
            "added": added,
            "removed": removed,
            "spec_hash": meta.spec_hash,
            "source": meta.source,
            "cache_path": path.display().to_string(),
        }),
        human,
    }
}

/// Names in `left` that are not in `right`.
///
/// Set-based, not a nested scan: both sides come from documents that may
/// declare very many operations, and comparing two tables of a hundred
/// thousand names each with `contains` is ten billion string comparisons
/// before the cache is even written.
fn difference(left: &[String], right: &[String]) -> Vec<String> {
    let right: HashSet<&str> = right.iter().map(String::as_str).collect();
    left.iter()
        .filter(|name| !right.contains(name.as_str()))
        .cloned()
        .collect()
}

/// Render a change list for humans, capped in length.
fn summarize(names: &[String]) -> String {
    if names.is_empty() {
        return "(none)".to_string();
    }
    let listed = names
        .iter()
        .take(MAX_LISTED_CHANGES)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match names.len().checked_sub(MAX_LISTED_CHANGES) {
        Some(rest) if rest > 0 => format!("{listed}, and {rest} more"),
        _ => listed,
    }
}

/// First 12 hex characters of a spec hash, enough to compare by eye.
fn short_hash(hash: &str) -> String {
    let short: String = hash.chars().take(12).collect();
    format!("sha256:{short}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn difference_reports_only_missing_names() {
        let before = names(&["a", "b"]);
        let after = names(&["b", "c"]);
        assert_eq!(difference(&after, &before), names(&["c"]));
        assert_eq!(difference(&before, &after), names(&["a"]));
    }

    #[test]
    fn summarize_caps_long_lists() {
        assert_eq!(summarize(&[]), "(none)");
        let many: Vec<String> = (0..12).map(|index| format!("op{index}")).collect();
        let text = summarize(&many);
        assert!(text.ends_with("and 4 more"), "{text}");
        assert!(text.starts_with("op0, op1"), "{text}");
    }

    #[test]
    fn a_local_source_label_never_carries_a_path() {
        // The cache is a plain file; a path can name a private checkout.
        assert_eq!(LOCAL_SOURCE, "local file");
        assert!(!LOCAL_SOURCE.contains('/'));
    }

    /// The provenance label for a URL is an origin and nothing else: a
    /// path or query can carry a token, and the cache is a plain file.
    #[test]
    fn a_url_is_reduced_to_its_origin() {
        assert_eq!(
            engine::fetch::document_origin(
                "https://raw.example.com/openapi/main/spec.json?token=secret"
            )
            .as_deref(),
            Some("https://raw.example.com")
        );
        // A URL this channel would not fetch has no origin to record.
        assert_eq!(
            engine::fetch::document_origin("https://u:p@example.com/x"),
            None
        );
    }

    #[test]
    fn a_bad_document_is_a_usage_error_only_for_a_local_file() {
        use crate::exit::ExitCode;
        assert_eq!(
            compile_error("not JSON", Source::Local).code,
            ExitCode::Usage
        );
        assert_eq!(
            compile_error("not JSON", Source::Remote).code,
            ExitCode::Failure
        );
    }

    #[test]
    fn short_hash_is_a_prefix_of_the_full_hash() {
        let hash = "0123456789abcdef".repeat(4);
        assert_eq!(short_hash(&hash), "sha256:0123456789ab");
    }

    fn op(name: &str) -> OpSpec {
        OpSpec {
            name: name.to_string().into(),
            path: format!("/api/{name}").into(),
            summary: String::new().into(),
            content_type: String::new().into(),
            body_mode: engine::ir::BodyMode::KeyValue,
            params: Vec::new().into(),
            response_fields: Vec::new().into(),
        }
    }

    /// The TTY rendering never runs in the end-to-end tests (a pipe gets
    /// JSON by contract), so both states are checked here.
    #[test]
    fn a_sync_report_renders_in_both_output_states() {
        let meta = cache::CacheMeta::new("f".repeat(64), "https://spec.example".to_string());
        let report = sync_report(
            &[op("things.info"), op("things.brandNew")],
            &names(&["things.info", "things.gone"]),
            &meta,
            Path::new("/tmp/cache/ir-cache.bin"),
        );

        assert!(
            report.human.contains("synced 2 operations"),
            "{}",
            report.human
        );
        assert!(
            report.human.contains("new: things.brandNew"),
            "{}",
            report.human
        );
        assert!(
            report.human.contains("gone: things.gone"),
            "{}",
            report.human
        );
        assert!(
            report.human.contains("sha256:ffffffffffff"),
            "{}",
            report.human
        );
        assert!(
            report.human.contains("/tmp/cache/ir-cache.bin"),
            "{}",
            report.human
        );

        assert_eq!(report.json["operations"], Value::from(2));
        assert_eq!(report.json["added"], json!(["things.brandNew"]));
        assert_eq!(report.json["removed"], json!(["things.gone"]));
        assert_eq!(report.json["source"], Value::from("https://spec.example"));
        assert_eq!(report.json["spec_hash"], Value::from("f".repeat(64)));
    }
}
