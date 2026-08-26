//! Dual-state output rendering.
//!
//! stdout carries data in one of two states (per the CLI contract):
//!
//! - `Json`: pretty JSON, jq-consumable, no color or decoration. Chosen by
//!   `--json` or whenever stdout is not a TTY.
//! - `Table`: schema-driven table for list-shaped payloads, chosen only on
//!   a TTY. Column RANKING comes from the operation's compiled response
//!   schema via one generic policy - there is no per-endpoint rendering code
//!   and no per-endpoint data anywhere in the IR - and the payload decides
//!   which of the ranked fields are actually shown, since no schema facet
//!   here states that a field is always present. Non-list payloads fall back
//!   to JSON, and so does a payload whose keys the schema does not describe
//!   at all (spec drift, or a shape the spec never declared).
//!
//! Nothing here emits ANSI escapes, and cell text is scrubbed of control
//! characters: server-controlled strings must not smuggle escapes or line
//! breaks into the terminal. Any future decoration must be gated on
//! [`OutputMode::Table`] so that non-TTY output stays decoration-free.

use engine::{FieldSpec, ParamType};
use serde_json::{Map, Value};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Maximum number of columns shown in a table.
const MAX_TABLE_COLUMNS: usize = 4;
/// Maximum terminal width (in columns, not characters) of one table cell,
/// including the truncation marker.
const MAX_CELL_WIDTH: usize = 40;
/// Maximum number of grapheme clusters kept in one table cell.
///
/// Display width alone is not a resource bound: zero-width codepoints
/// (combining marks, joiners) let a visually tiny cell carry an unbounded
/// number of them.
const MAX_CELL_CLUSTERS: usize = 200;
/// Maximum number of codepoints kept in one table cell.
///
/// A single grapheme cluster can absorb an unlimited number of combining
/// marks, so a cluster count is not a resource bound on its own either.
const MAX_CELL_CHARS: usize = 400;
/// Marker appended to truncated cells.
const TRUNCATION_MARK: &str = "\u{2026}"; // …
/// Gap between table columns.
const COLUMN_GAP: &str = "  ";
/// Placeholder printed for an empty list in table mode.
const EMPTY_LIST_PLACEHOLDER: &str = "(no items)";
/// Schema `format` marking a field as an opaque identifier.
const UUID_FORMAT: &str = "uuid";
/// Schema `format` marking a field as a timestamp.
const DATE_TIME_FORMAT: &str = "date-time";
/// The conventional identifier field name, used only when no field carries
/// a `uuid` format (some schemas type their ids as plain strings).
const ID_FIELD: &str = "id";

/// How data is rendered on stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Raw pretty JSON, no decoration.
    Json,
    /// Human-readable table for list-shaped data (TTY only).
    Table,
}

/// Resolve the output mode: `--json` always wins, then TTY detection.
///
/// Non-TTY stdout always gets JSON, which also guarantees ANSI-free output
/// for pipes and redirects.
pub fn resolve_mode(json_flag: bool, stdout_is_tty: bool) -> OutputMode {
    if json_flag || !stdout_is_tty {
        OutputMode::Json
    } else {
        OutputMode::Table
    }
}

/// Render a response payload for the given mode (without trailing newline).
///
/// `schema` describes one item of the operation's response payload, as
/// compiled into the IR; pass an empty slice when the shape is unknown.
/// In table mode, payloads that are not a list of objects fall back to
/// pretty JSON so every payload shape stays renderable.
pub fn render(
    payload: &Value,
    mode: OutputMode,
    schema: &[FieldSpec],
) -> Result<String, serde_json::Error> {
    match mode {
        OutputMode::Json => serde_json::to_string_pretty(payload),
        OutputMode::Table => match try_render_table(payload, schema) {
            Some(table) => Ok(table),
            None => serde_json::to_string_pretty(payload),
        },
    }
}

