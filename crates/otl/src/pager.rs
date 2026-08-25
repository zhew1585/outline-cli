//! `$PAGER` handling for long output.
//!
//! Paging is an interactive convenience, never part of the data contract:
//! it happens only when stdout is a terminal AND the content does not fit
//! on one screen. A pipe, a redirect, `--json` or `--raw` all write straight
//! to stdout (see [`crate::render::OutputMode`]), so a script always gets
//! the bytes and nothing else.
//!
//! The pager is spawned with the content on its stdin - never through a
//! shell and never with the content as an argument - so a document body
//! cannot become part of a command line.

use std::ffi::OsString;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::exit::CliError;
use crate::stdio;

/// Environment variable naming the pager command.
pub const ENV_PAGER: &str = "PAGER";
/// Pager used when `$PAGER` is unset or empty.
const DEFAULT_PAGER: &str = "less";
/// Arguments given to the default pager.
///
/// `-R` keeps ANSI sequences that a document body may legitimately contain
/// from being escaped into noise; `-F` makes even a mis-measured short
/// document behave like plain output; `-X` stops `less` from clearing the
/// screen on exit, so the document stays in the scrollback.
const DEFAULT_PAGER_ARGS: &[&str] = &["-R", "-F", "-X"];
/// Terminal height assumed when it cannot be measured.
const FALLBACK_HEIGHT: u16 = 24;

/// A pager to spawn: a program plus its arguments, already split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pager {
    /// Program to run.
    pub program: OsString,
    /// Arguments passed to it.
    pub args: Vec<OsString>,
}

impl Pager {
    /// The pager named by `$PAGER`, or `less` with sensible defaults.
    ///
    /// `$PAGER` is split on ASCII whitespace only: no shell is involved, so
    /// quoting and metacharacters are not interpreted (and cannot be used
    /// to smuggle a second command).
    pub fn from_env() -> Option<Self> {
        Self::parse(std::env::var_os(ENV_PAGER))
    }

    /// [`Pager::from_env`] with the environment value supplied explicitly.
    ///
    /// `Some("")` (and any all-whitespace value) means "no pager": a user
    /// who sets `PAGER=` has asked for plain output.
    pub fn parse(value: Option<OsString>) -> Option<Self> {
        let Some(value) = value else {
            return Some(Self::default_pager());
        };
        // Splitting needs the value as text; a non-UTF-8 `$PAGER` is used
        // verbatim as a program name instead of being silently ignored.
        let Some(text) = value.to_str() else {
            return Some(Self {
                program: value,
                args: Vec::new(),
            });
        };
        let mut words = text.split_whitespace();
        let program = words.next()?;
        Some(Self {
            program: OsString::from(program),
            args: words.map(OsString::from).collect(),
        })
    }

    /// The built-in default pager.
    fn default_pager() -> Self {
        Self {
            program: OsString::from(DEFAULT_PAGER),
            args: DEFAULT_PAGER_ARGS.iter().map(OsString::from).collect(),
        }
    }
}

/// Write `text` to stdout, through a pager when it is worth it.
///
/// `paginate` must already encode the dual-state decision (terminal stdout,
/// no `--json`/`--raw`). Even then the pager is skipped when the content
/// fits on one screen.
///
/// A pager that cannot be started is a convenience failure, not a command
/// failure: the content is written straight to stdout with a warning on
/// stderr, and the exit code stays 0.
pub fn write(text: &str, paginate: bool) -> Result<(), CliError> {
    if !paginate || !exceeds_screen(text, terminal_height()) {
        return stdio::write_data(&with_trailing_newline(text));
    }
    let Some(pager) = Pager::from_env() else {
        return stdio::write_data(&with_trailing_newline(text));
    };
    match run(&pager, &with_trailing_newline(text)) {
        Ok(()) => Ok(()),
        Err(reason) => {
            stdio::write_diagnostic_line(&format!(
                "warning: could not run the pager ({reason}); writing to stdout instead"
            ));
            stdio::write_data(&with_trailing_newline(text))
        }
    }
}

