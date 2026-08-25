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
//!    and its size is within the limit - checked WITHOUT following links
//!    and WITHOUT opening anything that could block ([`read_capped`]);
//! 2. the open handle is still a regular file, and the read itself is
//!    bounded, so a file that grows or is swapped mid-way gains nothing;
//! 3. magic, layout version, body checksum, and a decode that consumes
//!    exactly the body;
//! 4. the decode is bounded by element count and decoded footprint, not
//!    just by bytes consumed ([`BoundedOps`]);
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
const FORMAT_VERSION: u32 = 1;
/// Size of the raw prefix: magic + format version + checksum.
const PREFIX_LEN: usize = 8 + 4 + 32;

/// Maximum accepted cache FILE size, header included.
///
/// The file is read into memory and decoded, so the read, the decoder, and
/// the decoded table are all bounded. The number is deliberately small:
/// the vendored spec's 113 operations compile to about 16 KiB, so this is
/// roughly 7000 operations of headroom, and it is also the multiplier on
/// every decode-amplification bound below (see [`BoundedOps`]).
pub const MAX_CACHE_FILE_BYTES: usize = 1024 * 1024;

/// Maximum size of the encoded body, i.e. the file limit minus the header.
///
/// This is the number every check uses, and it is derived rather than
/// written twice on purpose: when the two were independent, a table could
/// encode to something that fit the *body* limit, get written, and then be
/// rejected on load for exceeding the *file* limit - a cache that reported
/// success and never worked.
pub const MAX_CACHE_BODY_BYTES: usize = MAX_CACHE_FILE_BYTES - PREFIX_LEN;

/// Hard ceiling on the number of operations a cache may declare.
///
/// A byte limit alone does NOT bound the decoded table: bincode's limit
/// counts bytes CONSUMED, and a minimal `OpSpec` encodes to six bytes
/// while occupying well over a hundred once decoded. Without a count
/// ceiling a valid-looking cache could ask for a million of them.
/// (Chosen at roughly seventy times the vendored spec's operation count.)
pub const MAX_CACHED_OPS: usize = 8192;

/// Ceiling on the total decoded footprint of the operation table.
///
/// Checked as the table is decoded, element by element, so decoding stops
/// at the first operation that pushes the total past it instead of
/// finishing and then being rejected.
const MAX_DECODED_BYTES: usize = 8 * 1024 * 1024;

/// Fewest encoded bytes one operation can possibly occupy: four
/// zero-length strings, a body-mode discriminant and an empty parameter
/// list. Used to reject an impossible element count before decoding.
const MIN_ENCODED_OP_BYTES: usize = 6;

/// Bincode configuration: fixed, so a cache written by one build decodes
/// identically in the next, and limited as a backstop.
///
/// The limit is deliberately NOT the file limit. It binds the decoder only
/// (the encoder never consults it, so [`store_at`] checks the encoded
/// length itself), and what it counts is bytes consumed PLUS the claims
/// the decoder makes for containers - which for a table of real
/// operations comes to roughly twice the body. Setting it to the body
/// limit would therefore reject files this build had just written.
///
/// The real bounds on a decode are the ones that mean something: the file
/// size, the element ceiling, and the decoded footprint ([`BoundedOps`]).
/// This limit exists to stop a forged inner container length before those
/// checks get a turn, so it is set to the footprint budget.
fn bincode_config() -> impl bincode::config::Config {
    bincode::config::standard().with_limit::<MAX_DECODED_BYTES>()
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
    ops: BoundedOps,
}

/// The operation table, decoded under an explicit element and footprint
/// budget.
///
/// # Why a byte limit is not enough
///
/// bincode's `with_limit` counts the bytes the decoder CONSUMES. It says
/// nothing about what those bytes turn into: a minimal `OpSpec` encodes to
/// six bytes (four empty strings, a discriminant, an empty parameter list)
/// and occupies well over a hundred once decoded, and the serde path never
/// charges the decoder for a decoded structure. So a one-megabyte cache
/// could ask for a hundred thousand operations and get tens of megabytes
/// of heap - all of it allocated BEFORE any validation could discard the
/// file.
///
/// # What this bounds
///
/// The sequence is pulled element by element (bincode hands over a
/// pull-based `SeqAccess` and allocates nothing itself), and decoding stops
/// at the first element that breaks a rule:
///
/// - the declared element count must not exceed [`MAX_CACHED_OPS`], nor
///   what the remaining bytes could possibly encode;
/// - the running decoded footprint must stay under [`MAX_DECODED_BYTES`];
/// - capacity is reserved for what is plausible, never for what the file
///   claims.
///
/// One operation is still decoded whole before its footprint is counted,
/// so the peak is that budget plus one operation's worth - bounded by the
/// file limit, which is why that limit is small.
struct BoundedOps(Vec<OpSpec>);

impl Serialize for BoundedOps {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BoundedOps {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(OpsVisitor)
    }
}

struct OpsVisitor;