/// Render a list of objects as a table, or `None` when the payload does
/// not have that shape.
fn try_render_table(payload: &Value, schema: &[FieldSpec]) -> Option<String> {
    let rows = payload.as_array()?;
    if rows.is_empty() {
        return Some(EMPTY_LIST_PLACEHOLDER.to_string());
    }
    let objects: Vec<_> = rows.iter().map(Value::as_object).collect::<Option<_>>()?;
    let columns = select_columns(&objects, schema);
    if columns.is_empty() {
        return None;
    }

    let header: Vec<String> = columns.iter().map(|key| sanitize_cell(key)).collect();
    let body: Vec<Vec<String>> = objects
        .iter()
        .map(|row| columns.iter().map(|key| cell_text(row.get(*key))).collect())
        .collect();
    Some(layout_table(&header, &body))
}

/// Pick up to [`MAX_TABLE_COLUMNS`] columns, from the response schema when
/// it describes this payload and from the data itself otherwise.
///
/// The schema supplies the RANKING, which is a property of the operation and
/// therefore stable across responses. Which of the ranked fields become
/// columns depends on what the payload actually carries, because an OpenAPI
/// schema cannot say that: `nullable: false` only forbids an explicit null
/// for a field that IS present, and the vendored spec declares no `required`
/// list for any response schema at all. Selecting purely from the schema
/// would therefore print columns that are empty in every row while crowding
/// out fields the response does carry.
///
/// So a schema field becomes a candidate only if some row HAS CONTENT for
/// it, and the top [`MAX_TABLE_COLUMNS`] candidates win, in ranked order.
/// Content, not mere presence: a nullable field is often present and
/// explicitly `null` in every row, which renders as a blank cell, and a
/// column that is blank in every row is exactly the noise this filter exists
/// to remove - it would also consume one of the four slots and push out a
/// field that does have something to show. Neither row order nor map
/// iteration order can influence the result.
///
/// The data-driven policy remains for payloads no schema describes - a raw
/// `--body` call, an operation whose spec declares no response shape, or
/// spec drift - and is chosen when the schema contributes no candidate.
fn select_columns<'a>(rows: &[&'a Map<String, Value>], schema: &'a [FieldSpec]) -> Vec<&'a str> {
    let from_schema: Vec<&'a str> = rank_schema_columns(schema)
        .into_iter()
        .filter(|key| rows.iter().any(|row| has_content(row.get(*key))))
        .take(MAX_TABLE_COLUMNS)
        .collect();
    if from_schema.is_empty() {
        return select_data_columns(rows);
    }
    from_schema
}

/// Whether a value would render as anything the reader can see.
///
/// The question is about the CELL, not the raw value: [`sanitize_cell`] turns
/// control characters into spaces, so `"\u{1b}"` is content by any
/// string-level test and blank on screen, and a zero-width character such as
/// `"\u{200b}"` survives sanitizing but occupies no column. Several such
/// fields in a row could otherwise fill all four columns with nothing.
///
/// Absent and `null` are empty by definition. `false` and `0` are content:
/// they are values a reader wants to see.
fn has_content(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => renders_visibly(text),
        Some(_) => true,
    }
}

/// Whether the cell this text renders to would occupy any terminal column.
///
/// Mirrors [`sanitize_cell`] - control characters become spaces - and then
/// asks whether any grapheme cluster is both printable and non-zero-width.
fn renders_visibly(raw: &str) -> bool {
    raw.graphemes(true).any(|cluster| {
        let printable = cluster
            .chars()
            .any(|c| !c.is_control() && !c.is_whitespace());
        printable && display_width_of(cluster) > 0
    })
}

