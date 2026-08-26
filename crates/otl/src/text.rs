//! One classification of characters that are unsafe to print, shared by
//! every surface that prints them.
//!
//! Three surfaces emit text the user did not write: configuration
//! diagnostics (a profile name, a path), table cells (a document title from
//! the server) and completion descriptions (an operation summary). Each was
//! given its own filter, and each round of review found one of them behind
//! the others - control characters everywhere, then bidi and zero-width
//! characters in the diagnostics only, while the same override reaching a
//! table cell reversed the rest of the row.
//!
//! So the CLASSIFICATION lives here once, as an exhaustive enum. A surface
//! chooses how to render each hazard, because the right answer differs -
//! a diagnostic wants a visible marker, a data cell wants honest width - but
//! no surface can be unaware of a category, because [`hazard`] returns one
//! and the caller has to match on it.

/// A reason a character must not be forwarded to a terminal verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hazard {
    /// C0/C1 control character: ESC introduces an escape sequence, BEL and
    /// the OSC terminators drive the terminal, and a newline forges a line
    /// of output that looks like ours.
    Control,
    /// A bidirectional embedding, override, isolate or mark.
    ///
    /// These have SCOPE: an unterminated `U+202E` reverses the visual order
    /// of everything after it, which in a table means the rest of the row,
    /// not just the cell that carried it. `U+200E`/`U+200F` are unscoped but
    /// still reorder the neutral characters around them, across a cell
    /// boundary just as happily.
    BidiFormat,
    /// A zero-width or otherwise invisible character.
    ///
    /// Occupies no column, so it can hide inside a value, pad a name to
    /// evade a comparison, or make two different strings render identically.
    Invisible,
    /// A zero-width joiner or non-joiner.
    ///
    /// Invisible in the same way, but unlike the rest it carries MEANING in
    /// ordinary text: `U+200D` is what makes an emoji ligature one glyph
    /// (the family emoji is six codepoints joined by three of them), and
    /// `U+200C` is required to spell Persian and Hindi words correctly.
    /// Dropping it from data would corrupt the text; keeping it in a name
    /// being compared or quoted would let two different names look alike.
    /// Its own category, so each surface answers that separately.
    Joiner,
}

/// Classify a character, or `None` when it is safe to print.
pub fn hazard(c: char) -> Option<Hazard> {
    if c.is_control() {
        return Some(Hazard::Control);
    }
    match c {
        // Marks, embeddings, overrides and isolates.
        '\u{061c}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}' => Some(Hazard::BidiFormat),
        // Meaningful in text, invisible on its own.
        '\u{200c}' | '\u{200d}' => Some(Hazard::Joiner),
        // Zero-width and other invisible formatting.
        '\u{00ad}' | '\u{180e}' | '\u{200b}' | '\u{2060}'..='\u{2064}' | '\u{feff}' => {
            Some(Hazard::Invisible)
        }
        _ => None,
    }
}

/// Whether a string contains anything that must not be printed verbatim.
pub fn has_hazard(text: &str) -> bool {
    text.chars().any(|c| hazard(c).is_some())
}

#[cfg(test)]
mod tests {
    use super::{hazard, Hazard};

    #[test]
    fn every_category_is_recognized() {
        assert_eq!(hazard('\u{1b}'), Some(Hazard::Control));
        assert_eq!(hazard('\n'), Some(Hazard::Control));
        assert_eq!(hazard('\u{7}'), Some(Hazard::Control));
        for c in [
            '\u{202e}', '\u{202a}', '\u{2066}', '\u{2069}', '\u{200f}', '\u{61c}',
        ] {
            assert_eq!(hazard(c), Some(Hazard::BidiFormat), "U+{:04X}", c as u32);
        }
        for c in ['\u{200b}', '\u{feff}', '\u{00ad}', '\u{2060}'] {
            assert_eq!(hazard(c), Some(Hazard::Invisible), "U+{:04X}", c as u32);
        }
        for c in ['\u{200c}', '\u{200d}'] {
            assert_eq!(hazard(c), Some(Hazard::Joiner), "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn a_joiner_is_its_own_category_because_it_carries_meaning() {
        // The family emoji is six codepoints held together by three ZWJs;
        // dropping them would render four separate people. A surface that
        // shows DATA has to keep them, and one that shows a NAME being
        // compared should not.
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
        assert_eq!(
            family
                .chars()
                .filter(|c| hazard(*c) == Some(Hazard::Joiner))
                .count(),
            3
        );
    }

    #[test]
    fn ordinary_text_is_not_a_hazard() {
        for c in ['a', ' ', '中', '😀', 'מ', 'ا', '\u{301}'] {
            assert_eq!(hazard(c), None, "U+{:04X}", c as u32);
        }
    }
}
