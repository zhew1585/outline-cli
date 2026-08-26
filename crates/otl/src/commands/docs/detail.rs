//! Reporting one document after it was written.
//!
//! `otl docs create` and `otl docs update` both answer with the same shape
//! (the document's identity, its link, and when it last changed), so both
//! use this module. Field selection only; the layout comes from
//! [`crate::render`].

use anyhow::anyhow;
use serde_json::Value;

use crate::exit::CliError;
use crate::fields::{self, Column};
use crate::render::{self, OutputMode};
use crate::session::Session;
use crate::stdio;

/// Labelled fields shown in human-readable mode, in order.
///
/// Same column vocabulary as the list commands, so a timestamp reads the
/// same way everywhere.
const FIELDS: &[Column] = &[
    Column::plain("id", "/id"),
    Column::plain("title", "/title"),
    Column::timestamp("updated", "/updatedAt"),
    Column::plain("revision", "/revision"),
];

/// Print one document's metadata.
///
/// `Json` mode prints the server's document object verbatim (that is the
/// scriptable form, and it holds every field this summary leaves out);
/// `Table` mode prints the labelled essentials, including the absolute URL
/// and whether the document is still a draft.
pub fn report(session: &Session, document: &Value, mode: OutputMode) -> Result<(), CliError> {
    if mode == OutputMode::Json {
        let rendered = render::render_json(document)
            .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
        return stdio::write_data_line(&rendered);
    }
    let url = link(session, document);
    stdio::write_data_line(&render::render_pairs(&pairs(document, url)))
}

/// The labelled essentials of one document, in display order.
///
/// Fields the server did not send are dropped rather than shown blank;
/// `status` is always present, because "created" and "created where anyone
/// can see it" are different outcomes.
fn pairs(document: &Value, url: Option<String>) -> Vec<(&'static str, String)> {
    let mut pairs: Vec<(&'static str, String)> = FIELDS
        .iter()
        .map(|column| (column.header, fields::cell(document, column)))
        .filter(|(_, value)| !value.is_empty())
        .collect();
    if let Some(url) = url {
        pairs.push(("url", url));
    }
    pairs.push(("status", status(document).to_string()));
    pairs
}

/// The document's absolute URL, or `None` with a diagnostic.
///
/// A link that cannot be built must not fail the command: the document has
/// already been created or updated, and reporting that as a failure would
/// invite a duplicate retry.
fn link(session: &Session, document: &Value) -> Option<String> {
    let path = fields::string_at(document, "/url")?;
    match session.absolute_url(path) {
        Ok(url) => Some(url),
        Err(error) => {
            stdio::write_diagnostic_line(&format!("warning: {error}"));
            None
        }
    }
}

/// Whether the document is published or still a draft.
///
/// Outline reports this through `publishedAt`, which is null for a draft. A
/// draft is invisible to the rest of the workspace, so saying so is the
/// difference between "stored" and "stored where anyone can find it".
fn status(document: &Value) -> &'static str {
    match document.pointer("/publishedAt") {
        Some(Value::String(_)) => "published",
        _ => "draft",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn document() -> Value {
        json!({
            "id": "doc-1",
            "title": "Release notes",
            "updatedAt": "2026-08-20T15:30:37.000Z",
            "revision": 4,
            "url": "/doc/release-notes-abc123",
            "publishedAt": "2026-08-20T15:30:37.000Z",
        })
    }

    #[test]
    fn reports_the_essentials_of_a_published_document() {
        let url = Some("https://docs.example.com/doc/release-notes-abc123".to_string());
        // Golden file: byte-for-byte, including label alignment.
        assert_eq!(
            format!("{}\n", render::render_pairs(&pairs(&document(), url))),
            include_str!("../../../tests/golden/docs_detail_pairs.txt")
        );
    }

    #[test]
    fn a_url_that_could_not_be_built_is_simply_absent() {
        let rendered = render::render_pairs(&pairs(&document(), None));
        assert!(!rendered.contains("url"), "{rendered}");
        assert!(rendered.contains("doc-1"), "{rendered}");
    }

    #[test]
    fn a_draft_is_labelled_as_such() {
        let mut document = document();
        document["publishedAt"] = Value::Null;
        let rendered = render::render_pairs(&pairs(&document, None));
        let status = rendered.lines().last().unwrap_or_default();
        assert!(status.starts_with("status"), "{rendered}");
        assert!(status.ends_with("draft"), "{rendered}");
    }

    #[test]
    fn absent_fields_are_dropped_rather_than_shown_blank() {
        let document = json!({ "id": "doc-1" });
        let rendered = render::render_pairs(&pairs(&document, None));
        assert!(!rendered.contains("title"), "{rendered}");
        assert!(!rendered.contains("revision"), "{rendered}");
    }

    #[test]
    fn a_title_cannot_smuggle_terminal_escapes_or_line_breaks() {
        let mut document = document();
        document["title"] = json!("evil\u{1b}[31m\nstatus  published");
        let rendered = render::render_pairs(&pairs(&document, None));
        assert!(!rendered.contains('\u{1b}'), "escape survived: {rendered}");
        // The forged extra line must have been folded into the title cell.
        assert_eq!(
            rendered.lines().count(),
            pairs(&document, None).len(),
            "{rendered}"
        );
    }
}
