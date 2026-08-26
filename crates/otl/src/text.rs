//! Server-provided text that has to be safe to print or to name a file
//! with.
//!
//! Three places need the same judgement about which characters are content
//! and which are instructions - document titles in a diagnostic, document
//! titles as file names, and URL paths handed to a browser - so the
//! judgement lives here once instead of being re-derived (and drifting) at
//! each of them.

/// Ranges of the Unicode `Cf` (format) category, plus SOFT HYPHEN.
///
/// These characters are invisible or re-order the text around them. They are
/// NOT `char::is_control`, which is exactly why they keep being missed: an
/// earlier hand-picked list here named U+202E and U+2066..U+2069 while
/// quietly letting U+061C, U+06DD, U+070F, U+0600..U+0605 and the whole
/// U+13430 block through.
///
/// The table is therefore the CATEGORY, not a selection from it: every
/// assigned `Cf` codepoint as of Unicode 15.1. None of them has a
/// legitimate use in a file name or a URL path, and in a diagnostic line
/// they can make the printed text read as something other than what it is.
///
/// Deliberately NOT applied to document bodies: `otl docs view` keeps them,
/// because bidirectional marks are ordinary content in Arabic, Hebrew and
/// Indic prose and the body is the thing the user asked to read.
const FORMAT_RANGES: &[(char, char)] = &[
    ('\u{00ad}', '\u{00ad}'),   // SOFT HYPHEN
    ('\u{0600}', '\u{0605}'),   // ARABIC NUMBER SIGN .. ARABIC NUMBER MARK ABOVE
    ('\u{061c}', '\u{061c}'),   // ARABIC LETTER MARK
    ('\u{06dd}', '\u{06dd}'),   // ARABIC END OF AYAH
    ('\u{070f}', '\u{070f}'),   // SYRIAC ABBREVIATION MARK
    ('\u{0890}', '\u{0891}'),   // ARABIC POUND / PIASTRE MARK ABOVE
    ('\u{08e2}', '\u{08e2}'),   // ARABIC DISPUTED END OF AYAH
    ('\u{180e}', '\u{180e}'),   // MONGOLIAN VOWEL SEPARATOR
    ('\u{200b}', '\u{200f}'),   // ZERO WIDTH SPACE .. RIGHT-TO-LEFT MARK
    ('\u{202a}', '\u{202e}'),   // embedding and override controls
    ('\u{2060}', '\u{2064}'),   // WORD JOINER .. INVISIBLE PLUS
    ('\u{2066}', '\u{206f}'),   // isolates and the deprecated format controls
    ('\u{feff}', '\u{feff}'),   // ZERO WIDTH NO-BREAK SPACE (BOM)
    ('\u{fff9}', '\u{fffb}'),   // interlinear annotation controls
    ('\u{110bd}', '\u{110bd}'), // KAITHI NUMBER SIGN
    ('\u{110cd}', '\u{110cd}'), // KAITHI NUMBER SIGN ABOVE
    ('\u{13430}', '\u{1343f}'), // Egyptian hieroglyph format controls
    ('\u{1bca0}', '\u{1bca3}'), // shorthand format controls
    ('\u{1d173}', '\u{1d17a}'), // musical format controls
    ('\u{e0001}', '\u{e0001}'), // LANGUAGE TAG (deprecated)
    ('\u{e0020}', '\u{e007f}'), // tag characters
];

/// Maximum length, in characters, of a piece of server text quoted into a
/// diagnostic line.
const MAX_QUOTED_CHARS: usize = 80;

/// Marker appended when quoted text was cut short.
const ELLIPSIS: char = '\u{2026}';

/// Whether `c` is an invisible or text-reordering formatting character.
pub fn is_invisible(c: char) -> bool {
    FORMAT_RANGES
        .iter()
        .any(|(first, last)| c >= *first && c <= *last)
}

/// Make server-provided text safe to embed in a diagnostic line.
///
/// A document title reaches stderr whenever the CLI has to talk about a
/// document, and a title is written by anyone who can edit that document.
/// Three things are taken away:
///
/// - control characters, which a terminal executes rather than prints (the
///   same reasoning as [`crate::pager`]: `ESC ] 52` sets the clipboard);
/// - newlines specifically, because a diagnostic is line-oriented and a
///   title containing one could forge an extra failure line;
/// - invisible and reordering formatting characters, which would let the
///   quoted title misrepresent itself.
///
/// The result is also length-bounded: a title can be long, and a summary
/// listing twenty of them should stay readable.
pub fn quote(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
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
    use super::*;

    #[test]
    fn the_format_ranges_are_ordered_and_well_formed() {
        // A reversed range silently matches nothing; an unordered table is
        // a sign that an edit landed in the wrong place.
        let mut previous: Option<char> = None;
        for (first, last) in FORMAT_RANGES {
            assert!(first <= last, "reversed range {first:?}..{last:?}");
            if let Some(previous) = previous {
                assert!(previous < *first, "unordered range at {first:?}");
            }
            previous = Some(*last);
        }
    }

    #[test]
    fn covers_the_format_characters_a_hand_picked_list_kept_missing() {
        for c in [
            '\u{0600}',
            '\u{0605}',
            '\u{061c}',
            '\u{06dd}',
            '\u{070f}',
            '\u{0890}',
            '\u{0891}',
            '\u{08e2}',
            '\u{110bd}',
            '\u{110cd}',
            '\u{13430}',
            '\u{1343f}',
            '\u{1bca0}',
            '\u{1bca3}',
        ] {
            assert!(is_invisible(c), "missed U+{:04X}", c as u32);
        }
    }

    #[test]
    fn covers_the_ones_that_were_already_listed() {
        for c in [
            '\u{00ad}',
            '\u{200b}',
            '\u{200f}',
            '\u{202e}',
            '\u{2060}',
            '\u{2066}',
            '\u{206f}',
            '\u{feff}',
            '\u{fff9}',
            '\u{1d173}',
            '\u{e0001}',
            '\u{e0041}',
        ] {
            assert!(is_invisible(c), "missed U+{:04X}", c as u32);
        }
    }

    #[test]
    fn ordinary_text_is_not_treated_as_formatting() {
        for c in [
            'a',
            'Z',
            '0',
            ' ',
            '-',
            '\u{4e2d}',
            '\u{e9}',
            '\u{5d0}',
            '\u{1f600}',
        ] {
            assert!(!is_invisible(c), "rejected U+{:04X}", c as u32);
        }
        // Just outside two ranges.
        assert!(!is_invisible('\u{2010}'));
        assert!(!is_invisible('\u{2065}'));
        assert!(!is_invisible('\u{05ff}'));
    }

    #[test]
    fn quoting_neutralizes_terminal_control_sequences() {
        // OSC 52 sets the system clipboard from a document title.
        let quoted = quote("before\u{1b}]52;c;cGF5bG9hZA==\u{7}after");
        assert!(!quoted.contains('\u{1b}'), "{quoted:?}");
        assert!(!quoted.contains('\u{7}'), "{quoted:?}");
    }

    #[test]
    fn quoting_keeps_a_title_on_one_line() {
        // A newline would let a title forge an extra failure entry.
        let quoted = quote("real title\n  fake-id: forged failure");
        assert!(!quoted.contains('\n'), "{quoted:?}");
        assert!(quoted.starts_with("real title"), "{quoted:?}");
    }

    #[test]
    fn quoting_drops_reordering_characters() {
        let quoted = quote("report-\u{202e}fdp");
        assert_eq!(quoted, "report-fdp");
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
