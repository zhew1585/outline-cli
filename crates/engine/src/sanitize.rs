//! Credential hygiene for server-provided text.
//!
//! Server-controlled strings (error bodies) are hostile input: they may
//! reflect our own bearer token back, wrap it in control characters to slip
//! past an exact match, or be cut mid-token by the response read cap. This
//! module is the single place where such text is made safe, and it runs at
//! error CONSTRUCTION time so that every error field is credential-free by
//! construction (see [`crate::error`]).
//!
//! The order of operations is load-bearing:
//!
//! 1. redact exact occurrences of the secret in the raw text;
//! 2. normalize control characters and whitespace runs - this step can
//!    REASSEMBLE a secret the server split with control characters, so it
//!    must never be the last one;
//! 3. redact again on the normalized text;
//! 4. if the normalizer could still reassemble the secret (whitespace and
//!    control characters squeezed out), discard the text entirely - a
//!    server that does this is deliberately smuggling the credential;
//! 5. only then cap the length.

/// Placeholder shown instead of secrets.
pub const REDACTED: &str = "***";
/// Minimum length (in chars) of a secret fragment worth redacting, and of a
/// secret worth checking for smuggling.
const MIN_SECRET_FRAGMENT_CHARS: usize = 4;

/// Make server-provided text safe to display and cap it at `cap` chars.
///
/// `body_was_capped` must be true when the text comes from a response body
/// that hit the read cap: the trailing run is then a fragment of unknown
/// provenance (possibly the start of a reflected secret) and is replaced by
/// [`REDACTED`] wholesale.
pub fn clean_server_text(raw: &str, secret: &str, body_was_capped: bool, cap: usize) -> String {
    let untrusted = if body_was_capped {
        replace_trailing_run(raw)
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

/// Replace the trailing run of non-whitespace characters with [`REDACTED`].
///
/// Used when a response body hit the read cap: whatever the cut left at the
/// end is a fragment of unknown provenance, so it is dropped as a unit
/// rather than trusted to be shorter than a secret prefix.
fn replace_trailing_run(text: &str) -> String {
    let kept = text.trim_end_matches(|c: char| !c.is_whitespace());
    if kept.len() == text.len() {
        return text.to_string();
    }
    format!("{kept}{REDACTED}")
}

/// Turn control characters and whitespace into single spaces and trim.
fn normalize(raw: &str) -> String {
    let collapsed = raw
        .chars()
        .map(|c| {
            if c.is_control() || c.is_whitespace() {
                ' '
            } else {
                c
            }
        })
        .fold(String::new(), |mut acc, c| {
            if c != ' ' || !acc.ends_with(' ') {
                acc.push(c);
            }
            acc
        });
    collapsed.trim().to_string()
}

/// Whether the secret is still recoverable from `text` by deleting
/// whitespace and control characters.
///
/// This catches a server that interleaves control characters (or newlines,
/// which [`normalize`] turns into spaces) inside a reflected token so that
/// no exact match ever fires, yet a human reading stderr can simply close
/// the gaps. Comparison is ASCII-case-insensitive; short secrets are
/// skipped to avoid discarding messages over an incidental match.
fn smuggles_secret(text: &str, secret: &str) -> bool {
    let squeezed_secret = squeeze(secret);
    if squeezed_secret.chars().count() < MIN_SECRET_FRAGMENT_CHARS {
        return false;
    }
    squeeze(text).contains(&squeezed_secret)
}

/// Drop whitespace and control characters and fold ASCII case.
fn squeeze(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 200;

    #[test]
    fn redacts_exact_occurrence() {
        let cleaned = clean_server_text(
            "invalid header: Bearer s3cret-token",
            "s3cret-token",
            false,
            CAP,
        );
        assert_eq!(cleaned, "invalid header: Bearer ***");
    }

    #[test]
    fn discards_text_that_smuggles_secret_through_control_chars() {
        // PoC from the adversarial review: NUL inside the reflected token
        // defeats the exact match, and the normalizer would otherwise turn
        // it into a space a human can simply delete.
        let secret = "reflected-secret-token";
        let raw = "reflected-\u{0}secret-token\u{1b}[31m";
        let cleaned = clean_server_text(raw, secret, false, CAP);
        assert_eq!(cleaned, REDACTED);
        assert!(!squeeze(&cleaned).contains(&squeeze(secret)));
    }

    #[test]
    fn discards_text_that_smuggles_secret_through_newlines() {
        let secret = "reflected-secret-token";
        let cleaned = clean_server_text("reflected-\nsecret-token", secret, false, CAP);
        assert_eq!(cleaned, REDACTED);
    }

    #[test]
    fn discards_text_that_smuggles_secret_with_changed_case() {
        let secret = "reflected-secret-token";
        let cleaned = clean_server_text("REFLECTED-\tSECRET-TOKEN", secret, false, CAP);
        assert_eq!(cleaned, REDACTED);
    }

    #[test]
    fn capped_body_drops_short_trailing_fragment() {
        // PoC from the adversarial review: the read cap leaves a 2-char
        // token prefix, shorter than MIN_SECRET_FRAGMENT_CHARS.
        let secret = "reflected-secret-token";
        let cleaned = clean_server_text("\n\n\nre", secret, true, CAP);
        assert_eq!(cleaned, REDACTED);
        assert!(!cleaned.contains("re"));
    }

    #[test]
    fn capped_body_keeps_leading_words() {
        let cleaned = clean_server_text("database is down, retry lat", "secret-token", true, CAP);
        assert_eq!(cleaned, "database is down, retry ***");
    }

    #[test]
    fn uncapped_body_keeps_its_last_word() {
        let cleaned = clean_server_text("document not found", "secret-token", false, CAP);
        assert_eq!(cleaned, "document not found");
    }

    #[test]
    fn short_secret_does_not_discard_incidental_match() {
        // A secret shorter than the fragment minimum must not make every
        // message vanish.
        let cleaned = clean_server_text("abc is not a valid id", "abc", false, CAP);
        assert_eq!(cleaned, "*** is not a valid id");
    }

    #[test]
    fn empty_secret_leaves_text_alone() {
        assert_eq!(
            clean_server_text("plain message", "", false, CAP),
            "plain message"
        );
    }

    #[test]
    fn caps_length_after_redaction() {
        let cleaned = clean_server_text(&"x".repeat(500), "secret-token", false, CAP);
        assert_eq!(cleaned.chars().count(), CAP);
    }

    #[test]
    fn strips_control_characters_and_collapses_whitespace() {
        let cleaned =
            clean_server_text("remote\nfailure\x1b[31m  here", "secret-token", false, CAP);
        assert!(!cleaned.contains('\x1b'));
        assert!(!cleaned.contains('\n'));
        assert_eq!(cleaned, "remote failure [31m here");
    }
}
