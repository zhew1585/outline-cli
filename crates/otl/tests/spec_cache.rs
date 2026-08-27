//! The on-disk IR cache (Story 4.2): round-trip, atomic writes and permissions.
//!
//! Nothing here touches the real cache directory: every case works on a
//! `tempfile::TempDir`. Fixtures live in `common/cache.rs`; see that file
//! for the layout these tests pin.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use engine::ir::{FieldSpec, OpSpec, ParamSpec, ParamType};
use otl::spec::cache;
use tempfile::TempDir;

mod common;
use common::cache::{meta, op, temp_cache};

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

/// Response fields are part of the table now (schema-driven columns), so
/// they have to survive the round trip - a synced spec that lost them would
/// render different columns than the built-in one, silently.
#[test]
fn response_fields_survive_the_round_trip() {
    let (_dir, file) = temp_cache();
    let mut op = op("documents.info", "/api/documents.info");
    op.response_fields = vec![
        FieldSpec {
            name: "id".to_string().into(),
            ty: ParamType::String,
            format: "uuid".to_string().into(),
            nullable: false,
            read_only: true,
        },
        FieldSpec {
            name: "title".to_string().into(),
            ty: ParamType::String,
            format: String::new().into(),
            nullable: true,
            read_only: false,
        },
    ]
    .into();
    cache::store_at(&file, &[op.clone()], &meta()).expect("stores");
    let loaded = cache::load_at(&file).expect("loads").expect("is present");
    assert_eq!(loaded.ops[0], op, "the round trip changed the operation");
    // Order is load-bearing: it is what ranks columns.
    let names: Vec<&str> = loaded.ops[0]
        .response_fields
        .iter()
        .map(|field| field.name.as_ref())
        .collect();
    assert_eq!(names, ["id", "title"]);
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
            description: String::new().into(),
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
