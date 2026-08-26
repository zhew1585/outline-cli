//! The on-disk IR cache (Story 4.2): round-trip, atomicity, and every
//! rejection path.
//!
//! Nothing here touches the real cache directory: every case works on a
//! `tempfile::TempDir`. The hostile cases build a cache file byte by byte,
//! which also pins the file layout:
//!
//! ```text
//! magic(8) | layout version(4 LE) | sha256(body)(32) | body
//! body = meta_len(4 LE) | meta | op_count(4 LE) | [ op_len(4 LE) | op ]*
//! ```
//!
//! Each record is one bincode value. Building the bytes by hand is what
//! lets a test declare something the encoder would never write - an
//! impossible operation count, a record that lies about its length - which
//! is exactly what a hostile cache does.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use engine::ir::{BodyMode, OpSpec, ParamSpec, ParamType};
use otl::spec::cache::{self, CacheMeta};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const MAGIC: [u8; 8] = *b"OTL-IRC\x00";
const FORMAT_VERSION: u32 = 2;

/// A table as the framing layer sees it.
struct Body {
    meta: CacheMeta,
    ops: Vec<OpSpec>,
}

/// Encode one record the way the cache does.
fn record<T: Serialize>(value: &T) -> Vec<u8> {
    bincode::serde::encode_to_vec(value, bincode::config::standard().with_limit::<32_768>())
        .unwrap()
}

fn push_record(body: &mut Vec<u8>, bytes: &[u8]) {
    body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    body.extend_from_slice(bytes);
}

/// Frame a body from parts, bypassing every check `store_at` makes.
fn frame(body: &Body) -> Vec<u8> {
    frame_with_count(body, body.ops.len() as u32)
}

/// [`frame`], but with a declared operation count of the caller's choosing
/// - so a test can claim thousands of operations in a handful of bytes.
fn frame_with_count(body: &Body, declared: u32) -> Vec<u8> {
    let mut out = Vec::new();
    push_record(&mut out, &record(&body.meta));
    out.extend_from_slice(&declared.to_le_bytes());
    for op in &body.ops {
        push_record(&mut out, &record(op));
    }
    out
}

fn op(name: &str, path: &str) -> OpSpec {
    OpSpec {
        name: name.to_string().into(),
        path: path.to_string().into(),
        summary: "summary".to_string().into(),
        content_type: "application/json".to_string().into(),
        body_mode: BodyMode::KeyValue,
        params: Vec::new().into(),
    }
}

fn meta() -> CacheMeta {
    CacheMeta::new("a".repeat(64), "https://spec.example".to_string())
}

/// Write a cache file with a valid header around an arbitrary body.
fn write_body(file: &Path, magic: [u8; 8], version: u32, body: &[u8]) {
    let mut raw = Vec::new();
    raw.extend_from_slice(&magic);
    raw.extend_from_slice(&version.to_le_bytes());
    raw.extend_from_slice(&Sha256::digest(body));
    raw.extend_from_slice(body);
    fs::write(file, raw).unwrap();
}

/// Write a cache file from a framed table.
fn write_raw(file: &Path, magic: [u8; 8], version: u32, body: &Body) {
    write_body(file, magic, version, &frame(body));
}

fn temp_cache() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("ir-cache.bin");
    (dir, file)
}

