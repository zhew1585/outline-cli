//! `otl docs view <id>`.
//!
//! The datum of this command is the document's markdown, so - unlike the
//! list commands - a pipe gets the markdown and not JSON. The dual-state
//! rule still holds, it is just spelled with a different default:
//!
//! - `--json` (explicit): the raw document object, for scripts;
//! - `--raw`, or a non-terminal stdout: the markdown, byte-for-byte - no
//!   pager, no filtering, not even an added trailing newline;
//! - a terminal stdout: the markdown prepared for display (see
//!   [`crate::pager`]), through `$PAGER` when it does not fit on one screen;
//! - `--web`: the document's URL on stdout, and the browser opened at it.
//!
//! # Reading part of a document
//!
//! `--outline` and `--section` exist for the caller who wants to change one
//! part of a page and would otherwise have to hold all of it. Neither saves
//! a byte on the wire - `documents.info` returns the whole body and has no
//! field selection, so the full text is fetched either way - and that is
//! not what they are for. What they save is what the CALLER has to take in,
//! which for an agent is the cost that actually binds.
//!
//! So the pair is deliberately asymmetric in size: `--outline` is a few
//! dozen bytes of structure that says what is in the document and how to
//! address it, and `--section` is the one part that answers to an address.
//! Together they replace "read the page" with two cheap reads.
//!
//! `--outline` follows the ordinary dual-state rule rather than this
//! command's markdown-first one, because its datum is structure and not
//! markdown: a pipe gets JSON. `--section` keeps the markdown-first rule,
//! because its datum IS markdown - and that form is the byte-exact one.

use anyhow::anyhow;
use clap::Args;
use serde_json::{json, Value};

use crate::browser;
use crate::config::Overrides;
use crate::exit::CliError;
use crate::fields;
use crate::pager;
use crate::render::{self, OutputMode};
use crate::session::Session;
use crate::stdio;

use super::section::{self, Heading};

/// The compiled operation this command drives.
const OPERATION: &str = "documents.info";

/// Arguments for `otl docs view`.
#[derive(Debug, Args)]
#[command(after_long_help = "API contract:
  This command uses documents.info.

  Inspect it with:
    otl api describe documents.info --json

JSON shape:
  This command's datum is markdown, so stdout is NOT JSON unless --json is
  spelled out - a pipe gets the markdown bytes, not an object.

    otl docs view ID              -> markdown (pager on a terminal)
    otl docs view ID --raw        -> markdown, byte for byte
    otl docs view ID --json       -> the documents.info document, verbatim
                                     (.id, .title, .text, .url, ...)
    otl docs view ID --web --json -> { id, title, url }, this CLI's own
                                     object: the absolute URL it opened,
                                     not the server's relative .url

  --raw and --json are mutually exclusive: one prints the body, the other
  prints the metadata.

Reading part of a document:
  --outline and --section let you locate and read one section without
  taking in the whole page. Neither reduces what is fetched (documents.info
  has no field selection); both reduce what you have to hold.

    otl docs view ID --outline           -> the heading tree, and .revision
    otl docs view ID --section 'Deploy'  -> that section's markdown

  --outline is the exception to the rule above: its datum is structure, not
  markdown, so it follows the usual dual-state rule and a pipe gets JSON:

    { id, title, revision, updatedAt, bytes,
      sections: [ { level, title, path, line, bytes } ] }

  `path` is the address to hand back to --section, or to `otl docs update
  --section`; `revision` is the value for their --if-revision. A section
  runs to the next heading of the same or a higher level, so `bytes` on a
  parent includes its children.

  --section --json wraps the same markdown with its position:

    { id, revision, path, level, line, bytes, text }

  Prefer plain --section (or --raw) when the bytes must be exact: like
  every object this CLI authors, the JSON form is scrubbed of terminal
  control characters, while the markdown form is verbatim.")]
pub struct ViewArgs {
    /// Document id (UUID or the short urlId from its URL).
    pub id: String,

    /// Write the markdown straight to stdout, never through a pager.
    #[arg(long, conflicts_with = "outline")]
    pub raw: bool,

    /// Open the document in the default browser (honors `$BROWSER`).
    #[arg(long, conflicts_with_all = ["raw", "outline", "section"])]
    pub web: bool,

    /// Print the heading tree and the document's revision instead of its
    /// body, as the cheap way to find what to read or edit next.
    #[arg(long, conflicts_with = "section")]
    pub outline: bool,

    /// Print only the section this heading names, heading line included
    /// (`--outline` lists every address; `Parent > Child` disambiguates).
    #[arg(long, value_name = "HEADING")]
    pub section: Option<String>,
}

