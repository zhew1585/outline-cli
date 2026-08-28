//! YAML frontmatter: written by `otl docs export`, removed by `otl docs
//! create` and `otl docs update`.
//!
//! An exported file has to say WHICH document it is, or a local copy is a
//! dead end: the file name is a sanitized derivative of the title, so it
//! cannot be turned back into an id, and a directory of markdown carries no
//! record of where it came from. The block this module writes closes that
//! loop:
//!
//! ```text
//! ---
//! outline_id: "55baa74a-bad1-4b16-a0d0-ec103c656b8e"
//! outline_url_id: "engKBTOaWe"
//! title: "Billing dunning - design"
//! revision: 15
//! updated_at: "2026-08-27T16:17:58.967Z"
//! ---
//! ```
//!
//! # It is removed on the way back in, not just written on the way out
//!
//! Metadata that a write pushes back into the document BODY is worse than
//! no metadata: every round trip would bury another block at the top of the
//! page. So [`split`] runs on every body `otl docs create` and `otl docs
//! update` read, and the block never reaches the server as text.
//!
//! # Every string is double-quoted, on purpose
//!
//! A title is server-controlled text. It can start with `-`, contain a `:`,
//! be the word `null`, or look like a number - each of which changes what a
//! plain YAML scalar MEANS. Quoting every string (and escaping what has to
//! be escaped) makes the block's meaning independent of what the title
//! happens to say. `revision` is the one unquoted value, because it is a
//! number and reads as one.
//!
//! # Detection is conservative in the other direction
//!
//! A markdown document may legitimately open with `---` - a horizontal
//! rule, or a thematic break before a second one. Stripping that as
//! frontmatter would silently delete a chunk of somebody's document on the
//! way to the server, which is the worst failure this module could have:
//! nothing about the result looks wrong until the paragraph is missed.
//!
//! So [`split`] removes a leading block only when all three hold: it is
//! fenced on BOTH sides, every line inside it is a `key: value` pair, and
//! one of those keys is `outline_id` or `outline_url_id`. That third
//! condition is what makes the removal provably about THIS CLI's block -
//! no horizontal rule and no other tool's frontmatter declares an
//! `outline_id`. A file whose frontmatter carries neither key is somebody
//! else's metadata, and it is sent to the server as the body it appears to
//! be. Visible and correctable beats silent and not.

use serde_json::Value;

use crate::fields;

/// Fence line that opens and closes the block.
const FENCE: &str = "---";

/// The alternative closing fence YAML allows (end of document).
const END_FENCE: &str = "...";

/// Key holding the document's UUID.
const ID_KEY: &str = "outline_id";

/// Key holding the document's short url id.
const URL_ID_KEY: &str = "outline_url_id";

/// Key holding the revision the local copy was taken from.
const REVISION_KEY: &str = "revision";

/// Longest frontmatter block [`split`] will consider, in lines.
///
/// A bound rather than a rule about content: without one, a document whose
/// first line is `---` and which never closes the block would be scanned to
/// the end before being (correctly) passed through. The cap keeps that cost
/// proportional to a frontmatter block rather than to a document.
const MAX_BLOCK_LINES: usize = 64;

/// What a leading frontmatter block told us about the document.
///
/// Only the fields a write can ACT on are parsed. Everything else in the
/// block is removed from the body and otherwise ignored: this is not a
/// general YAML reader, and pretending otherwise would invite callers to
/// depend on parsing this module does not do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontMatter {
    /// `outline_id` - the document's UUID.
    pub id: Option<String>,
    /// `outline_url_id` - the short id from the document's URL.
    ///
    /// Kept because a caller may name the document either way, so a command
    /// line id that disagrees with `outline_id` is not yet a conflict: it
    /// may be this.
    pub url_id: Option<String>,
    /// `revision` - which version of the document this copy was taken from.
    pub revision: Option<u64>,
}

impl FrontMatter {
    /// Whether `candidate` names the document this block describes.
    ///
    /// Either spelling counts, because either is what a caller would have
    /// copied out of a URL. A block that names no id at all cannot
    /// contradict anything, so it matches.
    pub fn names(&self, candidate: &str) -> bool {
        let known = [self.id.as_deref(), self.url_id.as_deref()];
        if known.iter().all(Option::is_none) {
            return true;
        }
        known.iter().flatten().any(|known| *known == candidate)
    }

    /// The id to write to, preferring the UUID.
    pub fn document_id(&self) -> Option<&str> {
        self.id.as_deref().or(self.url_id.as_deref())
    }
}

