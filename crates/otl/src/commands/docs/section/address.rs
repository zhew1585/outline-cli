//! Turning a caller's string into one heading, and saying why when it does
//! not.
//!
//! # Why matching never falls back to a substring
//!
//! An address that half-matches the wrong section is the one outcome worth
//! more than an extra round trip. So matching is exact, then
//! case-insensitive, and then it stops - and the refusal carries the
//! document's own outline, so the retry is informed rather than another
//! guess. One extra command is cheap; editing the wrong chapter is not.
//!
//! # Why a rejected anchor gets to name sections
//!
//! [`locate`] exists so that a refused `--find-text` can say *which
//! sections* its matches fall in, not merely how many there were. A
//! local-file editor cannot do that - a string is all it has - while here
//! the heading tree is already parsed. That difference is what turns "3
//! matches" into a next step.

use super::parse::Heading;
use super::PATH_SEPARATOR;

/// How many positions an error message lists before it summarizes.
///
/// Long enough that a real document's whole outline fits, short enough that
/// a pathological body cannot turn one refusal into a page of output.
const MAX_LISTED: usize = 40;

/// Why an address did not name exactly one section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    /// Nothing matched.
    NotFound,
    /// Several headings matched. Carries their indices into the heading
    /// list, in document order.
    Ambiguous(Vec<usize>),
}

/// One segment of an address: a title, and optionally a pinned level.
struct Segment {
    level: Option<u8>,
    title: String,
}

/// Find the one heading an address names.
///
/// The address is one or more `>`-separated segments, matched against the
/// tail of a heading's ancestor chain, so `Deploy > Rollback` names the
/// `Rollback` directly under `Deploy` and `Rollback` alone names it only if
/// no other heading is called that. A segment may carry its own `#` markers
/// to pin that heading's level (`## Deploy > ### Rollback`).
pub fn resolve<'a>(found: &'a [Heading], address: &str) -> Result<&'a Heading, Unresolved> {
    let segments = parse_address(address);
    if segments.is_empty() {
        return Err(Unresolved::NotFound);
    }
    for fold_case in [false, true] {
        let hits: Vec<usize> = found
            .iter()
            .enumerate()
            .filter(|(_, heading)| matches(heading, &segments, fold_case))
            .map(|(index, _)| index)
            .collect();
        match hits.len() {
            0 => continue,
            1 => return Ok(&found[hits[0]]),
            _ => return Err(Unresolved::Ambiguous(hits)),
        }
    }
    Err(Unresolved::NotFound)
}

/// Split an address into its segments, dropping empty ones so that a
/// trailing or doubled separator is forgiving rather than fatal.
fn parse_address(address: &str) -> Vec<Segment> {
    address
        .split(PATH_SEPARATOR)
        .filter_map(|raw| {
            let raw = raw.trim();
            if raw.is_empty() {
                return None;
            }
            let hashes = raw
                .chars()
                .take_while(|character| *character == '#')
                .count();
            let title = raw[hashes..].trim();
            if title.is_empty() {
                return None;
            }
            Some(Segment {
                level: (1..=6).contains(&hashes).then_some(hashes as u8),
                title: title.to_string(),
            })
        })
        .collect()
}

/// Whether one heading's chain ends with these segments.
fn matches(heading: &Heading, segments: &[Segment], fold_case: bool) -> bool {
    let chain = heading.chain();
    if segments.len() > chain.len() {
        return false;
    }
    let tail = &chain[chain.len() - segments.len()..];
    segments.iter().zip(tail).all(|(segment, (level, title))| {
        segment.level.is_none_or(|pinned| pinned == *level)
            && same(&segment.title, title, fold_case)
    })
}

/// Title comparison, exact or case-insensitive.
fn same(left: &str, right: &str, fold_case: bool) -> bool {
    if fold_case {
        left.to_lowercase() == right.to_lowercase()
    } else {
        left == right
    }
}

/// Where one match of a plain string anchor is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// 1-based line number of the match.
    pub line: usize,
    /// The section that encloses it, when the match is under a heading.
    pub section: Option<String>,
}