/// Run `otl docs view`.
pub fn run(
    cmd: &ViewArgs,
    mode: OutputMode,
    json_requested: bool,
    overrides: &Overrides,
) -> Result<(), CliError> {
    if cmd.raw && json_requested {
        return Err(CliError::usage(anyhow!(
            "--raw prints the document's markdown and --json prints its \
             metadata as JSON; pass one or the other"
        )));
    }
    let session = Session::open(overrides)?;
    let document = session.call_data(OPERATION, &[("id".to_string(), cmd.id.clone())])?;
    if cmd.web {
        return open_in_browser(&session, &document, json_requested);
    }
    if cmd.outline {
        return print_outline(&document, mode, json_requested);
    }
    if let Some(address) = &cmd.section {
        return print_section(&document, address, mode, json_requested, cmd.raw);
    }
    if json_requested {
        return print_json(&document);
    }
    let text = markdown(&document);
    // Interactive display (control-character filtering, a trailing newline,
    // and a pager past one screen) is for humans only: a terminal, no
    // --raw, no --json. Everything else gets the bytes verbatim.
    let interactive = mode == OutputMode::Table && !cmd.raw;
    pager::write(&text, interactive)
}

/// The document's markdown body.
///
/// A document whose body was not returned (Outline can answer with a
/// rich-text `data` field instead of `text`) yields empty output plus a
/// diagnostic, rather than a silent blank.
fn markdown(document: &Value) -> String {
    match fields::string_at(document, "/text") {
        Some(text) => text.to_string(),
        None => {
            stdio::write_diagnostic_line(
                "warning: the server returned no markdown body for this document",
            );
            String::new()
        }
    }
}

/// Print the raw document object.
fn print_json(document: &Value) -> Result<(), CliError> {
    let rendered = render::render_json(document)
        .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
    stdio::write_data_line(&rendered)
}

/// Print an object this command composed.
///
/// The scrubbing renderer, because these are authored objects mixing server
/// text (heading titles, a section body) with locally computed positions.
/// Nothing round-trips them, so the verbatim renderer's premise - that a
/// caller diffs or replays the server's own bytes - does not apply. The
/// caller who needs the bytes exact has the markdown form, which is
/// verbatim, and is pointed at it in `--help`.
fn print_authored(payload: &Value) -> Result<(), CliError> {
    let rendered = render::render_json_scrubbed(payload)
        .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
    stdio::write_data_line(&rendered)
}

/// Print the document's heading tree.
fn print_outline(document: &Value, mode: OutputMode, json_requested: bool) -> Result<(), CliError> {
    let text = markdown(document);
    let found = section::headings(&text);
    if json_requested || mode == OutputMode::Json {
        return print_authored(&json!({
            "id": fields::string_at(document, "/id"),
            "title": fields::string_at(document, "/title"),
            "revision": revision_value(document),
            "updatedAt": fields::string_at(document, "/updatedAt"),
            "bytes": text.len(),
            "sections": found.iter().map(outline_entry).collect::<Vec<Value>>(),
        }));
    }
    stdio::write_data_line(&outline_block(document, &text, &found))
}

/// One section's entry in the JSON outline.
fn outline_entry(heading: &Heading) -> Value {
    json!({
        "level": heading.level,
        "title": heading.title,
        "path": heading.path(),
        "line": heading.line,
        "bytes": heading.end - heading.start,
    })
}

