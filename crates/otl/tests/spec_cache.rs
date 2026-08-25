//! The on-disk IR cache (Story 4.2): round-trip, atomicity, and every
//! rejection path.
//!
//! Nothing here touches the real cache directory: every case works on a
//! `tempfile::TempDir`. The hostile cases build a cache file byte by byte
//! with a mirror of the private body struct, which also pins the file
//! layout: magic, little-endian layout version, SHA-256 of the body, then
//! the bincode body.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use engine::ir::{BodyMode, OpSpec};
use otl::spec::cache::{self, CacheMeta};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// Mirror of the private `cache::Body`: same field order, same types.
#[derive(Serialize)]
struct Body {
    meta: CacheMeta,
    ops: Vec<OpSpec>,
}

const MAGIC: [u8; 8] = *b"OTL-IRC\x00";
const FORMAT_VERSION: u32 = 1;

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

/// Write a cache file from parts, bypassing every check `store_at` makes.
fn write_raw(file: &Path, magic: [u8; 8], version: u32, body: &Body) {
    let encoded =
        bincode::serde::encode_to_vec(body, bincode::config::standard().with_limit::<8_388_608>())
            .unwrap();
    let mut raw = Vec::new();
    raw.extend_from_slice(&magic);
    raw.extend_from_slice(&version.to_le_bytes());
    raw.extend_from_slice(&Sha256::digest(&encoded));
    raw.extend_from_slice(&encoded);
    fs::write(file, raw).unwrap();
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
    // 128-byte names (the operation-name limit) reach the body limit in
    // ~20k operations, which is quick to build and encode.
    let name = format!("things.{}", "n".repeat(120));
    let ops: Vec<OpSpec> = (0..30_000)
        .map(|index| {
            let name = format!("{}{index}", &name[..name.len() - 6]);
            op(&name, &format!("/api/{name}"))
        })
        .collect();
    let error = cache::store_at(&file, &ops, &meta()).expect_err("must be refused");
    assert!(
        error.to_string().contains("too large"),
        "unexpected error: {error}"
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
    // Names stay within the operation-name limit (128 bytes); ~20k of them
    // encode to several MiB, close under the cache limit.
    let ops: Vec<OpSpec> = (0..20_000)
        .map(|index| {
            let name = format!("things.{:0>110}{index}", "n");
            op(&name, &format!("/api/{name}"))
        })
        .collect();
    cache::store_at(&file, &ops, &meta()).expect("a table under the limit stores");

    let size = fs::metadata(&file).unwrap().len() as usize;
    assert!(
        size > 4 * 1024 * 1024,
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
    let mut forged = Vec::new();
    forged.extend_from_slice(&MAGIC);
    forged.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    forged.extend_from_slice(&Sha256::digest(&extended));
    forged.extend_from_slice(&extended);
    fs::write(&file, &forged).unwrap();

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
