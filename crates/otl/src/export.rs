//! Turning document titles into safe file names.
//!
//! Every name written by `otl docs export` comes from a document title,
//! which is arbitrary server-controlled text. This module is the single
//! place that turns such text into ONE path component, and it is
//! deliberately paranoid: the output can never contain a path separator, a
//! `..` segment, a character Windows rejects, a Windows device name, a
//! trailing dot or space, an invisible re-ordering character, or a name
//! that collides with one already used in the same directory - including on
//! a case-insensitive filesystem.
//!
//! Nothing here touches the filesystem, so all of it is unit-testable.

use std::collections::HashSet;

/// Replacement for every character that cannot appear in a file name.
const REPLACEMENT: char = '-';
/// Prefix that makes a Windows device name usable again.
const RESERVED_PREFIX: char = '_';
/// Name used when a title sanitizes down to nothing.
const FALLBACK_STEM: &str = "untitled";
/// Maximum length in BYTES of one generated path component (before the
/// `.md` extension).
///
/// Filesystem limits are per byte, not per character: 255 bytes is the
/// common ceiling, and a CJK title reaches it three times faster than an
/// ASCII one. The cap leaves room for the extension and a de-duplication
/// suffix.
const MAX_STEM_BYTES: usize = 96;
/// Highest de-duplication suffix tried before giving up.
const MAX_DEDUP_ATTEMPTS: u32 = 10_000;

/// Characters that are illegal in a Windows file name, plus the two path
/// separators. `:` is included for macOS as well, where it is a separator
/// in some path APIs.
const ILLEGAL: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Windows device names. Reserved with or without an extension, in any
/// letter case, so `CON`, `con.md` and `CoN.txt` are all unusable.
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
    "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    "clock$", "conin$", "conout$",
];

/// Invisible characters that can reorder or hide the rest of a name.
///
/// They are not `char::is_control`, but a right-to-left override in a file
/// name makes `report-<RLO>fdp.md` look like `report-md.pdf`. Names are
/// security-relevant output, so these are dropped.
const INVISIBLE: &[char] = &[
    '\u{200b}', '\u{200c}', '\u{200d}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}',
    '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{feff}',
];

/// Sanitize one arbitrary string into a single safe path component.
///
/// The result is never empty, never longer than [`MAX_STEM_BYTES`] bytes,
/// contains no separator or illegal character, is not a Windows device
/// name, and neither starts nor ends with a dot or a space.
pub fn safe_stem(raw: &str) -> String {
    let replaced = replace_illegal(raw);
    let dotted = neutralize_dot_runs(&replaced);
    let collapsed = collapse_runs(&dotted);
    let trimmed = trim_edges(&collapsed);
    let capped = truncate_bytes(trimmed, MAX_STEM_BYTES);
    // Truncation can re-expose a trailing dot or space, so trim again.
    let capped = trim_edges(capped);
    if capped.is_empty() {
        return FALLBACK_STEM.to_string();
    }
    if is_reserved(capped) {
        // Prefixing keeps the original readable while making the name
        // usable on Windows; re-cap in case the prefix pushed it over.
        let prefixed = format!("{RESERVED_PREFIX}{capped}");
        return truncate_bytes(&prefixed, MAX_STEM_BYTES).to_string();
    }
    capped.to_string()
}

/// Map every character that cannot appear in a file name to
/// [`REPLACEMENT`], and drop the invisible ones entirely.
fn replace_illegal(raw: &str) -> String {
    raw.chars()
        .filter(|c| !INVISIBLE.contains(c))
        .map(|c| {
            if c.is_control() || ILLEGAL.contains(&c) {
                REPLACEMENT
            } else {
                c
            }
        })
        .collect()
}

/// Replace every run of two or more dots with a single [`REPLACEMENT`].
///
/// A single dot is a legitimate part of a title (`v1.2 release notes`), but
/// `..` is the parent-directory name. Even inside one component it is worth
/// removing: nothing good comes of a file called `..-..-etc-passwd.md`, and
/// a later normalization step elsewhere must never be able to read it as a
/// traversal.
fn neutralize_dot_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut dots = 0_usize;
    let flush = |out: &mut String, dots: usize| match dots {
        0 => {}
        1 => out.push('.'),
        _ => out.push(REPLACEMENT),
    };
    for c in text.chars() {
        if c == '.' {
            dots += 1;
            continue;
        }
        flush(&mut out, dots);
        dots = 0;
        out.push(c);
    }
    flush(&mut out, dots);
    out
}

