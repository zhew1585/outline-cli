//! The on-disk IR cache written by `otl spec sync`.
//!
//! One file holds the whole compiled table. Its layout is a fixed raw
//! prefix followed by a bincode body:
//!
//! ```text
//! magic (8 bytes) | format version (4 bytes, LE) | body checksum (32) | bincode(Body)
//! ```
//!
//! The prefix is deliberately outside bincode so that "this is not our
//! file", "this is an older layout" and "this is damaged" can be told
//! apart without decoding anything.
//!
//! # The cache is never trusted
//!
//! A cache file is a separate trust boundary from the spec it came from:
//! it can be truncated by a full disk, bit-flipped, left behind by another
//! CLI version, replaced by a pipe or a symlink, or written by something
//! else entirely. So every load checks, in order:
//!
//! 1. the path is a regular file (not a symlink, pipe, socket or device)
//!    and its size is within the limit - checked WITHOUT following links,
//!    before anything is opened ([`read_capped`]);
//! 2. the open runs under a watchdog (a path that became a pipe in the
//!    meantime cannot block the process forever), the open handle is the
//!    SAME file by device and inode, and the read itself is bounded - so a
//!    file that grows, or is swapped for another regular file, gains
//!    nothing;
//! 3. magic, layout version, body checksum, and a decode that consumes
//!    exactly the body;
//! 4. the decode is bounded by framing: element count, per-record size and
//!    decoded footprint, not just bytes consumed ([`super::bounded`]);
//! 5. IR schema version and CLI version;
//! 6. the provenance record, and the safety of every operation
//!    ([`super::validate_ops`]).
//!
//! Any failure discards the cache and the caller falls back to the
//! built-in table - there is no repair path and no migration, by design.
//!
//! # Writes are atomic
//!
//! A new cache goes to a temporary file in the SAME directory (so the
//! rename cannot cross a filesystem boundary and stop being atomic), is
//! flushed and fsynced, and only then renamed over the old one. A reader
//! therefore only ever sees a complete file.

