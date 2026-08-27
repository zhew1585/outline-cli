//! The migration path across an `IR_SCHEMA_VERSION` bump.
//!
//! Every bump invalidates the cache `otl spec sync` has written on a user's
//! machine (recursive response descriptors took the IR from 6 to 7, the
//! `children_omitted` marker from 7 to 8). That is expected and by design -
//! a cache is regenerable - but "expected" only counts if the four things a
//! user then does actually work. This file builds a cache in an OLD shape, by
//! hand, and drives them:
//!
//! 1. ordinary commands keep working, on the built-in table;
//! 2. the warning says the cache is OUTDATED, not damaged, and names the fix;
//! 3. `otl doctor` reports it and does not call the environment broken;
//! 4. `otl spec sync` rebuilds it and `otl spec reset` clears it.
//!
//! # Why the old records are written out by hand, and what that does NOT
//! prove
//!
//! Operation records are `bincode`: positional, with no field names. A test
//! that wrote a CURRENT record and only lowered the version number in the
//! metadata would describe a file no build ever produced, so the v6 structs
//! are mirrored below, field for field and variant for variant, and
//! serialized with the same encoder.
//!
//! What that buys is a PLAUSIBLE fixture, and nothing more: `cache::load`
//! compares the version in the metadata before it decodes any operation, so
//! these records are never decoded and this file cannot tell whether the
//! mirror is faithful. Do not read the mirror as an assertion about the v6
//! layout. Nor is the exact old version load-bearing - every version other
//! than the current one takes the same path - and that is why the mirror is
//! left at 6 rather than chased forward on each bump: a fixture that has to
//! be rewritten to keep testing the same gate is a fixture that will be
//! rewritten wrongly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use assert_cmd::Command;
use serde::Serialize;
use serde_json::Value;
use tempfile::TempDir;

mod common;
use common::cache::{push_record, record, write_body, FORMAT_VERSION, MAGIC};
use common::isolate;

/// The version these hand-written records claim to be. Any version other
/// than the current one exercises the same gate; see the module note.
const PREVIOUS_IR_SCHEMA_VERSION: u32 = 6;

// --- the v6 layout, mirrored ------------------------------------------------
//
// Same field order and same variant order as `engine::ir` had at version 6.
// `Cow<'static, str>` encodes as a string and `Cow<'static, [T]>` as a
// sequence, so these produce byte-identical records to what that build wrote.

#[derive(Serialize)]
enum OldParamType {
    String,
    #[allow(dead_code)]
    Integer,
    #[allow(dead_code)]
    Boolean,
    #[allow(dead_code)]
    Number,
    #[allow(dead_code)]
    Json,
}

#[derive(Serialize)]
enum OldBodyMode {
    KeyValue,
    #[allow(dead_code)]
    RawJsonOnly,
    #[allow(dead_code)]
    Unsupported,
}

/// v6 `ParamSpec`, before response fields gained recursive metadata.
#[derive(Serialize)]
struct OldParamSpec {
    name: String,
    ty: OldParamType,
    required: bool,
    nullable: bool,
    enum_values: Vec<String>,
    format: String,
    minimum: Option<f64>,
    maximum: Option<f64>,
    description: String,
}

#[derive(Serialize)]
struct OldFieldSpec {
    name: String,
    ty: OldParamType,
    format: String,
    nullable: bool,
    read_only: bool,
}

#[derive(Serialize)]
struct OldOpSpec {
    name: String,
    path: String,
    summary: String,
    content_type: String,
    body_mode: OldBodyMode,
    params: Vec<OldParamSpec>,
    response_fields: Vec<OldFieldSpec>,
}

fn old_op() -> OldOpSpec {
    OldOpSpec {
        name: "things.info".to_string(),
        path: "/api/things.info".to_string(),
        summary: "A table written by the previous build".to_string(),
        content_type: "application/json".to_string(),
        body_mode: OldBodyMode::KeyValue,
        params: vec![OldParamSpec {
            name: "id".to_string(),
            ty: OldParamType::String,
            required: true,
            nullable: false,
            enum_values: Vec::new(),
            format: "uuid".to_string(),
            minimum: None,
            maximum: None,
            description: "Thing identifier".to_string(),
        }],
        response_fields: vec![OldFieldSpec {
            name: "id".to_string(),
            ty: OldParamType::String,
            format: "uuid".to_string(),
            nullable: false,
            read_only: true,
        }],
    }
}

/// Write a cache in the v6 shape into a fresh cache directory.
fn previous_version_cache() -> TempDir {
    let dir = TempDir::new().unwrap();
    let mut meta =
        otl::spec::cache::CacheMeta::new("a".repeat(64), "https://spec.example".to_string());
    meta.ir_schema_version = PREVIOUS_IR_SCHEMA_VERSION;

    let ops = vec![old_op()];
    let mut body = Vec::new();
    push_record(&mut body, &record(&meta));
    body.extend_from_slice(&(ops.len() as u32).to_le_bytes());
    for op in &ops {
        push_record(&mut body, &record(op));
    }
    write_body(
        &dir.path().join("ir-cache.bin"),
        MAGIC,
        FORMAT_VERSION,
        &body,
    );
    dir
}