/// Every place `needle` occurs, with the section each one falls in.
///
/// Stops after `limit` matches: the count is for a message, and a message
/// that lists ten thousand positions is not one.
pub fn locate(text: &str, needle: &str, found: &[Heading], limit: usize) -> Vec<Location> {
    let mut located = Vec::new();
    if needle.is_empty() {
        return located;
    }
    let mut from = 0;
    while located.len() < limit {
        let Some(offset) = text[from..].find(needle) else {
            break;
        };
        let at = from + offset;
        located.push(Location {
            line: 1 + text[..at].matches('\n').count(),
            // The innermost enclosing heading is the last one that starts at
            // or before the match, because sections are in document order.
            section: found
                .iter()
                .rev()
                .find(|heading| heading.start <= at)
                .map(Heading::path),
        });
        // One character forward, not one byte: the next candidate start has
        // to be a character boundary, and an overlapping match still counts.
        match text[at..].chars().next() {
            Some(character) => from = at + character.len_utf8(),
            None => break,
        }
        if from >= text.len() {
            break;
        }
    }
    located
}

/// The message for an address that did not name exactly one section.
///
/// `flag` is the flag that carried the address, so the same builder serves
/// `--section` and `--delete-section` without either one guessing.
pub fn unresolved_message(
    flag: &str,
    address: &str,
    found: &[Heading],
    error: &Unresolved,
) -> String {
    match error {
        Unresolved::Ambiguous(hits) => {
            let listed: Vec<&Heading> = hits
                .iter()
                .filter_map(|index| found.get(*index))
                .take(MAX_LISTED)
                .collect();
            format!(
                "{flag} {address:?} matches {} headings in this document, so \
                 it does not say which one to edit:\n{}\n\
                 Name the parent ({flag} 'Deploy {PATH_SEPARATOR} Notes') or \
                 pin the level ({flag} '## Notes').",
                hits.len(),
                catalogue(&listed),
            )
        }
        Unresolved::NotFound if found.is_empty() => format!(
            "{flag} {address:?} cannot be resolved: this document has no \
             markdown headings, so it has no addressable sections. Pipe a \
             whole new body without {flag} to replace it entirely."
        ),
        Unresolved::NotFound => {
            let listed: Vec<&Heading> = found.iter().take(MAX_LISTED).collect();
            let omitted = found.len() - listed.len();
            let tail = if omitted == 0 {
                String::new()
            } else {
                format!("\n  ... and {omitted} more")
            };
            format!(
                "{flag} {address:?} matches no heading in this document, \
                 which contains:\n{}{tail}",
                catalogue(&listed),
            )
        }
    }
}