use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use engine::ir::{OpSpec, IR_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Environment variable that overrides the cache directory.
///
/// Needed for tests (a test must never write the developer's real cache)
/// and useful for sandboxes; `directories` resolves the location
/// otherwise, which on macOS ignores `XDG_CACHE_HOME` by design.
pub const CACHE_DIR_ENV: &str = "OTL_CACHE_DIR";

/// Application name used to derive the per-platform cache directory:
/// `~/.cache/outline-cli` (Linux, honouring `XDG_CACHE_HOME`),
/// `~/Library/Caches/outline-cli` (macOS),
/// `%LOCALAPPDATA%\outline-cli\cache` (Windows).
const APPLICATION: &str = "outline-cli";

/// File name of the cache inside the cache directory.
const CACHE_FILE_NAME: &str = "ir-cache.bin";
/// Prefix of the same-directory temporary file used for atomic writes.
///
/// The rest of the name is random and the file is created exclusively; see
/// [`store_at`] for why a predictable name would be a vulnerability.
const TEMP_FILE_PREFIX: &str = "ir-cache.bin.tmp.";

/// How long opening the cache may take before the path is treated as
/// something other than a plain file. Generous: it only has to outlast a
/// slow disk.
const OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Why a cache path that is not a plain file is refused.
const NOT_REGULAR_REASON: &str = "it is not the regular file this build wrote (a symlink, pipe, \
     socket, device or directory is never a cache, and neither is a file that was \
     swapped in while it was being opened)";

/// Length of a hex SHA-256 digest.
const SPEC_HASH_HEX_LEN: usize = 64;
/// Longest accepted provenance string (an origin or a fixed placeholder).
const MAX_SOURCE_BYTES: usize = 256;

/// Magic marker: identifies the file and its purpose.
const MAGIC: [u8; 8] = *b"OTL-IRC\x00";
/// Layout version of the file itself (prefix + body encoding).
///
/// Bumped when the container changes; the IR schema has its own version
/// inside the body. Either mismatch discards the whole file.
///
/// Version 2 replaced a single encoded value with the framed table in
/// [`super::bounded`], which is what bounds decode-time allocation.
const FORMAT_VERSION: u32 = 2;
/// Size of the raw prefix: magic + format version + checksum.
const PREFIX_LEN: usize = 8 + 4 + 32;

/// Maximum accepted cache FILE size, header included.
///
/// The file is read into memory and decoded, so the read, the decoder, and
/// the decoded table are all bounded. The number is deliberately small:
/// the vendored spec's 113 operations compile to about 16 KiB, so this is
/// roughly 7000 operations of headroom, and it is also the multiplier on
/// every decode-amplification bound (see [`super::bounded`]).
pub const MAX_CACHE_FILE_BYTES: usize = 1024 * 1024;

/// Maximum size of the encoded body, i.e. the file limit minus the header.
///
/// This is the number every check uses, and it is derived rather than
/// written twice on purpose: when the two were independent, a table could
/// encode to something that fit the *body* limit, get written, and then be
/// rejected on load for exceeding the *file* limit - a cache that reported
/// success and never worked.
pub const MAX_CACHE_BODY_BYTES: usize = MAX_CACHE_FILE_BYTES - PREFIX_LEN;

/// Ceilings that bound a decoded table, defined with the framing that
/// enforces them and re-exported here as the cache's public limits.
pub use super::bounded::{MAX_CACHED_OPS, MAX_DECODED_BYTES, MAX_OP_RECORD_BYTES};

/// Provenance of a cached table, kept for diagnostics and for deciding
/// whether a sync would change anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheMeta {
    /// `engine::ir::IR_SCHEMA_VERSION` the table was compiled against.
    pub ir_schema_version: u32,
    /// Version of the CLI that wrote the file.
    pub cli_version: String,
    /// Hex SHA-256 of the source document, i.e. the spec-hash half of the
    /// cache key.
    pub spec_hash: String,
    /// Where the document came from: an origin (`https://host`) or a
    /// placeholder for a local file. Never a full URL or a filesystem
    /// path - both can carry secrets.
    pub source: String,
    /// Wall-clock time of the sync, in seconds since the Unix epoch.
    pub synced_at_unix: u64,
}

impl CacheMeta {
    /// Metadata for a table compiled right now by this build.
    pub fn new(spec_hash: String, source: String) -> Self {
        Self {
            ir_schema_version: IR_SCHEMA_VERSION,
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
            spec_hash,
            source,
            synced_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or_default(),
        }
    }
}

/// A cache file that passed every check.
#[derive(Debug, Clone)]
pub struct CachedIr {
    /// Provenance of the cached table.
    pub meta: CacheMeta,
    /// The compiled operations.
    pub ops: Vec<OpSpec>,
}

/// Why a cache could not be located, read, or written.
///
/// Every load failure is recoverable by design: the caller discards the
/// cache and uses the built-in table. [`CacheError::is_stale`] separates
/// "written by another version" (expected after an upgrade) from damage.
#[derive(Debug, Error)]
pub enum CacheError {
    /// No cache directory could be determined for this platform/user.
    #[error(
        "no cache directory could be determined for this platform (no home directory?); \
             set {CACHE_DIR_ENV} to choose one"
    )]
    NoCacheDir,
    /// A filesystem operation failed. Carries the io error kind only: the
    /// message of an io error can embed the path, which may be private.
    #[error("cannot {action} the spec cache at {path}: {kind}")]
    Io {
        /// What was being attempted (`read`, `write`, ...).
        action: &'static str,
        /// The path involved, as displayed by the OS.
        path: PathBuf,
        /// Kind of the underlying io error.
        kind: std::io::ErrorKind,
    },
    /// The file is not a usable cache: wrong magic, bad checksum, or a
    /// body that does not decode.
    #[error("the spec cache at {path} is damaged: {reason}")]
    Damaged {
        /// The cache path.
        path: PathBuf,
        /// What was wrong, in one clause.
        reason: String,
    },
    /// The file was written by a different CLI version or against another
    /// IR schema. Not damage: an expected consequence of upgrading.
    #[error("the spec cache at {path} was written by a different version: {reason}")]
    Stale {
        /// The cache path.
        path: PathBuf,
        /// Which version differs.
        reason: String,
    },
    /// The whole table encodes to more than the cache format allows.
    ///
    /// Raised BEFORE writing anything: a file that could not be loaded
    /// back must never be reported as a successful sync.
    #[error("the compiled spec encodes to {encoded} bytes, over the {limit} byte cache limit")]
    TooLarge {
        /// Size the table encoded to.
        encoded: usize,
        /// The limit it exceeded.
        limit: usize,
    },
    /// The table breaks one of the cache format's structural limits:
    /// too many operations, one operation too large, or too much memory
    /// once decoded.
    ///
    /// A separate variant per cause, because "trim the document" and
    /// "trim one operation's parameters" are different instructions.
    #[error("the compiled spec does not fit the cache format: {}", .0.reason())]
    Unsupportable(#[from] super::bounded::TableError),
}