/// Rank EVERY displayable schema field, best column first.
///
/// One rule set for every operation, derived from facets any OpenAPI schema
/// can state - there is no per-endpoint knowledge here or in the IR:
///
/// 1. anything not displayable as a single value (objects, arrays, unions)
///    is dropped;
/// 2. the IDENTITY column is the first `uuid`-formatted field that cannot be
///    null, or else a field literally named `id`;
/// 3. the LABEL column is the first plain string that cannot be null (a
///    string with no `format`, so not an id, timestamp, URL or e-mail),
///    preferring one the schema does NOT mark `readOnly`: a writable string
///    is the name the user gave the object, while a read-only one is derived
///    from it;
/// 4. then the timestamps that cannot be null, in declaration order;
/// 5. then the remaining fields that cannot be null, then the nullable ones.
///
/// `nullable: false` is used as "worth a column when present" - a value that
/// cannot be null is never a blank cell - and never as "always present",
/// which no OpenAPI facet here states. Ties are always broken by the
/// schema's own declaration order, which is how the spec ranks its fields.
///
/// The full ranking is returned, not the top four: the caller drops fields
/// the payload does not carry before taking a limited number of columns.
fn rank_schema_columns(schema: &[FieldSpec]) -> Vec<&str> {
    let scalars: Vec<&FieldSpec> = schema
        .iter()
        .filter(|field| field.ty != ParamType::Json)
        .collect();
    let identity = scalars
        .iter()
        .position(|f| f.format == UUID_FORMAT && !f.nullable)
        .or_else(|| scalars.iter().position(|f| f.name == ID_FIELD));
    let label = scalars
        .iter()
        .enumerate()
        .filter(|(index, f)| Some(*index) != identity && is_plain_label(f))
        .min_by_key(|(index, f)| (f.read_only, *index))
        .map(|(index, _)| index);

    let chosen = |index: usize| Some(index) == identity || Some(index) == label;
    let rest = scalars
        .iter()
        .enumerate()
        .filter(|(index, _)| !chosen(*index));
    let timestamps = rest
        .clone()
        .filter(|(_, f)| f.format == DATE_TIME_FORMAT && !f.nullable);
    let non_null = rest
        .clone()
        .filter(|(_, f)| f.format != DATE_TIME_FORMAT && !f.nullable);
    let nullable = rest.filter(|(_, f)| f.nullable);

    identity
        .into_iter()
        .chain(label)
        .chain(timestamps.chain(non_null).chain(nullable).map(|(i, _)| i))
        .map(|index| scalars[index].name.as_ref())
        .collect()
}

/// Whether a field reads as the object's human label: a string the schema
/// constrains no further (an id, timestamp, URL or e-mail all carry a
/// `format`) and that cannot be null.
fn is_plain_label(field: &FieldSpec) -> bool {
    field.ty == ParamType::String && field.format.is_empty() && !field.nullable
}

/// Pick columns from the response itself, for payloads no schema describes.
///
/// Determinism rules, so the same payload always yields the same header:
///
/// - the candidate set is the UNION of the keys of every row, never a
///   sample, so an optional field missing from the first row (or from any
///   other) cannot change the column set;
/// - ordering is by fixed priority and then by key name, so it depends on
///   neither row order nor JSON object iteration order;
/// - a key whose value is a container in any row is dropped entirely, so a
///   column is never half-rendered;
/// - a key with no visible content in any row is dropped, by the same
///   [`has_content`] test the schema path uses. Both paths need it: this one
///   is reached whenever the schema contributes nothing, which is exactly
///   when every schema-ranked field was blank, and picking four more blank
///   columns here would push out the one field that does have something.
fn select_data_columns<'a>(rows: &[&'a Map<String, Value>]) -> Vec<&'a str> {
    let mut candidates: Vec<&str> = Vec::new();
    for row in rows {
        for key in row.keys() {
            if !candidates.contains(&key.as_str()) {
                candidates.push(key);
            }
        }
    }
    let mut columns: Vec<&str> = candidates
        .into_iter()
        .filter(|key| {
            rows.iter()
                .filter_map(|row| row.get(*key))
                .all(|value| !value.is_object() && !value.is_array())
        })
        .filter(|key| rows.iter().any(|row| has_content(row.get(*key))))
        .collect();
    columns.sort_by(|left, right| {
        key_priority(left)
            .cmp(&key_priority(right))
            .then_with(|| left.cmp(right))
    });
    columns.truncate(MAX_TABLE_COLUMNS);
    columns
}

