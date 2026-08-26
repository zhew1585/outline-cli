//! One classification of characters that are unsafe to print, shared by
//! every surface that RENDERS text for a human to read.
//!
//! Four such surfaces emit text the user did not write: configuration
//! diagnostics (a profile name, a path), table cells (a document title from
//! the server), completion descriptions (an operation summary) and the
//! diagnostic stream itself (a document id, a file name, an error from the
//! server). Each was given its own filter, and each round of review found
//! one of them behind the others - control characters everywhere, then bidi
//! and zero-width characters in the diagnostics only, while the same
//! override reaching a table cell reversed the rest of the row; then, once
//! all of those were fixed, a newly added message interpolated a file name
//! with no filter at all.
//!
//! So the CLASSIFICATION lives here once, as an exhaustive enum. A surface
//! chooses how to render each hazard, because the right answer differs -
//! a diagnostic wants a visible marker, a data cell wants honest width - but
//! no surface can be unaware of a category, because [`hazard`] returns one
//! and the caller has to match on it.
//!
//! # The table is the whole `Cf` category, and it lives in the engine
//!
//! Reviews found the hand-picked version of this list short three times
//! running: first `U+202E`, then `U+061C` and the `U+206A..U+206F` block,
//! then `U+070F`, `U+0600..U+0605` and the `U+13430` block. A partial list
//! reads as complete, so the surviving table covers every assigned
//! `General_Category=Cf` codepoint (Unicode 15.1) plus `U+00AD` and
//! `U+180E`.
//!
//! That table is [`engine::text`], not this module, because the engine
//! scrubs server text on its way to stderr and needs the same answer. It
//! was reaching a DIFFERENT one - it classified by rendered width, so the
//! 27 `Cf` codepoints a terminal gives a column to survived its scrub while
//! this module dropped them. One table cannot disagree with itself.
//!
//! # `--json` is a deliberate exemption
//!
//! JSON output is not a rendering, it is the payload: its contract is that
//! `jq` can consume it and that it round-trips to the same value the server
//! sent. Substituting or dropping a codepoint there would corrupt data to
//! protect a terminal that was not the intended consumer, and would break
//! the round-trip property `render_golden`'s JSON test asserts. So `--json`
//! (and the non-TTY default, which is the same path) emits exactly what
//! arrived, bidi and all.
//!
//! The consequence is worth stating rather than leaving implied: piping
//! `--json` through a pager or `cat` on a terminal can still show reordered
//! text, because the bytes are the server's. A reader who wants the
//! protected form is looking at the table, which is what a TTY gets by
//! default. `json_mode_is_exempt_from_hazard_scrubbing` pins this as a
//! decision rather than an oversight.

/// Maximum length, in characters, of a piece of foreign text quoted into a
/// diagnostic line.
const MAX_QUOTED_CHARS: usize = 80;

/// Marker appended when quoted text was cut short.
const ELLIPSIS: char = '\u{2026}';

/// The hazard classification, and the classifier itself.
///
/// Re-exported rather than defined here: it moved down into the engine so
/// that the engine's own scrub of server text and this crate's four
/// rendering surfaces answer from ONE table. They did not, and the engine's
/// half was the weaker of the two - it classified by rendered width, which
/// keeps every `Cf` codepoint a terminal gives a column to.
///
/// Callers keep using `crate::text::{hazard, Hazard}`; only the definition
/// moved.
pub use engine::text::{has_hazard, hazard, Hazard};

/// Make foreign text safe to embed in ONE line of a diagnostic.
///
/// A document title, a document id or a file name reaches stderr whenever
/// the CLI has to talk about it, and all three are written by someone other
/// than the person reading the message. Every hazard is therefore removed
/// rather than marked: a diagnostic is prose, and a marker in the middle of
/// a quoted name is noise.
///
/// Newlines go too. That is the difference between this and the scrub in
/// [`crate::stdio`], which keeps them: a diagnostic legitimately spans
/// lines, but a single quoted VALUE inside one must not be able to add a
/// line and pose as another entry in a list. The two are layers - this one
/// is precise about a particular value, that one bounds the damage of a
/// message that forgets to call this.
///
/// The result is also length-bounded: a title can be long, and a summary
/// listing twenty of them should stay readable.
pub fn quote(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter_map(|c| match hazard(c) {
            None => Some(c),
            // Prose, so a removed character needs no marker - but it must
            // not silently fuse the words around it either.
            Some(Hazard::Control) => Some(' '),
            Some(Hazard::BidiFormat | Hazard::Invisible | Hazard::Joiner) => None,
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= MAX_QUOTED_CHARS {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(MAX_QUOTED_CHARS).collect();
    out.push(ELLIPSIS);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{quote, ELLIPSIS, MAX_QUOTED_CHARS};

    #[test]
    fn quoting_neutralizes_terminal_control_sequences() {
        // OSC 52 sets the system clipboard from a document title.
        let quoted = quote("before\u{1b}]52;c;cGF5bG9hZA==\u{7}after");
        assert!(!quoted.contains('\u{1b}'), "{quoted:?}");
        assert!(!quoted.contains('\u{7}'), "{quoted:?}");
    }

    #[test]
    fn quoting_keeps_a_value_on_one_line() {
        // A newline would let a title forge an extra failure entry in the
        // list it appears in.
        let quoted = quote("real title\n  fake-id: forged failure");
        assert!(!quoted.contains('\n'), "{quoted:?}");
        assert!(quoted.starts_with("real title"), "{quoted:?}");
    }

    #[test]
    fn quoting_drops_every_invisible_category() {
        assert_eq!(quote("report-\u{202e}fdp"), "report-fdp");
        assert_eq!(quote("a\u{200b}b"), "ab");
        assert_eq!(quote("a\u{200d}b"), "ab");
        assert_eq!(quote("a\u{e0067}b"), "ab");
        assert_eq!(quote("a\u{070f}b"), "ab");
    }

    #[test]
    fn quoting_bounds_the_length() {
        let quoted = quote(&"a".repeat(500));
        assert_eq!(quoted.chars().count(), MAX_QUOTED_CHARS + 1);
        assert!(quoted.ends_with(ELLIPSIS));
    }

    #[test]
    fn quoting_leaves_ordinary_titles_alone() {
        assert_eq!(quote("Deploy runbook"), "Deploy runbook");
        assert_eq!(quote("  padded  "), "padded");
        assert_eq!(quote(""), "");
        assert_eq!(
            quote("\u{4e2d}\u{6587}\u{6807}\u{9898}"),
            "\u{4e2d}\u{6587}\u{6807}\u{9898}"
        );
    }
}