impl CacheError {
    /// Whether this is a version mismatch rather than damage.
    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Stale { .. })
    }

    /// The command that resolves this situation.
    pub fn remedy(&self) -> &'static str {
        match self {
            Self::NoCacheDir | Self::Io { .. } => {
                "run `otl spec sync` again, or `otl spec reset` to drop the cache"
            }
            Self::TooLarge { .. } => {
                "check that --url or --spec points at the intended document; if it \
                 really is this large, cut it down to the operations you need"
            }
            Self::Unsupportable(error) => error.remedy(),
            Self::Damaged { .. } | Self::Stale { .. } => {
                "run `otl spec sync` to rebuild it, or `otl spec reset` to drop it"
            }
        }
    }
}

/// The directory holding the cache.
pub fn dir() -> Result<PathBuf, CacheError> {
    if let Some(value) = std::env::var_os(CACHE_DIR_ENV) {
        if !value.is_empty() {
            return Ok(PathBuf::from(value));
        }
    }
    // `directories` owns every platform difference here; never hand-build
    // a path from `$HOME` (Windows has no such convention).
    ProjectDirs::from("", "", APPLICATION)
        .map(|dirs| dirs.cache_dir().to_path_buf())
        .ok_or(CacheError::NoCacheDir)
}

/// Full path of the cache file.
pub fn path() -> Result<PathBuf, CacheError> {
    Ok(dir()?.join(CACHE_FILE_NAME))
}

/// Load the cache, if one exists and is usable.
///
/// `Ok(None)` means there is simply no cache (the normal case: the
/// built-in table applies). An `Err` means a file exists but must be
/// discarded; it is never fatal to the caller.
pub fn load() -> Result<Option<CachedIr>, CacheError> {
    load_at(&path()?)
}

/// [`load`] against an explicit file, for tests and for `doctor`.
pub fn load_at(file: &Path) -> Result<Option<CachedIr>, CacheError> {
    let raw = match read_capped(file)? {
        Some(raw) => raw,
        None => return Ok(None),
    };
    let body = checked_body(file, &raw)?;
    // Provenance first, then the versions, and only then the operations.
    // A table written for another `IR_SCHEMA_VERSION` must be reported as
    // OUTDATED - which it is, and which `otl spec sync` fixes - rather than
    // as damage, and the way to guarantee that is never to interpret its
    // operation records at all.
    let (meta, cursor) = super::bounded::decode_meta(body).map_err(CacheError::Unsupportable)?;
    check_versions(file, &meta)?;
    let damaged = |reason: String| CacheError::Damaged {
        path: file.to_path_buf(),
        reason,
    };
    check_meta(&meta).map_err(damaged)?;
    // The framed decode is where every allocation bound lives, including
    // the rule that the body is exactly one table and nothing else: bytes
    // left over mean this file did not come from this code path, however
    // well its checksum matches.
    let ops = super::bounded::decode_ops(body, cursor).map_err(CacheError::Unsupportable)?;
    super::validate_ops(&ops).map_err(damaged)?;
    Ok(Some(CachedIr { meta, ops }))
}

