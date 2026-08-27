//! Credential hygiene for server-provided text.
//!
//! Server-controlled strings (error bodies) are hostile input: they may
//! reflect our own bearer token back, interleave it with junk to slip past
//! an exact match, or be cut mid-token by the response read cap. This
//! module is the single place where such text is made safe, and it runs at
//! error CONSTRUCTION time so that every error field is credential-free by
//! construction (see [`crate::error`]).
//!
//! Two rules do the heavy lifting, both categorical rather than lists of
//! known-bad characters:
//!
//! 1. **Visibility.** A character is kept only if it renders as something.
//!    Zero-width codepoints (combining marks, ZWSP/ZWJ, soft hyphen, BOM,
//!    variation selectors, tag characters, ...) are dropped and control
//!    characters become spaces.
//! 2. **Skeleton FRAGMENT comparison.** The candidate text and the secret
//!    are both reduced to lowercase alphanumerics only. If ANY window of
//!    [`MIN_SECRET_FRAGMENT_CHARS`] consecutive skeleton characters of the
//!    secret appears anywhere in the text's skeleton, the field is
//!    discarded wholesale.
//!
//! The ordering is load-bearing: redact exact occurrences first (so
//! ordinary echoes keep a readable message), then normalize, then redact
//! again, then apply the fragment check, and only then cap length.

use unicode_width::UnicodeWidthChar;

use crate::text::{hazard, Hazard};

/// Placeholder shown instead of secrets.
pub const REDACTED: &str = "***";
/// Minimum length (in chars) of a secret fragment worth redacting, and of a
/// secret skeleton worth checking for smuggling.
const MIN_SECRET_FRAGMENT_CHARS: usize = 4;
/// How many trailing runs to drop from text cut by a read cap.
///
/// Two, not one: when the cut lands on whitespace, the fragment of unknown
/// provenance sits one run further in.
const CAPPED_TAIL_RUNS: usize = 2;

/// Make server-provided text safe to display and cap it at `cap` chars.
///
/// `may_be_truncated` must be true only when THIS text may itself be cut
/// mid-way (a raw body that hit the read cap). Its trailing runs are then
/// dropped, since a cut fragment could be the start of a reflected secret.
/// It must be false for text extracted from a structure that parsed
/// successfully: those fields are complete, and dropping their last words
/// would corrupt a legitimate diagnostic for no security gain.
pub fn clean_server_text(raw: &str, secret: &str, may_be_truncated: bool, cap: usize) -> String {
    clean_server_text_for(raw, std::slice::from_ref(&secret), may_be_truncated, cap)
}

/// [`clean_server_text`] against EVERY secret a request carried.
///
/// One request can involve more than one credential (the channel replaces
/// a rejected token with a renewed one and replays), so every secret still
/// in play must be passed here; a single fragment match from any of them
/// discards the text.
pub fn clean_server_text_for(
    raw: &str,
    secrets: &[&str],
    may_be_truncated: bool,
    cap: usize,
) -> String {
    let untrusted = if may_be_truncated {
        replace_trailing_runs(raw)
    } else {
        raw.to_string()
    };
    // Exact redaction first, for every secret, so an honest message that
    // quotes a whole token stays readable.
    let redacted_once = secrets
        .iter()
        .fold(untrusted, |text, secret| redact_secret(&text, secret));
    let normalized = normalize(&redacted_once);
    let redacted = secrets
        .iter()
        .fold(normalized, |text, secret| redact_secret(&text, secret));
    // Then the categorical backstop: any surviving fragment of any secret
    // means the whole field goes.
    if secrets
        .iter()
        .any(|secret| leaks_fragment(&redacted, secret))
    {
        return REDACTED.to_string();
    }
    redacted
        .chars()
        .take(cap)
        .collect::<String>()
        .trim()
        .to_string()
}