/// The human outline: the document's identity, then its heading tree.
///
/// The tree goes through the shared table renderer, so a heading title is
/// sanitized and width-bounded exactly like a title in any list. That bound
/// can clip a long heading, which is why `--help` sends a caller who needs
/// the exact address to the JSON form.
fn outline_block(document: &Value, text: &str, found: &[Heading]) -> String {
    let mut pairs: Vec<(&'static str, String)> = Vec::new();
    for (label, pointer) in [("id", "/id"), ("title", "/title")] {
        let value = fields::text_at(document, pointer);
        if !value.is_empty() {
            pairs.push((label, value));
        }
    }
    let revision = fields::text_at(document, "/revision");
    if !revision.is_empty() {
        pairs.push(("revision", revision));
    }
    pairs.push(("bytes", text.len().to_string()));
    pairs.push(("sections", found.len().to_string()));
    let mut block = render::render_pairs(&pairs);
    if found.is_empty() {
        block.push_str("\n\nThis document has no markdown headings.");
        return block;
    }
    let rows: Vec<Vec<String>> = found
        .iter()
        .map(|heading| {
            vec![
                heading.line.to_string(),
                (heading.end - heading.start).to_string(),
                format!(
                    "{}{}",
                    "  ".repeat(usize::from(heading.level).saturating_sub(1)),
                    heading.title
                ),
            ]
        })
        .collect();
    block.push_str("\n\n");
    block.push_str(&render::render_columns(
        &["line", "bytes", "heading"],
        &rows,
    ));
    block
}

/// Print one section of the document.
fn print_section(
    document: &Value,
    address: &str,
    mode: OutputMode,
    json_requested: bool,
    raw: bool,
) -> Result<(), CliError> {
    let text = markdown(document);
    let found = section::headings(&text);
    let heading = section::resolve(&found, address).map_err(|error| {
        CliError::usage(anyhow!(section::unresolved_message(
            "--section",
            address,
            &found,
            &error
        )))
    })?;
    let body = &text[heading.range()];
    if json_requested {
        return print_authored(&json!({
            "id": fields::string_at(document, "/id"),
            "revision": revision_value(document),
            "path": heading.path(),
            "level": heading.level,
            "line": heading.line,
            "bytes": body.len(),
            "text": body,
        }));
    }
    let interactive = mode == OutputMode::Table && !raw;
    pager::write(body, interactive)
}

/// The document's `revision`, or JSON null when the server did not send one.
///
/// Null rather than absent, and never a locally invented number: this is
/// the value a caller passes to `--if-revision`, so "the server did not say"
/// has to be distinguishable from any revision at all.
fn revision_value(document: &Value) -> Value {
    document
        .pointer("/revision")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Print the document's absolute URL and open a browser at it.
///
/// The URL goes to stdout FIRST: if no browser can be launched the user
/// still has the link, and a script that only wants the link gets it.
fn open_in_browser(
    session: &Session,
    document: &Value,
    json_requested: bool,
) -> Result<(), CliError> {
    let path = fields::string_at(document, "/url").ok_or_else(|| {
        CliError::failure(anyhow!(
            "the server did not return a URL for this document, so there is \
             nothing to open"
        ))
    })?;
    let url = session.absolute_url(path)?;
    if json_requested {
        let payload = serde_json::json!({
            "id": fields::string_at(document, "/id"),
            "title": fields::string_at(document, "/title"),
            "url": url,
        });
        print_json(&payload)?;
    } else {
        stdio::write_data_line(&url)?;
    }
    browser::open(&url)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const BODY: &str = "intro\n\n## Deploy\n\nsteps\n\n### Rollback\n\nundo\n\n## FAQ\n\nanswers\n";

    fn document() -> Value {
        json!({
            "id": "doc-1",
            "title": "Runbook",
            "revision": 12,
            "updatedAt": "2026-08-20T15:30:37.000Z",
            "text": BODY,
        })
    }

    #[test]
    fn the_json_outline_carries_every_address_and_the_revision() {
        let text = markdown(&document());
        let found = section::headings(&text);
        let entries: Vec<Value> = found.iter().map(outline_entry).collect();
        let paths: Vec<&str> = entries
            .iter()
            .filter_map(|entry| entry["path"].as_str())
            .collect();
        assert_eq!(paths, ["Deploy", "Deploy > Rollback", "FAQ"]);
        assert_eq!(entries[0]["level"], 2);
        assert_eq!(entries[0]["line"], 3);
        // A parent's byte count includes its children.
        let deploy = entries[0]["bytes"].as_u64().unwrap();
        let rollback = entries[1]["bytes"].as_u64().unwrap();
        assert!(deploy > rollback, "{deploy} should contain {rollback}");
        assert_eq!(revision_value(&document()), json!(12));
    }

    /// The point of `--outline`: it is small enough to read instead of the
    /// document, or it buys nothing.
    #[test]
    fn the_outline_is_much_smaller_than_the_body_it_describes() {
        let mut document = document();
        let big = format!("{BODY}{}", "filler line\n".repeat(4000));
        document["text"] = json!(big);
        let text = markdown(&document);
        let found = section::headings(&text);
        let entries: Vec<Value> = found.iter().map(outline_entry).collect();
        let outline = render::render_json_scrubbed(&json!(entries)).unwrap();
        assert!(
            outline.len() * 20 < text.len(),
            "outline is {} bytes against a {} byte body",
            outline.len(),
            text.len()
        );
    }

    #[test]
    fn a_revision_the_server_did_not_send_is_null_rather_than_invented() {
        let mut document = document();
        document.as_object_mut().unwrap().remove("revision");
        assert_eq!(revision_value(&document), Value::Null);
    }

    #[test]
    fn the_human_outline_shows_the_tree_indented_under_its_parents() {
        let text = markdown(&document());
        let found = section::headings(&text);
        let block = outline_block(&document(), &text, &found);
        assert!(block.contains("revision"), "{block}");
        assert!(block.contains("Deploy"), "{block}");
        // Rollback is a level deeper, so it is indented further than Deploy.
        let deploy = block.find("Deploy").unwrap();
        let rollback = block.find("Rollback").unwrap();
        let column = |at: usize| at - block[..at].rfind('\n').map_or(0, |start| start + 1);
        assert!(
            column(rollback) > column(deploy),
            "Rollback is not indented under Deploy:\n{block}"
        );
    }

    #[test]
    fn a_document_with_no_headings_says_so_rather_than_printing_an_empty_table() {
        let mut document = document();
        document["text"] = json!("just prose\n");
        let text = markdown(&document);
        let found = section::headings(&text);
        let block = outline_block(&document, &text, &found);
        assert!(block.contains("no markdown headings"), "{block}");
    }

    /// A heading title is server text reaching a terminal, so the tree has
    /// to be as inert as any other table.
    #[test]
    fn a_hostile_heading_cannot_forge_a_line_or_move_the_cursor() {
        let mut document = document();
        document["text"] = json!("## evil\u{1b}[31m\nreal\n\n## B\n\nb\n");
        let text = markdown(&document);
        let found = section::headings(&text);
        let block = outline_block(&document, &text, &found);
        assert!(!block.contains('\u{1b}'), "escape survived:\n{block}");
    }
}
