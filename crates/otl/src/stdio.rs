//! Non-panicking stdout/stderr writes.
//!
//! `println!`/`eprintln!` panic when a write fails, which turns the routine
//! case of a script closing the pipe early (`otl ... | head -1`) into exit
//! code 101 plus a panic message - neither of which is in the public
//! exit-code table. Every write on a user-visible path goes through this
//! module instead.
//!
//! Broken pipe is treated as normal completion: the reader asked us to
//! stop, so we stop quietly (no diagnostics, exit code 0), the way
//! well-behaved Unix filters do.

use std::io::{self, ErrorKind, Write};

use anyhow::anyhow;

use crate::exit::CliError;

/// Write one line of DATA to stdout.
///
/// - broken pipe: `Ok(())` - the consumer stopped reading; exit quietly.
/// - any other write failure: a generic failure (exit code 1).
pub fn write_data_line(text: &str) -> Result<(), CliError> {
    write_stdout(|handle| writeln!(handle, "{text}"))
}

/// Write DATA to stdout verbatim (no trailing newline added).
///
/// Same failure contract as [`write_data_line`]: for multi-line payloads
/// that already carry their own line endings, such as `otl api list`.
pub fn write_data(text: &str) -> Result<(), CliError> {
    write_stdout(|handle| handle.write_all(text.as_bytes()))
}

/// Run one write against a locked stdout and classify its outcome.
fn write_stdout(
    write: impl FnOnce(&mut io::StdoutLock<'_>) -> io::Result<()>,
) -> Result<(), CliError> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    match write(&mut handle).and_then(|()| handle.flush()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(CliError::failure(anyhow!(
            "failed writing to stdout: {}",
            error.kind()
        ))),
    }
}

/// Write one line of DIAGNOSTICS to stderr, ignoring any write failure.
///
/// Diagnostics are best-effort by definition: if stderr is closed there is
/// nowhere left to report that fact, and panicking would replace the real
/// exit code with 101.
///
/// Every diagnostic passes through [`scrub_terminal_controls`] on the way
/// out. See that function for why the scrub lives HERE rather than at each
/// place that builds a message.
pub fn write_diagnostic_line(text: &str) {
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    let _ = writeln!(handle, "{}", scrub_terminal_controls(text));
    let _ = handle.flush();
}

/// Remove everything a terminal would EXECUTE, keeping newlines.
///
/// Diagnostics are assembled from authored prose plus values that came from
/// somewhere else - a server, a filesystem, a config file - and a terminal
/// treats some of those bytes as commands rather than text: `ESC ] 52` sets
/// the system clipboard, `ESC ] 8` forges a hyperlink, others move the
/// cursor or repaint the screen. None of that belongs in a diagnostic.
///
/// The scrub is at the SINK on purpose. Doing it per call site means every
/// new message that interpolates a foreign value has to remember; putting
/// it here makes the property hold for every message, present or future.
///
/// Newlines survive, because a diagnostic legitimately spans lines (the
/// export summary lists one failure per line) and collapsing those would
/// make authored output worse to read. A foreign value could therefore
/// still introduce a line break, which is why values that must stay on one
/// line ALSO go through [`crate::text::quote`] at their call site. The two
/// are layers, not alternatives: this one bounds the damage of forgetting,
/// that one is precise about a particular value.
///
/// Every other hazard [`crate::text::hazard`] knows about goes too. The
/// match below is exhaustive on purpose: a new category added to that enum
/// has to be answered here rather than silently falling through to
/// "forward it".
///
/// Note what the answers say about this surface. A diagnostic is prose, so
/// a removed character needs no visible marker (unlike a table cell, where
/// `render` substitutes one to keep the width honest). And `Joiner` is
/// dropped rather than kept, because a diagnostic quotes a NAME - an id, a
/// path, a title being reported - and two names that render identically
/// while differing underneath is the problem, not the emoji.
pub fn scrub_terminal_controls(text: &str) -> String {
    use crate::text::Hazard;

    text.chars()
        .filter_map(|c| match crate::text::hazard(c) {
            None => Some(c),
            // The one hazard that survives, and only this one: see above.
            Some(Hazard::Control) if c == '\n' => Some(c),
            Some(Hazard::Control) => Some(' '),
            Some(Hazard::BidiFormat | Hazard::Invisible | Hazard::Joiner) => None,
        })
        .collect()
}

/// [`scrub_terminal_controls`], and then force the result onto ONE line.
///
/// For the human renderings that are built as a LIST of lines and joined:
/// `otl doctor`'s report and every `otl auth` result. There, a foreign value
/// that arrived with a newline - a profile name out of a config file, an
/// account name from the server, a path from the environment - must not be
/// able to add an entry and pose as another line's verdict.
///
/// The newline exception in [`scrub_terminal_controls`] exists because an
/// authored diagnostic legitimately spans lines. That is a property of the
/// MESSAGE, not of a value inside it, so the surfaces that assemble their own
/// lines take this instead. Shared rather than copied per surface: the two
/// that need it reached for it independently, and a second copy is how one of
/// them ends up without the fold.
pub fn scrub_to_one_line(text: &str) -> String {
    scrub_terminal_controls(text).replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_scrubbing_folds_a_forged_entry_back_into_its_line() {
        // A profile name carrying a newline would otherwise read as a second
        // entry in whatever list it was interpolated into.
        let folded = scrub_to_one_line("profile: real\nmethod: forged");
        assert!(!folded.contains('\n'), "{folded:?}");
        assert!(folded.starts_with("profile: real"), "{folded:?}");
        // And the hazards `scrub_terminal_controls` removes still go, each
        // the way that function decided: a bidi override is DROPPED (it
        // occupies no column, so a marker would widen the text) and a
        // control becomes a space (it usually stands where one belongs).
        assert_eq!(scrub_to_one_line("a\u{202e}b\u{1b}c"), "ab c");
    }

    #[test]
    fn scrubbing_removes_terminal_control_sequences() {
        // OSC 52 sets the system clipboard; a diagnostic must not be able to.
        let scrubbed = scrub_terminal_controls("a\u{1b}]52;c;cGF5bG9hZA==\u{7}b");
        assert!(!scrubbed.contains('\u{1b}'), "{scrubbed:?}");
        assert!(!scrubbed.contains('\u{7}'), "{scrubbed:?}");
        assert!(scrubbed.starts_with('a') && scrubbed.ends_with('b'));
    }

    #[test]
    fn scrubbing_keeps_newlines_so_multi_line_diagnostics_survive() {
        // The export summary is one failure per line; collapsing it would
        // make authored output worse, and the per-value `text::quote` is
        // what keeps a foreign value from introducing a break.
        assert_eq!(
            scrub_terminal_controls("summary:\n  a: reason\n  b: reason"),
            "summary:\n  a: reason\n  b: reason"
        );
    }

    #[test]
    fn scrubbing_removes_carriage_returns_and_other_controls() {
        // `\r` alone would let a message overwrite the line before it.
        assert_eq!(scrub_terminal_controls("a\rb\tc\u{0}d"), "a b c d");
    }

    #[test]
    fn scrubbing_removes_reordering_format_characters() {
        assert_eq!(scrub_terminal_controls("report-\u{202e}fdp"), "report-fdp");
    }

    #[test]
    fn scrubbing_leaves_ordinary_text_alone() {
        for text in [
            "warning: something happened",
            "\u{4e2d}\u{6587}\u{8bca}\u{65ad}",
            "path/to/file.md",
            "",
        ] {
            assert_eq!(scrub_terminal_controls(text), text);
        }
    }
}
