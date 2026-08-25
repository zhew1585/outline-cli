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
//! CLI version, or written by something else entirely. So every load
//! checks, in order: file size, magic, layout version, body checksum,
//! bincode decode, IR schema version, CLI version, and finally the safety
//! of every operation name and path ([`super::validate_ops`]). Any failure
//! discards the cache and the caller falls back to the built-in table -
//! there is no repair path and no migration, by design.
//!
//! # Writes are atomic
//!
//! A new cache goes to a temporary file in the SAME directory (so the
//! rename cannot cross a filesystem boundary and stop being atomic), is
//! flushed and fsynced, and only then renamed over the old one. A reader
//! therefore only ever sees a complete file.

use std::fmt;
use std::fs::{self, File};
use std::io::Write;
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
const TEMP_FILE_PREFIX: &str = "ir-cache.bin.tmp";

/// Magic marker: identifies the file and its purpose.
const MAGIC: [u8; 8] = *b"OTL-IRC\x00";
/// Layout version of the file itself (prefix + body encoding).
///
/// Bumped when the container changes; the IR schema has its own version
/// inside the body. Either mismatch discards the whole file.
const FORMAT_VERSION: u32 = 1;
/// Size of the raw prefix: magic + format version + checksum.
const PREFIX_LEN: usize = 8 + 4 + 32;

/// Maximum accepted cache file size.
///
/// The file is read into memory and decoded, so both the read and the
/// bincode decoder are bounded: a corrupt length prefix must not turn into
/// a huge allocation.
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;

/// Bincode configuration. Fixed and bounded: a cache written by one build
/// must decode identically in the next, and never allocate past the cap.
fn bincode_config() -> impl bincode::config::Config {
    bincode::config::standard().with_limit::<MAX_CACHE_BYTES>()
}

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

/// The bincode-encoded part of a cache file.
#[derive(Serialize, Deserialize)]
struct Body {
    meta: CacheMeta,
    ops: Vec<OpSpec>,
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
    let body = decode_body(file, &raw)?;
    check_versions(file, &body.meta)?;
    super::validate_ops(&body.ops).map_err(|reason| CacheError::Damaged {
        path: file.to_path_buf(),
        reason,
    })?;
    Ok(Some(CachedIr {
        meta: body.meta,
        ops: body.ops,
    }))
}

/// Read the file, refusing anything implausibly large before allocating.
fn read_capped(file: &Path) -> Result<Option<Vec<u8>>, CacheError> {
    let metadata = match fs::metadata(file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("read", file, error)),
    };
    if metadata.len() > MAX_CACHE_BYTES as u64 {
        return Err(CacheError::Damaged {
            path: file.to_path_buf(),
            reason: format!("it is larger than the {MAX_CACHE_BYTES} byte limit"),
        });
    }
    fs::read(file)
        .map(Some)
        .map_err(|error| io_error("read", file, error))
}

/// Check the raw prefix and decode the body.
fn decode_body(file: &Path, raw: &[u8]) -> Result<Body, CacheError> {
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
    bincode::serde::decode_from_slice::<Body, _>(body, bincode_config())
        .map(|(body, _)| body)
        .map_err(|error| damaged(format!("it could not be decoded ({error})")))
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
pub fn store_at(file: &Path, ops: &[OpSpec], meta: &CacheMeta) -> Result<(), CacheError> {
    let body = Body {
        meta: meta.clone(),
        ops: ops.to_vec(),
    };
    let encoded = bincode::serde::encode_to_vec(&body, bincode_config()).map_err(|error| {
        CacheError::Damaged {
            path: file.to_path_buf(),
            reason: format!("the table could not be encoded ({error})"),
        }
    })?;

    let dir = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|error| io_error("create", dir, error))?;
    // Same directory as the target: a rename across filesystems is not
    // atomic (and usually fails outright).
    let temp = dir.join(format!("{TEMP_FILE_PREFIX}-{}", std::process::id()));
    if let Err(error) = write_all(&temp, &encoded) {
        // Never leave a partial file behind.
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    fs::rename(&temp, file).map_err(|error| io_error("replace", file, error))
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

/// Write prefix + body to `temp`, flush and fsync it.
///
/// fsync before the rename is what makes the cache crash-safe: a rename
/// that reaches the disk before the data would otherwise leave a
/// zero-length "valid" file behind.
fn write_all(temp: &Path, encoded: &[u8]) -> Result<(), CacheError> {
    let io = |action: &'static str| {
        move |error: std::io::Error| CacheError::Io {
            action,
            path: temp.to_path_buf(),
            kind: error.kind(),
        }
    };
    let mut handle = create_private(temp).map_err(io("create"))?;
    handle.write_all(&MAGIC).map_err(io("write"))?;
    handle
        .write_all(&FORMAT_VERSION.to_le_bytes())
        .map_err(io("write"))?;
    handle.write_all(&checksum(encoded)).map_err(io("write"))?;
    handle.write_all(encoded).map_err(io("write"))?;
    handle.flush().map_err(io("write"))?;
    handle.sync_all().map_err(io("write"))
}

/// Create a file for writing, owner-only where the platform has file
/// modes.
///
/// The cache holds no secrets (a public spec), but a file only the owner
/// can write is one fewer way for another local account to feed this
/// process a table of request paths. Windows has no POSIX mode bits; the
/// per-user ACL of the local app-data directory is what protects it there.
fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
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
