//! Golden-file tests for the schema/data-driven table renderer.
//!
//! Per project rule, all output rendering is covered by golden files.
//! Regenerate with: `OTL_UPDATE_GOLDEN=1 cargo test -p outline-cli --test
//! render_golden` (then review the diff by eye before committing).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use otl::render::{render, resolve_mode, OutputMode};
use serde_json::{json, Value};

/// Compare rendered output against a golden file (or rewrite it when
/// `OTL_UPDATE_GOLDEN` is set).
fn assert_golden(payload: &Value, mode: OutputMode, golden_name: &str) {
    let rendered = format!("{}\n", render(payload, mode).unwrap());
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(golden_name);
    if std::env::var_os("OTL_UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read golden file {}: {error}", path.display()));
    assert_eq!(
        rendered, expected,
        "output does not match golden file {golden_name}"
    );
}

fn documents_payload() -> Value {
    json!([
        {
            "id": "doc-1",
            "title": "Welcome to Acme",
            "text": "A very long markdown body that must never become a table column",
            "collectionId": "col-9",
            "updatedAt": "2026-08-01T10:00:00.000Z",
            "createdAt": "2026-07-01T09:00:00.000Z"
        },
        {
            "id": "doc-2",
            "title": "Roadmap 2026",
            "text": "More markdown",
            "collectionId": "col-9",
            "updatedAt": "2026-08-20T15:30:00.000Z",
            "createdAt": "2026-05-11T08:12:00.000Z"
        }
    ])
}

#[test]
fn table_prefers_id_title_and_at_columns() {
    // text (long body) and collectionId must lose to id/title/updatedAt/
    // createdAt without any documents-specific rendering code.
    assert_golden(
        &documents_payload(),
        OutputMode::Table,
        "table_documents.txt",
    );
}

#[test]
fn table_uses_name_when_there_is_no_title() {
    let payload = json!([
        { "id": "col-1", "name": "Engineering", "sort": {"field": "index"}, "index": "a" },
        { "id": "col-2", "name": "Design", "sort": {"field": "index"}, "index": "b" }
    ]);
    // `sort` is an object: never a column. Booleans/numbers render as JSON.
    assert_golden(&payload, OutputMode::Table, "table_collections.txt");
}

#[test]
fn table_truncates_long_cells() {
    let payload = json!([
        {
            "id": "doc-long",
            "title": "An extremely long document title that keeps going well past the cell cap"
        },
        { "id": "doc-short", "title": "Short" }
    ]);
    assert_golden(&payload, OutputMode::Table, "table_truncation.txt");
}

#[test]
fn table_handles_missing_keys_and_scalar_types() {
    let payload = json!([
        { "id": "a", "title": "Has title", "pinned": true, "revision": 3 },
        { "id": "b", "pinned": false, "revision": 14 },
        { "id": "c", "title": null, "pinned": true, "revision": 7 }
    ]);
    assert_golden(&payload, OutputMode::Table, "table_missing_keys.txt");
}

#[test]
fn table_renders_empty_list_placeholder() {
    assert_golden(&json!([]), OutputMode::Table, "table_empty.txt");
}

#[test]
fn table_strips_control_characters_from_server_data() {
    // Server-controlled strings must not smuggle ANSI escapes or newlines
    // into the terminal.
    let payload = json!([
        { "id": "doc-evil", "title": "red\u{1b}[31mtext\nline" }
    ]);
    let rendered = render(&payload, OutputMode::Table).unwrap();
    assert!(!rendered.contains('\u{1b}'), "ESC leaked: {rendered:?}");
    assert!(
        !rendered.contains("text\nline"),
        "newline in cell leaked: {rendered:?}"
    );
}

#[test]
fn table_mode_falls_back_to_json_for_non_list_payloads() {
    let object = json!({ "id": "doc-1", "title": "Hello" });
    let table = render(&object, OutputMode::Table).unwrap();
    let json_out = render(&object, OutputMode::Json).unwrap();
    assert_eq!(table, json_out);

    let scalars = json!(["a", "b"]);
    assert_eq!(
        render(&scalars, OutputMode::Table).unwrap(),
        render(&scalars, OutputMode::Json).unwrap()
    );
}

#[test]
fn json_mode_is_pretty_json_without_decoration() {
    let rendered = render(&documents_payload(), OutputMode::Json).unwrap();
    let reparsed: Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(reparsed, documents_payload());
    assert!(!rendered.contains('\u{1b}'), "ANSI in JSON output");
}

#[test]
fn mode_resolution_follows_flag_then_tty() {
    // --json always wins; otherwise a TTY gets the table and a pipe gets
    // JSON (which also disables any ANSI decoration).
    assert_eq!(resolve_mode(true, true), OutputMode::Json);
    assert_eq!(resolve_mode(true, false), OutputMode::Json);
    assert_eq!(resolve_mode(false, true), OutputMode::Table);
    assert_eq!(resolve_mode(false, false), OutputMode::Json);
}
