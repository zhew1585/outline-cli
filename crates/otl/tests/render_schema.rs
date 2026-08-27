//! Schema-driven table columns.
//!
//! The schema supplies the column RANKING, which is a property of the
//! operation; the payload decides which of the ranked fields are shown, since
//! no OpenAPI facet here states that a field is always present. Both halves
//! are covered, plus the fallback to the data-driven policy when the schema
//! contributes nothing.
//!
//! Regenerate golden files with: `OTL_UPDATE_GOLDEN=1 cargo test -p
//! outline-cli --test render_schema` (then review the diff by eye).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use engine::{FieldContainer, FieldSpec, ParamType};
use otl::render::{render, OutputMode};
use serde_json::{json, Value};

/// No response schema: the data-driven fallback policy.
const NO_SCHEMA: &[FieldSpec] = &[];

/// A response-field descriptor, as `build.rs` compiles them.
fn field(
    name: &'static str,
    ty: ParamType,
    format: &'static str,
    nullable: bool,
    read_only: bool,
) -> FieldSpec {
    FieldSpec {
        name: std::borrow::Cow::Borrowed(name),
        ty,
        format: std::borrow::Cow::Borrowed(format),
        nullable,
        read_only,
        depth: 0,
        container: FieldContainer::None,
    }
}

/// Whether golden files may be rewritten instead of asserted.
fn golden_update_requested() -> bool {
    std::env::var("OTL_UPDATE_GOLDEN").ok().as_deref() == Some("1") && std::env::var("CI").is_err()
}

