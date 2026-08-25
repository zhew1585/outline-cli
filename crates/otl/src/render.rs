//! Dual-state output rendering.
//!
//! stdout carries data in one of two states (per the CLI contract):
//!
//! - `Json`: pretty JSON, jq-consumable, no color or decoration. Chosen by
//!   `--json` or whenever stdout is not a TTY.
//! - `Table`: schema/data-driven table for list-shaped payloads, chosen
//!   only on a TTY. Columns are picked generically from the data (no
//!   per-endpoint rendering code); non-list payloads fall back to JSON.
//!
//! Nothing here emits ANSI escapes, and cell text is scrubbed of control
//! characters: server-controlled strings must not smuggle escapes or line
//! breaks into the terminal. Any future decoration must be gated on
//! [`OutputMode::Table`] so that non-TTY output stays decoration-free.

use serde_json::Value;
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
/// In table mode, payloads that are not a list of objects fall back to
/// pretty JSON so every payload shape stays renderable.
pub fn render(payload: &Value, mode: OutputMode) -> Result<String, serde_json::Error> {
    match mode {
        OutputMode::Json => serde_json::to_string_pretty(payload),
        OutputMode::Table => match try_render_table(payload) {
            Some(table) => Ok(table),
            None => serde_json::to_string_pretty(payload),
        },
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

/// Render a list of objects as a table, or `None` when the payload does
/// not have that shape.
///
// TODO(story-1.5b): thread the operation's response schema in here (an
// `&OpSpec` carrying response field descriptors from the IR) and select
// columns from the SCHEMA instead of the observed rows. Until the IR gains
// response schemas, the column set is derived from the response itself; it
// is made deterministic within a response by unioning keys across all rows
// and ordering them by a fixed priority, so it cannot drift between rows or
// with map iteration order - but two responses of the same operation that
// omit different optional fields can still differ.
fn try_render_table(payload: &Value) -> Option<String> {
    let rows = payload.as_array()?;
    if rows.is_empty() {
        return Some(EMPTY_LIST_PLACEHOLDER.to_string());
    }
    let objects: Vec<_> = rows.iter().map(Value::as_object).collect::<Option<_>>()?;
    let columns = select_columns(&objects);
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

/// Pick up to [`MAX_TABLE_COLUMNS`] keys, preferring identity and
/// timestamp-like keys.
///
/// Determinism rules, so the same response always yields the same header:
///
/// - the candidate set is the UNION of the keys of every row, never a
///   sample, so an optional field missing from the first row (or from any
///   other) cannot change the column set;
/// - ordering is by fixed priority and then by key name, so it depends on
///   neither row order nor JSON object iteration order;
/// - a key whose value is a container in any row is dropped entirely, so a
///   column is never half-rendered.
fn select_columns<'a>(rows: &[&'a serde_json::Map<String, Value>]) -> Vec<&'a String> {
    let mut candidates: Vec<&String> = Vec::new();
    for row in rows {
        for key in row.keys() {
            if !candidates.contains(&key) {
                candidates.push(key);
            }
        }
    }
    let mut columns: Vec<&String> = candidates
        .into_iter()
        .filter(|key| {
            rows.iter()
                .filter_map(|row| row.get(*key))
                .all(|value| !value.is_object() && !value.is_array())
        })
        .collect();
    columns.sort_by(|left, right| {
        key_priority(left)
            .cmp(&key_priority(right))
            .then_with(|| left.cmp(right))
    });
    columns.truncate(MAX_TABLE_COLUMNS);
    columns
}

/// Ranking used to auto-pick key columns; lower is better. Ties are broken
/// by key name in [`select_columns`], never by input order.
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