impl<'de> serde::de::Visitor<'de> for OpsVisitor {
    type Value = BoundedOps;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "at most {MAX_CACHED_OPS} operations")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let declared = seq.size_hint();
        if let Some(count) = declared {
            if count > MAX_CACHED_OPS {
                return Err(serde::de::Error::custom(format!(
                    "it declares {count} operations, more than the {MAX_CACHED_OPS} allowed"
                )));
            }
        }
        // Trust the smaller of "what it says" and "what could fit".
        let plausible = MAX_CACHE_BODY_BYTES / MIN_ENCODED_OP_BYTES;
        let capacity = declared.unwrap_or(0).min(MAX_CACHED_OPS).min(plausible);
        let mut ops: Vec<OpSpec> = Vec::with_capacity(capacity);
        let mut footprint = 0usize;
        while let Some(op) = seq.next_element::<OpSpec>()? {
            if ops.len() >= MAX_CACHED_OPS {
                return Err(serde::de::Error::custom(format!(
                    "it contains more than the {MAX_CACHED_OPS} operations allowed"
                )));
            }
            footprint = footprint.saturating_add(footprint_of(&op));
            if footprint > MAX_DECODED_BYTES {
                return Err(serde::de::Error::custom(format!(
                    "its operations decode to more than the {MAX_DECODED_BYTES} byte limit"
                )));
            }
            ops.push(op);
        }
        Ok(BoundedOps(ops))
    }
}

/// Rough heap footprint of one decoded operation: the struct itself, the
/// bytes of its owned strings, and its parameter and enum containers.
///
/// Approximate on purpose - it is a budget, not an accounting - but it
/// must never UNDER-count a field that an attacker can multiply, which is
/// why the containers are charged by element size and not just by length.
fn footprint_of(op: &OpSpec) -> usize {
    let text = op.name.len() + op.path.len() + op.summary.len() + op.content_type.len();
    let params: usize = op
        .params
        .iter()
        .map(|param| {
            std::mem::size_of::<engine::ir::ParamSpec>()
                + param.name.len()
                + param.format.len()
                + param.enum_values.len() * std::mem::size_of::<std::borrow::Cow<'static, str>>()
                + param
                    .enum_values
                    .iter()
                    .map(|value| value.len())
                    .sum::<usize>()
        })
        .sum();
    std::mem::size_of::<OpSpec>() + text + params
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
    /// The compiled table does not fit the cache format.
    ///
    /// Raised BEFORE writing anything: a file that could not be loaded
    /// back must never be reported as a successful sync.
    #[error(
        "the compiled spec is too large to cache: {encoded} bytes of operations, \
         limit {MAX_CACHE_BODY_BYTES}"
    )]
    TooLarge {
        /// Size the table encoded to.
        encoded: usize,
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
            Self::TooLarge { .. } => {
                "the document declares far more operations than a real API has; \
                 check that --url or --spec points at the right document"
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
    let damaged = |reason: String| CacheError::Damaged {
        path: file.to_path_buf(),
        reason,
    };
    check_meta(&body.meta).map_err(damaged)?;
    let ops = body.ops.0;
    super::validate_ops(&ops).map_err(damaged)?;
    Ok(Some(CachedIr {
        meta: body.meta,
        ops,
    }))
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
    let not_regular = || {
        damaged(
            "it is not a regular file (a symlink, pipe, socket, device or \
             directory is never a cache this build wrote)"
                .to_string(),
        )
    };
    // Deliberately does NOT follow symlinks.
    let metadata = match fs::symlink_metadata(file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("read", file, error)),
    };
    if !metadata.is_file() {
        return Err(not_regular());
    }
    if metadata.len() > MAX_CACHE_FILE_BYTES as u64 {
        return Err(damaged(format!(
            "it is larger than the {MAX_CACHE_FILE_BYTES} byte limit"
        )));
    }

    let handle = fs::File::open(file).map_err(|error| io_error("read", file, error))?;
    // Re-check through the OPEN handle: if the path was swapped between the
    // two calls, this is what notices.
    if !handle
        .metadata()
        .map_err(|error| io_error("read", file, error))?
        .is_file()
    {
        return Err(not_regular());
    }
    let mut raw = Vec::new();
    // One byte over the limit, so hitting it is detectable rather than
    // silently truncating.
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
    let (decoded, consumed) = bincode::serde::decode_from_slice::<Body, _>(body, bincode_config())
        .map_err(|error| damaged(format!("it could not be decoded ({error})")))?;
    // A cache file is exactly one encoded body and nothing else. Trailing
    // bytes mean the file is not what this build writes - whoever produced
    // it was not this code path - so it is not trusted, even though the
    // decoder stopped happily and the checksum covers the suffix too.
    if consumed != body.len() {
        return Err(damaged(format!(
            "it carries {} unexpected trailing bytes",
            body.len() - consumed
        )));
    }
    Ok(decoded)
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
    let encoded = encode(file, ops, meta)?;
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

/// Encode the body, refusing a table that would not load back.
///
/// bincode's byte limit binds the decoder only, so the encoded length is
/// checked here: writing a file that the loader would reject as oversized
/// would report a successful sync that silently never takes effect.
fn encode(file: &Path, ops: &[OpSpec], meta: &CacheMeta) -> Result<Vec<u8>, CacheError> {
    let body = Body {
        meta: meta.clone(),
        ops: BoundedOps(ops.to_vec()),
    };
    let encoded = bincode::serde::encode_to_vec(&body, bincode_config()).map_err(|error| {
        CacheError::Damaged {
            path: file.to_path_buf(),
            reason: format!("the table could not be encoded ({error})"),
        }
    })?;
    if encoded.len() > MAX_CACHE_BODY_BYTES {
        return Err(CacheError::TooLarge {
            encoded: encoded.len(),
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
