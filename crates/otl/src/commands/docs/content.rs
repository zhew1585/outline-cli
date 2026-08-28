//! Reading document bodies from stdin or a file.
//!
//! `otl docs create` and `otl docs update` both take markdown from the same
//! two places, with the same rules, so both use this module:
//!
//! - `--file PATH` reads that file, and standard input is not touched at
//!   all. Precedence rather than a conflict error on purpose: whether a
//!   redirected stdin actually HOLDS anything cannot be known without
//!   reading it, and reading it could block forever on a pipe that never
//!   closes. A script whose stdin happens to be `/dev/null` must not fail
//!   for using `--file`, so `--file` simply wins (and says so in its help).
//! - otherwise, a stdin that is NOT a terminal is read to the end (this is
//!   the `cat notes.md | otl docs create` case);
//! - a stdin that IS a terminal means "no body was supplied". The command
//!   never blocks waiting for a human to type a document.
//!
//! Both sources also go through [`super::frontmatter`]: a body that opens
//! with the block `otl docs export` writes has it removed here, once, so no
//! write command can push this CLI's own metadata into a document as text.
//! What the block SAID is kept on the [`Body`] for `update` to act on.
//!
//! A source that yields nothing but whitespace also counts as "no body".
//! That test runs on the body AFTER the block is removed, so a file holding
//! nothing but frontmatter is "no body" - which is what makes `otl docs
//! update <id> --title X --file meta.md` a title change rather than an
//! attempt to store a metadata block.
//! That matters in both directions: `otl docs update <id> --title X` run
//! from a script (where stdin is `/dev/null` or closed) must not be read as
//! "replace the document with nothing", and an accidental `| otl docs
//! create` with no input must not store a blank document. Genuinely
//! blanking a body is possible, but only by spelling it out through `otl
//! api documents.update id=<id> text=`.
//!
//! Both sources are capped: the body is held in memory and copied into a
//! JSON request, so an unbounded read would be an out-of-memory abort
//! instead of a clean usage error.

use std::fs::File;
use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::anyhow;

use crate::exit::CliError;

use super::frontmatter::{self, FrontMatter};

/// Maximum accepted size of a document body, in bytes.
///
/// The same order of magnitude as the `--body` cap in `otl api`. Outline
/// enforces its own (smaller) limit server-side; this one exists purely so
/// that a huge or endless input fails as a usage error.
pub const MAX_CONTENT_BYTES: u64 = 8 * 1024 * 1024;

/// Where a document body came from, for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Piped or redirected standard input.
    Stdin,
    /// A `--file` path.
    File,
}

impl Origin {
    /// How to name this source in a message.
    fn label(self) -> &'static str {
        match self {
            Self::Stdin => "standard input",
            Self::File => "the --file argument",
        }
    }
}

/// A document body that was actually supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Body {
    /// The markdown text, with any `otl` frontmatter block removed.
    pub text: String,
    /// Where it came from.
    pub origin: Origin,
    /// What the removed block said, when there was one.
    pub front: Option<FrontMatter>,
}

/// Read the document body, if one was supplied.
///
/// `Ok(None)` means no usable body was offered: no `--file` and a terminal
/// stdin, or a source that held nothing but whitespace.
pub fn read(file: Option<&Path>) -> Result<Option<Body>, CliError> {
    read_with_stdin_tty(file, std::io::stdin().is_terminal())
}

/// [`read`] with the stdin-is-a-terminal decision supplied, for tests.
fn read_with_stdin_tty(file: Option<&Path>, stdin_is_tty: bool) -> Result<Option<Body>, CliError> {
    if let Some(path) = file {
        return Ok(supplied(read_file(path)?, Origin::File));
    }
    if stdin_is_tty {
        return Ok(None);
    }
    Ok(supplied(read_stdin()?, Origin::Stdin))
}

/// Wrap read text as a body, unless it is blank (see the module docs).
///
/// The frontmatter block comes off BEFORE the blankness test, so a file
/// that is nothing but a block reads as "no body" rather than as a body
/// made of metadata.
fn supplied(text: String, origin: Origin) -> Option<Body> {
    let (front, body) = frontmatter::split(&text);
    let text = body.to_string();
    (!text.trim().is_empty()).then_some(Body {
        text,
        origin,
        front,
    })
}

/// Read a file with the size cap applied.
fn read_file(path: &Path) -> Result<String, CliError> {
    let show = path.display().to_string();
    let io_error =
        |error: std::io::Error| CliError::usage(anyhow!("cannot read {show:?}: {}", error.kind()));
    let file = File::open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    // Cheap rejection first, then a bounded read: a file that grows between
    // the two - or whose reported size is meaningless, such as a fifo -
    // still cannot exhaust memory.
    if metadata.is_file() && metadata.len() > MAX_CONTENT_BYTES {
        return Err(too_large(Origin::File));
    }
    read_capped(file, Origin::File)
}

/// Read standard input to the end, with the size cap applied.
fn read_stdin() -> Result<String, CliError> {
    read_capped(std::io::stdin().lock(), Origin::Stdin)
}

