//! Finding a document's headings, and the span each one owns.
//!
//! # What counts as a heading
//!
//! ATX headings (`#` through `######`) outside fenced code blocks, which is
//! what Outline's editor produces. Fences are tracked so that a `#` comment
//! inside a shell example is never mistaken for a heading - the failure
//! that would matter most, because it would put a section boundary in the
//! middle of a code block and splice a document there.
//!
//! Setext headings (an `===` or `---` underline) are not recognized. The
//! consequence is that such a heading is not offered as an address, which
//! is a refusal to act rather than acting in the wrong place.

use std::ops::Range;

/// The indentation a fence or heading may carry before it stops being one.
///
/// Four spaces starts an indented code block in CommonMark, so three is the
/// limit for both constructs.
const MAX_INDENT: usize = 3;

/// One heading, and the span of the document it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// Number of leading `#`, 1 through 6.
    pub level: u8,
    /// Heading text, with the `#` markers and any closing run removed.
    pub title: String,
    /// Enclosing headings, outermost first.
    ancestors: Vec<(u8, String)>,
    /// 1-based line number of the heading line.
    pub line: usize,
    /// Byte offset of the start of the heading line.
    pub start: usize,
    /// Byte offset just past the section, exclusive.
    ///
    /// The section runs to the next heading of the same or a higher level,
    /// so a parent section contains its children. That is the reading of
    /// "this section" that matches how a document is edited: replacing a
    /// chapter replaces what is under it.
    pub end: usize,
}

impl Heading {
    /// The address that resolves to exactly this heading.
    ///
    /// This is the string [`super::resolve`] is meant to be handed back, so
    /// it is built in the same vocabulary that parses.
    pub fn path(&self) -> String {
        let mut path = String::new();
        for (_, title) in &self.ancestors {
            path.push_str(title);
            path.push(' ');
            path.push(super::PATH_SEPARATOR);
            path.push(' ');
        }
        path.push_str(&self.title);
        path
    }

    /// Ancestors and self, outermost first, for address matching.
    pub(super) fn chain(&self) -> Vec<(u8, &str)> {
        let mut chain: Vec<(u8, &str)> = self
            .ancestors
            .iter()
            .map(|(level, title)| (*level, title.as_str()))
            .collect();
        chain.push((self.level, self.title.as_str()));
        chain
    }

    /// The section's own byte range.
    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// Every heading in a document body, in document order.
pub fn headings(text: &str) -> Vec<Heading> {
    let mut found = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut open: Vec<(u8, String)> = Vec::new();
    for (number, (start, line)) in lines(text).enumerate() {
        // Fence state first: a heading is only a heading outside one.
        if let Some(marker) = fence {
            if closes_fence(line, marker) {
                fence = None;
            }
            continue;
        }
        if let Some(marker) = opens_fence(line) {
            fence = Some(marker);
            continue;
        }
        let Some((level, title)) = atx_heading(line) else {
            continue;
        };
        // A heading closes every open heading at its level or deeper.
        open.retain(|(open_level, _)| *open_level < level);
        found.push(Heading {
            level,
            title: title.clone(),
            ancestors: open.clone(),
            line: number + 1,
            start,
            // Filled in below, once the following headings are known.
            end: text.len(),
        });
        open.push((level, title));
    }
    close_sections(&mut found, text.len());
    found
}

/// Set each section's `end` to where the next same-or-higher heading starts.
fn close_sections(found: &mut [Heading], document_len: usize) {
    for index in 0..found.len() {
        let level = found[index].level;
        found[index].end = found[index + 1..]
            .iter()
            .find(|heading| heading.level <= level)
            .map_or(document_len, |heading| heading.start);
    }
}

/// Lines with their byte offsets, newline stripped from the content.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split_inclusive('\n').map(move |raw| {
        let start = offset;
        offset += raw.len();
        (start, raw.trim_end_matches('\n').trim_end_matches('\r'))
    })
}

/// The `(character, run length)` of a fence this line opens.
fn opens_fence(line: &str) -> Option<(char, usize)> {
    let body = line.trim_start_matches(' ');
    if line.len() - body.len() > MAX_INDENT {
        return None;
    }
    let character = body.chars().next()?;
    if character != '`' && character != '~' {
        return None;
    }
    let run = body.chars().take_while(|found| *found == character).count();
    (run >= 3).then_some((character, run))
}

/// Whether this line closes an open fence.
///
/// A closing fence is the same character, at least as long, and carries
/// nothing else: an info string is only allowed on the opening one.
fn closes_fence(line: &str, open: (char, usize)) -> bool {
    let Some((character, run)) = opens_fence(line) else {
        return false;
    };
    character == open.0
        && run >= open.1
        && line
            .trim_start_matches(' ')
            .trim_start_matches(character)
            .trim()
            .is_empty()
}