/// The frontmatter block for one `documents.info` response.
///
/// `None` when the response carried none of the fields - an empty block
/// would be a fence around nothing, and a file is better off without it.
///
/// Absent fields are omitted rather than written as `null`, matching what
/// the write receipt does: absent and null are different claims, and only
/// the server gets to make the second one.
pub fn block(document: &Value) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for (key, pointer) in [(ID_KEY, "/id"), (URL_ID_KEY, "/urlId"), ("title", "/title")] {
        if let Some(text) = fields::string_at(document, pointer) {
            lines.push(format!("{key}: {}", quote(text)));
        }
    }
    // A revision is a number and is written as one, so a sync script can
    // compare it without unquoting. A server that sends something else for
    // it is not describing a revision, and the key is left out.
    if let Some(revision) = document.pointer("/revision").and_then(Value::as_u64) {
        lines.push(format!("{REVISION_KEY}: {revision}"));
    }
    if let Some(text) = fields::string_at(document, "/updatedAt") {
        lines.push(format!("updated_at: {}", quote(text)));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("{FENCE}\n{}\n{FENCE}\n", lines.join("\n")))
}

/// Split a leading frontmatter block off a document body.
///
/// Returns the block's parsed fields (when there was one) and the body
/// without it. The single blank line the writer puts between the closing
/// fence and the body is consumed too, so a file this module wrote comes
/// back byte-identical to the body it was built from.
///
/// Every rejection returns the input unchanged, which is the safe
/// direction: the cost of not recognizing a block is a visible stray block
/// in the document, and the cost of recognizing one that is not there is a
/// silently deleted paragraph.
pub fn split(text: &str) -> (Option<FrontMatter>, &str) {
    let Some(rest) = strip_line(text, FENCE) else {
        return (None, text);
    };
    let mut body = rest;
    let mut front = FrontMatter::default();
    // Whether the block declared an id key at all - see the module docs on
    // why an id key, present or empty, is what identifies the block as this
    // CLI's rather than a horizontal rule.
    let mut identified = false;
    for _ in 0..MAX_BLOCK_LINES {
        let (line, after) = next_line(body);
        if line.trim_end() == FENCE || line.trim_end() == END_FENCE {
            if !identified {
                return (None, text);
            }
            // The blank line the writer emits after the fence belongs to
            // the block, not to the document.
            let after = strip_line(after, "").unwrap_or(after);
            return (Some(front), after);
        }
        let Some((key, value)) = entry(line) else {
            // Not a `key: value` line, so this was never a frontmatter
            // block - most likely a document opening with a horizontal
            // rule. Hand back what came in.
            return (None, text);
        };
        identified |= absorb(&mut front, key, value);
        if after.is_empty() {
            return (None, text);
        }
        body = after;
    }
    (None, text)
}

/// Record one recognized key, reporting whether it was an id key.
fn absorb(front: &mut FrontMatter, key: &str, value: &str) -> bool {
    match key {
        ID_KEY => front.id = non_empty(unquote(value)),
        URL_ID_KEY => front.url_id = non_empty(unquote(value)),
        REVISION_KEY => front.revision = unquote(value).parse().ok(),
        _ => return false,
    }
    matches!(key, ID_KEY | URL_ID_KEY)
}

/// `text` unless it is empty.
fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

/// `text` with a leading line equal to `expected` (ignoring a trailing
/// carriage return) removed, or `None` when the first line is something
/// else.
///
/// An `expected` of `""` therefore consumes one blank line.
fn strip_line<'a>(text: &'a str, expected: &str) -> Option<&'a str> {
    let (line, rest) = next_line(text);
    (line.trim_end_matches('\r') == expected).then_some(rest)
}

/// The first line of `text` (without its newline) and everything after it.
fn next_line(text: &str) -> (&str, &str) {
    match text.split_once('\n') {
        Some((line, rest)) => (line, rest),
        None => (text, ""),
    }
}

/// The key and value of one `key: value` line, or `None` when the line is
/// not one.
///
/// A blank line inside a block is allowed and carries nothing, so it comes
/// back as a key that matches nothing. Anything else - prose, a list item,
/// an indented continuation - is what tells [`split`] this is not
/// frontmatter.
fn entry(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_end_matches('\r');
    if line.trim().is_empty() {
        return Some(("", ""));
    }
    // Indentation would mean a nested value, which this module does not
    // write and will not guess at.
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let (key, value) = line.split_once(':')?;
    let usable = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    usable.then(|| (key, value.trim()))
}