/// Check the provenance record itself.
///
/// It is displayed (`spec sync` prints the source, `doctor` will print the
/// rest), so the same reasoning as for operation text applies: a hostile
/// writer must not be able to put terminal escapes there. The hash is
/// compared against a freshly computed one, so anything that is not a
/// plain hex digest is meaningless as well.
fn check_meta(meta: &CacheMeta) -> Result<(), String> {
    if meta.spec_hash.len() != SPEC_HASH_HEX_LEN
        || !meta.spec_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("its recorded spec hash is not a hex digest".to_string());
    }
    if !spec_compile::is_display_safe(&meta.source, MAX_SOURCE_BYTES) {
        return Err("its recorded source is too long or contains control characters".to_string());
    }
    Ok(())
}

/// Read the cache file, or report that there is none.
///
/// The cache is a file THIS code wrote, by creating a regular file and
/// renaming it into place, so anything else found at that path is not a
/// cache and is refused before it can do harm:
///
/// - a SYMLINK is rejected outright (`symlink_metadata` does not follow
///   it). A link pointing at `/dev/zero` would otherwise report length 0
///   and then read forever; one pointing anywhere else is simply not our
///   file.
/// - a FIFO, socket, device or directory is rejected as well: opening a
///   FIFO blocks until a writer appears, and no read cap can interrupt an
///   open.
/// - the read itself is bounded by `take`, so a plain file that GROWS
///   between the size check and the read cannot get past the limit either.
///
/// Every rejection is a discardable [`CacheError`]: the caller falls back
/// to the built-in table rather than failing the command.
fn read_capped(file: &Path) -> Result<Option<Vec<u8>>, CacheError> {
    let damaged = |reason: String| CacheError::Damaged {
        path: file.to_path_buf(),
        reason,
    };
    let not_regular = || damaged(NOT_REGULAR_REASON.to_string());
    let Some(expected) = stat_regular(file)? else {
        return Ok(None);
    };
    // Opening under a watchdog: the type check above cannot cover the
    // instant between itself and the open, and a path that became a FIFO in
    // that instant would block here forever - never reaching the fallback
    // this whole function exists to reach.
    let handle = super::openfile::open_with_timeout(file, OPEN_TIMEOUT)
        .map_err(|error| damaged(error.describe()))?;
    // Re-check through the OPEN handle, by IDENTITY and not just by type: a
    // path swapped between the two calls may well point at another regular
    // file.
    let opened = handle
        .metadata()
        .map_err(|error| io_error("read", file, error))?;
    if !opened.is_file() || !super::openfile::is_same_file(&expected, &opened) {
        return Err(not_regular());
    }
    // Reserved from the size just stat'd, capped at the limit: without it
    // `read_to_end` grows by doubling and peaks at roughly three times the
    // file. One byte over, so hitting the limit is detectable rather than
    // silently truncating.
    let reserve = expected.len().min(MAX_CACHE_FILE_BYTES as u64) as usize + 1;
    let mut raw = Vec::with_capacity(reserve);
    let read = handle
        .take(MAX_CACHE_FILE_BYTES as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|error| io_error("read", file, error))?;
    if read > MAX_CACHE_FILE_BYTES {
        return Err(damaged(format!(
            "it is larger than the {MAX_CACHE_FILE_BYTES} byte limit"
        )));
    }
    Ok(Some(raw))
}

/// Stat the cache path without following symlinks, returning `None` when
/// there is simply no cache.
///
/// Rejects anything that is not a plain file of plausible size BEFORE it is
/// opened, which is the only place that check can do any good: opening a
/// FIFO blocks until a writer appears, and following a symlink to a device
/// hides both its type and its endless length.
fn stat_regular(file: &Path) -> Result<Option<fs::Metadata>, CacheError> {
    let metadata = match fs::symlink_metadata(file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("read", file, error)),
    };
    if !metadata.is_file() {
        return Err(CacheError::Damaged {
            path: file.to_path_buf(),
            reason: NOT_REGULAR_REASON.to_string(),
        });
    }
    if metadata.len() > MAX_CACHE_FILE_BYTES as u64 {
        return Err(CacheError::Damaged {
            path: file.to_path_buf(),
            reason: format!("it is larger than the {MAX_CACHE_FILE_BYTES} byte limit"),
        });
    }
    Ok(Some(metadata))
}

