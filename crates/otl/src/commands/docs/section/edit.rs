//! Expressing "this span becomes this text" as the smallest safe write.
//!
//! # Why widening can give up, and what happens then
//!
//! A body can defeat the search for a unique anchor: a section repeated
//! verbatim in a document that is itself repetitive may need most of the
//! page before it is unique, at which point `findText` plus its replacement
//! is larger than the new body alone. [`Edit::Replace`] is the answer, and
//! it is not a fallback in the sense of being worse - it is the same write,
//! expressed the other way, and `lastRevision` makes it exactly as safe.
//! Only the number of bytes on the wire differs, and the caller is told
//! which one it got.

use std::ops::Range;

use super::parse::Heading;

/// How far the anchor may grow before [`Edit::Replace`] is preferred.
///
/// Each step is one line in one direction. Forty covers every real
/// repetition (a duplicated heading, a boilerplate table) while keeping the
/// pathological case from walking a 1.5 MB body a line at a time.
const MAX_WIDENING_STEPS: usize = 40;

/// How a section edit is expressed to `documents.update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// `editMode=patch`: only the anchor and its replacement travel.
    ///
    /// `find_text` is guaranteed to occur exactly once in the body it was
    /// computed from, which is what makes the server's string match
    /// unambiguous.
    Patch {
        find_text: String,
        replacement: String,
    },
    /// The whole new body, when no anchor short of the document was unique.
    Replace { text: String },
}

/// Replace one section's text, heading line included.
///
/// The heading travels with the body deliberately: it is what makes
/// renaming a heading, or changing its level, expressible at all, and it
/// keeps this the exact inverse of reading the section - the bytes handed
/// back are the bytes that were read.
///
/// The separation that followed the old section is preserved rather than
/// taken from the new text, so a replacement written without a trailing
/// blank line cannot weld its section onto the next heading.
pub fn replace(text: &str, heading: &Heading, body: &str) -> Edit {
    let original = &text[heading.range()];
    let trailing = original.len() - original.trim_end_matches('\n').len();
    let mut section = body.trim_end_matches('\n').to_string();
    section.push_str(&"\n".repeat(trailing));
    edit(text, heading.range(), &section)
}

/// Remove one section, heading line and trailing separation included.
pub fn delete(text: &str, heading: &Heading) -> Edit {
    edit(text, heading.range(), "")
}

/// Express "this range becomes this text" as the smallest safe request.
fn edit(text: &str, range: Range<usize>, section: &str) -> Edit {
    let mut low = range.start;
    let mut high = range.end;
    for _ in 0..=MAX_WIDENING_STEPS {
        // An anchor that spans the document is the document: sending it as
        // `findText` would put the whole body on the wire twice.
        if low == 0 && high == text.len() {
            break;
        }
        if is_unique(text, &text[low..high]) {
            let mut replacement =
                String::with_capacity((range.start - low) + section.len() + (high - range.end));
            replacement.push_str(&text[low..range.start]);
            replacement.push_str(section);
            replacement.push_str(&text[range.end..high]);
            return Edit::Patch {
                find_text: text[low..high].to_string(),
                replacement,
            };
        }
        // Backwards first: the lines above a section are its heading
        // context, which is what usually distinguishes two copies of it.
        if let Some(start) = previous_line(text, low) {
            low = start;
        } else if let Some(end) = next_line(text, high) {
            high = end;
        } else {
            break;
        }
    }
    let mut whole = String::with_capacity(text.len() + section.len());
    whole.push_str(&text[..range.start]);
    whole.push_str(section);
    whole.push_str(&text[range.end..]);
    Edit::Replace { text: whole }
}

/// Whether `needle` occurs exactly once in `haystack`.
///
/// Counts every starting position, including overlapping ones, and stops at
/// the second: "at most one place this could land" is the question, and an
/// overlapping second occurrence is still a second place.
fn is_unique(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut seen = 0;
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        seen += 1;
        if seen > 1 {
            return false;
        }
        let at = from + offset;
        // One character forward, not one byte: the next candidate start has
        // to be a character boundary or the slice above would panic.
        match haystack[at..].chars().next() {
            Some(character) => from = at + character.len_utf8(),
            None => break,
        }
        if from >= haystack.len() {
            break;
        }
    }
    seen == 1
}

/// Start of the line before `offset`, which must be a line start.
fn previous_line(text: &str, offset: usize) -> Option<usize> {
    if offset == 0 {
        return None;
    }
    // `offset - 1` is the newline that ended the previous line; the line
    // before it starts just past the newline before that, or at zero.
    Some(text[..offset - 1].rfind('\n').map_or(0, |at| at + 1))
}

