//! One classification of characters that are unsafe to print verbatim.
//!
//! Lives in the engine because BOTH layers need the same answer. The engine
//! scrubs text a server sent before it reaches stderr; the CLI scrubs text a
//! server, a config file or a filesystem produced before it reaches a table
//! cell, a completion description or a diagnostic. Two tables drift, and
//! this one already has: the CLI's list was found short of `U+202E`, then of
//! `U+061C` and `U+206A..U+206F`, then of `U+070F`, `U+0600..U+0605` and the
//! `U+13430` block - while the engine was meanwhile classifying by RENDERED
//! WIDTH, which keeps 27 of those same codepoints because a terminal gives
//! them a column.
//!
//! Nothing here is protocol-specific: it is a function from `char` to a
//! category, with no notion of what the text describes.
//!
//! # The table is the whole `Cf` category, not a selection from it
//!
//! A partial list reads as complete, so the entries below cover every
//! assigned `General_Category=Cf` codepoint (Unicode 15.1) plus `U+00AD` and
//! `U+180E`. [`tests::every_format_character_is_classified`] pins that by
//! enumerating the ranges rather than by sampling them.
//!
//! # What it does NOT decide
//!
//! How to RENDER a hazard is the caller's, because the right answer differs:
//! a diagnostic wants the character gone, a data cell wants honest width, and
//! a payload (`--json`) wants the server's bytes untouched. [`hazard`]
//! returns a category so that a caller has to match on it and cannot be
//! unaware of one.

/// A reason a character must not be forwarded to a terminal verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hazard {
    /// C0/C1 control character: ESC introduces an escape sequence, BEL and
    /// the OSC terminators drive the terminal, and a newline forges a line
    /// of output that looks like ours.
    Control,
    /// A bidirectional embedding, override, isolate or mark, or one of the
    /// deprecated shaping controls.
    ///
    /// These have SCOPE: an unterminated `U+202E` reverses the visual order
    /// of everything after it, which in a table means the rest of the row,
    /// not just the cell that carried it. `U+200E`/`U+200F` are unscoped but
    /// still reorder the neutral characters around them, across a cell
    /// boundary just as happily. The `U+206A..U+206F` block is deprecated
    /// but still changes how the text after it is shaped.
    BidiFormat,
    /// A zero-width or otherwise invisible character.
    ///
    /// Occupies no column, so it can hide inside a value, pad a name to
    /// evade a comparison, or make two different strings render identically.
    Invisible,
    /// Invisible, but semantically required by the text it sits in.
    ///
    /// Invisible in the same way as the category above, but unlike the rest
    /// these carry MEANING in ordinary text, so a surface showing DATA has
    /// to keep them while a surface showing a NAME being compared or quoted
    /// should not:
    ///
    /// - `U+200D` is what makes an emoji ligature one glyph (the family
    ///   emoji is six codepoints joined by three of them) and `U+200C` is
    ///   required to spell Persian and Hindi words correctly;
    /// - the tag block `U+E0020..U+E007F` spells out the subdivision in a
    ///   flag emoji - dropping it turns the Scotland flag into a black one.
    ///
    /// The name is historical: joiners were the first members. What the
    /// category means is the sentence above, and membership follows from
    /// that rather than from the word.
    Joiner,
}

/// Classify a character, or `None` when it is safe to print.
pub fn hazard(c: char) -> Option<Hazard> {
    if c.is_control() {
        return Some(Hazard::Control);
    }
    match c {
        // Marks, embeddings, overrides, isolates, and the deprecated
        // shaping controls that still affect the text after them.
        '\u{061c}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}'
        | '\u{206a}'..='\u{206f}' => Some(Hazard::BidiFormat),
        // Meaningful in text, invisible on its own.
        '\u{200c}' | '\u{200d}' | '\u{e0020}'..='\u{e007f}' => Some(Hazard::Joiner),
        // Zero-width and other invisible formatting.
        '\u{00ad}'
        | '\u{0600}'..='\u{0605}'
        | '\u{06dd}'
        | '\u{070f}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08e2}'
        | '\u{180e}'
        | '\u{200b}'
        | '\u{2060}'..='\u{2064}'
        | '\u{feff}'
        | '\u{fff9}'..='\u{fffb}'
        | '\u{110bd}'
        | '\u{110cd}'
        | '\u{13430}'..='\u{1343f}'
        | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}'
        | '\u{e0001}' => Some(Hazard::Invisible),
        _ => None,
    }
}

/// Whether a string contains anything that must not be printed verbatim.
pub fn has_hazard(text: &str) -> bool {
    text.chars().any(|c| hazard(c).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_flag_subdivision_is_in_the_same_category_as_a_joiner() {
        // The Scotland flag is a black flag plus `gbsct` spelled in tag
        // characters and a terminator. They are invisible, and they are the
        // data - so they belong with the joiners rather than with the
        // characters a data cell may drop.
        let scotland = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}";
        assert_eq!(
            scotland
                .chars()
                .filter(|c| hazard(*c) == Some(Hazard::Joiner))
                .count(),
            6
        );
    }

    #[test]
    fn every_format_character_is_classified() {
        // The list has been found short three times. These are every
        // assigned `General_Category=Cf` codepoint plus U+00AD and U+180E,
        // so a fourth omission fails here instead of in a review.
        let ranges: &[(u32, u32)] = &[
            (0x00ad, 0x00ad),
            (0x0600, 0x0605),
            (0x061c, 0x061c),
            (0x06dd, 0x06dd),
            (0x070f, 0x070f),
            (0x0890, 0x0891),
            (0x08e2, 0x08e2),
            (0x180e, 0x180e),
            (0x200b, 0x200f),
            (0x202a, 0x202e),
            (0x2060, 0x2064),
            (0x2066, 0x206f),
            (0xfeff, 0xfeff),
            (0xfff9, 0xfffb),
            (0x110bd, 0x110bd),
            (0x110cd, 0x110cd),
            (0x13430, 0x1343f),
            (0x1bca0, 0x1bca3),
            (0x1d173, 0x1d17a),
            (0xe0001, 0xe0001),
            (0xe0020, 0xe007f),
        ];
        for (first, last) in ranges {
            for code in *first..=*last {
                let Some(c) = char::from_u32(code) else {
                    panic!("U+{code:04X} is not a scalar value");
                };
                assert!(
                    hazard(c).is_some(),
                    "U+{code:04X} is a format character but was classified as safe"
                );
            }
        }
        // And the specific ones earlier reviews named as missing.
        for c in ['\u{070f}', '\u{1bca0}', '\u{0600}', '\u{13430}', '\u{206a}'] {
            assert!(hazard(c).is_some(), "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn ordinary_text_is_not_a_hazard() {
        for c in [
            'a',
            ' ',
            '\u{4e2d}',
            '\u{1f600}',
            '\u{5de}',
            '\u{627}',
            '\u{301}',
        ] {
            assert_eq!(hazard(c), None, "U+{:04X}", c as u32);
        }
        // Just outside several ranges.
        for c in ['\u{2010}', '\u{2065}', '\u{05ff}', '\u{0892}'] {
            assert_eq!(hazard(c), None, "U+{:04X}", c as u32);
        }
        assert!(!has_hazard("a plain title"));
        assert!(has_hazard("a \u{202e}title"));
    }
}