fn otl(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    isolate(&mut cmd)
        .env("OTL_CACHE_DIR", cache)
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY");
    cmd
}

fn run(cache: &Path, args: &[&str]) -> (String, String, i32) {
    let output = otl(cache).args(args).output().unwrap();
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// The first thing a user does after upgrading is run a command. It works,
/// on the built-in table, and the diagnostic tells the truth about why.
#[test]
fn a_cache_from_the_previous_ir_version_is_outdated_not_damaged() {
    let cache = previous_version_cache();
    let (stdout, stderr, code) = run(cache.path(), &["api", "list"]);
    assert_eq!(code, 0, "an old cache must never brick a command: {stderr}");

    // The whole point of decoding the provenance record before the operation
    // records: `bincode` is positional, so reading v6 operations as v7 would
    // fail somewhere in the middle and be reported as corruption.
    assert!(stderr.contains("outdated"), "{stderr}");
    assert!(!stderr.contains("damaged"), "{stderr}");
    assert!(stderr.contains("IR schema version 6"), "{stderr}");
    assert!(stderr.contains("spec sync"), "no remedy named: {stderr}");

    // The built-in table took over: the old cache's operation is gone and
    // the vendored ones are back.
    let rows: Value = serde_json::from_str(&stdout).expect("json listing");
    let names: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert!(names.contains(&"documents.info"), "built-in table missing");
    assert!(!names.contains(&"things.info"), "the stale table was used");
}

/// `describe` is on the same table resolution, so it must answer the same
/// way rather than, say, failing where `list` succeeded.
#[test]
fn describe_falls_back_to_the_built_in_table_too() {
    let cache = previous_version_cache();
    let (stdout, stderr, code) = run(cache.path(), &["api", "describe", "documents.info"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("outdated"), "{stderr}");
    let contract: Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(contract["source"], "built-in", "{contract}");
}

/// `doctor` exists to explain exactly this state, and a discarded cache is
/// a WARNING: nothing about it stops `otl` from working.
#[test]
fn doctor_reports_the_discarded_cache_without_calling_the_environment_broken() {
    let cache = previous_version_cache();
    let (stdout, _, _) = run(
        cache.path(),
        &[
            "doctor",
            "--offline",
            "--json",
            "--url",
            "https://example.invalid",
        ],
    );
    let report: Value = serde_json::from_str(&stdout).expect("json report");
    let spec = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["check"] == "local-spec")
        .expect("no local-spec check");
    assert_eq!(spec["status"], "warn", "{spec}");
    let text = spec.to_string();
    assert!(
        text.contains("outdated") || text.contains("built-in"),
        "{spec}"
    );
}

/// The fix the diagnostic names has to actually work, and `spec reset` has
/// to be able to clear a file this build cannot read.
#[test]
fn sync_rebuilds_the_cache_and_reset_clears_it() {
    let cache = previous_version_cache();
    let document = cache.path().join("document.json");
    std::fs::write(
        &document,
        r#"{"openapi":"3.0.0","paths":{"/things.info":{"post":{
             "summary":"Rebuilt","requestBody":{"content":{"application/json":{"schema":{
               "type":"object","required":["id"],
               "properties":{"id":{"type":"string","description":"Which thing."}}}}}}}}}}"#,
    )
    .unwrap();

    let (stdout, stderr, code) = run(
        cache.path(),
        &["spec", "sync", "--spec", document.to_str().unwrap()],
    );
    assert_eq!(
        code, 0,
        "sync could not replace an old cache: {stdout}{stderr}"
    );

    // The rebuilt table is effective, carries the new field, and no longer
    // warns about anything.
    let (stdout, stderr, code) = run(cache.path(), &["api", "describe", "things.info"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "still warning after a sync: {stderr}");
    let contract: Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(contract["source"], "synced", "{contract}");
    assert_eq!(contract["parameters"][0]["description"], "Which thing.");

    let (_, stderr, code) = run(cache.path(), &["spec", "reset"]);
    assert_eq!(code, 0, "{stderr}");
    let (stdout, stderr, code) = run(cache.path(), &["api", "describe", "documents.info"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let contract: Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(contract["source"], "built-in", "{contract}");
}

/// `spec reset` must clear an unreadable cache too - a user who hits the
/// warning and reaches for the bigger hammer cannot be told the file is
/// unreadable and therefore undeletable.
#[test]
fn reset_removes_a_cache_this_build_cannot_read() {
    let cache = previous_version_cache();
    let (_, stderr, code) = run(cache.path(), &["spec", "reset"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        !cache.path().join("ir-cache.bin").exists(),
        "the old cache survived a reset"
    );
    let (_, stderr, code) = run(cache.path(), &["api", "list"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "still warning after a reset: {stderr}");
}