/// Collapse runs of whitespace to one space and runs of [`REPLACEMENT`] to
/// one, so a title of nothing but slashes does not become a wall of dashes.
fn collapse_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous: Option<char> = None;
    for c in text.chars() {
        let normalized = if c.is_whitespace() { ' ' } else { c };
        let repeated =
            previous == Some(normalized) && (normalized == ' ' || normalized == REPLACEMENT);
        if !repeated {
            out.push(normalized);
        }
        previous = Some(normalized);
    }
    out
}

/// Trim characters that must not sit at either end of a name.
///
/// Windows silently strips trailing dots and spaces, which would make
/// `foo.` and `foo` the same file; a leading dot would hide the export; a
/// leading dash would make the path look like a flag to the next tool that
/// reads it.
fn trim_edges(text: &str) -> &str {
    text.trim_matches(|c: char| c == '.' || c.is_whitespace() || c == REPLACEMENT)
}

/// Truncate to at most `limit` bytes, never splitting a character.
fn truncate_bytes(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Whether a name is a Windows device name (extension and case ignored).
fn is_reserved(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name);
    let lowered = base.to_ascii_lowercase();
    RESERVED.contains(&lowered.as_str())
}

/// The names already used inside ONE directory.
///
/// Uniqueness is tracked case-insensitively because macOS and Windows
/// filesystems are: `Deploy.md` and `deploy.md` are one file there, and
/// exporting both would silently lose a document.
#[derive(Debug, Default)]
pub struct Names {
    used: HashSet<String>,
}

impl Names {
    /// An empty name set for a fresh directory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim a unique safe stem derived from `title`.
    ///
    /// The stem is reserved for both `<stem>.md` and a `<stem>/` directory,
    /// so a document and its children's folder can share it while a second
    /// document with the same title cannot.
    pub fn claim(&mut self, title: &str) -> String {
        let base = safe_stem(title);
        if self.insert(&base) {
            return base;
        }
        for suffix in 2..=MAX_DEDUP_ATTEMPTS {
            let candidate = with_suffix(&base, suffix);
            if self.insert(&candidate) {
                return candidate;
            }
        }
        // Unreachable for any realistic collection: it would need 10_000
        // documents with the same title in one directory. Falling back to a
        // long random-ish name is still better than returning a duplicate.
        let unique = with_suffix(&base, MAX_DEDUP_ATTEMPTS + self.used.len() as u32);
        let _ = self.insert(&unique);
        unique
    }

    /// Claim an already-sanitized stem verbatim, de-duplicating if taken.
    ///
    /// Used for a document's own file inside the directory named after it,
    /// so that `Deploy-2/` holds `Deploy-2.md` and not `Deploy.md`: the
    /// directory and the document it belongs to carry the same name.
    pub fn claim_exact(&mut self, stem: &str) -> String {
        if self.insert(stem) {
            return stem.to_string();
        }
        self.claim(stem)
    }

    /// Record a name, reporting whether it was still free.
    fn insert(&mut self, name: &str) -> bool {
        self.used.insert(collision_key(name))
    }
}

/// The key two names collide under.
///
/// Two things have to be folded away, because two filesystems in wide use
/// fold them and would otherwise turn two documents into one file:
///
/// - **case**, via full Unicode lowercasing (not just ASCII): macOS and
///   Windows are case-insensitive by default;
/// - **normalization**, via NFC: `é` written as one codepoint (U+00E9) and
///   as `e` + a combining acute (U+0065 U+0301) are DIFFERENT byte strings
///   but the same directory entry on a macOS volume.
///
/// NFC rather than NFKC on purpose: NFKC would also fold compatibility
/// characters (`ﬁ` to `fi`, full-width to ASCII), which real filesystems do
/// not, so it would merge names that can coexist.
///
/// The fold is deliberately conservative, so a filesystem with even broader
/// equivalence could still fuse two names. That is caught downstream rather
/// than assumed away: `otl docs export` re-checks the identity of each file
/// it writes and reports a collision instead of counting two exports.
fn collision_key(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    name.to_lowercase().nfc().collect()
}

