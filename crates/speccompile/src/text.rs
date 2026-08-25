//! Text rules for strings that come out of an untrusted document.
//!
//! Everything compiled into the IR is eventually printed: summaries by
//! `api list`, content types and parameter names by validation errors,
//! enum values by "allowed values are ..." diagnostics. A terminal reads
//! some of those bytes as commands - `ESC [ ... m` recolours, `ESC ] 52 ; ...`
//! writes the clipboard, a bare `\r` rewrites the line the user just read,
//! and a `\n` or `\t` forges a row in a line-oriented output format.
//!
//! So text from a document is treated as data, never as terminal input:
//!
//! - display-only text (a summary) is SANITIZED - dangerous characters are
//!   dropped and the length is capped, which cannot break anything because
//!   nothing dispatches on it;
//! - text with meaning (parameter names, content types, formats, enum
//!   values) is VALIDATED - it is compared against user input or sent on
//!   the wire, so silently rewriting it would change behaviour. A document
//!   that carries control characters there is rejected as a whole.
//!
//! Lengths are capped as well: a single 9 MiB summary is a denial of
//! service against both the terminal and the IR cache.

/// Longest summary kept, in characters (not bytes).
pub const MAX_SUMMARY_CHARS: usize = 200;
/// The same limit expressed in bytes, for validating a summary that was
/// already compiled (a cached one): UTF-8 needs up to four bytes per
/// character, so this is the widest a legitimate summary can be.
pub const MAX_SUMMARY_BYTES: usize = MAX_SUMMARY_CHARS * 4;
/// Longest accepted request-body parameter name, in bytes.
pub const MAX_PARAM_NAME_BYTES: usize = 128;
/// Longest accepted request content type, in bytes.
pub const MAX_CONTENT_TYPE_BYTES: usize = 128;
/// Longest accepted `format` value, in bytes.
pub const MAX_FORMAT_BYTES: usize = 64;
/// Longest accepted single enum value, in bytes.
pub const MAX_ENUM_VALUE_BYTES: usize = 256;
/// Most enum values kept for one parameter.
///
/// Every one of them can end up in a single "allowed values" diagnostic,
/// so an unbounded enum is a terminal flood; it is also unbounded IR.
pub const MAX_ENUM_VALUES: usize = 256;

/// Whether a character must never reach a terminal verbatim.
///
/// `char::is_control` covers the Unicode `Cc` category: the C0 range
/// (including `\n`, `\r`, `\t`, `NUL` and `ESC`), `DEL`, and the C1 range.
/// The rest are the characters that move text around without being
/// controls: the Unicode line/paragraph separators, and the bidi overrides
/// and isolates that can visually reorder a line (making `documents.info`
/// read as something else).
fn is_dangerous(character: char) -> bool {
    character.is_control()
        || matches!(character, '\u{2028}' | '\u{2029}' | '\u{feff}')
        || ('\u{202a}'..='\u{202e}').contains(&character)
        || ('\u{2066}'..='\u{2069}').contains(&character)
}

/// Whether `text` may be printed verbatim: no dangerous characters and no
/// longer than `max_bytes`.
pub fn is_display_safe(text: &str, max_bytes: usize) -> bool {
    text.len() <= max_bytes && !text.chars().any(is_dangerous)
}

/// Drop dangerous characters and cap the length, for display-only text.
///
/// Collapses runs of whitespace to single spaces on the way (a summary is
/// one line) and trims the result, so a document cannot pad output with
/// leading blanks either.
pub fn sanitize_display(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|character| !is_dangerous(*character))
        .take(MAX_SUMMARY_CHARS)
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_drops_terminal_control_sequences() {
        let hostile = "ok \u{1b}]52;c;cGFzcwo=\u{7} \u{1b}[31mred\u{1b}[0m";
        let clean = sanitize_display(hostile);
        assert!(!clean.contains('\u{1b}'), "{clean:?}");
        assert!(!clean.contains('\u{7}'), "{clean:?}");
        assert!(clean.contains("red"), "{clean:?}");
        assert!(is_display_safe(&clean, MAX_SUMMARY_CHARS));
    }

    #[test]
    fn sanitize_drops_row_forging_characters() {
        for hostile in [
            "a\nb.op\tforged",
            "a\rb",
            "a\u{2028}b",
            "a\u{0}b",
            "a\u{202e}b",
            "a\u{feff}b",
        ] {
            let clean = sanitize_display(hostile);
            assert!(
                is_display_safe(&clean, MAX_SUMMARY_CHARS),
                "{hostile:?} -> {clean:?}"
            );
            assert!(!clean.contains('\n') && !clean.contains('\t'), "{clean:?}");
        }
    }

    #[test]
    fn sanitize_caps_the_length() {
        let long = "x".repeat(9 * 1024 * 1024);
        let clean = sanitize_display(&long);
        assert_eq!(clean.chars().count(), MAX_SUMMARY_CHARS);
    }

    #[test]
    fn sanitize_collapses_whitespace_and_trims() {
        assert_eq!(sanitize_display("  a   b  "), "a b");
    }

    #[test]
    fn display_safe_rejects_controls_and_overlong_text() {
        assert!(is_display_safe("plain text", 32));
        assert!(is_display_safe("naïve über 中文", 64));
        assert!(!is_display_safe("with\u{1b}escape", 32));
        assert!(!is_display_safe("with\nnewline", 32));
        assert!(!is_display_safe("too long", 3));
    }
}
