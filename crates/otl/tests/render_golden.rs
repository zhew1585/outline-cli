//! Golden-file tests for the data-driven table renderer and its layout
//! (widths, grapheme clusters, control characters, truncation).
//!
//! The schema-driven column policy has its own file, `render_schema.rs`.
//!
//! Per project rule, all output rendering is covered by golden files.
//! Regenerate with: `OTL_UPDATE_GOLDEN=1 cargo test -p outline-cli --test
//! render_golden` (then review the diff by eye before committing).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use engine::FieldSpec;
use otl::render::{render, resolve_mode, OutputMode};
use serde_json::{json, Value};

/// No response schema: the data-driven fallback policy.
const NO_SCHEMA: &[FieldSpec] = &[];

/// Whether golden files may be rewritten instead of asserted.
///
/// Requires the exact value `1` (so `OTL_UPDATE_GOLDEN=0`, `false`, or an
/// empty value do NOT rewrite anything), and never applies when `CI` is
/// set: on CI a rendering regression must fail, not overwrite the evidence.
fn golden_update_requested() -> bool {
    may_update_golden(
        std::env::var("OTL_UPDATE_GOLDEN").ok().as_deref(),
        std::env::var("CI").ok().as_deref(),
    )
}

/// Pure decision behind [`golden_update_requested`], so the gate itself is
/// testable without mutating the process environment (which would need
/// `unsafe`, forbidden workspace-wide).
fn may_update_golden(update: Option<&str>, ci: Option<&str>) -> bool {
    update == Some("1") && ci.is_none()
}

/// Compare rendered output against a golden file (or rewrite it when
/// `OTL_UPDATE_GOLDEN=1` outside CI).
fn assert_golden(payload: &Value, mode: OutputMode, golden_name: &str) {
    assert_golden_with(payload, mode, NO_SCHEMA, golden_name);
}

/// Same, with an explicit response schema driving column selection.
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

/// Column names for a payload, ignoring the data-dependent padding.
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
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    assert!(!rendered.contains('\u{1b}'), "ESC leaked: {rendered:?}");
    assert!(
        !rendered.contains("text\nline"),
        "newline in cell leaked: {rendered:?}"
    );
}

#[test]
fn table_mode_falls_back_to_json_for_non_list_payloads() {
    let object = json!({ "id": "doc-1", "title": "Hello" });
    let table = render(&object, OutputMode::Table, NO_SCHEMA).unwrap();
    let json_out = render(&object, OutputMode::Json, NO_SCHEMA).unwrap();
    assert_eq!(table, json_out);

    let scalars = json!(["a", "b"]);
    assert_eq!(
        render(&scalars, OutputMode::Table, NO_SCHEMA).unwrap(),
        render(&scalars, OutputMode::Json, NO_SCHEMA).unwrap()
    );
}

#[test]
fn json_mode_is_pretty_json_without_decoration() {
    let rendered = render(&documents_payload(), OutputMode::Json, NO_SCHEMA).unwrap();
    let reparsed: Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(reparsed, documents_payload());
    assert!(!rendered.contains('\u{1b}'), "ANSI in JSON output");
}

#[test]
fn golden_update_gate_accepts_only_exact_one_outside_ci() {
    // A stray OTL_UPDATE_GOLDEN must not let a rendering regression
    // overwrite its own expected output.
    for (value, ci, expected) in [
        (Some("1"), None, true),
        (Some("1"), Some("true"), false),
        (Some("1"), Some(""), false),
        (Some("0"), None, false),
        (Some("false"), None, false),
        (Some(""), None, false),
        (Some("yes"), None, false),
        (None, None, false),
    ] {
        assert_eq!(
            may_update_golden(value, ci),
            expected,
            "OTL_UPDATE_GOLDEN={value:?} CI={ci:?}"
        );
    }
}

