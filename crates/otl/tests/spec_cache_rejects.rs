//! The on-disk IR cache: every rejection path.
//!
//! Nothing here touches the real cache directory: every case works on a
//! `tempfile::TempDir`. Fixtures live in `common/cache.rs`; see that file
//! for the layout these tests pin.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use engine::ir::{FieldSpec, OpSpec, ParamSpec, ParamType};
use otl::spec::cache::{self, CacheMeta};
use tempfile::TempDir;

mod common;
use common::cache::{
    frame, frame_with_count, meta, op, temp_cache, write_body, write_raw, Body, FORMAT_VERSION,
    MAGIC,
};

/// Hostile text in a response field name would land in a column HEADER.
#[test]
fn a_cache_with_escapes_in_a_field_name_is_rejected() {
    let (_dir, file) = temp_cache();
    let mut hostile = op("things.info", "/api/things.info");
    hostile.response_fields = vec![FieldSpec {
        name: "id\u{1b}[31m".to_string().into(),
        ty: ParamType::String,
        format: String::new().into(),
        nullable: false,
        read_only: false,
    }]
    .into();
    write_raw(
        &file,
        MAGIC,
        FORMAT_VERSION,
        &Body {
            meta: meta(),
            ops: vec![hostile],
        },
    );
    let error = cache::load_at(&file).expect_err("must be refused");
    assert!(!error.is_stale(), "{error}");
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
    // and long before anything could be allocated for those containers -
    // and the message says WHICH operation and why, the same way the write
    // path does, rather than "a record is too big".
    let error = cache::load_at(&file).expect_err("must be refused");
    assert!(!error.is_stale(), "{error}");
    let text = format!("{error} {}", error.remedy());
    assert!(text.contains("operation #0"), "does not say which: {text}");
    assert!(
        text.contains(&cache::MAX_OP_RECORD_BYTES.to_string()),
        "does not say the limit: {text}"
    );
    assert!(
        text.contains("parameters or enumerated values"),
        "generic remedy on a per-operation limit: {text}"
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

/// Each resource limit has to say WHICH limit was hit, with the actual
/// number, and point somewhere useful. "Too big" with the wrong cause
/// sends the user looking in the wrong place.
#[test]
fn each_resource_limit_reports_its_own_cause_and_remedy() {
    let (_dir, file) = temp_cache();

    // Too many operations: names the count and the ceiling.
    let many: Vec<OpSpec> = (0..cache::MAX_CACHED_OPS + 1)
        .map(|index| {
            let name = format!("t{index}.i");
            op(&name, &format!("/api/{name}"))
        })
        .collect();
    let error = cache::store_at(&file, &many, &meta()).expect_err("must be refused");
    let text = format!("{error} {}", error.remedy());
    assert!(text.contains(&many.len().to_string()), "{text}");
    assert!(text.contains(&cache::MAX_CACHED_OPS.to_string()), "{text}");
    assert!(text.contains("operations you need"), "no remedy: {text}");

    // One operation too large: names THAT operation, and its remedy talks
    // about parameters rather than about the document's size.
    let mut fat = op("things.big", "/api/things.big");
    fat.params = (0..4000)
        .map(|index| ParamSpec {
            name: format!("parameter{index}").into(),
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
    let error = cache::store_at(&file, &[fat], &meta()).expect_err("must be refused");
    let text = format!("{error} {}", error.remedy());
    assert!(text.contains("things.big"), "does not say which: {text}");
    assert!(
        text.contains(&cache::MAX_OP_RECORD_BYTES.to_string()),
        "{text}"
    );
    assert!(text.contains("parameters"), "wrong remedy: {text}");
    assert!(
        !text.contains("far fewer operations"),
        "blames the operation count for a parameter problem: {text}"
    );

    // Nothing was written for either.
    assert!(!file.exists());
}
