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

/// Maximum number of columns shown in a table.
const MAX_TABLE_COLUMNS: usize = 4;
/// Maximum number of characters kept in one table cell (including the
/// truncation marker).
const MAX_CELL_CHARS: usize = 40;
/// Marker appended to truncated cells.
const TRUNCATION_MARK: char = '\u{2026}'; // …
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

/// Render a list of objects as a table, or `None` when the payload does
/// not have that shape.
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
/// timestamp-like keys, purely from the data (schema-driven, generic).
fn select_columns<'a>(rows: &[&'a serde_json::Map<String, Value>]) -> Vec<&'a String> {
    let mut candidates: Vec<&String> = Vec::new();
    for row in rows {
        for key in row.keys() {
            if !candidates.contains(&key) {
                candidates.push(key);
            }
        }
    }
    // A key qualifies only if it is scalar in every row where it appears.
    let mut columns: Vec<&String> = candidates
        .into_iter()
        .filter(|key| {
            rows.iter()
                .filter_map(|row| row.get(*key))
                .all(|value| !value.is_object() && !value.is_array())
        })
        .collect();
    columns.sort_by_key(|key| key_priority(key));
    columns.truncate(MAX_TABLE_COLUMNS);
    columns
}

/// Ranking used to auto-pick key columns; lower is better. The sort is
/// stable, so equal-priority keys keep their first-seen order.
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
/// and truncate to [`MAX_CELL_CHARS`] characters.
fn sanitize_cell(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if cleaned.chars().count() <= MAX_CELL_CHARS {
        cleaned
    } else {
        let mut truncated: String = cleaned.chars().take(MAX_CELL_CHARS - 1).collect();
        truncated.push(TRUNCATION_MARK);
        truncated
    }
}

/// Lay out header and body cells with padded, gap-separated columns.
fn layout_table(header: &[String], body: &[Vec<String>]) -> String {
    let widths: Vec<usize> = header
        .iter()
        .enumerate()
        .map(|(index, name)| {
            body.iter()
                .map(|row| char_width(&row[index]))
                .chain([char_width(name)])
                .max()
                .unwrap_or(0)
        })
        .collect();

    let render_row = |cells: &[String]| -> String {
        let padded: Vec<String> = cells
            .iter()
            .zip(widths.iter().copied())
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        padded.join(COLUMN_GAP).trim_end().to_string()
    };

    let mut lines = vec![render_row(header)];
    lines.extend(body.iter().map(|row| render_row(row)));
    lines.join("\n")
}

/// Column width of a cell, counted in characters.
fn char_width(text: &str) -> usize {
    text.chars().count()
}