/// Ranking used to auto-pick key columns without a schema; lower is better.
/// Ties are broken by key name in [`select_data_columns`], never by input
/// order.
fn key_priority(key: &str) -> u8 {
    match key {
        "id" => 0,
        "title" => 1,
        "name" => 2,
        "updatedAt" => 3,
        _ if key.ends_with("At") => 4,
        // Foreign-key-ish columns are usually noise next to `id`.
        _ if key.ends_with("Id") => 6,
        _ => 5,
    }
}

/// The display text of one cell.
fn cell_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => sanitize_cell(text),
        Some(other) => sanitize_cell(&other.to_string()),
    }
}

/// Replace control characters (ANSI escapes, newlines, tabs) with spaces
/// and truncate the cell to fit every cell limit.
///
/// Truncation works on GRAPHEME CLUSTERS, never on codepoints: an emoji
/// ligature (skin-tone modifier, ZWJ sequence, family emoji) is one cluster
/// two columns wide, whose codepoint widths sum to far more, and splitting
/// it would emit a mangled fragment.
///
/// Three independent limits apply, and whichever trips first truncates:
/// display width ([`MAX_CELL_WIDTH`]), cluster count
/// ([`MAX_CELL_CLUSTERS`]) and codepoint count ([`MAX_CELL_CHARS`]). Width
/// alone bounds neither the terminal's work nor our output size, because
/// zero-width codepoints are free in width terms.
fn sanitize_cell(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if fits_cell(&cleaned) {
        return cleaned;
    }

    let width_budget = MAX_CELL_WIDTH.saturating_sub(display_width_of(TRUNCATION_MARK));
    let mut truncated = String::new();
    let mut width = 0;
    let mut chars = 0;
    for (index, cluster) in cleaned.graphemes(true).enumerate() {
        let next_width = width + display_width_of(cluster);
        let next_chars = chars + cluster.chars().count();
        if next_width > width_budget
            || index + 1 >= MAX_CELL_CLUSTERS
            || next_chars > MAX_CELL_CHARS
        {
            break;
        }
        truncated.push_str(cluster);
        width = next_width;
        chars = next_chars;
    }
    truncated.push_str(TRUNCATION_MARK);
    truncated
}

/// Whether a cell needs no truncation.
fn fits_cell(text: &str) -> bool {
    display_width(text) <= MAX_CELL_WIDTH
        && text.graphemes(true).count() <= MAX_CELL_CLUSTERS
        && text.chars().count() <= MAX_CELL_CHARS
}

/// Terminal column width of a string, summed over grapheme clusters.
///
/// Per-codepoint sums are wrong for emoji ligatures: `UnicodeWidthStr`'s
/// rules only hold when applied to a whole cluster.
fn display_width(text: &str) -> usize {
    text.graphemes(true).map(display_width_of).sum()
}

/// Terminal column width of one grapheme cluster.
fn display_width_of(cluster: &str) -> usize {
    UnicodeWidthStr::width(cluster)
}

/// Lay out header and body cells with padded, gap-separated columns.
///
/// Padding is computed from terminal display width, not character count:
/// `format!("{:<width$}")` pads to a character count, which misaligns every
/// column after a CJK or emoji cell.
fn layout_table(header: &[String], body: &[Vec<String>]) -> String {
    let widths: Vec<usize> = header
        .iter()
        .enumerate()
        .map(|(index, name)| {
            body.iter()
                .map(|row| display_width(&row[index]))
                .chain([display_width(name)])
                .max()
                .unwrap_or(0)
        })
        .collect();

    let render_row = |cells: &[String]| -> String {
        let padded: Vec<String> = cells
            .iter()
            .zip(widths.iter().copied())
            .map(|(cell, width)| pad_to_width(cell, width))
            .collect();
        padded.join(COLUMN_GAP).trim_end().to_string()
    };

    let mut lines = vec![render_row(header)];
    lines.extend(body.iter().map(|row| render_row(row)));
    lines.join("\n")
}

/// Right-pad `text` with spaces until it occupies `width` terminal columns.
fn pad_to_width(text: &str, width: usize) -> String {
    let padding = width.saturating_sub(display_width(text));
    format!("{text}{}", " ".repeat(padding))
}