/// Check the raw prefix and return the framed body.
///
/// Stops at the header on purpose: the caller decodes the provenance record,
/// checks the versions, and only then asks for the operations - see
/// [`super::bounded::decode_table`] for why that order is load-bearing.
fn checked_body<'a>(file: &Path, raw: &'a [u8]) -> Result<&'a [u8], CacheError> {
    let damaged = |reason: String| CacheError::Damaged {
        path: file.to_path_buf(),
        reason,
    };
    if raw.len() < PREFIX_LEN {
        return Err(damaged(
            "it is truncated (shorter than its header)".to_string(),
        ));
    }
    let (prefix, body) = raw.split_at(PREFIX_LEN);
    if prefix[..8] != MAGIC {
        return Err(damaged("it is not an otl spec cache file".to_string()));
    }
    let version = u32::from_le_bytes([prefix[8], prefix[9], prefix[10], prefix[11]]);
    if version != FORMAT_VERSION {
        return Err(CacheError::Stale {
            path: file.to_path_buf(),
            reason: format!("cache layout version {version}, this build writes {FORMAT_VERSION}"),
        });
    }
    if prefix[12..] != checksum(body) {
        return Err(damaged(
            "its checksum does not match its contents".to_string(),
        ));
    }
    Ok(body)
}

/// Reject a cache this build cannot interpret.
///
/// Both halves of the cache key beyond the spec hash are checked here:
/// the IR schema version (the shape of the table) and the CLI version
/// (the code that interprets it). A mismatch discards the file whole -
/// migrating a cache is never worth the risk of interpreting an old table
/// with new rules.
fn check_versions(file: &Path, meta: &CacheMeta) -> Result<(), CacheError> {
    let stale = |reason: String| CacheError::Stale {
        path: file.to_path_buf(),
        reason,
    };
    if meta.ir_schema_version != IR_SCHEMA_VERSION {
        return Err(stale(format!(
            "IR schema version {}, this build understands {IR_SCHEMA_VERSION}",
            meta.ir_schema_version
        )));
    }
    let current = env!("CARGO_PKG_VERSION");
    if meta.cli_version != current {
        return Err(stale(format!(
            "CLI version {}, this build is {current}",
            DisplayVersion(&meta.cli_version)
        )));
    }
    Ok(())
}

/// A version string from the file, printed defensively: the value is
/// attacker-controllable, so it is length-capped and stripped of anything
/// that is not a plain version character.
struct DisplayVersion<'a>(&'a str);

impl fmt::Display for DisplayVersion<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let safe: String = self
            .0
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
            .take(32)
            .collect();
        if safe.is_empty() {
            return f.write_str("<unreadable>");
        }
        f.write_str(&safe)
    }
}

/// Write a compiled table to the cache, atomically.
///
/// Returns the path written.
pub fn store(ops: &[OpSpec], meta: &CacheMeta) -> Result<PathBuf, CacheError> {
    let file = path()?;
    store_at(&file, ops, meta)?;
    Ok(file)
}