/// One YAML scalar as its string value.
///
/// Handles the two quoted forms this module writes or a person would type,
/// and passes a plain scalar through trimmed. Not a YAML parser: an
/// unterminated quote is treated as plain text rather than as an error,
/// because the only thing at stake is one metadata field.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        return value[1..value.len() - 1].replace("''", "'");
    }
    if !(bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"') {
        return value.trim().to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value[1..value.len() - 1].chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => out.extend(escape_code(&mut chars, 4)),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// The character named by a `\u`/`\U` escape's next `width` hex digits.
fn escape_code(chars: &mut std::str::Chars<'_>, width: usize) -> Option<char> {
    let digits: String = chars.by_ref().take(width).collect();
    u32::from_str_radix(&digits, 16)
        .ok()
        .and_then(char::from_u32)
}

/// One string as a double-quoted YAML scalar.
///
/// Control characters become escapes rather than raw bytes: a title holding
/// a newline would otherwise end the line and turn the rest of the title
/// into a key of its own, which is how a document title gets to decide what
/// the block says. Every control character is in the BMP, so the four-digit
/// `\u` form always suffices.
fn quote(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    fn document() -> Value {
        json!({
            "id": "55baa74a-bad1-4b16-a0d0-ec103c656b8e",
            "urlId": "engKBTOaWe",
            "title": "Billing dunning",
            "revision": 15,
            "updatedAt": "2026-08-27T16:17:58.967Z",
            "text": "body",
        })
    }

    #[test]
    fn writes_the_documented_block() {
        assert_eq!(
            block(&document()).unwrap(),
            "---\n\
             outline_id: \"55baa74a-bad1-4b16-a0d0-ec103c656b8e\"\n\
             outline_url_id: \"engKBTOaWe\"\n\
             title: \"Billing dunning\"\n\
             revision: 15\n\
             updated_at: \"2026-08-27T16:17:58.967Z\"\n\
             ---\n"
        );
    }

    #[test]
    fn fields_the_server_did_not_send_are_omitted_not_nulled() {
        let block = block(&json!({ "id": "doc-1" })).unwrap();
        assert_eq!(block, "---\noutline_id: \"doc-1\"\n---\n");
        assert!(!block.contains("null"), "{block:?}");
    }

    #[test]
    fn a_response_with_none_of_the_fields_gets_no_block() {
        assert_eq!(block(&json!({ "text": "body" })), None);
        assert_eq!(block(&json!("not an object")), None);
    }

    #[test]
    fn a_revision_that_is_not_a_number_is_left_out() {
        let block = block(&json!({ "id": "doc-1", "revision": "15" })).unwrap();
        assert!(!block.contains("revision"), "{block:?}");
    }

    #[test]
    fn a_title_cannot_break_out_of_the_block() {
        // The attack this quoting exists for: a title that ends the line
        // and writes a key of its own.
        let block = block(&json!({
            "id": "doc-1",
            "title": "evil\noutline_id: \"other\"",
        }))
        .unwrap();
        let file = format!("{block}\nbody\n");
        let (front, body) = split(&file);
        let front = front.unwrap();
        assert_eq!(front.id.as_deref(), Some("doc-1"));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn a_title_holding_quotes_backslashes_and_controls_round_trips() {
        for title in [
            "a \"quoted\" title",
            "back\\slash",
            "tab\there",
            "bell\u{7}",
            "- leading dash",
            "null",
            "12345",
            "colon: inside",
        ] {
            let block = block(&json!({ "id": "doc-1", "title": title })).unwrap();
            let value = block
                .lines()
                .find_map(|line| line.strip_prefix("title: "))
                .unwrap();
            assert_eq!(unquote(value), title, "{title:?} became {value:?}");
        }
    }

    #[test]
    fn what_export_writes_is_what_a_write_strips() {
        let block = block(&document()).unwrap();
        let body = "# Billing dunning\n\nbody\n";
        let file = format!("{block}\n{body}");
        let (front, stripped) = split(&file);
        let front = front.unwrap();
        assert_eq!(
            front.id.as_deref(),
            Some("55baa74a-bad1-4b16-a0d0-ec103c656b8e")
        );
        assert_eq!(front.url_id.as_deref(), Some("engKBTOaWe"));
        assert_eq!(front.revision, Some(15));
        // Byte-identical to the body the block was written in front of.
        assert_eq!(stripped, body);
    }

    #[test]
    fn a_document_opening_with_a_horizontal_rule_is_left_alone() {
        for text in [
            // A rule, then prose - no closing fence at all.
            "---\n\nSome prose about the thing.\n",
            // Two rules with prose between them: fenced on both sides, but
            // the inside is not `key: value`.
            "---\nSome prose.\n---\n\nMore.\n",
            // A list, which a sloppier check would read as keys.
            "---\n- one\n- two\n---\n",
            // Indented, so a nested value this module never writes.
            "---\n  key: value\n---\n",
            // The dangerous one: prose that HAPPENS to hold a colon, so it
            // parses as `key: value` and is fenced on both sides. Only the
            // absence of an outline id keeps this paragraph in the
            // document.
            "---\nNote: this is a rule, not metadata\n---\n\nMore.\n",
            // Another tool's frontmatter. Not ours to remove.
            "---\ntitle: Notes\ntags: a, b\n---\n\nbody\n",
        ] {
            let (front, body) = split(text);
            assert_eq!(front, None, "{text:?} was taken as frontmatter");
            assert_eq!(body, text);
        }
    }

    #[test]
    fn an_id_key_is_what_identifies_the_block_even_when_it_is_empty() {
        // The marker is the KEY, not a usable value: a hand-blanked
        // outline_id still says "this block came from otl".
        let (front, body) = split("---\noutline_id: \"\"\ntitle: Notes\n---\n\nbody\n");
        assert_eq!(front.unwrap().id, None);
        assert_eq!(body, "body\n");
    }

    #[test]
    fn the_short_id_alone_also_identifies_the_block() {
        let (front, body) = split("---\noutline_url_id: engKBTOaWe\n---\n\nbody\n");
        assert_eq!(front.unwrap().url_id.as_deref(), Some("engKBTOaWe"));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn an_unclosed_block_is_left_alone_rather_than_swallowing_the_document() {
        let text = format!("---\nkey: value\n{}", "prose\n".repeat(200));
        let (front, body) = split(&text);
        assert_eq!(front, None);
        assert_eq!(body, text);
    }

    #[test]
    fn a_block_longer_than_the_cap_is_left_alone() {
        let keys = (0..MAX_BLOCK_LINES + 2)
            .map(|n| format!("k{n}: v"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = format!("---\n{keys}\n---\n\nbody\n");
        assert_eq!(split(&text), (None, text.as_str()));
    }

    #[test]
    fn unknown_keys_are_removed_from_the_body_and_otherwise_ignored() {
        let (front, body) = split("---\nauthor: someone\noutline_id: doc-1\n---\n\nbody\n");
        assert_eq!(front.unwrap().id.as_deref(), Some("doc-1"));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn blank_lines_inside_a_block_are_allowed() {
        let (front, body) = split("---\noutline_id: doc-1\n\nrevision: 3\n---\nbody\n");
        let front = front.unwrap();
        assert_eq!(front.id.as_deref(), Some("doc-1"));
        assert_eq!(front.revision, Some(3));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn crlf_files_are_handled() {
        let (front, body) = split("---\r\noutline_id: doc-1\r\n---\r\n\r\nbody\r\n");
        assert_eq!(front.unwrap().id.as_deref(), Some("doc-1"));
        assert_eq!(body, "body\r\n");
    }

    #[test]
    fn either_spelling_of_the_id_names_the_document() {
        let front = FrontMatter {
            id: Some("55baa74a".to_string()),
            url_id: Some("engKBTOaWe".to_string()),
            revision: None,
        };
        assert!(front.names("55baa74a"));
        // A caller who copied the short id out of the URL is not in
        // conflict with the UUID in the file.
        assert!(front.names("engKBTOaWe"));
        assert!(!front.names("something-else"));
        assert_eq!(front.document_id(), Some("55baa74a"));
    }

    #[test]
    fn a_block_naming_no_id_contradicts_nothing() {
        assert!(FrontMatter::default().names("anything"));
        assert_eq!(FrontMatter::default().document_id(), None);
    }

    #[test]
    fn a_url_id_alone_is_usable_as_the_document_id() {
        let front = FrontMatter {
            id: None,
            url_id: Some("engKBTOaWe".to_string()),
            revision: None,
        };
        assert_eq!(front.document_id(), Some("engKBTOaWe"));
    }

    #[test]
    fn an_unquoted_value_is_read_too() {
        // Written by hand, or by a tool that does not quote.
        let (front, _) = split("---\noutline_id: doc-1\nrevision: 7\n---\n");
        let front = front.unwrap();
        assert_eq!(front.id.as_deref(), Some("doc-1"));
        assert_eq!(front.revision, Some(7));
    }

    #[test]
    fn an_empty_id_reads_as_absent_rather_than_as_an_empty_document_id() {
        let (front, _) = split("---\noutline_id: \"\"\n---\n");
        assert_eq!(front.unwrap().id, None);
    }

    #[test]
    fn a_body_that_is_only_a_block_leaves_nothing_behind() {
        let (front, body) = split("---\noutline_id: doc-1\n---\n");
        assert_eq!(front.unwrap().id.as_deref(), Some("doc-1"));
        assert_eq!(body, "");
    }
}