/// Compare rendered output against a golden file, with an explicit schema.
fn assert_golden_with(payload: &Value, mode: OutputMode, schema: &[FieldSpec], golden_name: &str) {
    let rendered = format!("{}\n", render(payload, mode, schema).unwrap());
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(golden_name);
    if golden_update_requested() {
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

/// The `Document` response schema, in the vendored spec's own field order
/// (abridged to the fields that matter for column selection).
fn document_schema() -> Vec<FieldSpec> {
    vec![
        field("id", ParamType::String, "uuid", false, true),
        field("collectionId", ParamType::String, "uuid", true, false),
        field("parentDocumentId", ParamType::String, "uuid", true, false),
        field("title", ParamType::String, "", false, false),
        field("fullWidth", ParamType::Boolean, "", false, false),
        field("icon", ParamType::String, "", true, false),
        field("text", ParamType::String, "", false, false),
        field("url", ParamType::String, "", false, true),
        field("urlId", ParamType::String, "", false, false),
        field("tasks", ParamType::Json, "", false, false),
        field("revision", ParamType::Number, "", false, true),
        field("createdAt", ParamType::String, "date-time", false, true),
        field("createdBy", ParamType::Json, "", false, false),
        field("updatedAt", ParamType::String, "date-time", false, true),
        field("publishedAt", ParamType::String, "date-time", true, true),
        field("archivedAt", ParamType::String, "date-time", true, true),
    ]
}

/// The `Collection` response schema: here the schema declares a read-only
/// `url` BEFORE the writable `name`, which is what makes the read-only
/// signal load-bearing.
fn collection_schema() -> Vec<FieldSpec> {
    vec![
        field("id", ParamType::String, "uuid", false, true),
        field("url", ParamType::String, "", false, true),
        field("urlId", ParamType::String, "", false, true),
        field("name", ParamType::String, "", false, false),
        field("description", ParamType::String, "", true, false),
        field("sort", ParamType::Json, "", false, false),
        field("index", ParamType::String, "", true, false),
        field("sharing", ParamType::Boolean, "", false, false),
        field("createdAt", ParamType::String, "date-time", false, true),
        field("updatedAt", ParamType::String, "date-time", false, true),
    ]
}

#[test]
fn schema_picks_identity_label_and_timestamps() {
    // id (the only non-nullable uuid), title (the first non-nullable plain
    // string that is not read-only - `text` loses on declaration order, `url`
    // on being read-only), then the non-nullable timestamps in schema order.
    assert_golden_with(
        &documents_payload(),
        OutputMode::Table,
        &document_schema(),
        "schema_table_documents.txt",
    );
}

#[test]
fn schema_prefers_a_writable_label_over_a_read_only_one() {
    let payload = json!([
        {
            "id": "col-1",
            "url": "/collection/engineering-abc",
            "urlId": "abc",
            "name": "Engineering",
            "sort": { "field": "index" },
            "createdAt": "2026-07-01T09:00:00.000Z",
            "updatedAt": "2026-08-01T10:00:00.000Z"
        },
        {
            "id": "col-2",
            "url": "/collection/design-def",
            "urlId": "def",
            "name": "Design",
            "sort": { "field": "index" },
            "createdAt": "2026-05-11T08:12:00.000Z",
            "updatedAt": "2026-08-20T15:30:00.000Z"
        }
    ]);
    assert_golden_with(
        &payload,
        OutputMode::Table,
        &collection_schema(),
        "schema_table_collections.txt",
    );
}

/// Column NAMES for a payload, ignoring the padding (which follows the
/// widest cell and is therefore data-dependent by design).
fn header_of(payload: &Value, schema: &[FieldSpec]) -> Vec<String> {
    render(payload, OutputMode::Table, schema)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn schema_columns_are_stable_for_responses_carrying_the_same_fields() {
    // Two responses of the same operation with the same field set render the
    // same header, whatever the row order or the values.
    let schema = document_schema();
    let first = header_of(&documents_payload(), &schema);
    let reordered = json!([
        documents_payload()[1].clone(),
        documents_payload()[0].clone()
    ]);
    assert_eq!(header_of(&reordered, &schema), first);
    let same_shape = json!([
        { "id": "x", "title": "T", "text": "b", "collectionId": "c",
          "updatedAt": "u", "createdAt": "c2" }
    ]);
    assert_eq!(header_of(&same_shape, &schema), first);
}

#[test]
fn a_sparse_response_shows_the_fields_it_has_not_empty_columns() {
    // `nullable: false` is not `required`, and the vendored spec declares
    // no `required` list for any response schema. Selecting columns from the
    // schema alone gave four columns, three of them empty in every row, with
    // the one real field squeezed out by the column cap.
    let schema = document_schema();
    let sparse = json!([{ "id": "d1", "icon": "flame" }]);
    let header = header_of(&sparse, &schema);
    assert_eq!(header, vec!["id", "icon"], "degenerate column set");

    let rendered = render(&sparse, OutputMode::Table, &schema).unwrap();
    assert!(rendered.contains("flame"), "present field lost: {rendered}");

    // No column may be empty in every row.
    let lines: Vec<&str> = rendered.lines().collect();
    for (index, name) in header.iter().enumerate() {
        let has_data = lines[1..].iter().any(|line| {
            line.split_whitespace()
                .nth(index)
                .is_some_and(|c| !c.is_empty())
        });
        assert!(has_data, "column {name} is empty in every row: {rendered}");
    }
}

#[test]
fn a_present_field_is_never_crowded_out_by_an_absent_one() {
    // `title` outranks `icon`, but a response without `title` must still
    // show `icon` rather than reserving the column for nothing.
    let schema = document_schema();
    let without_title = json!([
        { "id": "d1", "icon": "a", "urlId": "u1", "createdAt": "c", "updatedAt": "up" },
        { "id": "d2", "icon": "b", "urlId": "u2", "createdAt": "c", "updatedAt": "up" }
    ]);
    let header = header_of(&without_title, &schema);
    assert!(!header.contains(&"title".to_string()), "{header:?}");
    assert_eq!(header.len(), 4, "{header:?}");
    assert!(header.starts_with(&["id".to_string()]), "{header:?}");
}

#[test]
fn the_schema_still_supplies_the_ranking_not_the_payload() {
    // Which fields appear follows the payload; the ORDER follows the schema.
    // `updatedAt` is declared after `createdAt`, and `text` after `title`,
    // whatever order the response happens to list them in.
    let schema = document_schema();
    let payload = json!([{
        "updatedAt": "u", "text": "body", "title": "T", "createdAt": "c", "id": "d1"
    }]);
    let header = header_of(&payload, &schema);
    assert_eq!(header, vec!["id", "title", "createdAt", "updatedAt"]);
}

#[test]
fn schema_never_promotes_a_long_body_or_a_container_to_a_column() {
    let header = render(&documents_payload(), OutputMode::Table, &document_schema())
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    for noise in ["text", "tasks", "createdBy", "collectionId"] {
        assert!(!header.contains(noise), "{noise} became a column: {header}");
    }
}

#[test]
fn a_payload_the_schema_does_not_describe_falls_back_to_the_data() {
    // Spec drift, or a shape the spec never declared: rather than an empty
    // table, the data-driven policy takes over.
    let payload = json!([
        { "alpha": "a", "beta": "b" },
        { "alpha": "c", "beta": "d" }
    ]);
    let rendered = render(&payload, OutputMode::Table, &document_schema()).unwrap();
    assert!(rendered.contains("alpha"), "{rendered}");
    assert!(rendered.contains("beta"), "{rendered}");
    assert_eq!(
        rendered,
        render(&payload, OutputMode::Table, NO_SCHEMA).unwrap()
    );
}

#[test]
fn an_all_json_schema_falls_back_to_the_data() {
    // `auth.info` returns an envelope of objects only: nothing displayable,
    // so the schema contributes no columns.
    let schema = vec![
        field("user", ParamType::Json, "", false, false),
        field("team", ParamType::Json, "", false, false),
    ];
    let payload = json!([{ "id": "x", "title": "y" }]);
    assert_eq!(
        render(&payload, OutputMode::Table, &schema).unwrap(),
        render(&payload, OutputMode::Table, NO_SCHEMA).unwrap()
    );
}

#[test]
fn a_schema_without_a_uuid_falls_back_to_the_id_field_name() {
    let schema = vec![
        field("name", ParamType::String, "", false, false),
        field("id", ParamType::String, "", false, true),
        field("count", ParamType::Number, "", false, false),
    ];
    let payload = json!([{ "id": "1", "name": "n", "count": 2 }]);
    let header = render(&payload, OutputMode::Table, &schema)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert!(header.starts_with("id"), "identity not first: {header}");
}

#[test]
fn schema_columns_are_capped_and_ordered_deterministically() {
    let schema = document_schema();
    let payload = documents_payload();
    let header = render(&payload, OutputMode::Table, &schema)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert_eq!(header.split("  ").filter(|c| !c.is_empty()).count(), 4);
    // Repeated rendering is byte-identical (no map-iteration dependence).
    for _ in 0..3 {
        assert_eq!(
            render(&payload, OutputMode::Table, &schema).unwrap(),
            render(&payload, OutputMode::Table, &schema).unwrap()
        );
    }
}

#[test]
fn json_mode_ignores_the_schema_entirely() {
    // `--json` output is the payload verbatim, whatever the schema says.
    assert_eq!(
        render(&documents_payload(), OutputMode::Json, &document_schema()).unwrap(),
        render(&documents_payload(), OutputMode::Json, NO_SCHEMA).unwrap()
    );
}

#[test]
fn the_compiled_ir_drives_real_operations() {
    // Not a hand-written schema: the actual IR entry compiled from the
    // vendored spec must yield the same generic choice.
    let documents = otl::ops::find("documents.list").unwrap();
    let header = render(
        &documents_payload(),
        OutputMode::Table,
        &documents.response_fields,
    )
    .unwrap()
    .lines()
    .next()
    .unwrap()
    .to_string();
    assert!(header.starts_with("id"), "{header}");
    assert!(header.contains("title"), "{header}");
    assert!(
        header.contains("createdAt") && header.contains("updatedAt"),
        "{header}"
    );
    assert!(
        !header.contains("text"),
        "long body became a column: {header}"
    );

    let collections = otl::ops::find("collections.list").unwrap();
    let payload = json!([{ "id": "c1", "url": "/c/x", "name": "Engineering" }]);
    let header = render(&payload, OutputMode::Table, &collections.response_fields)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert!(header.contains("name"), "{header}");
    assert!(
        !header
            .split("  ")
            .next()
            .unwrap_or_default()
            .contains("url"),
        "a read-only url outranked the name: {header}"
    );
}

#[test]
fn columns_that_are_null_in_every_row_are_not_shown() {
    // Presence of a key is not content. `icon`, `color` and
    // `publishedAt` are legitimately nullable and can be present-but-null in
    // every row; each would take one of the four slots and render blank,
    // pushing out a field that does have something to show.
    let schema = document_schema();
    let payload = json!([
        { "id": "d1", "icon": null, "color": null, "publishedAt": null, "urlId": "keep-me" },
        { "id": "d2", "icon": null, "color": null, "publishedAt": null, "urlId": "keep-me-2" }
    ]);
    let header = header_of(&payload, &schema);
    assert_eq!(header, vec!["id", "urlId"], "blank columns were kept");

    let rendered = render(&payload, OutputMode::Table, &schema).unwrap();
    assert!(rendered.contains("keep-me"), "{rendered}");
}

#[test]
fn blank_strings_do_not_earn_a_column_but_false_and_zero_do() {
    let schema = vec![
        field("id", ParamType::String, "uuid", false, true),
        field("title", ParamType::String, "", false, false),
        field("fullWidth", ParamType::Boolean, "", false, false),
        field("revision", ParamType::Number, "", false, true),
    ];
    // An empty (or whitespace-only) title renders as a blank cell.
    let payload = json!([
        { "id": "d1", "title": "   ", "fullWidth": false, "revision": 0 },
        { "id": "d2", "title": "", "fullWidth": false, "revision": 0 }
    ]);
    let header = header_of(&payload, &schema);
    assert!(!header.contains(&"title".to_string()), "{header:?}");
    // `false` and `0` are values a reader wants to see.
    assert!(header.contains(&"fullWidth".to_string()), "{header:?}");
    assert!(header.contains(&"revision".to_string()), "{header:?}");
}

/// Whether a payload has a visibly non-empty value for `key` in some row.
///
/// An INDEPENDENT reimplementation of the property under test, deliberately
/// not reading the rendered table: a blank middle column shifts every later
/// value left, so `split_whitespace().nth(i)` cannot tell which column a cell
/// belongs to.
fn visible_in_payload(payload: &Value, key: &str) -> bool {
    payload
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row.get(key))
        .any(|value| match value {
            Value::Null => false,
            Value::String(text) => text.chars().any(|c| {
                !c.is_control()
                    && !c.is_whitespace()
                    && unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) > 0
            }),
            _ => true,
        })
}

#[test]
fn no_selected_column_is_ever_blank_in_every_row() {
    // The invariant, over payload shapes that include the invisible cases:
    // an ESC (which sanitizing turns into a space) and a zero-width space
    // (which survives sanitizing but occupies no column).
    let schema = document_schema();
    for payload in [
        json!([{ "id": "d1" }]),
        json!([{ "id": "d1", "icon": null }]),
        json!([{ "id": "d1", "title": "", "icon": "x" }]),
        json!([{ "id": "d1", "title": null, "createdAt": "c", "updatedAt": null }]),
        json!([{ "id": "d1", "title": "\u{1b}", "urlId": "visible" }]),
        json!([{ "id": "d1", "title": "\u{200b}", "urlId": "visible" }]),
        json!([{ "id": "d1", "title": " \u{200b}\u{1b} ", "text": "body", "urlId": "u" }]),
        documents_payload(),
    ] {
        for column in header_of(&payload, &schema) {
            assert!(
                visible_in_payload(&payload, &column),
                "column {column} is blank in every row of {payload}"
            );
        }
    }
}

#[test]
fn invisible_values_do_not_crowd_out_visible_ones() {
    // `has_content` judges the RAW string, so a high-priority field
    // holding "\u{1b}" or "\u{200b}" counts as content, fills a column
    // with nothing, and pushes a real value past the four-column cap.
    let schema = document_schema();
    let payload = json!([
        {
            "id": "d1",
            "title": "\u{1b}",
            "icon": "\u{200b}",
            "color": "\u{200b}\u{200b}",
            "text": "\u{1b}\u{1b}",
            "urlId": "visible-1",
            "createdAt": "2026-08-01"
        },
        {
            "id": "d2",
            "title": "\u{1b}",
            "icon": "\u{200b}",
            "color": "\u{200b}\u{200b}",
            "text": "\u{1b}\u{1b}",
            "urlId": "visible-2",
            "createdAt": "2026-08-02"
        }
    ]);
    let header = header_of(&payload, &schema);
    assert!(
        !header.contains(&"title".to_string()),
        "an invisible cell took a column: {header:?}"
    );
    for invisible in ["icon", "color", "text"] {
        assert!(
            !header.contains(&invisible.to_string()),
            "{invisible} took a column: {header:?}"
        );
    }
    assert!(header.contains(&"urlId".to_string()), "{header:?}");
    assert!(header.contains(&"createdAt".to_string()), "{header:?}");
    let rendered = render(&payload, OutputMode::Table, &schema).unwrap();
    assert!(rendered.contains("visible-1"), "{rendered}");
}

#[test]
fn a_payload_whose_schema_fields_are_all_null_falls_back_to_the_data() {
    // Nothing the schema ranks has content, so the data-driven policy takes
    // over rather than printing a table of blanks.
    let schema = document_schema();
    let payload = json!([{ "id": null, "title": null, "extra": "visible" }]);
    let rendered = render(&payload, OutputMode::Table, &schema).unwrap();
    assert_eq!(
        rendered,
        render(&payload, OutputMode::Table, NO_SCHEMA).unwrap()
    );
    assert!(rendered.contains("extra"), "{rendered}");
}

#[test]
fn the_data_driven_fallback_also_requires_content() {
    // The content filter is on the schema path; when the schema
    // contributes nothing, the fallback must not re-select those same blank
    // fields by name priority and push the one field with data past the
    // column cap.
    let schema = document_schema();
    let payload = json!([{
        "id": null,
        "title": "",
        "updatedAt": null,
        "createdAt": "\u{200b}",
        "extra": "visible"
    }]);
    let header = header_of(&payload, &schema);
    assert_eq!(header, vec!["extra"], "blank fields took the columns");
    let rendered = render(&payload, OutputMode::Table, &schema).unwrap();
    assert!(rendered.contains("visible"), "{rendered}");
}

#[test]
fn the_fallback_drops_blank_columns_with_no_schema_at_all() {
    // The same rule with no schema in play: a raw `--body` response whose
    // high-priority fields are empty must not crowd out the useful one.
    let payload = json!([
        { "id": null, "title": "", "name": "\u{1b}", "updatedAt": null, "zeta": "visible" },
        { "id": null, "title": "  ", "name": "\u{200b}", "updatedAt": null, "zeta": "also" }
    ]);
    let header = header_of(&payload, NO_SCHEMA);
    assert_eq!(header, vec!["zeta"], "blank fields took the columns");
}

#[test]
fn no_selected_column_is_blank_on_either_path() {
    // One invariant, both selectors: with a schema, without a schema, and in
    // the case where the schema contributes nothing and the fallback runs.
    let schema = document_schema();
    for payload in [
        json!([{ "id": null, "title": "", "createdAt": "\u{200b}", "extra": "visible" }]),
        json!([{ "id": "d1", "title": "\u{1b}", "urlId": "u" }]),
        json!([{ "alpha": "", "beta": null, "gamma": "g" }]),
        json!([{ "id": "d1", "title": "T", "createdAt": "c", "updatedAt": "u" }]),
    ] {
        for columns in [header_of(&payload, &schema), header_of(&payload, NO_SCHEMA)] {
            for column in columns {
                assert!(
                    visible_in_payload(&payload, &column),
                    "column {column} is blank in every row of {payload}"
                );
            }
        }
    }
}