/// [`store`] against an explicit file, for tests.
///
/// The temporary file is created in the target directory with a RANDOM
/// name and `O_EXCL` semantics (via `tempfile`), which is what makes the
/// write safe rather than merely atomic:
///
/// - a predictable name (a PID, say) can be pre-created by anyone who can
///   write the cache directory. As a symlink it would redirect this write
///   onto any file the user can write; as an existing mode-0666 file it
///   would keep its permissions, because an open that does not CREATE the
///   file cannot set its mode. Exclusive creation fails instead.
/// - `tempfile` also creates it 0600 on Unix, and its `persist` uses
///   `MOVEFILE_REPLACE_EXISTING` on Windows, where `fs::rename` refuses to
///   replace an existing target - so a second sync works there.
/// - if anything fails, the `NamedTempFile` guard removes the file on
///   drop; nothing is left behind and the previous cache is untouched.
pub fn store_at(file: &Path, ops: &[OpSpec], meta: &CacheMeta) -> Result<(), CacheError> {
    // Whatever this function writes, [`load_at`] must accept: a cache that
    // reports success and is then rejected on every command is worse than
    // a failed sync. So the load-side rules are applied here too, on the
    // table and on the size of its encoding.
    super::validate_ops(ops).map_err(|reason| CacheError::Damaged {
        path: file.to_path_buf(),
        reason,
    })?;
    check_meta(meta).map_err(|reason| CacheError::Damaged {
        path: file.to_path_buf(),
        reason,
    })?;
    // Including the version rules: a table stamped for another build would
    // be written happily and then rejected as stale on the very next
    // command, which is the same broken promise as an oversized one.
    check_versions(file, meta)?;
    let encoded = encode(ops, meta)?;
    let dir = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|error| io_error("create", dir, error))?;
    // Same directory as the target: a rename across filesystems is not
    // atomic (and usually fails outright).
    let mut temp = tempfile::Builder::new()
        .prefix(TEMP_FILE_PREFIX)
        .tempfile_in(dir)
        .map_err(|error| io_error("create", dir, error))?;
    write_all(temp.as_file_mut(), &encoded)
        .map_err(|error| io_error("write", temp.path(), error))?;
    temp.persist(file)
        .map_err(|error| io_error("replace", file, error.error))?;
    Ok(())
}

/// Frame the body, refusing a table that would not load back.
///
/// Every limit the loader enforces is enforced here first - operation
/// count, per-operation record size, decoded footprint, and the encoded
/// size of the whole body. A cache that reports success and is then
/// discarded on the next command is the same broken promise as a failed
/// sync, only quieter.
fn encode(ops: &[OpSpec], meta: &CacheMeta) -> Result<Vec<u8>, CacheError> {
    let encoded = super::bounded::encode_table(meta, ops).map_err(CacheError::Unsupportable)?;
    if encoded.len() > MAX_CACHE_BODY_BYTES {
        return Err(CacheError::TooLarge {
            encoded: encoded.len(),
            limit: MAX_CACHE_BODY_BYTES,
        });
    }
    Ok(encoded)
}

/// One filesystem failure, reduced to (action, path, error kind).
///
/// The io error itself is not retained: its message can embed the path a
/// second time and, on some platforms, other filesystem detail.
fn io_error(action: &'static str, path: &Path, error: std::io::Error) -> CacheError {
    CacheError::Io {
        action,
        path: path.to_path_buf(),
        kind: error.kind(),
    }
}

/// Write prefix + body to the open temporary file, flush and fsync it.
///
/// fsync before the rename is what makes the cache crash-safe: a rename
/// that reaches the disk before the data would otherwise leave a
/// zero-length "valid" file behind.
fn write_all(handle: &mut std::fs::File, encoded: &[u8]) -> std::io::Result<()> {
    handle.write_all(&MAGIC)?;
    handle.write_all(&FORMAT_VERSION.to_le_bytes())?;
    handle.write_all(&checksum(encoded))?;
    handle.write_all(encoded)?;
    handle.flush()?;
    handle.sync_all()
}

/// Delete the cache, returning the path if one was removed.
pub fn reset() -> Result<Option<PathBuf>, CacheError> {
    let file = path()?;
    match fs::remove_file(&file) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CacheError::Io {
            action: "delete",
            path: file,
            kind: error.kind(),
        }),
    }
}

/// SHA-256 of a byte slice.
fn checksum(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Hex SHA-256 of a document, the spec-hash half of the cache key.
pub fn spec_hash(document: &str) -> String {
    let digest = Sha256::digest(document.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Deliberately manual: pulling in a hex crate for one line is not
        // worth a dependency.
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}