#[test]
fn stores_and_loads_a_table() {
    let (_dir, file) = temp_cache();
    let ops = vec![op("things.info", "/api/things.info")];
    let meta = meta();
    cache::store_at(&file, &ops, &meta).expect("stores");

    let loaded = cache::load_at(&file).expect("loads").expect("is present");
    assert_eq!(loaded.ops, ops);
    assert_eq!(loaded.meta, meta);
    assert_eq!(loaded.meta.cli_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(loaded.meta.ir_schema_version, engine::IR_SCHEMA_VERSION);
}

#[test]
fn a_missing_cache_is_not_an_error() {
    let (_dir, file) = temp_cache();
    assert!(cache::load_at(&file).expect("no error").is_none());
}

#[test]
fn a_store_leaves_no_temporary_file_behind() {
    let (dir, file) = temp_cache();
    cache::store_at(&file, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    let leftovers: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "ir-cache.bin")
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
}

/// Replacing an existing cache must work on every platform. This is a
/// Windows regression test in particular: `std::fs::rename` there refuses
/// to replace an existing target, so a plain rename made every sync after
/// the first one fail. The CI matrix runs this on windows-latest.
#[test]
fn a_store_replaces_an_existing_cache() {
    let (_dir, file) = temp_cache();
    cache::store_at(&file, &[op("old.op", "/api/old.op")], &meta()).expect("stores");
    cache::store_at(&file, &[op("new.op", "/api/new.op")], &meta()).expect("replaces");
    let loaded = cache::load_at(&file).expect("loads").expect("is present");
    assert_eq!(loaded.ops.len(), 1);
    assert_eq!(loaded.ops[0].name, "new.op");

    // ...repeatedly, and the temporary files do not accumulate either.
    for index in 0..3 {
        let name = format!("op{index}.x");
        cache::store_at(&file, &[op(&name, &format!("/api/{name}"))], &meta())
            .expect("replaces again");
    }
    let entries: Vec<String> = fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, ["ir-cache.bin"], "leftovers: {entries:?}");
}

/// A cache file left behind by something else must not lend its
/// permissions to the new one: the store creates a fresh file and moves it
/// over the old path, so the result is 0600 no matter what was there.
#[cfg(unix)]
#[test]
fn a_store_does_not_inherit_a_loose_permission_mode() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, file) = temp_cache();
    fs::write(&file, b"pre-existing").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).unwrap();

    cache::store_at(&file, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "inherited mode {mode:o} from the old file");
}