/// One line per heading: where it is, and the address that names it.
///
/// Every line is scrubbed and folded to one line, because heading text is
/// server data being interpolated into a message bound for a terminal.
fn catalogue(listed: &[&Heading]) -> String {
    listed
        .iter()
        .map(|heading| {
            crate::stdio::scrub_to_one_line(&format!("  L{:<5} {}", heading.line, heading.path()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::super::{headings, SAMPLE};
    use super::*;

    fn section<'a>(text: &'a str, found: &[Heading], address: &str) -> &'a str {
        &text[resolve(found, address).unwrap().range()]
    }

    #[test]
    fn every_path_round_trips_through_resolve() {
        let found = headings(SAMPLE);
        for heading in &found {
            let resolved = resolve(&found, &heading.path()).unwrap();
            assert_eq!(resolved, heading, "{} did not round-trip", heading.path());
        }
    }

    #[test]
    fn an_address_can_pin_a_level_and_name_a_parent() {
        let found = headings(SAMPLE);
        assert!(section(SAMPLE, &found, "### Rollback").starts_with("### Rollback"));
        assert!(section(SAMPLE, &found, "## Deploy > ### Rollback").starts_with("### Rollback"));
        // A pinned level that is wrong matches nothing.
        assert_eq!(
            resolve(&found, "## Rollback").unwrap_err(),
            Unresolved::NotFound
        );
        // A parent that is wrong matches nothing.
        assert_eq!(
            resolve(&found, "FAQ > Rollback").unwrap_err(),
            Unresolved::NotFound
        );
    }

    #[test]
    fn case_is_a_fallback_and_never_beats_an_exact_match() {
        let text = "## deploy\n\nlower\n\n## Deploy\n\nupper\n";
        let found = headings(text);
        assert_eq!(section(text, &found, "Deploy"), "## Deploy\n\nupper\n");
        assert_eq!(section(text, &found, "deploy"), "## deploy\n\nlower\n\n");
        // Only when neither is exact does folding apply - and then both
        // match, so the caller is told rather than guessed at.
        assert_eq!(
            resolve(&found, "DEPLOY").unwrap_err(),
            Unresolved::Ambiguous(vec![0, 1])
        );
    }

    #[test]
    fn a_repeated_heading_is_reported_with_every_position() {
        let text = "## Notes\n\nfirst\n\n## Notes\n\nsecond\n";
        let found = headings(text);
        assert_eq!(
            resolve(&found, "Notes").unwrap_err(),
            Unresolved::Ambiguous(vec![0, 1])
        );
    }

    #[test]
    fn an_empty_or_separator_only_address_resolves_to_nothing() {
        let found = headings(SAMPLE);
        for address in ["", "   ", ">", " > > ", "##"] {
            assert_eq!(
                resolve(&found, address).unwrap_err(),
                Unresolved::NotFound,
                "{address:?} resolved to something"
            );
        }
    }

    #[test]
    fn a_missing_address_is_answered_with_the_outline() {
        let found = headings(SAMPLE);
        let message = unresolved_message("--section", "Nope", &found, &Unresolved::NotFound);
        for address in ["Deploy", "Deploy > Rollback", "FAQ"] {
            assert!(message.contains(address), "{address} missing:\n{message}");
        }
    }

    #[test]
    fn an_ambiguous_address_is_answered_with_the_matches_and_a_remedy() {
        let text = "## Notes\n\nfirst\n\n## Other\n\nx\n\n## Notes\n\nsecond\n";
        let found = headings(text);
        let error = resolve(&found, "Notes").unwrap_err();
        let message = unresolved_message("--section", "Notes", &found, &error);
        assert!(message.contains("matches 2 headings"), "{message}");
        assert!(
            message.contains("L1") && message.contains("L9"),
            "{message}"
        );
        assert!(message.contains("pin the level"), "{message}");
        // The other section is not offered as if it matched.
        assert!(!message.contains("Other"), "{message}");
    }

    #[test]
    fn a_document_with_no_headings_says_so_rather_than_listing_nothing() {
        let message = unresolved_message("--section", "Deploy", &[], &Unresolved::NotFound);
        assert!(message.contains("no markdown headings"), "{message}");
    }

    /// Heading text is server data going into a terminal message.
    #[test]
    fn a_hostile_heading_cannot_forge_a_line_in_the_refusal() {
        let text = "## evil\u{1b}[31m\n\nbody\n\n## also\u{202e}bad\n\nx\n";
        let found = headings(text);
        let message = unresolved_message("--section", "Nope", &found, &Unresolved::NotFound);
        assert!(!message.contains('\u{1b}'), "escape survived:\n{message}");
        assert!(!message.contains('\u{202e}'), "bidi survived:\n{message}");
        // One line of prose, then exactly one line per heading.
        assert_eq!(message.lines().count(), 3, "{message}");
    }

    #[test]
    fn a_long_outline_is_truncated_with_a_count() {
        let text = (0..MAX_LISTED + 5)
            .map(|index| format!("## H{index}\n\nbody\n\n"))
            .collect::<String>();
        let found = headings(&text);
        let message = unresolved_message("--section", "Nope", &found, &Unresolved::NotFound);
        assert!(message.contains("... and 5 more"), "{message}");
    }

    #[test]
    fn locate_names_the_section_each_match_falls_in() {
        let text = "preamble\n\n## Deploy\n\nrestart it\n\n## Rollback\n\nrestart it\n";
        let found = headings(text);
        let located = locate(text, "restart it", &found, 10);
        assert_eq!(located.len(), 2);
        assert_eq!(located[0].section.as_deref(), Some("Deploy"));
        assert_eq!(located[1].section.as_deref(), Some("Rollback"));
        assert_eq!(located[0].line, 5);
        assert_eq!(located[1].line, 9);
    }

    #[test]
    fn a_match_before_the_first_heading_has_no_section() {
        let text = "preamble text\n\n## Deploy\n\nsteps\n";
        let found = headings(text);
        let located = locate(text, "preamble", &found, 10);
        assert_eq!(located.len(), 1);
        assert_eq!(located[0].section, None);
        assert_eq!(located[0].line, 1);
    }

    #[test]
    fn locate_stops_at_the_limit_and_survives_multibyte_input() {
        let text = "回滚".repeat(50);
        let located = locate(&text, "回滚", &[], 5);
        assert_eq!(located.len(), 5);
        assert!(locate(&text, "", &[], 5).is_empty());
        // Overlapping matches are separate positions.
        assert_eq!(locate("aaa", "aa", &[], 5).len(), 2);
    }
}