/// The `(level, title)` of an ATX heading, if this line is one.
fn atx_heading(line: &str) -> Option<(u8, String)> {
    let body = line.trim_start_matches(' ');
    if line.len() - body.len() > MAX_INDENT {
        return None;
    }
    let level = body
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &body[level..];
    // `#five` is not a heading: CommonMark wants whitespace, or nothing,
    // after the run. Without this a `#!/bin/sh` line becomes one.
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    Some((level as u8, strip_closing_run(rest.trim()).to_string()))
}

/// Drop a heading's optional closing `#` run.
///
/// Only when whitespace precedes it, so `C# vs F#` keeps both.
fn strip_closing_run(title: &str) -> &str {
    let without = title.trim_end_matches('#');
    if without.len() == title.len() {
        return title;
    }
    if without.is_empty() {
        return "";
    }
    if without.ends_with(' ') || without.ends_with('\t') {
        return without.trim_end();
    }
    title
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::super::SAMPLE;
    use super::*;

    fn titles(text: &str) -> Vec<String> {
        headings(text)
            .into_iter()
            .map(|heading| format!("{}:{}", heading.level, heading.title))
            .collect()
    }

    #[test]
    fn headings_carry_level_line_and_span() {
        let found = headings(SAMPLE);
        assert_eq!(titles(SAMPLE), ["2:Deploy", "3:Rollback", "2:FAQ"]);
        assert_eq!(found[0].line, 3);
        // A parent section contains its children.
        assert_eq!(
            &SAMPLE[found[0].range()],
            "## Deploy\n\ndeploy steps\n\n### Rollback\n\nrollback steps\n\n"
        );
        assert_eq!(
            &SAMPLE[found[1].range()],
            "### Rollback\n\nrollback steps\n\n"
        );
        // The last section runs to the end of the document.
        assert_eq!(found[2].end, SAMPLE.len());
    }

    #[test]
    fn a_path_names_every_ancestor() {
        let found = headings(SAMPLE);
        assert_eq!(found[0].path(), "Deploy");
        assert_eq!(found[1].path(), "Deploy > Rollback");
        assert_eq!(found[2].path(), "FAQ");
    }

    /// The failure that would matter most: a `#` comment inside a fence
    /// becoming a section boundary, which would splice a code block.
    #[test]
    fn a_hash_inside_a_fence_is_not_a_heading() {
        let text = "## Deploy\n\n```sh\n# not a heading\n## also not\n```\n\nafter\n";
        assert_eq!(titles(text), ["2:Deploy"]);
        assert_eq!(headings(text)[0].end, text.len());
    }

    #[test]
    fn fences_of_both_kinds_and_uneven_length_are_tracked() {
        let text =
            "# A\n\n~~~\n## hidden\n~~~\n\n````\n## also hidden\n```\nstill inside\n````\n\n## B\n";
        assert_eq!(titles(text), ["1:A", "2:B"]);
    }

    #[test]
    fn shebangs_and_indented_hashes_are_not_headings() {
        // No space after the run, four spaces of indent, and seven hashes.
        let text = "#!/bin/sh\n\n    ## indented\n\n####### seven\n\n## real\n";
        assert_eq!(titles(text), ["2:real"]);
    }

    #[test]
    fn a_closing_hash_run_is_dropped_but_a_sharp_in_a_title_is_kept() {
        assert_eq!(titles("## Deploy ##\n"), ["2:Deploy"]);
        assert_eq!(titles("## C# vs F#\n"), ["2:C# vs F#"]);
        assert_eq!(titles("## a#b\n"), ["2:a#b"]);
        assert_eq!(titles("## ##\n"), ["2:"]);
    }

    #[test]
    fn a_document_with_no_headings_yields_nothing() {
        assert!(headings("just prose\n\nmore prose\n").is_empty());
        assert!(headings("").is_empty());
    }

    #[test]
    fn a_body_with_no_trailing_newline_still_spans_to_its_end() {
        let text = "## A\n\nbody";
        let found = headings(text);
        assert_eq!(&text[found[0].range()], text);
    }

    #[test]
    fn crlf_input_keeps_its_line_endings_in_the_span() {
        let text = "## A\r\n\r\nbody\r\n\r\n## B\r\n\r\nother\r\n";
        let found = headings(text);
        assert_eq!(titles(text), ["2:A", "2:B"]);
        assert_eq!(&text[found[0].range()], "## A\r\n\r\nbody\r\n\r\n");
    }

    /// A deeper heading after a shallower one nests; a shallower one after a
    /// deeper one closes it, and so does a sibling.
    #[test]
    fn nesting_follows_heading_levels_in_both_directions() {
        let text = "# A\n\n## B\n\n### C\n\n## D\n\n# E\n";
        let found = headings(text);
        let paths: Vec<String> = found.iter().map(Heading::path).collect();
        assert_eq!(paths, ["A", "A > B", "A > B > C", "A > D", "E"]);
    }
}