#[test]
fn table_column_set_is_stable_across_heterogeneous_rows() {
    // Optional fields present in only some rows must not change the column
    // set, and neither must row order: the header is the union of all keys
    // in fixed priority order.
    let full = json!({
        "id": "a", "title": "First", "updatedAt": "2026-08-01T00:00:00Z", "pinned": true
    });
    let sparse = json!({ "id": "b" });
    let other = json!({ "id": "c", "title": "Third" });

    let header_of = |payload: &Value| -> String {
        render(payload, OutputMode::Table, NO_SCHEMA)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string()
    };

    let expected = header_of(&json!([full, sparse, other]));
    assert_eq!(header_of(&json!([sparse, full, other])), expected);
    assert_eq!(header_of(&json!([other, sparse, full])), expected);
    assert_eq!(header_of(&json!([sparse, other, full])), expected);
    // The sparse row alone cannot suppress columns contributed by others.
    assert!(expected.contains("title"), "header: {expected}");
    assert!(expected.contains("updatedAt"), "header: {expected}");
}

#[test]
fn table_aligns_cjk_emoji_and_combining_characters() {
    // Terminal alignment is display width, not char count: CJK is 2
    // columns, combining marks 0. Every row must place the last column at
    // the same display column.
    let payload = json!([
        { "id": "1", "title": "中文标题", "updatedAt": "A" },
        { "id": "2", "title": "ascii", "updatedAt": "B" },
        { "id": "3", "title": "e\u{301}mile", "updatedAt": "C" },
        { "id": "4", "title": "ok \u{1f600}", "updatedAt": "D" }
    ]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    assert_golden(&payload, OutputMode::Table, "table_unicode.txt");

    // Compute the display column at which the final cell starts on each
    // line; they must all agree.
    let width = |text: &str| -> usize {
        text.chars()
            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
            .sum()
    };
    let starts: Vec<usize> = rendered
        .lines()
        .map(|line| {
            let last = line.rsplit("  ").next().unwrap_or("");
            width(&line[..line.len() - last.len()])
        })
        .collect();
    assert!(
        starts.windows(2).all(|pair| pair[0] == pair[1]),
        "columns misaligned: {starts:?} in\n{rendered}"
    );
}

/// Display width of a string, measured per grapheme cluster (the only way
/// emoji ligature widths come out right).
fn cluster_width(text: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

#[test]
fn table_aligns_emoji_ligatures() {
    // per-codepoint width sums are wrong for ligatures -
    // skin-tone modifiers, ZWJ sequences and family emoji all render as 2
    // columns but sum to 4-8 per codepoint. Every row's last column must
    // still start at the same display column.
    let payload = json!([
        { "id": "1", "title": "\u{1f600}", "updatedAt": "2026-08-01" },
        { "id": "2", "title": "\u{1f44d}\u{1f3fd}", "updatedAt": "2026-08-02" },
        { "id": "3", "title": "\u{1f469}\u{200d}\u{1f4bb}", "updatedAt": "2026-08-03" },
        {
            "id": "4",
            "title": "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}",
            "updatedAt": "2026-08-04"
        }
    ]);
    assert_golden(&payload, OutputMode::Table, "table_emoji.txt");

    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    let starts: Vec<usize> = rendered
        .lines()
        .map(|line| {
            let last = line.rsplit("  ").next().unwrap_or("");
            cluster_width(&line[..line.len() - last.len()])
        })
        .collect();
    assert!(
        starts.windows(2).all(|pair| pair[0] == pair[1]),
        "columns misaligned: {starts:?} in\n{rendered}"
    );
}

#[test]
fn table_never_splits_a_grapheme_cluster() {
    // A long run of ZWJ sequences must be cut between clusters, never
    // inside one (which would emit a mangled fragment).
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
    let payload = json!([{ "id": "1", "title": family.repeat(40) }]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    let cell = rendered.lines().nth(1).unwrap();
    // Every emoji present must appear as a whole family cluster: no bare
    // ZWJ at a cut point, and no dangling joiner before the ellipsis.
    assert!(
        !cell.contains("\u{200d}\u{2026}"),
        "cut inside cluster: {cell:?}"
    );
    assert!(!cell.ends_with('\u{200d}'), "dangling joiner: {cell:?}");
    assert!(rendered.contains('\u{2026}'), "not truncated: {rendered:?}");
}

#[test]
fn table_caps_zero_width_codepoint_floods() {
    // 100k combining accents are one grapheme cluster of
    // display width 1, so a width-only cap lets a cell carry ~200 KB.
    // Absolute cluster and codepoint caps must bound it.
    let flood = format!("a{}", "\u{301}".repeat(100_000));
    let payload = json!([{ "id": "1", "title": flood, "updatedAt": "2026-08-01" }]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    assert!(
        rendered.len() < 4_000,
        "output not bounded: {} bytes",
        rendered.len()
    );
    assert!(rendered.contains('\u{2026}'), "not truncated: {rendered:?}");
    // Rows stay aligned even after the flood is cut.
    assert!(rendered.contains("2026-08-01"), "row lost: {rendered:?}");
}

#[test]
fn table_caps_many_zero_width_clusters() {
    // The same flood spread over many clusters (each accent on its own
    // base character) must be bounded too.
    let flood = "a\u{301}".repeat(100_000);
    let payload = json!([{ "id": "1", "title": flood }]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    assert!(
        rendered.len() < 4_000,
        "output not bounded: {} bytes",
        rendered.len()
    );
    assert!(rendered.contains('\u{2026}'), "not truncated: {rendered:?}");
}

#[test]
fn table_truncates_wide_characters_by_display_width() {
    // 40 CJK characters are 80 columns wide; truncation must cut to the
    // width budget on a character boundary and never panic.
    let wide = "中".repeat(40);
    let payload = json!([{ "id": "1", "title": wide }]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    let title_cell = rendered.lines().nth(1).unwrap();
    let width: usize = title_cell
        .chars()
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
        .sum();
    assert!(rendered.contains('\u{2026}'), "not truncated: {rendered}");
    // id column (1) + gap (2) + at most 40 columns of title.
    assert!(width <= 1 + 2 + 40, "cell too wide ({width}): {title_cell}");
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

// ---------------------------------------------------------------------------
// Schema-driven column selection.
//
// The column set becomes a property of the OPERATION, taken from the response
// schema compiled into the IR, instead of a property of one response body.
// The data-driven policy above stays as the fallback for payloads no schema
// describes, so its golden files are unchanged.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Bidi and invisible characters in cell data.
//
// `is_control()` does not cover the Unicode FORMAT characters, and an
// unterminated right-to-left override does not stop at the cell boundary: it
// reverses the visual order of the rest of the ROW. Any Outline user who can
// name a document controls this text, and the same repository already strips
// these characters in `config/error.rs` and `engine/sanitize.rs`.
// ---------------------------------------------------------------------------

#[test]
fn bidi_overrides_never_reach_stdout_from_a_cell() {
    let payload = json!([
        { "id": "a1", "title": "invoice\u{202e}gnp.exe", "updatedAt": "2024-01-01" },
        { "id": "b2", "title": "ordinary", "updatedAt": "2024-01-02" }
    ]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    for (name, ch) in [
        ("RLO U+202E", '\u{202e}'),
        ("LRO U+202D", '\u{202d}'),
        ("RLE U+202B", '\u{202b}'),
        ("PDF U+202C", '\u{202c}'),
        ("LRI U+2066", '\u{2066}'),
        ("RLI U+2067", '\u{2067}'),
        ("FSI U+2068", '\u{2068}'),
        ("PDI U+2069", '\u{2069}'),
        ("RLM U+200F", '\u{200f}'),
        ("LRM U+200E", '\u{200e}'),
        ("ALM U+061C", '\u{61c}'),
    ] {
        assert!(
            !rendered.contains(ch),
            "{name} reached stdout: {rendered:?}"
        );
    }
    // The row after the tampered cell is still intact.
    assert!(rendered.contains("2024-01-01"), "{rendered}");
    assert!(rendered.contains("ordinary"), "{rendered}");
}

#[test]
fn invisible_characters_never_reach_stdout_from_a_cell() {
    let payload = json!([{
        "id": "a1",
        "title": "we\u{200b}b\u{feff}si\u{00ad}te\u{2060}x",
        "updatedAt": "2024-01-01"
    }]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    for ch in [
        '\u{200b}', '\u{200c}', '\u{200d}', '\u{feff}', '\u{00ad}', '\u{2060}',
    ] {
        assert!(
            !rendered.contains(ch),
            "U+{:04X} reached stdout: {rendered:?}",
            ch as u32
        );
    }
}

#[test]
fn legitimate_right_to_left_text_still_renders() {
    // Stripping the FORMAT characters must not touch the letters: a Hebrew
    // or Arabic title is RTL because of its own characters, and the terminal
    // orders it correctly without any explicit override.
    let payload = json!([{ "id": "a1", "title": "מסמך עברית", "updatedAt": "x" }]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    assert!(rendered.contains("מסמך עברית"), "{rendered}");
}

#[test]
fn a_cell_of_only_invisible_characters_does_not_win_a_column() {
    // Nothing is left after cleaning, so the column is not worth one of the
    // four slots - the same rule that keeps an all-null column out.
    let payload = json!([
        { "id": "a1", "title": "\u{200b}\u{feff}", "urlId": "visible" },
        { "id": "b2", "title": "\u{00ad}\u{2060}", "urlId": "visible-2" }
    ]);
    let header = header_of(&payload, NO_SCHEMA);
    assert!(!header.contains(&"title".to_string()), "{header:?}");
    assert!(header.contains(&"urlId".to_string()), "{header:?}");
}

#[test]
fn a_bidi_override_is_shown_as_tampering_rather_than_hidden() {
    // Deliberately NOT dropped like an invisible character: a title built to
    // reorder the row is something the reader should be able to see, so it
    // renders as a replacement marker and keeps its column.
    let payload = json!([
        { "id": "a1", "title": "\u{202e}", "urlId": "u1" },
        { "id": "b2", "title": "\u{2066}", "urlId": "u2" }
    ]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    assert!(
        rendered.contains('\u{fffd}'),
        "no marker shown: {rendered:?}"
    );
    assert!(!rendered.contains('\u{202e}'), "{rendered:?}");
    assert!(
        header_of(&payload, NO_SCHEMA).contains(&"title".to_string()),
        "the marker should keep its column"
    );
}

#[test]
fn a_zero_width_joiner_survives_because_it_is_part_of_the_text() {
    // U+200D is what makes an emoji ligature one glyph; dropping it as an
    // invisible character would corrupt the value being displayed.
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
    let payload = json!([{ "id": "a1", "title": family, "updatedAt": "x" }]);
    let rendered = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    assert!(
        rendered.contains(family),
        "the ligature was broken: {rendered:?}"
    );
}

#[test]
fn json_mode_is_exempt_from_hazard_scrubbing() {
    // A deliberate exemption, pinned so it reads as a decision rather than an
    // `--json` is the payload, not a rendering: its
    // contract is that `jq` consumes it and that it round-trips to what the
    // server sent, so substituting a codepoint there would corrupt data to
    // protect a terminal that is not the intended consumer.
    //
    // The trade-off is real and one-directional: the table is what a TTY gets
    // by default, and the table IS scrubbed.
    let hostile = "invoice\u{202e}gnp.exe\u{200b}";
    let payload = json!([{ "id": "a1", "title": hostile, "updatedAt": "x" }]);

    let json = render(&payload, OutputMode::Json, NO_SCHEMA).unwrap();
    let reparsed: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(reparsed, payload, "--json must round-trip byte-exactly");
    assert_eq!(
        reparsed[0]["title"], hostile,
        "--json must not alter the server's value"
    );

    // The same payload through the human-readable path is protected.
    let table = render(&payload, OutputMode::Table, NO_SCHEMA).unwrap();
    assert!(
        !table.contains('\u{202e}'),
        "the table must scrub: {table:?}"
    );
    assert!(
        !table.contains('\u{200b}'),
        "the table must scrub: {table:?}"
    );
}