/// Read at most [`MAX_CONTENT_BYTES`] of UTF-8 text.
fn read_capped(source: impl Read, origin: Origin) -> Result<String, CliError> {
    let mut text = String::new();
    let read = source
        .take(MAX_CONTENT_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                return CliError::usage(anyhow!(
                    "the document body from {} is not valid UTF-8",
                    origin.label()
                ));
            }
            CliError::usage(anyhow!(
                "cannot read the document body from {}: {}",
                origin.label(),
                error.kind()
            ))
        })?;
    if read as u64 > MAX_CONTENT_BYTES {
        return Err(too_large(origin));
    }
    Ok(text)
}

/// The over-the-cap error for one source.
fn too_large(origin: Origin) -> CliError {
    CliError::usage(anyhow!(
        "the document body from {} is too large: the limit is {MAX_CONTENT_BYTES} bytes",
        origin.label()
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::io::Write;

    use super::*;

    #[test]
    fn a_terminal_stdin_means_no_body() {
        // Never block waiting for a human to type a document.
        assert_eq!(read_with_stdin_tty(None, true).unwrap(), None);
    }

    #[test]
    fn a_file_is_read_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, "# Title\n\nbody\n").unwrap();
        let body = read_with_stdin_tty(Some(&path), true).unwrap().unwrap();
        assert_eq!(body.text, "# Title\n\nbody\n");
        assert_eq!(body.origin, Origin::File);
    }

    #[test]
    fn a_file_is_read_even_when_stdin_is_not_a_terminal() {
        // --file wins over stdin; the conflicting-sources check is what
        // stops the ambiguous case from getting this far.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, "from file").unwrap();
        let body = read_with_stdin_tty(Some(&path), false).unwrap().unwrap();
        assert_eq!(body.text, "from file");
    }

    #[test]
    fn a_missing_file_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.md");
        let error = read_with_stdin_tty(Some(&path), true).unwrap_err();
        assert_eq!(error.code, crate::exit::ExitCode::Usage);
    }

    #[test]
    fn an_oversized_file_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.md");
        let mut file = std::fs::File::create(&path).unwrap();
        // One byte over the cap, written in chunks to keep the test cheap.
        let chunk = vec![b'a'; 64 * 1024];
        let mut written = 0_u64;
        while written <= MAX_CONTENT_BYTES {
            file.write_all(&chunk).unwrap();
            written += chunk.len() as u64;
        }
        file.flush().unwrap();
        drop(file);
        let error = read_with_stdin_tty(Some(&path), true).unwrap_err();
        assert_eq!(error.code, crate::exit::ExitCode::Usage);
        assert!(error.to_string().contains("too large"), "{error}");
    }

    #[test]
    fn non_utf8_input_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bytes.md");
        std::fs::write(&path, [0xff_u8, 0xfe, 0xfd]).unwrap();
        let error = read_with_stdin_tty(Some(&path), true).unwrap_err();
        assert_eq!(error.code, crate::exit::ExitCode::Usage);
        assert!(error.to_string().contains("UTF-8"), "{error}");
    }

    #[test]
    fn a_blank_source_counts_as_no_body() {
        // `otl docs update <id> --title X` from a script has an empty stdin;
        // that must not read as "replace the body with nothing".
        let dir = tempfile::tempdir().unwrap();
        for content in ["", "   ", "\n\t\n"] {
            let path = dir.path().join("blank.md");
            std::fs::write(&path, content).unwrap();
            assert_eq!(
                read_with_stdin_tty(Some(&path), true).unwrap(),
                None,
                "{content:?} was taken as a body"
            );
        }
    }

    #[test]
    fn an_exported_files_frontmatter_is_removed_and_remembered() {
        // The round trip this feature exists for: what `otl docs export`
        // wrote must not come back as document text.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "---\n\
             outline_id: \"55baa74a\"\n\
             outline_url_id: \"engKBTOaWe\"\n\
             title: \"Notes\"\n\
             revision: 15\n\
             ---\n\
             \n\
             # Notes\n\
             \n\
             body\n",
        )
        .unwrap();
        let body = read_with_stdin_tty(Some(&path), true).unwrap().unwrap();
        assert_eq!(body.text, "# Notes\n\nbody\n");
        let front = body.front.unwrap();
        assert_eq!(front.id.as_deref(), Some("55baa74a"));
        assert_eq!(front.revision, Some(15));
    }

    #[test]
    fn a_file_that_is_only_frontmatter_counts_as_no_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.md");
        std::fs::write(&path, "---\noutline_id: \"doc-1\"\n---\n").unwrap();
        assert_eq!(read_with_stdin_tty(Some(&path), true).unwrap(), None);
    }

    #[test]
    fn a_document_opening_with_a_horizontal_rule_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rule.md");
        let text = "---\nNote: a rule, not metadata\n---\n\nbody\n";
        std::fs::write(&path, text).unwrap();
        let body = read_with_stdin_tty(Some(&path), true).unwrap().unwrap();
        assert_eq!(body.text, text);
        assert_eq!(body.front, None);
    }

    #[test]
    fn a_capped_read_accepts_exactly_the_limit() {
        let text = "a".repeat(MAX_CONTENT_BYTES as usize);
        let body = read_capped(text.as_bytes(), Origin::Stdin).unwrap();
        assert_eq!(body.len(), MAX_CONTENT_BYTES as usize);
    }

    #[test]
    fn a_capped_read_rejects_one_byte_over_the_limit() {
        let text = "a".repeat(MAX_CONTENT_BYTES as usize + 1);
        assert!(read_capped(text.as_bytes(), Origin::Stdin).is_err());
    }
}
