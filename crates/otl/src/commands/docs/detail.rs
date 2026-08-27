//! Reporting one document after it was written.
//!
//! `otl docs create` and `otl docs update` both answer with the same shape
//! (the document's identity, its link, and when it last changed), so both
//! use this module. Field selection only; the layout comes from
//! [`crate::render`].
//!
//! # Why a write receipt is not the whole document
//!
//! Outline answers `documents.update` with the document it just stored -
//! body included. Echoing that back makes the receipt as large as the
//! document: appending one line to a 46 KB page returned 46 KB, and an
//! agent driving this CLI pays for every byte of it in a context window
//! that then has to hold the page it already had.
//!
//! So both write commands report the IDENTITY fields (see
//! [`RECEIPT_FIELDS`]) and drop the rest. Nothing is lost, because the
//! whole response is still one command away, through either of the two
//! surfaces whose contract is to forward it verbatim:
//!
//! - `otl docs view <id> --json` - the same document, read back;
//! - `otl api documents.update ...` - the raw operation, unfiltered. That
//!   is what `otl api` is FOR: the curated commands offer a chosen shape,
//!   `otl api` offers the server's.

use anyhow::anyhow;
use serde_json::{Map, Value};

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

/// The fields a write receipt keeps, in the response schema's own order.
///
/// What they have in common: each one answers "where did the write land,
/// and which version is it now" - identity, placement, ordering. What is
/// absent is everything whose size is unbounded (`text`, `data`,
/// `tasks`) or whose value the caller supplied moments ago (`icon`,
/// `color`, `fullWidth`) or that costs a nested object to say something
/// this receipt does not claim (`createdBy`, `updatedBy`).
///
/// `publishedAt` earns its place by being the one field that reports an
/// outcome rather than an input: a draft is invisible to the workspace, so
/// "stored" and "stored where anyone can find it" have to be tellable
/// apart. Table mode spells this out as `status` (see [`status`]); JSON
/// keeps the server's own field, so the value stays the server's.
const RECEIPT_FIELDS: &[&str] = &[
    "id",
    "collectionId",
    "parentDocumentId",
    "title",
    "url",
    "urlId",
    "revision",
    "createdAt",
    "updatedAt",
    "publishedAt",
];

/// Print one document's metadata.
///
/// `Json` mode prints the identity fields of the stored document - the
/// scriptable form, and deliberately NOT the whole response (see the module
/// docs); `Table` mode prints the labelled essentials, including the
/// absolute URL and whether the document is still a draft.
pub fn report(session: &Session, document: &Value, mode: OutputMode) -> Result<(), CliError> {
    if mode == OutputMode::Json {
        let rendered = render::render_json_scrubbed(&receipt(document))
            .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
        return stdio::write_data_line(&rendered);
    }
    let url = link(session, document);
    stdio::write_data_line(&render::render_pairs(&pairs(document, url)))
}

/// The write receipt: [`RECEIPT_FIELDS`] of the stored document.
///
/// Two shapes are left alone rather than projected:
///
/// - a response that is not an object at all cannot hold a document body
///   either, so forwarding it whole costs nothing and hides nothing;
/// - an object holding none of the receipt fields would project to `{}` -
///   an empty success, which reads as "written, and here is nothing" when
///   what actually happened is that the response was not the shape this
///   command knows. It is forwarded with a diagnostic instead, because a
///   write that already succeeded must not be reported as a failure and
///   invite a duplicate retry.
///
/// Fields the server did not send are dropped rather than sent as null,
/// matching what [`pairs`] does for the human summary: absent and null are
/// different claims, and only the server gets to make the second one.
fn receipt(document: &Value) -> Value {
    let Some(fields) = document.as_object() else {
        return document.clone();
    };
    let kept: Map<String, Value> = RECEIPT_FIELDS
        .iter()
        .filter_map(|name| fields.get_key_value(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    if kept.is_empty() {
        stdio::write_diagnostic_line(
            "warning: the response holds none of the expected document \
             fields; reporting it unfiltered",
        );
        return document.clone();
    }
    Value::Object(kept)
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]

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

    /// [`document`] as the API actually answers a write: the body, the
    /// rich-text mirror of the body, and the nested actor objects.
    fn full_response() -> Value {
        let mut document = document();
        document["text"] = json!("# Release notes\n\nthe whole page\n");
        document["data"] = json!({ "type": "doc", "content": [] });
        document["collectionId"] = json!("col-1");
        document["tasks"] = json!({ "completed": 0, "total": 0 });
        document["collaboratorIds"] = json!(["user-1"]);
        document["updatedBy"] = json!({ "id": "user-1", "name": "Ada" });
        document
    }

    #[test]
    fn the_receipt_keeps_identity_and_drops_the_body() {
        let receipt = receipt(&full_response());
        for kept in ["id", "title", "url", "revision", "updatedAt", "publishedAt"] {
            assert!(receipt.get(kept).is_some(), "{kept} missing: {receipt}");
        }
        assert_eq!(receipt["collectionId"], json!("col-1"));
        // The point of the whole change: none of the unbounded fields, and
        // none of the nested actors, survive into the receipt.
        for dropped in ["text", "data", "tasks", "collaboratorIds", "updatedBy"] {
            assert!(
                receipt.get(dropped).is_none(),
                "{dropped} survived: {receipt}"
            );
        }
    }

    #[test]
    fn the_receipt_is_a_small_fraction_of_the_response() {
        // A 46 KB page returned 46 KB of receipt before this projection.
        let mut response = full_response();
        response["text"] = json!("x".repeat(46 * 1024));
        let full = render::render_json_scrubbed(&response).unwrap();
        let slim = render::render_json_scrubbed(&receipt(&response)).unwrap();
        assert!(
            slim.len() * 50 < full.len(),
            "receipt is {} bytes against {} bytes of response",
            slim.len(),
            full.len()
        );
    }

    #[test]
    fn receipt_fields_the_server_did_not_send_are_absent_rather_than_null() {
        let receipt = receipt(&json!({ "id": "doc-1", "text": "body" }));
        assert_eq!(receipt, json!({ "id": "doc-1" }));
    }

    #[test]
    fn a_response_holding_no_known_field_is_forwarded_rather_than_emptied() {
        // Reporting `{}` here would read as "written, and here is nothing".
        let strange = json!({ "unexpected": "shape" });
        assert_eq!(receipt(&strange), strange);
        let not_an_object = json!("done");
        assert_eq!(receipt(&not_an_object), not_an_object);
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