/// Feed `text` to a pager on its stdin and wait for it to finish.
///
/// Returns `Err` only when the pager could not be run at all. A pager the
/// user quits early closes its stdin, which shows up as a broken pipe here
/// and is normal completion - the same rule stdout writes follow.
fn run(pager: &Pager, text: &str) -> Result<(), String> {
    let mut child = Command::new(&pager.program)
        .args(&pager.args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{}: {}", pager.program.to_string_lossy(), error.kind()))?;
    if let Some(mut stdin) = child.stdin.take() {
        // Ignore write failures: a pager that exits early (`q` on the first
        // screen) closes the pipe, which is not an error.
        let _ = stdin.write_all(text.as_bytes());
        let _ = stdin.flush();
    }
    // The pager's own exit status is the user's business, not ours: quitting
    // with `q` is success, and a non-zero status must not become our own.
    let _ = child.wait();
    Ok(())
}

/// Whether `text` needs more than one screen of `height` rows.
///
/// One row is reserved for the shell prompt that follows the output, so
/// content exactly as tall as the terminal is still printed plainly.
pub fn exceeds_screen(text: &str, height: u16) -> bool {
    let budget = usize::from(height).saturating_sub(1);
    // Counting all lines of a huge document is wasted work; stop as soon as
    // the budget is exceeded.
    text.lines().take(budget + 1).count() > budget
}

/// The terminal height in rows, falling back to [`FALLBACK_HEIGHT`].
fn terminal_height() -> u16 {
    terminal_size::terminal_size()
        .map(|(_, terminal_size::Height(rows))| rows)
        .filter(|rows| *rows > 0)
        .unwrap_or(FALLBACK_HEIGHT)
}

/// `text` with exactly one trailing newline (and no newline added to empty
/// content, which would otherwise print a blank line).
fn with_trailing_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        return text.to_string();
    }
    format!("{text}\n")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn unset_pager_falls_back_to_less() {
        let pager = Pager::parse(None).unwrap();
        assert_eq!(pager.program, OsString::from("less"));
        assert!(pager.args.contains(&OsString::from("-R")));
    }

    #[test]
    fn empty_pager_disables_paging() {
        // `PAGER=` is an explicit request for plain output.
        assert_eq!(Pager::parse(Some(OsString::from(""))), None);
        assert_eq!(Pager::parse(Some(OsString::from("   "))), None);
    }

    #[test]
    fn pager_value_is_split_without_a_shell() {
        let pager = Pager::parse(Some(OsString::from("less -S -N"))).unwrap();
        assert_eq!(pager.program, OsString::from("less"));
        assert_eq!(pager.args, vec![OsString::from("-S"), OsString::from("-N")]);
    }

    #[test]
    fn pager_metacharacters_are_not_interpreted() {
        // No shell means `;` and `|` are just argument text, so a crafted
        // $PAGER cannot chain a second command.
        let pager = Pager::parse(Some(OsString::from("less; rm -rf /"))).unwrap();
        assert_eq!(pager.program, OsString::from("less;"));
        assert_eq!(
            pager.args,
            vec![
                OsString::from("rm"),
                OsString::from("-rf"),
                OsString::from("/")
            ]
        );
    }

    #[test]
    fn short_content_does_not_need_a_pager() {
        let text = "a\nb\nc";
        assert!(!exceeds_screen(text, 24));
        assert!(!exceeds_screen("", 24));
    }

    #[test]
    fn content_taller_than_the_screen_needs_a_pager() {
        let text = "x\n".repeat(40);
        assert!(exceeds_screen(&text, 24));
    }

    #[test]
    fn one_row_is_reserved_for_the_prompt() {
        let text = "x\n".repeat(23);
        assert!(!exceeds_screen(&text, 24), "23 lines fit a 24-row screen");
        let text = "x\n".repeat(24);
        assert!(exceeds_screen(&text, 24), "24 lines do not");
    }

    #[test]
    fn tiny_or_zero_heights_do_not_underflow() {
        assert!(exceeds_screen("a", 1));
        assert!(exceeds_screen("a", 0));
    }

    #[test]
    fn trailing_newline_is_added_once() {
        assert_eq!(with_trailing_newline("a"), "a\n");
        assert_eq!(with_trailing_newline("a\n"), "a\n");
        assert_eq!(with_trailing_newline(""), "");
    }

    #[cfg(unix)]
    #[test]
    fn spawns_the_pager_with_content_on_stdin() {
        // A pager receives the document on stdin, never as an argument.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("captured");
        let pager = Pager {
            program: OsString::from("sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from(format!("cat > {}", out.display())),
            ],
        };
        run(&pager, "line one\nline two\n").unwrap();
        let captured = std::fs::read_to_string(&out).unwrap();
        assert_eq!(captured, "line one\nline two\n");
    }

    #[test]
    fn a_missing_pager_is_reported_rather_than_panicking() {
        let pager = Pager {
            program: OsString::from("otl-no-such-pager-9c7a"),
            args: Vec::new(),
        };
        assert!(run(&pager, "text").is_err());
    }
}