/// `base` with a `-N` de-duplication suffix, keeping the byte cap.
fn with_suffix(base: &str, suffix: u32) -> String {
    let tail = format!("{REPLACEMENT}{suffix}");
    let room = MAX_STEM_BYTES.saturating_sub(tail.len());
    let head = trim_edges(truncate_bytes(base, room));
    if head.is_empty() {
        return format!("{FALLBACK_STEM}{tail}");
    }
    format!("{head}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_path_separators_and_traversal() {
        // The single most important property: one title can never become
        // more than one path component, and never an escape upwards.
        for title in [
            "../../etc/passwd",
            "..\\..\\windows\\system32",
            "a/b",
            "a\\b",
            "..",
            ".",
            "...",
            "/",
        ] {
            let stem = safe_stem(title);
            assert!(!stem.contains('/'), "{title:?} -> {stem:?}");
            assert!(!stem.contains('\\'), "{title:?} -> {stem:?}");
            assert!(!stem.is_empty(), "{title:?} -> empty");
            assert_ne!(stem, "..", "{title:?}");
            assert_ne!(stem, ".", "{title:?}");
            assert!(!stem.starts_with('.'), "{title:?} -> hidden file {stem:?}");
        }
        assert_eq!(safe_stem("../../etc/passwd"), "etc-passwd");
        assert_eq!(safe_stem(".."), FALLBACK_STEM);
    }

    #[test]
    fn replaces_characters_windows_rejects() {
        let stem = safe_stem("a<b>c:d\"e|f?g*h");
        for illegal in ILLEGAL {
            assert!(!stem.contains(*illegal), "kept {illegal:?} in {stem:?}");
        }
    }

    #[test]
    fn drops_control_characters_and_newlines() {
        let stem = safe_stem("re\u{1b}[31mport\nline\u{0}");
        assert!(
            !stem.chars().any(char::is_control),
            "control character survived: {stem:?}"
        );
    }

    #[test]
    fn drops_invisible_reordering_characters() {
        // `report-<RLO>fdp.md` renders as `report-md.pdf`.
        let stem = safe_stem("report-\u{202e}fdp");
        assert!(!stem.contains('\u{202e}'), "{stem:?}");
        assert_eq!(stem, "report-fdp");
    }

    #[test]
    fn escapes_windows_device_names() {
        for reserved in ["CON", "con", "CoN", "nul", "COM1", "lpt9", "aux", "prn"] {
            let stem = safe_stem(reserved);
            assert!(!is_reserved(&stem), "{reserved:?} -> {stem:?}");
        }
        // Reserved with an extension too: `con.md` is unusable on Windows,
        // and the exporter appends `.md` to every stem.
        assert!(!is_reserved(&safe_stem("con.txt")));
        // A name that merely starts with a device name is fine.
        assert_eq!(safe_stem("console"), "console");
        assert_eq!(safe_stem("com10"), "com10");
    }

    #[test]
    fn trims_trailing_dots_and_spaces() {
        // Windows strips these silently, which would fuse two names.
        assert_eq!(safe_stem("Notes... "), "Notes");
        assert_eq!(safe_stem("  Notes  "), "Notes");
        assert_eq!(safe_stem(".hidden"), "hidden");
    }

    #[test]
    fn empty_and_whitespace_only_titles_get_a_fallback() {
        for title in ["", "   ", "\t\n", "....", "///"] {
            assert!(!safe_stem(title).is_empty(), "{title:?}");
        }
        assert_eq!(safe_stem(""), FALLBACK_STEM);
        assert_eq!(safe_stem("   "), FALLBACK_STEM);
    }

    #[test]
    fn caps_the_length_in_bytes_without_splitting_characters() {
        let long = "\u{4e2d}".repeat(200); // 3 bytes each
        let stem = safe_stem(&long);
        assert!(stem.len() <= MAX_STEM_BYTES, "{} bytes", stem.len());
        assert!(stem.chars().all(|c| c == '\u{4e2d}'), "split a character");

        let ascii = "a".repeat(500);
        assert_eq!(safe_stem(&ascii).len(), MAX_STEM_BYTES);
    }

    #[test]
    fn truncation_does_not_leave_a_trailing_dot() {
        // A cap that lands right after a dot would recreate the Windows
        // trailing-dot problem.
        let title = format!("{}.tail", "a".repeat(MAX_STEM_BYTES - 1));
        let stem = safe_stem(&title);
        assert!(!stem.ends_with('.'), "{stem:?}");
    }

    #[test]
    fn collapses_runs_of_replacements() {
        assert_eq!(safe_stem("a////b"), "a-b");
        assert_eq!(safe_stem("a    b"), "a b");
    }

    #[test]
    fn single_dots_are_part_of_a_title_but_dot_runs_are_not() {
        assert_eq!(safe_stem("v1.2 release notes"), "v1.2 release notes");
        assert_eq!(safe_stem("a..b"), "a-b");
    }

    #[test]
    fn unique_titles_keep_their_names() {
        let mut names = Names::new();
        assert_eq!(names.claim("Deploy"), "Deploy");
        assert_eq!(names.claim("Runbook"), "Runbook");
    }

    #[test]
    fn duplicate_titles_get_numbered() {
        let mut names = Names::new();
        assert_eq!(names.claim("Deploy"), "Deploy");
        assert_eq!(names.claim("Deploy"), "Deploy-2");
        assert_eq!(names.claim("Deploy"), "Deploy-3");
    }

    #[test]
    fn collisions_are_detected_case_insensitively() {
        // macOS and Windows would otherwise fuse the two files and lose a
        // document.
        let mut names = Names::new();
        assert_eq!(names.claim("Deploy"), "Deploy");
        assert_eq!(names.claim("deploy"), "deploy-2");
        assert_eq!(names.claim("DEPLOY"), "DEPLOY-3");
    }

    #[test]
    fn titles_that_sanitize_to_the_same_name_are_still_distinct() {
        // Different titles, one safe name: they must not overwrite.
        let mut names = Names::new();
        assert_eq!(names.claim("a/b"), "a-b");
        assert_eq!(names.claim("a\\b"), "a-b-2");
        assert_eq!(names.claim(""), FALLBACK_STEM);
        assert_eq!(names.claim("   "), format!("{FALLBACK_STEM}-2"));
    }

    #[test]
    fn deduplicated_names_stay_within_the_byte_cap() {
        let mut names = Names::new();
        let long = "\u{4e2d}".repeat(200);
        for _ in 0..5 {
            let stem = names.claim(&long);
            assert!(
                stem.len() <= MAX_STEM_BYTES,
                "{stem:?} is {} bytes",
                stem.len()
            );
        }
    }

    #[test]
    fn collisions_are_detected_across_unicode_normalization_forms() {
        // NFC `é` and NFD `e`+U+0301 are one directory entry on macOS. Two
        // documents must not both claim it.
        let mut names = Names::new();
        assert_eq!(names.claim("Caf\u{e9}"), "Caf\u{e9}");
        assert_eq!(
            names.claim("Cafe\u{301}"),
            "Cafe\u{301}-2",
            "NFD spelling of the same title reused the name"
        );
    }

    #[test]
    fn normalization_and_case_folding_combine() {
        let mut names = Names::new();
        assert_eq!(names.claim("\u{c9}clair"), "\u{c9}clair"); // NFC É
        assert_eq!(names.claim("e\u{301}clair"), "e\u{301}clair-2"); // NFD é
    }

    #[test]
    fn compatibility_characters_are_not_folded_away() {
        // NFKC would merge these; no mainstream filesystem does, so folding
        // them would refuse names that can legitimately coexist.
        let mut names = Names::new();
        assert_eq!(names.claim("\u{fb01}le"), "\u{fb01}le"); // U+FB01 LATIN SMALL LIGATURE FI
        assert_eq!(names.claim("file"), "file");
    }

    #[test]
    fn distinct_titles_still_get_distinct_names() {
        // Guard against an over-eager fold: normalization must not collapse
        // genuinely different names.
        let mut names = Names::new();
        for title in ["alpha", "beta", "\u{4e2d}\u{6587}", "\u{e9}", "\u{ea}"] {
            let claimed = names.claim(title);
            assert!(
                !claimed.ends_with("-2"),
                "{title:?} collided with an earlier name as {claimed:?}"
            );
        }
    }

    #[test]
    fn claim_exact_keeps_an_already_safe_stem() {
        // A document's own file inside its own directory must carry the
        // directory's name, suffix included.
        let mut names = Names::new();
        assert_eq!(names.claim_exact("Deploy-2"), "Deploy-2");
        assert_eq!(names.claim_exact("Deploy-2"), "Deploy-2-2");
    }

    #[test]
    fn every_claimed_name_is_unique_over_many_collisions() {
        let mut names = Names::new();
        let claimed: HashSet<String> = (0..500)
            .map(|_| names.claim("Same Title").to_lowercase())
            .collect();
        assert_eq!(claimed.len(), 500, "a claimed name repeated");
    }
}
