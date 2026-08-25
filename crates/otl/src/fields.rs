//! Field selection for the curated commands.
//!
//! The curated commands do not get their own rendering code: they declare
//! WHICH fields to show (as RFC 6901 JSON pointers into a response row) and
//! this module turns rows into cell text. Layout, sanitizing and truncation
//! stay in [`crate::render`], which is shared with the generic table
//! renderer. Adding a command therefore adds a column list, never a
//! renderer.

use serde_json::Value;

/// One column of a curated table.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    /// Column heading.
    pub header: &'static str,
    /// RFC 6901 pointer to the value inside one row.
    pub pointer: &'static str,
    /// Post-processing applied to the extracted text.
    pub format: Format,
}

impl Column {
    /// A column showing a value verbatim.
    pub const fn plain(header: &'static str, pointer: &'static str) -> Self {
        Self {
            header,
            pointer,
            format: Format::Plain,
        }
    }

    /// A column showing an ISO 8601 timestamp in a shortened form.
    pub const fn timestamp(header: &'static str, pointer: &'static str) -> Self {
        Self {
            header,
            pointer,
            format: Format::Timestamp,
        }
    }

    /// A column showing free-form prose collapsed onto one line.
    pub const fn snippet(header: &'static str, pointer: &'static str) -> Self {
        Self {
            header,
            pointer,
            format: Format::Snippet,
        }
    }
}

/// How an extracted value is turned into display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Verbatim (numbers and booleans via their JSON form).
    Plain,
    /// `2026-08-20T15:30:37.000Z` shown as `2026-08-20 15:30 UTC`.
    Timestamp,
    /// Runs of whitespace collapsed to single spaces, ends trimmed.
    Snippet,
}

/// The headers of a column list.
pub fn headers(columns: &[Column]) -> Vec<&'static str> {
    columns.iter().map(|column| column.header).collect()
}

/// Extract one table row per item.
pub fn rows(items: &[Value], columns: &[Column]) -> Vec<Vec<String>> {
    items
        .iter()
        .map(|item| columns.iter().map(|column| cell(item, column)).collect())
        .collect()
}

/// The display text of one cell.
pub fn cell(item: &Value, column: &Column) -> String {
    let raw = text_at(item, column.pointer);
    match column.format {
        Format::Plain => raw,
        Format::Timestamp => shorten_timestamp(&raw),
        Format::Snippet => collapse_whitespace(&raw),
    }
}

/// The scalar at `pointer`, as text.
///
/// Missing values, JSON `null`, and containers all render as the empty
/// string: a table cell is a scalar slot, and dumping a nested object into
/// one would be noise, not information.
pub fn text_at(item: &Value, pointer: &str) -> String {
    match item.pointer(pointer) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

/// The string at `pointer`, or `None` when it is absent, null or not a
/// string.
pub fn string_at<'a>(item: &'a Value, pointer: &str) -> Option<&'a str> {
    item.pointer(pointer).and_then(Value::as_str)
}

/// Shorten an ISO 8601 UTC timestamp to minute precision.
///
/// Only the exact shape Outline emits (`YYYY-MM-DDTHH:MM:SS...Z`) is
/// rewritten, to `YYYY-MM-DD HH:MM UTC`. The zone marker is kept because
/// dropping it would silently present UTC as local time. Anything else is
/// passed through untouched rather than guessed at.
fn shorten_timestamp(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let shaped = bytes.len() >= 17
        && raw.ends_with('Z')
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[..16]
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13) || byte.is_ascii_digit());
    if !shaped {
        return raw.to_string();
    }
    format!("{} {} UTC", &raw[..10], &raw[11..16])
}

/// Collapse every run of whitespace to a single space and trim the ends.
///
/// Search snippets are prose that can contain newlines; a table cell is one
/// line. (Control characters are additionally neutralized by
/// [`crate::render`], which every cell passes through.)
fn collapse_whitespace(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_nested_values_by_pointer() {
        let row = json!({ "context": "hello", "document": { "title": "Deploy" } });
        assert_eq!(text_at(&row, "/document/title"), "Deploy");
        assert_eq!(text_at(&row, "/context"), "hello");
    }

    #[test]
    fn missing_null_and_container_values_render_empty() {
        let row = json!({ "a": null, "b": { "c": 1 }, "d": [1, 2] });
        assert_eq!(text_at(&row, "/a"), "");
        assert_eq!(text_at(&row, "/b"), "");
        assert_eq!(text_at(&row, "/d"), "");
        assert_eq!(text_at(&row, "/nope"), "");
        assert_eq!(text_at(&row, "/nope/deeper"), "");
    }

    #[test]
    fn numbers_and_booleans_render_as_json() {
        let row = json!({ "n": 42, "f": 1.5, "b": true });
        assert_eq!(text_at(&row, "/n"), "42");
        assert_eq!(text_at(&row, "/f"), "1.5");
        assert_eq!(text_at(&row, "/b"), "true");
    }

    #[test]
    fn shortens_outline_timestamps_and_keeps_the_zone() {
        assert_eq!(
            shorten_timestamp("2026-08-20T15:30:37.000Z"),
            "2026-08-20 15:30 UTC"
        );
        assert_eq!(
            shorten_timestamp("2026-08-20T15:30:37Z"),
            "2026-08-20 15:30 UTC"
        );
    }

    #[test]
    fn passes_through_anything_that_is_not_that_shape() {
        for raw in [
            "",
            "not a date",
            "2026-08-20",
            "2026-08-20T15:30:37+02:00",
            "20260820T153037Z",
            "\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}",
        ] {
            assert_eq!(shorten_timestamp(raw), raw, "rewrote {raw:?}");
        }
    }

    #[test]
    fn timestamp_shortening_never_panics_on_multibyte_input() {
        // Slicing by byte index would panic mid-codepoint; the shape check
        // must therefore reject anything non-ASCII before slicing.
        for raw in [
            "2026-08-\u{00e9}0T15:30:37Z",
            "\u{1f600}\u{1f600}\u{1f600}Z",
        ] {
            let _ = shorten_timestamp(raw);
        }
    }

    #[test]
    fn snippets_collapse_onto_one_line() {
        assert_eq!(
            collapse_whitespace("  a\n b\t\tc  "),
            "a b c",
            "newlines and tabs must not survive into a table cell"
        );
    }

    #[test]
    fn builds_rows_from_columns() {
        let items = vec![json!({ "document": { "title": "T" }, "context": "c" })];
        let columns = [
            Column::plain("Title", "/document/title"),
            Column::snippet("Context", "/context"),
        ];
        assert_eq!(
            rows(&items, &columns),
            vec![vec!["T".to_string(), "c".to_string()]]
        );
        assert_eq!(headers(&columns), vec!["Title", "Context"]);
    }
}