/// [`redact_secret`] for every secret in play.
pub fn redact_all(text: &str, secrets: &[&str]) -> String {
    secrets.iter().fold(text.to_string(), |text, secret| {
        redact_secret(&text, secret)
    })
}

/// Replace every occurrence of `secret` in `text` with [`REDACTED`], then
/// redact a trailing prefix of the secret left behind by a cut.
///
/// An empty secret is left alone (a plain `str::replace("")` would insert
/// the marker between every character).
pub fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    redact_cut_secret_tail(&text.replace(secret, REDACTED), secret)
}

/// Redact a trailing prefix of `secret` (at least
/// [`MIN_SECRET_FRAGMENT_CHARS`] chars long).
///
/// A read cap can cut a reflected secret mid-way, and such a cut fragment
/// can only appear at the very end of the capped text, where an exact match
/// cannot find it. A cut mid-character leaves a U+FFFD from lossy decoding
/// after the fragment, so trailing replacement characters are ignored when
/// matching.
fn redact_cut_secret_tail(text: &str, secret: &str) -> String {
    let trimmed = text.trim_end_matches(char::REPLACEMENT_CHARACTER);
    let fragment = secret
        .char_indices()
        .map(|(index, c)| &secret[..index + c.len_utf8()])
        .rev()
        .filter(|prefix| prefix.chars().count() >= MIN_SECRET_FRAGMENT_CHARS)
        .find(|prefix| trimmed.ends_with(prefix));
    match fragment {
        Some(prefix) => {
            let kept = &trimmed[..trimmed.len() - prefix.len()];
            format!("{kept}{REDACTED}")
        }
        None => text.to_string(),
    }
}

/// Drop the trailing [`CAPPED_TAIL_RUNS`] runs of non-whitespace text and
/// mark the loss with [`REDACTED`].
///
/// Used when text may have been cut by a read cap: whatever sits at the end
/// is of unknown provenance and no length assumption about it is safe, so
/// it goes as a unit. Repeating the drop covers a cut that landed on
/// whitespace, which would otherwise leave the fragment one run further in.
fn replace_trailing_runs(text: &str) -> String {
    let mut kept = text;
    let mut dropped_any = false;
    for _ in 0..CAPPED_TAIL_RUNS {
        let trimmed = kept.trim_end();
        let dropped = trimmed.trim_end_matches(|c: char| !c.is_whitespace());
        if dropped.len() == trimmed.len() {
            break;
        }
        kept = dropped;
        dropped_any = true;
    }
    if dropped_any {
        format!("{kept}{REDACTED}")
    } else {
        text.to_string()
    }
}

/// Keep only what renders: control characters and whitespace become single
/// spaces, zero-width codepoints are dropped entirely.
///
/// Categorical by construction - the decision comes from the character's
/// display width, not from a list of known offenders - so invisible
/// smuggling vehicles (ZWSP, ZWJ, soft hyphen, BOM, variation selectors,
/// combining marks, tag characters) are all removed without naming any of
/// them.
fn normalize(raw: &str) -> String {
    let collapsed = raw
        .chars()
        .filter_map(|c| match (hazard(c), UnicodeWidthChar::width(c)) {
            // Control characters and whitespace read as gaps.
            (Some(Hazard::Control), _) | (_, None) => Some(' '),
            _ if c.is_whitespace() => Some(' '),
            // Renders as nothing, or renders as something other than what
            // it is: neither rule contains the other, so both are checked.
            (Some(_), _) | (_, Some(0)) => None,
            _ => Some(c),
        })
        .fold(String::new(), |mut acc, c| {
            if c != ' ' || !acc.ends_with(' ') {
                acc.push(c);
            }
            acc
        });
    collapsed.trim().to_string()
}

