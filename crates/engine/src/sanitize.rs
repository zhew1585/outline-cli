//! Credential hygiene for server-provided text.
//!
//! Server-controlled strings (error bodies) are hostile input: they may
//! reflect our own bearer token back, interleave it with junk to slip past
//! an exact match, or be cut mid-token by the response read cap. This
//! module is the single place where such text is made safe, and it runs at
//! error CONSTRUCTION time so that every error field is credential-free by
//! construction (see [`crate::error`]).
//!
//! Two rules do the heavy lifting, both deliberately CATEGORICAL rather
//! than lists of known-bad characters:
//!
//! 1. **Visibility.** A character is kept only if it renders as something.
//!    Zero-width codepoints (combining marks, ZWSP/ZWJ, soft hyphen, BOM,
//!    variation selectors, tag characters, ...) are dropped and control
//!    characters become spaces. Nothing invisible ever reaches stderr, so
//!    what a reader sees is what the text actually is.
//! 2. **Skeleton comparison.** The candidate text and the secret are both
//!    reduced to lowercase alphanumerics only - every separator, visible or
//!    not, is discarded. If the secret's skeleton appears in the text's
//!    skeleton, the field is discarded wholesale. This defeats ANY
//!    interleaving (invisible characters, punctuation, whitespace, mixed
//!    case) with one rule instead of enumerating character classes.
//!
//! The remaining ordering is still load-bearing: redact exact occurrences
//! first (so ordinary echoes keep a readable message), then normalize, then
//! redact again, then apply the skeleton check, and only then cap length.

use unicode_width::UnicodeWidthChar;

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
    let untrusted = if may_be_truncated {
        replace_trailing_runs(raw)
    } else {
        raw.to_string()
    };
    let normalized = normalize(&redact_secret(&untrusted, secret));
    let redacted = redact_secret(&normalized, secret);
    if smuggles_secret(&redacted, secret) {
        return REDACTED.to_string();
    }
    redacted
        .chars()
        .take(cap)
        .collect::<String>()
        .trim()
        .to_string()
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
        .filter_map(|c| match UnicodeWidthChar::width(c) {
            // Control characters (width None) and whitespace read as gaps.
            None => Some(' '),
            _ if c.is_whitespace() => Some(' '),
            // Renders as nothing: it can only be there to mislead.
            Some(0) => None,
            Some(_) => Some(c),
        })
        .fold(String::new(), |mut acc, c| {
            if c != ' ' || !acc.ends_with(' ') {
                acc.push(c);
            }
            acc
        });
    collapsed.trim().to_string()
}

/// Whether the secret is still recoverable from `text` once both are
/// reduced to their skeletons.
///
/// See the module docs: this is the categorical backstop. Any interleaving
/// a server invents - invisible characters, punctuation, spacing, case
/// changes - collapses away, so a single containment check catches it.
/// Secrets whose skeleton is too short are skipped so that an incidental
/// match cannot suppress legitimate diagnostics.
fn smuggles_secret(text: &str, secret: &str) -> bool {
    let secret_skeleton = skeleton(secret);
    if secret_skeleton.chars().count() < MIN_SECRET_FRAGMENT_CHARS {
        return false;
    }
    skeleton(text).contains(&secret_skeleton)
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
    fn capped_text_drops_short_trailing_fragment() {
        // The read cap leaves a 2-char token prefix, shorter than
        // MIN_SECRET_FRAGMENT_CHARS.
        let cleaned = clean_server_text("\n\n\nre", TOKEN, true, CAP);
        assert_eq!(cleaned, REDACTED);
    }

    #[test]
    fn capped_text_drops_fragment_behind_trailing_whitespace() {
        // Re-review PoC: the cut lands on whitespace, so the fragment sits
        // one run further in and a single drop would miss it.
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