/// End of the line starting at `offset`, which must be a line start.
fn next_line(text: &str, offset: usize) -> Option<usize> {
    if offset >= text.len() {
        return None;
    }
    Some(
        text[offset..]
            .find('\n')
            .map_or(text.len(), |at| offset + at + 1),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::super::{headings, resolve, SAMPLE};
    use super::*;

    fn patch(text: &str, address: &str, body: Option<&str>) -> (String, String) {
        let found = headings(text);
        let heading = resolve(&found, address).unwrap();
        let edit = match body {
            Some(body) => replace(text, heading, body),
            None => delete(text, heading),
        };
        match edit {
            Edit::Patch {
                find_text,
                replacement,
            } => (find_text, replacement),
            Edit::Replace { .. } => panic!("expected a patch for {address}"),
        }
    }

    #[test]
    fn replacing_a_section_patches_only_that_section() {
        let (find_text, replacement) = patch(SAMPLE, "FAQ", Some("## FAQ\n\nnew answers"));
        assert_eq!(find_text, "## FAQ\n\nanswers\n");
        assert_eq!(replacement, "## FAQ\n\nnew answers\n");
        // And the patch is what the server would have produced.
        assert_eq!(
            SAMPLE.replace(&find_text, &replacement),
            "intro line\n\n## Deploy\n\ndeploy steps\n\n### Rollback\n\n\
             rollback steps\n\n## FAQ\n\nnew answers\n"
        );
    }

    /// The separation before the next heading comes from the document, not
    /// from the new text, so a body without a trailing blank line cannot
    /// weld two sections together.
    #[test]
    fn the_trailing_separation_is_preserved_however_the_body_was_written() {
        for supplied in [
            "## Deploy\n\nrewritten",
            "## Deploy\n\nrewritten\n",
            "## Deploy\n\nrewritten\n\n\n\n",
        ] {
            let (_, replacement) = patch(SAMPLE, "Deploy", Some(supplied));
            assert!(
                replacement.ends_with("rewritten\n\n"),
                "{supplied:?} produced {replacement:?}"
            );
        }
    }

    #[test]
    fn replacing_a_section_can_rename_its_heading() {
        let (_, replacement) = patch(SAMPLE, "FAQ", Some("## Questions\n\nanswers"));
        assert!(replacement.starts_with("## Questions"), "{replacement:?}");
    }

    #[test]
    fn deleting_a_section_removes_its_heading_and_its_children() {
        let (find_text, replacement) = patch(SAMPLE, "Deploy", None);
        assert_eq!(replacement, "");
        assert_eq!(
            SAMPLE.replace(&find_text, &replacement),
            "intro line\n\n## FAQ\n\nanswers\n",
            "the child section goes with its parent, and nothing else moves"
        );
    }

    #[test]
    fn deleting_a_child_leaves_its_parent() {
        let (find_text, replacement) = patch(SAMPLE, "Deploy > Rollback", None);
        assert_eq!(
            SAMPLE.replace(&find_text, &replacement),
            "intro line\n\n## Deploy\n\ndeploy steps\n\n## FAQ\n\nanswers\n"
        );
    }

    /// The core guarantee: whatever the anchor ends up being, the document
    /// contains it exactly once, so the server's string match cannot land
    /// on the wrong copy.
    #[test]
    fn a_duplicated_section_still_gets_a_unique_anchor() {
        let text = "## A\n\nshared\n\n## Notes\n\nsame\n\n## B\n\nshared\n\n## Notes\n\nsame\n";
        let found = headings(text);
        // Both `## Notes` sections are byte-identical, so the section alone
        // is not an anchor; widening reaches the distinguishing heading.
        let second = &found[3];
        assert_eq!(&text[second.range()], "## Notes\n\nsame\n");
        let Edit::Patch {
            find_text,
            replacement,
        } = replace(text, second, "## Notes\n\nchanged")
        else {
            panic!("expected a patch");
        };
        assert!(is_unique(text, &find_text), "anchor is not unique");
        assert!(find_text.contains("## B"), "{find_text:?}");
        assert_eq!(
            text.replace(&find_text, &replacement),
            "## A\n\nshared\n\n## Notes\n\nsame\n\n## B\n\nshared\n\n## Notes\n\nchanged\n",
            "the first copy must be untouched"
        );
    }

    #[test]
    fn a_single_section_document_is_a_replace_rather_than_a_doubled_body() {
        let text = "## Only\n\nbody\n";
        let found = headings(text);
        assert_eq!(
            replace(text, &found[0], "## Only\n\nnew"),
            Edit::Replace {
                text: "## Only\n\nnew\n".to_string()
            },
            "anchoring the whole document would send it twice"
        );
    }

    #[test]
    fn widening_gives_up_on_a_body_that_cannot_be_anchored() {
        // Every line identical, so no window short of the document is
        // unique and the cap is what ends it.
        let text = "## S\n".repeat(MAX_WIDENING_STEPS * 4);
        let found = headings(&text);
        match replace(&text, &found[found.len() / 2], "## S\n\nreal body") {
            Edit::Replace { text: whole } => {
                assert!(whole.contains("real body"), "{whole:?}");
                assert!(whole.len() > text.len());
            }
            // Acceptable only if the anchor really is unique.
            Edit::Patch { find_text, .. } => assert!(is_unique(&text, &find_text)),
        }
    }

    /// Whatever the form, applying it must produce the intended document -
    /// that is the property, and the two variants are only encodings of it.
    #[test]
    fn either_form_applies_to_the_same_result() {
        let text = "## A\n\none\n\n## B\n\ntwo\n\n## C\n\nthree\n";
        let found = headings(text);
        let expected = "## A\n\none\n\n## B\n\nchanged\n\n## C\n\nthree\n";
        match replace(text, &found[1], "## B\n\nchanged") {
            Edit::Patch {
                find_text,
                replacement,
            } => assert_eq!(text.replace(&find_text, &replacement), expected),
            Edit::Replace { text: whole } => assert_eq!(whole, expected),
        }
    }

    #[test]
    fn uniqueness_counts_overlapping_occurrences() {
        assert!(is_unique("abcabd", "abc"));
        assert!(!is_unique("aaa", "aa"), "positions 0 and 1 both match");
        assert!(!is_unique("", "a"));
        assert!(!is_unique("a", ""));
        // Multi-byte input must not panic while stepping forward.
        assert!(is_unique("部署回滚", "回滚"));
        assert!(!is_unique("部署部署", "部署"));
    }

    #[test]
    fn line_stepping_stays_inside_the_document() {
        let text = "one\ntwo\nthree\n";
        assert_eq!(previous_line(text, 0), None);
        assert_eq!(previous_line(text, 4), Some(0));
        assert_eq!(previous_line(text, 8), Some(4));
        assert_eq!(next_line(text, 8), Some(14));
        assert_eq!(next_line(text, text.len()), None);
    }
}
