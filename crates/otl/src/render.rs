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
/// Shown in place of a character that must not reach the terminal but whose
/// presence the reader should see.
const REPLACEMENT: char = '\u{fffd}';
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

/// Render a payload as JSON, with no schema involved.
///
/// [`render`] takes the operation's response schema because it may pick
/// table columns from it; in JSON mode it never looks. Callers that only
/// ever emit JSON - the curated commands printing raw server rows, or a
/// summary they built themselves - say so with this instead of handing
/// `render` an empty schema that reads like an oversight.
pub fn render_json(payload: &Value) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(payload)
}

/// Render JSON that `otl` AUTHORED, scrubbing every string in it.
///
/// The `--json` exemption documented in [`crate::text`] is about one thing:
/// a SERVER RESPONSE has to round-trip byte-for-byte, because that payload
/// is the server's contract and `render_golden`'s JSON test pins it. That
/// reasoning does not reach an object this CLI writes itself - `otl doctor`'s
/// report, `otl api describe`'s contract, every `otl auth` result - which
/// nothing round-trips, and
/// which interleaves authored prose with text from a spec document, a
/// filesystem or a server error. Those are the same foreign values the human
/// rendering scrubs; the state they are printed in does not change what they
/// are.
///
/// So there is one rule with two sinks rather than two policies: [`render`]
/// (a response) is exempt, this (a document about one) is not. Scrubbing at
/// the SINK rather than at each field is deliberate for the same reason
/// [`crate::stdio::scrub_terminal_controls`] is - it holds for fields that
/// do not exist yet.
///
/// Object KEYS are scrubbed too. Every key today is an authored `&'static
/// str`, so this changes nothing; it costs one comparison and removes the
/// question.
pub fn render_json_scrubbed(payload: &Value) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&scrub_value(payload))
}

/// Deep-copy a JSON value with every string scrubbed.
fn scrub_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(crate::stdio::scrub_terminal_controls(text)),
        Value::Array(items) => Value::Array(items.iter().map(scrub_value).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| {
                    (
                        crate::stdio::scrub_terminal_controls(key),
                        scrub_value(item),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Render a table with caller-chosen columns.
///
/// The curated commands (`otl docs search`, `otl collections list`, ...)
/// pick their own columns instead of letting [`select_columns`] guess, but
/// share this layout, cell sanitizing and truncation with the generic
/// renderer - there is deliberately no per-command table code.
///
/// `rows` must be rectangular with respect to `headers`; shorter rows are
/// padded with empty cells and extra cells are dropped, so a mismatch
/// cannot panic on an index.
pub fn render_columns(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return EMPTY_LIST_PLACEHOLDER.to_string();
    }
    let header: Vec<String> = headers.iter().map(|name| sanitize_cell(name)).collect();
    let body: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..header.len())
                .map(|index| {
                    row.get(index)
                        .map(|cell| sanitize_cell(cell))
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();
    layout_table(&header, &body)
}

/// Render label/value pairs as an aligned two-column block.
///
/// Used by the curated commands that report ONE object (`otl docs create`,
/// `otl docs update`) rather than a list. Values are scrubbed of control
/// characters, like every other piece of server text that reaches a
/// terminal, but deliberately NOT truncated: a document URL is the point of
/// the output, and a 40-column cell would cut it in half.
pub fn render_pairs(pairs: &[(&str, String)]) -> String {
    let labels: Vec<String> = pairs
        .iter()
        .map(|(label, _)| scrub_control_chars(label))
        .collect();
    let Some(width) = labels.iter().map(|label| display_width(label)).max() else {
        return String::new();
    };
    pairs
        .iter()
        .zip(labels.iter())
        .map(|((_, value), label)| {
            format!(
                "{}{COLUMN_GAP}{}",
                pad_to_width(label, width),
                scrub_control_chars(value)
            )
            .trim_end()
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace control characters (ANSI escapes, newlines, tabs) with spaces.
///
/// The length-bounding half of [`sanitize_cell`] is deliberately absent:
/// this is for values that must stay whole (see [`render_pairs`]).
fn scrub_control_chars(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The terminal column width of `text`, summed over grapheme clusters.
///
/// Public because layout decisions outside this module need the same
/// measure - notably [`crate::pager`], which has to know how many terminal
/// rows a line will wrap onto. Character counts are wrong for CJK (two
/// columns each), combining marks (zero) and emoji ligatures (one cluster
/// of width two whose codepoints sum to more).
pub fn display_columns(text: &str) -> usize {
    display_width(text)
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
/// The question is about the CELL, not the raw value, so it runs the same
/// [`clean_char`] the renderer does and asks what is left. A string-level
/// test would call `"\u{1b}"` content and the cell would be blank (the
/// escape becomes a space), and it would call `"\u{200b}"` content too (the
/// zero-width space is dropped). Several such fields in one row could
/// otherwise fill all four columns with nothing.
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
/// Runs the same per-character cleaning [`sanitize_cell`] does, then asks
/// whether any grapheme cluster is both printable and non-zero-width. Sharing
/// [`clean_char`] is what keeps the two answers consistent: a category the
/// cleaner drops must not be a category the content test counts.
fn renders_visibly(raw: &str) -> bool {
    let cleaned: String = raw.chars().filter_map(clean_char).collect();
    cleaned.graphemes(true).any(|cluster| {
        let printable = cluster.chars().any(|c| !c.is_whitespace());
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
        .filter(|field| field.depth == 0 && field.ty != ParamType::Json)
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

/// How one character is rendered in a cell, or `None` when it is dropped.
///
/// Each category from [`crate::text::Hazard`] gets its own answer, for its
/// own reason - which is why the match below is exhaustive rather than a
/// catch-all: a category added later must be decided here, not defaulted.
///
/// - a CONTROL character becomes a space. Most of them stand where a space
///   belongs (a newline or tab in a title), so a space keeps the words apart
///   and the ANSI escape harmless.
/// - a BIDI FORMAT character becomes U+FFFD. Dropping it silently would hide
///   that a title was built to mislead, and its effect is not confined to
///   this cell: an unterminated override reorders the rest of the row.
/// - an INVISIBLE character is dropped. It occupies no column, so replacing
///   it would make the cell wider than the text it came from, and the
///   content test in [`has_content`] would start counting a cell of nothing
///   as a cell worth a column.
/// - a JOINER is kept. This is DATA: `U+200D` is what makes an emoji
///   ligature one glyph and `U+200C` is part of correctly spelled Persian
///   and Hindi, so removing it would corrupt the value being displayed. It
///   has no scope beyond the characters it joins, so it cannot affect the
///   rest of the row.
fn clean_char(c: char) -> Option<char> {
    match crate::text::hazard(c) {
        None | Some(crate::text::Hazard::Joiner) => Some(c),
        Some(crate::text::Hazard::Control) => Some(' '),
        Some(crate::text::Hazard::BidiFormat) => Some(REPLACEMENT),
        Some(crate::text::Hazard::Invisible) => None,
    }
}

/// Replace control characters (ANSI escapes, newlines, tabs) with spaces,
/// mark bidi format characters, drop invisible ones, and truncate the cell to
/// fit every cell limit.
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
    let cleaned: String = raw.chars().filter_map(clean_char).collect();
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