/// Whether ANY fragment of the secret survives in `text`.
///
/// Both sides are reduced to skeletons, so every interleaving a server
/// might invent (invisible characters, punctuation, spacing, case changes)
/// collapses away first. Then every window of
/// [`MIN_SECRET_FRAGMENT_CHARS`] consecutive skeleton characters of the
/// secret is looked for in the text, not just the whole skeleton.
///
/// Secrets whose whole skeleton is shorter than one window are skipped,
/// so a two-character "secret" cannot blank out every diagnostic.
fn leaks_fragment(text: &str, secret: &str) -> bool {
    let fragments: Vec<char> = skeleton(secret).chars().collect();
    if fragments.len() < MIN_SECRET_FRAGMENT_CHARS {
        return false;
    }
    let haystack = skeleton(text);
    if haystack.is_empty() {
        return false;
    }
    fragments
        .windows(MIN_SECRET_FRAGMENT_CHARS)
        .any(|window| haystack.contains(&window.iter().collect::<String>()))
}

/// Reduce text to lowercase alphanumerics: everything that could be used to
/// break up a token is discarded.
fn skeleton(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 200;
    const TOKEN: &str = "reflected-secret-token";

    /// Clean text that is known to be complete (the common case).
    fn clean(raw: &str, secret: &str) -> String {
        clean_server_text(raw, secret, false, CAP)
    }

    #[test]
    fn redacts_exact_occurrence() {
        assert_eq!(
            clean("invalid header: Bearer s3cret-token", "s3cret-token"),
            "invalid header: Bearer ***"
        );
    }

    #[test]
    fn discards_token_smuggled_through_any_separator() {
        // One rule must cover every interleaving vehicle: invisible format
        // characters, control characters, whitespace, punctuation, case.
        for (name, raw) in [
            ("ZWSP", "reflected-\u{200b}secret-token"),
            ("ZWNJ", "reflected-\u{200c}secret-token"),
            ("ZWJ", "reflected\u{200d}-secret-token"),
            ("soft hyphen", "reflected-\u{ad}secret-token"),
            ("BOM", "reflected-\u{feff}secret-token"),
            ("variation selector", "reflected-\u{fe0f}secret-token"),
            ("combining mark", "reflected-s\u{301}ecret-token"),
            ("tag char", "reflected-\u{e0041}secret-token"),
            ("NUL", "reflected-\u{0}secret-token"),
            ("newline", "reflected-\nsecret-token"),
            ("space", "reflected- secret-token"),
            ("punctuation", "reflected-.:.secret-token"),
            ("mixed case", "REFLECTED-\tSecret-Token"),
            ("wrapped", "prefix reflected-\u{200b}secret-token suffix"),
        ] {
            let cleaned = clean(raw, TOKEN);
            // The invariant is that the token is not recoverable, however a
            // reader mangles the output: some inputs collapse to a plain
            // exact match (token replaced in place, message preserved),
            // others are discarded whole. Both are safe.
            assert!(
                !skeleton(&cleaned).contains(&skeleton(TOKEN)),
                "{name} recoverable: {cleaned:?}"
            );
            assert!(
                cleaned.contains(REDACTED),
                "{name} not marked as redacted: {cleaned:?}"
            );
        }
    }

    #[test]
    fn normalize_drops_invisible_characters_from_output() {
        // Even when no secret is involved, invisible junk must not reach
        // stderr: what the reader sees must be what the text is.
        let cleaned = clean("aaa\u{200b}bbb\u{ad}ccc\u{feff}", "unrelated-token");
        assert_eq!(cleaned, "aaabbbccc");
    }

    #[test]
    fn normalize_drops_every_format_character_not_just_the_zero_width_ones() {
        // A terminal gives a column to 27 of the `Cf` codepoints, so width
        // alone does not drop them; the `hazard` rule must catch them too.
        let ranges: &[(u32, u32)] = &[
            (0x0600, 0x0605),
            (0x06dd, 0x06dd),
            (0x070f, 0x070f),
            (0x0890, 0x0891),
            (0x08e2, 0x08e2),
            (0xfff9, 0xfffb),
            (0x110bd, 0x110bd),
            (0x110cd, 0x110cd),
            (0x13430, 0x1343f),
            (0x1bca0, 0x1bca3),
            (0xe0001, 0xe0001),
        ];
        for (first, last) in ranges {
            for code in *first..=*last {
                let Some(c) = char::from_u32(code) else {
                    panic!("U+{code:04X} is not a scalar value");
                };
                let cleaned = clean(&format!("aaa{c}bbb"), "unrelated-token");
                assert_eq!(
                    cleaned, "aaabbb",
                    "U+{code:04X} survived the server-text scrub"
                );
            }
        }
    }

    #[test]
    fn normalize_still_drops_zero_width_characters_that_are_not_format_ones() {
        // The width rule is not redundant: a combining mark is not a `Cf`
        // codepoint, and stacking a few hundred of them on one letter is the
        // other way to make output unreadable.
        let cleaned = clean("a\u{301}\u{301}\u{489}b", "unrelated-token");
        assert_eq!(cleaned, "ab");
    }

    #[test]
    fn capped_text_drops_short_trailing_fragment() {
        // The read cap leaves a 2-char token prefix, shorter than
        // MIN_SECRET_FRAGMENT_CHARS.
        let cleaned = clean_server_text("\n\n\nre", TOKEN, true, CAP);
        assert_eq!(cleaned, REDACTED);
    }

    #[test]
    fn capped_text_drops_fragment_behind_trailing_whitespace() {
        // The cut lands on whitespace, so the fragment sits one run further
        // in and a single drop would miss it.
        let cleaned = clean_server_text("\n\n\nre\n", TOKEN, true, CAP);
        assert_eq!(cleaned, REDACTED);
        assert!(!cleaned.contains("re"), "fragment survived: {cleaned:?}");
    }

    #[test]
    fn capped_text_keeps_leading_words() {
        let cleaned = clean_server_text("database is down, retry lat", TOKEN, true, CAP);
        assert_eq!(cleaned, "database is down, ***");
    }

    #[test]
    fn complete_text_keeps_its_last_word() {
        // Fields extracted from a structure that parsed are complete: the
        // cap-tail treatment must not touch them.
        assert_eq!(clean("document not found", TOKEN), "document not found");
        assert_eq!(clean("validation_error", TOKEN), "validation_error");
    }

    #[test]
    fn discards_a_prefix_fragment_sitting_in_the_middle_of_a_sentence() {
        // A prefix followed by ordinary words, not at the end of the text.
        for raw in [
            "token reflec is not valid, please retry",
            "prefix reflected-sec suffix",
            "the value refl was rejected",
        ] {
            let cleaned = clean(raw, TOKEN);
            assert_eq!(
                cleaned, REDACTED,
                "a mid-string fragment survived: {cleaned:?}"
            );
        }
    }

    #[test]
    fn discards_an_arbitrary_middle_slice_of_the_secret() {
        // Every window of the secret must be caught, not just its prefix:
        // a server echoing successive slices would otherwise reassemble the
        // whole token across requests.
        let skeleton_chars: Vec<char> = skeleton(TOKEN).chars().collect();
        for start in 0..=skeleton_chars.len() - MIN_SECRET_FRAGMENT_CHARS {
            let slice: String = skeleton_chars[start..start + MIN_SECRET_FRAGMENT_CHARS]
                .iter()
                .collect();
            let raw = format!("rejected value {slice} was not accepted");
            let cleaned = clean(&raw, TOKEN);
            assert_eq!(
                cleaned, REDACTED,
                "slice at offset {start} ({slice:?}) survived: {cleaned:?}"
            );
        }
    }

    #[test]
    fn discards_a_fragment_however_it_is_broken_up() {
        // The fragment rule inherits the skeleton treatment, so separators
        // inside a fragment do not help either.
        for raw in [
            "value re-fle-cted here",
            "value r e f l here",
            "value R\u{200b}E\u{200b}F\u{200b}L here",
            "value [refl] here",
        ] {
            assert_eq!(
                clean(raw, TOKEN),
                REDACTED,
                "a separated fragment survived: {raw:?}"
            );
        }
    }

    #[test]
    fn a_full_echo_still_produces_a_readable_message() {
        // Ordering matters: exact redaction runs before the fragment check,
        // so the common honest case keeps its diagnostic instead of being
        // blanked out by its own fragments.
        assert_eq!(
            clean("invalid header: Bearer reflected-secret-token", TOKEN),
            "invalid header: Bearer ***"
        );
    }

    #[test]
    fn a_secret_too_short_to_have_a_window_cannot_blank_everything() {
        // A 3-character secret has no window, so it only gets exact
        // redaction - otherwise any 3-letter value would erase every
        // diagnostic the server ever sends.
        assert_eq!(
            clean("the id abc was rejected", "abc"),
            "the id *** was rejected"
        );
    }

    #[test]
    fn every_secret_in_play_is_redacted_not_just_the_last_one() {
        // The renew-and-replay case: the reply to the replayed request may
        // echo the token the FIRST attempt used. Both are redacted, and
        // because both were echoed in full the message stays readable.
        let stale = "old-secret-value-1";
        let fresh = "new-secret-value-2";
        let cleaned = clean_server_text_for(
            &format!("token {stale} was replaced by {fresh}"),
            &[fresh, stale],
            false,
            CAP,
        );
        assert_eq!(cleaned, "token *** was replaced by ***");
        for secret in [stale, fresh] {
            assert!(
                !skeleton(&cleaned).contains(&skeleton(secret)),
                "{secret} is recoverable from {cleaned:?}"
            );
        }
    }

    #[test]
    fn a_superseded_token_is_redacted_even_when_only_it_is_echoed() {
        // A server that saw T1, then T2 on the replay, and chooses to echo
        // only T1 back. A pipeline that knew just the current credential
        // would print T1 verbatim.
        let cleaned = clean_server_text_for(
            "the previous token old-secret-value-1 is no longer accepted",
            &["new-secret-value-2", "old-secret-value-1"],
            false,
            CAP,
        );
        assert!(
            !cleaned.contains("old-secret-value-1"),
            "the superseded token leaked: {cleaned:?}"
        );
        assert!(cleaned.contains(REDACTED), "{cleaned:?}");
    }

    #[test]
    fn a_fragment_of_any_secret_in_play_discards_the_text() {
        let cleaned = clean_server_text_for(
            "the value old-sec was rejected",
            &["current-token-value", "old-secret-value-1"],
            false,
            CAP,
        );
        assert_eq!(cleaned, REDACTED, "a fragment survived: {cleaned:?}");
    }

    #[test]
    fn unrelated_secrets_do_not_disturb_an_honest_message() {
        let cleaned = clean_server_text_for(
            "document not found",
            &["zzqqxxwwvvuu-1", "yyppmmkkjjhh-2"],
            false,
            CAP,
        );
        assert_eq!(cleaned, "document not found");
    }

    #[test]
    fn short_secret_does_not_discard_incidental_match() {
        assert_eq!(
            clean("abc is not a valid id", "abc"),
            "*** is not a valid id"
        );
    }

    #[test]
    fn empty_secret_leaves_text_alone() {
        assert_eq!(clean("plain message", ""), "plain message");
    }

    #[test]
    fn caps_length_after_redaction() {
        assert_eq!(clean(&"x".repeat(500), TOKEN).chars().count(), CAP);
    }

    #[test]
    fn strips_control_characters_and_collapses_whitespace() {
        let cleaned = clean("remote\nfailure\x1b[31m  here", TOKEN);
        assert!(!cleaned.contains('\x1b'));
        assert!(!cleaned.contains('\n'));
        assert_eq!(cleaned, "remote failure [31m here");
    }
}