/// A symlink where the cache should be is replaced, not followed: the
/// store must never write through it into a file elsewhere.
#[cfg(unix)]
#[test]
fn a_store_never_writes_through_a_symlink() {
    let dir = TempDir::new().unwrap();
    let victim = dir.path().join("victim.txt");
    fs::write(&victim, b"do not touch").unwrap();
    let file = dir.path().join("ir-cache.bin");
    std::os::unix::fs::symlink(&victim, &file).unwrap();

    cache::store_at(&file, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    assert_eq!(
        fs::read(&victim).unwrap(),
        b"do not touch",
        "the store wrote through the symlink"
    );
    assert!(!fs::symlink_metadata(&file).unwrap().is_symlink());
    assert!(cache::load_at(&file).expect("loads").is_some());
}

/// A temporary file someone else pre-created cannot capture the write:
/// names are random and creation is exclusive, so an existing one is
/// simply irrelevant.
#[test]
fn a_pre_existing_temporary_file_does_not_affect_the_store() {
    let (dir, file) = temp_cache();
    let squatted = dir
        .path()
        .join(format!("ir-cache.bin.tmp.{}", std::process::id()));
    fs::write(&squatted, b"squatted").unwrap();

    cache::store_at(&file, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    assert!(cache::load_at(&file).expect("loads").is_some());
    assert_eq!(
        fs::read(&squatted).unwrap(),
        b"squatted",
        "the store reused a predictable temp name"
    );
}

/// A table too large for the cache format must fail BEFORE any file is
/// written: a "successful" sync whose cache the loader then rejects as
/// oversized would silently never take effect.
#[test]
fn an_oversized_table_is_refused_without_writing_anything() {
    let (_dir, file) = temp_cache();
    // Stay under the operation-count ceiling and blow the BYTE limit
    // instead: long (but legal) names at the ceiling encode to megabytes.
    let filler = "n".repeat(110);
    let ops: Vec<OpSpec> = (0..cache::MAX_CACHED_OPS)
        .map(|index| {
            let name = format!("things.{filler}{index}");
            op(&name, &format!("/api/{name}"))
        })
        .collect();
    let error = cache::store_at(&file, &ops, &meta()).expect_err("must be refused");
    // The message names the CAUSE (encoded size) and both numbers, so a
    // user is not left guessing which limit they hit.
    let text = error.to_string();
    assert!(text.contains("encodes to"), "unexpected error: {text}");
    assert!(
        text.contains(&cache::MAX_CACHE_BODY_BYTES.to_string()),
        "{text}"
    );
    assert!(!file.exists(), "a rejected table was still written");
    let leftovers = fs::read_dir(file.parent().unwrap()).unwrap().count();
    assert_eq!(leftovers, 0, "temporary file left behind");
}

/// The size limits are one limit: whatever store accepts, load accepts -
/// header included. A table of several megabytes exercises the boundary
/// the two used to disagree about.
#[test]
fn the_store_and_load_size_limits_agree() {
    let (_dir, file) = temp_cache();
    // As close under both limits as a real table could get: the operation
    // ceiling, with names long enough to approach the byte limit too.
    let ops: Vec<OpSpec> = (0..cache::MAX_CACHED_OPS)
        .map(|index| {
            let name = format!("things.{:0>30}{index}", "n");
            op(&name, &format!("/api/{name}"))
        })
        .collect();
    cache::store_at(&file, &ops, &meta()).expect("a table under the limit stores");

    let size = fs::metadata(&file).unwrap().len() as usize;
    assert!(
        size > cache::MAX_CACHE_FILE_BYTES / 2,
        "the test table is too small to exercise the boundary: {size} bytes"
    );
    assert!(
        size <= cache::MAX_CACHE_FILE_BYTES,
        "stored {size} bytes, over the load limit"
    );
    assert_eq!(
        cache::load_at(&file)
            .expect("loads back")
            .expect("is present")
            .ops
            .len(),
        ops.len()
    );
}

#[test]
fn a_store_creates_missing_directories() {
    let dir = TempDir::new().unwrap();
    let file = dir
        .path()
        .join("nested")
        .join("deeper")
        .join("ir-cache.bin");
    cache::store_at(&file, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    assert!(cache::load_at(&file).expect("loads").is_some());
}

#[cfg(unix)]
#[test]
fn the_cache_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, file) = temp_cache();
    cache::store_at(&file, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "unexpected mode {mode:o}");
}

#[test]
fn a_truncated_cache_is_rejected() {
    let (_dir, file) = temp_cache();
    cache::store_at(&file, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    let raw = fs::read(&file).unwrap();
    for keep in [0, 8, 20, 44, raw.len() - 1] {
        fs::write(&file, &raw[..keep]).unwrap();
        let error = cache::load_at(&file).expect_err("must be rejected");
        assert!(!error.is_stale(), "truncation is damage, not staleness");
    }
}

#[test]
fn a_bit_flipped_cache_is_rejected_by_its_checksum() {
    let (_dir, file) = temp_cache();
    cache::store_at(&file, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    let mut raw = fs::read(&file).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 0xff;
    fs::write(&file, &raw).unwrap();
    let error = cache::load_at(&file).expect_err("must be rejected");
    assert!(
        error.to_string().contains("checksum"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_foreign_file_is_rejected() {
    let (_dir, file) = temp_cache();
    fs::write(&file, b"this is not a cache file at all, it is just text").unwrap();
    let error = cache::load_at(&file).expect_err("must be rejected");
    assert!(!error.is_stale(), "unexpected error: {error}");
}

#[test]
fn an_empty_file_is_rejected() {
    let (_dir, file) = temp_cache();
    fs::write(&file, b"").unwrap();
    assert!(cache::load_at(&file).is_err());
}

#[test]
fn another_layout_version_is_stale_not_damaged() {
    let (_dir, file) = temp_cache();
    write_raw(
        &file,
        MAGIC,
        FORMAT_VERSION + 1,
        &Body {
            meta: meta(),
            ops: vec![op("things.info", "/api/things.info")],
        },
    );
    let error = cache::load_at(&file).expect_err("must be rejected");
    assert!(error.is_stale(), "unexpected error: {error}");
}

#[test]
fn another_ir_schema_version_is_stale_not_damaged() {
    let (_dir, file) = temp_cache();
    let mut meta = meta();
    meta.ir_schema_version = engine::IR_SCHEMA_VERSION + 1;
    write_raw(
        &file,
        MAGIC,
        FORMAT_VERSION,
        &Body {
            meta,
            ops: vec![op("things.info", "/api/things.info")],
        },
    );
    let error = cache::load_at(&file).expect_err("must be rejected");
    assert!(error.is_stale(), "unexpected error: {error}");
    assert!(error.to_string().contains("IR schema"), "{error}");
}

#[test]
fn another_cli_version_is_stale_not_damaged() {
    let (_dir, file) = temp_cache();
    let mut meta = meta();
    meta.cli_version = "0.0.0-other".to_string();
    write_raw(
        &file,
        MAGIC,
        FORMAT_VERSION,
        &Body {
            meta,
            ops: vec![op("things.info", "/api/things.info")],
        },
    );
    let error = cache::load_at(&file).expect_err("must be rejected");
    assert!(error.is_stale(), "unexpected error: {error}");
    assert!(error.to_string().contains("0.0.0-other"), "{error}");
}

#[test]
fn a_version_string_from_the_file_is_never_echoed_raw() {
    let (_dir, file) = temp_cache();
    let mut meta = meta();
    // A hostile writer could put control characters or ANSI escapes there.
    meta.cli_version = "9.9.9\u{1b}[31mESCAPED\n\u{7}".to_string();
    write_raw(
        &file,
        MAGIC,
        FORMAT_VERSION,
        &Body {
            meta,
            ops: vec![op("things.info", "/api/things.info")],
        },
    );
    let error = cache::load_at(&file).expect_err("must be rejected");
    let text = error.to_string();
    assert!(!text.contains('\u{1b}'), "escape passed through: {text:?}");
    assert!(!text.contains('\u{7}'), "control char passed: {text:?}");
    assert!(text.contains("9.9.9"), "{text}");
}

#[test]
fn an_empty_operation_table_is_rejected() {
    let (_dir, file) = temp_cache();
    write_raw(
        &file,
        MAGIC,
        FORMAT_VERSION,
        &Body {
            meta: meta(),
            ops: Vec::new(),
        },
    );
    assert!(cache::load_at(&file).is_err());
}

/// The security case: a cache whose operation path escapes the base URL.
/// `https://host` + `@evil.example/x` is a request to evil.example with
/// the real host as userinfo - i.e. the bearer token leaves for another
/// origin. Such a table must never load.
#[test]
fn a_cache_with_a_hostile_operation_path_is_rejected() {
    for path in [
        "@evil.example/x",
        "//evil.example/x",
        "/api/../../x",
        ".evil",
        "/api/x?to=evil",
    ] {
        let (_dir, file) = temp_cache();
        write_raw(
            &file,
            MAGIC,
            FORMAT_VERSION,
            &Body {
                meta: meta(),
                ops: vec![op("things.info", path)],
            },
        );
        let error = cache::load_at(&file).expect_err("must be rejected");
        assert!(!error.is_stale(), "{path:?}: unexpected error: {error}");
    }
}

/// A valid body followed by extra bytes, with the checksum recomputed over
/// the whole thing, is not a file this build writes - so it is not trusted,
/// even though the decoder stops happily at the end of the object.
#[test]
fn trailing_bytes_after_the_body_are_rejected() {
    let (_dir, file) = temp_cache();
    cache::store_at(&file, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    let raw = fs::read(&file).unwrap();
    let (_, body) = raw.split_at(44);

    let mut extended = body.to_vec();
    extended.extend_from_slice(b"appended payload");
    write_body(&file, MAGIC, FORMAT_VERSION, &extended);

    let error = cache::load_at(&file).expect_err("must be rejected");
    assert!(!error.is_stale(), "unexpected error: {error}");
    assert!(error.to_string().contains("trailing"), "{error}");
}

/// The same-origin remap from the review: both fields are individually
/// well formed, but the path belongs to another operation, so calling the
/// harmless one would send the bearer token to the destructive one.
#[test]
fn a_cache_that_remaps_an_operation_to_another_endpoint_is_rejected() {
    let (_dir, file) = temp_cache();
    write_raw(
        &file,
        MAGIC,
        FORMAT_VERSION,
        &Body {
            meta: meta(),
            ops: vec![op("documents.search", "/api/documents.delete")],
        },
    );
    let error = cache::load_at(&file).expect_err("must be rejected");
    assert!(!error.is_stale(), "unexpected error: {error}");
    assert!(
        error
            .to_string()
            .contains("does not dispatch to its own endpoint"),
        "{error}"
    );
}

/// Hostile text in a cached operation is not printed: `api list` writes
/// summaries verbatim, and a terminal executes some byte sequences.
#[test]
fn a_cache_with_terminal_escapes_in_its_text_is_rejected() {
    for summary in [
        "\u{1b}]52;c;cGF3bmVk\u{7}",
        "forged\nthings.other\trow",
        "flip\u{202e}txet",
    ] {
        let (_dir, file) = temp_cache();
        let mut hostile = op("things.info", "/api/things.info");
        hostile.summary = summary.to_string().into();
        write_raw(
            &file,
            MAGIC,
            FORMAT_VERSION,
            &Body {
                meta: meta(),
                ops: vec![hostile],
            },
        );
        assert!(
            cache::load_at(&file).is_err(),
            "{summary:?} must be rejected"
        );
    }
}

/// The provenance record is displayed too, so it gets the same treatment.
#[test]
fn a_cache_with_a_hostile_provenance_record_is_rejected() {
    let cases = [
        CacheMeta {
            source: "https://x\u{1b}[31m".to_string(),
            ..meta()
        },
        CacheMeta {
            spec_hash: "not-a-hash".to_string(),
            ..meta()
        },
        CacheMeta {
            spec_hash: "z".repeat(64),
            ..meta()
        },
    ];
    for hostile in cases {
        let (_dir, file) = temp_cache();
        write_raw(
            &file,
            MAGIC,
            FORMAT_VERSION,
            &Body {
                meta: hostile,
                ops: vec![op("things.info", "/api/things.info")],
            },
        );
        assert!(cache::load_at(&file).is_err(), "must be rejected");
    }
}

/// Run `load_at` on another thread so a regression that blocks (a FIFO
/// opened without a type check) fails this test instead of hanging the
/// suite forever.
fn load_with_watchdog(file: &Path) -> Result<Option<cache::CachedIr>, cache::CacheError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let path = file.to_path_buf();
    std::thread::spawn(move || {
        let _ = sender.send(cache::load_at(&path));
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("load_at blocked: the cache path was opened without a type check")
}

/// A cache path that is not a regular file is never something this build
/// wrote, and reading it can hang (FIFO) or never end (`/dev/zero`).
#[cfg(unix)]
#[test]
fn a_cache_that_is_not_a_regular_file_is_refused_without_reading_it() {
    // A FIFO with no writer: `File::open` on it blocks indefinitely.
    let dir = TempDir::new().unwrap();
    let fifo = dir.path().join("ir-cache.bin");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let error = load_with_watchdog(&fifo).expect_err("a FIFO must be refused");
    assert!(!error.is_stale(), "{error}");
    assert!(error.to_string().contains("regular file"), "{error}");

    // A symlink to an endless device: `fs::metadata` would follow it and
    // report length 0, then read until memory ran out.
    let dir = TempDir::new().unwrap();
    let link = dir.path().join("ir-cache.bin");
    std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
    let error = load_with_watchdog(&link).expect_err("a symlink must be refused");
    assert!(error.to_string().contains("regular file"), "{error}");

    // Even a symlink to a perfectly good cache file: the loader only ever
    // reads the file it wrote, at the path it wrote it.
    let (_keep, real) = temp_cache();
    cache::store_at(&real, &[op("things.info", "/api/things.info")], &meta()).expect("stores");
    let dir = TempDir::new().unwrap();
    let link = dir.path().join("ir-cache.bin");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(load_with_watchdog(&link).is_err(), "symlink accepted");
}

#[test]
fn a_cache_path_that_is_a_directory_is_refused() {
    let dir = TempDir::new().unwrap();
    let as_dir = dir.path().join("ir-cache.bin");
    fs::create_dir(&as_dir).unwrap();
    assert!(load_with_watchdog(&as_dir).is_err());
}

/// The read is bounded by `take`, not by the size `metadata` reported, so a
/// file that grows in between cannot slip past the limit.
#[test]
fn a_cache_larger_than_the_limit_is_refused_by_the_read_itself() {
    let (_dir, file) = temp_cache();
    let oversized = vec![b'x'; cache::MAX_CACHE_FILE_BYTES + 1024];
    fs::write(&file, &oversized).unwrap();
    let error = cache::load_at(&file).expect_err("must be refused");
    assert!(error.to_string().contains("larger than"), "{error}");
}

/// The decode-amplification attack: a cache whose declared operation count
/// is small in bytes but enormous once decoded. bincode's byte limit does
/// not stop it, because a minimal `OpSpec` costs six encoded bytes and over
/// a hundred decoded.
#[test]
fn a_cache_declaring_more_operations_than_allowed_is_refused() {
    let (_dir, file) = temp_cache();
    let minimal: Vec<OpSpec> = (0..cache::MAX_CACHED_OPS + 1)
        .map(|index| {
            let name = format!("t{index}.i");
            op(&name, &format!("/api/{name}"))
        })
        .collect();
    // Well inside the byte limit - the count is what has to stop it.
    write_raw(
        &file,
        MAGIC,
        FORMAT_VERSION,
        &Body {
            meta: meta(),
            ops: minimal,
        },
    );
    let size = fs::metadata(&file).unwrap().len() as usize;
    assert!(size < cache::MAX_CACHE_FILE_BYTES, "{size} bytes");

    let error = cache::load_at(&file).expect_err("must be refused");
    assert!(!error.is_stale(), "{error}");
    assert!(error.to_string().contains("operations"), "{error}");
}

/// The same amplification through a single operation's parameter list,
/// which the element count alone would not catch: the decoded footprint is
/// what stops it.
#[test]
fn a_cache_whose_operations_decode_to_too_much_memory_is_refused() {
    let (_dir, file) = temp_cache();
    let mut fat = op("things.info", "/api/things.info");
    // Empty parameters: eight encoded bytes each, ~136 decoded.
    fat.params = (0..80_000)
        .map(|_| engine::ir::ParamSpec {
            name: String::new().into(),
            ty: engine::ir::ParamType::String,
            required: false,
            nullable: false,
            enum_values: Vec::new().into(),
            format: String::new().into(),
            minimum: None,
            maximum: None,
        })
        .collect::<Vec<_>>()
        .into();
    write_raw(
        &file,
        MAGIC,
        FORMAT_VERSION,
        &Body {
            meta: meta(),
            ops: vec![fat],
        },
    );
    let size = fs::metadata(&file).unwrap().len() as usize;
    assert!(size < cache::MAX_CACHE_FILE_BYTES, "{size} bytes");

    let error = cache::load_at(&file).expect_err("must be refused");
    assert!(!error.is_stale(), "{error}");
}

/// The construction that broke the previous bound: many enum containers
/// just past the point where serde's reservation logic stops capping and
/// starts doubling. Each one costs ~44 KiB of input and ~2 MiB of heap, so
/// a body well under the file limit could reach tens of megabytes before
/// anything measured it.
///
/// It is now refused by the per-record size limit, which is the point of
/// framing the table: an operation only has one record's worth of bytes to
/// declare its contents with.
#[test]
fn an_operation_packed_with_enum_containers_is_refused() {
    let (_dir, file) = temp_cache();
    // Just past serde's cautious reservation cap for `Cow<str>`
    // (1 MiB / 24 = 43,690 elements), which is where capacity doubles.
    let over_the_cap = 43_691;
    let mut fat = op("things.info", "/api/things.info");
    fat.params = (0..3)
        .map(|_| ParamSpec {
            name: "p".to_string().into(),
            ty: ParamType::String,
            required: false,
            nullable: false,
            enum_values: vec![std::borrow::Cow::Owned(String::new()); over_the_cap].into(),
            format: String::new().into(),
            minimum: None,
            maximum: None,
        })
        .collect::<Vec<_>>()
        .into();

    // The write path refuses it outright (here at the semantic enum cap,
    // which comes first) and leaves no file behind.
    let error = cache::store_at(&file, &[fat.clone()], &meta()).expect_err("must be refused");
    assert!(!file.exists(), "a rejected table was written: {error}");

    // The load path is the one that matters, because a hostile cache never
    // went through the write path. Framed by hand, the operation's record
    // is far past the per-record limit...
    let framed = frame(&Body {
        meta: meta(),
        ops: vec![fat],
    });
    assert!(
        framed.len() > 3 * 43_691,
        "the test input is not the amplifying shape: {} bytes",
        framed.len()
    );
    write_body(&file, MAGIC, FORMAT_VERSION, &framed);

    // ...so it is refused by the framing, before a byte of it is decoded
    // and long before anything could be allocated for those containers.
    let error = cache::load_at(&file).expect_err("must be refused");
    assert!(!error.is_stale(), "{error}");
    let text = error.to_string();
    assert!(
        text.contains("record declares") || text.contains("byte limit"),
        "not refused by the framing: {text}"
    );
}

/// A record may not lie about its own length: the framing checks it
/// against the limit AND against the bytes that actually remain, before
/// anything is allocated or decoded.
#[test]
fn a_record_that_lies_about_its_length_is_refused() {
    let (_dir, file) = temp_cache();
    let good = Body {
        meta: meta(),
        ops: vec![op("things.info", "/api/things.info")],
    };
    let framed = frame(&good);

    // The operation record's length field sits after the meta record and
    // the count; overstate it and the body ends before the record does.
    let meta_len = u32::from_le_bytes(framed[0..4].try_into().unwrap()) as usize;
    let op_len_at = 4 + meta_len + 4;
    let mut forged = framed.clone();
    forged[op_len_at..op_len_at + 4].copy_from_slice(&999_999u32.to_le_bytes());
    write_body(&file, MAGIC, FORMAT_VERSION, &forged);
    let error = cache::load_at(&file).expect_err("must be refused");
    assert!(!error.is_stale(), "{error}");

    // A length past the per-record limit is refused before the remaining
    // bytes even matter.
    let mut forged = framed.clone();
    forged[op_len_at..op_len_at + 4]
        .copy_from_slice(&((cache::MAX_OP_RECORD_BYTES + 1) as u32).to_le_bytes());
    write_body(&file, MAGIC, FORMAT_VERSION, &forged);
    assert!(cache::load_at(&file).is_err());
}

/// A short body that declares thousands of operations must be refused on
/// the spot, not reserved for: the count is checked against the bytes that
/// remain, not against the format's maximum.
#[test]
fn a_short_body_cannot_declare_a_huge_operation_count() {
    let (_dir, file) = temp_cache();
    let body = Body {
        meta: meta(),
        ops: vec![op("things.info", "/api/things.info")],
    };
    for declared in [8192u32, 100_000, u32::MAX] {
        write_body(
            &file,
            MAGIC,
            FORMAT_VERSION,
            &frame_with_count(&body, declared),
        );
        let error = cache::load_at(&file).expect_err("must be refused");
        assert!(!error.is_stale(), "{declared}: {error}");
    }
}

/// Whatever `store_at` accepts, `load_at` accepts - including the
/// footprint rule, which store used to skip.
#[test]
fn a_stored_table_always_loads_back() {
    let (_dir, file) = temp_cache();
    // Short, legal parameter names: encodes small, decodes big. This is
    // the shape that used to store "successfully" and be rejected on the
    // next command.
    let mut heavy = op("things.info", "/api/things.info");
    heavy.params = (0..2000)
        .map(|index| ParamSpec {
            name: format!("p{index}").into(),
            ty: ParamType::String,
            required: false,
            nullable: false,
            enum_values: Vec::new().into(),
            format: String::new().into(),
            minimum: None,
            maximum: None,
        })
        .collect::<Vec<_>>()
        .into();
    let table: Vec<OpSpec> = (0..200)
        .map(|index| {
            let mut op = heavy.clone();
            let name = format!("things.op{index}");
            op.path = format!("/api/{name}").into();
            op.name = name.into();
            op
        })
        .collect();

    match cache::store_at(&file, &table, &meta()) {
        Ok(()) => {
            let loaded = cache::load_at(&file)
                .expect("what store wrote, load must read")
                .expect("is present");
            assert_eq!(loaded.ops.len(), table.len());
        }
        Err(error) => {
            // Refusing is fine; writing a file that cannot be read is not.
            assert!(!file.exists(), "refused with {error}, but wrote a file");
        }
    }
}

/// The ceiling is not off by one, and a table right below it still works.
#[test]
fn a_cache_at_the_operation_ceiling_still_loads() {
    let (_dir, file) = temp_cache();
    let ops: Vec<OpSpec> = (0..cache::MAX_CACHED_OPS)
        .map(|index| {
            let name = format!("t{index}.i");
            op(&name, &format!("/api/{name}"))
        })
        .collect();
    cache::store_at(&file, &ops, &meta()).expect("stores");
    assert_eq!(
        cache::load_at(&file)
            .expect("loads")
            .expect("is present")
            .ops
            .len(),
        cache::MAX_CACHED_OPS
    );
}

/// `store_at` must not accept metadata that `load_at` would reject as
/// stale: writing a cache that the next command discards is the same
/// broken promise as writing an oversized one.
#[test]
fn storing_metadata_from_another_build_is_refused() {
    let (_dir, file) = temp_cache();
    let ops = [op("things.info", "/api/things.info")];
    for hostile in [
        CacheMeta {
            ir_schema_version: engine::IR_SCHEMA_VERSION + 1,
            ..meta()
        },
        CacheMeta {
            cli_version: "0.0.0-other".to_string(),
            ..meta()
        },
    ] {
        assert!(
            cache::store_at(&file, &ops, &hostile).is_err(),
            "stale metadata was written"
        );
        assert!(!file.exists(), "a rejected store still wrote a file");
    }
}

#[test]
fn spec_hash_is_stable_and_content_addressed() {
    let one = cache::spec_hash("{\"a\":1}");
    assert_eq!(one, cache::spec_hash("{\"a\":1}"));
    assert_ne!(one, cache::spec_hash("{\"a\":2}"));
    assert_eq!(one.len(), 64);
    assert!(one.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn the_cache_directory_honours_the_environment_override() {
    // Reading the resolved path is a pure function of the environment; the
    // CLI tests exercise the override end to end in a child process.
    let path = cache::path().expect("a cache path is resolvable");
    assert_eq!(
        path.file_name().map(|name| name.to_string_lossy()),
        Some("ir-cache.bin".into())
    );
}
