//! Writing document text to a terminal, and `$PAGER` handling.
//!
//! There are exactly two output paths, and the difference between them is
//! the whole point of this module:
//!
//! - **verbatim** (a pipe, a redirect, `--raw`): the bytes of the document
//!   and nothing else. No pager, no added newline, no filtering. A script
//!   that hashes or diffs the output must get what the API returned.
//! - **interactive** (a terminal, no `--raw`): a DISPLAY of the document.
//!   Control sequences are neutralized, a trailing newline is added so the
//!   shell prompt starts on its own line, and the result goes through
//!   `$PAGER` when it does not fit on one screen.
//!
//! The pager is spawned with the content on its stdin - never through a
//! shell and never with the content as an argument - so a document body
//! cannot become part of a command line.

use std::ffi::OsString;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::exit::CliError;
use crate::render;
use crate::stdio;

/// Environment variable naming the pager command.
pub const ENV_PAGER: &str = "PAGER";
/// Pager used when `$PAGER` is unset or empty.
const DEFAULT_PAGER: &str = "less";
/// Arguments given to the default pager.
///
/// `-F` makes even a mis-measured short document behave like plain output;
/// `-X` stops `less` from clearing the screen on exit, so the document stays
/// in the scrollback.
///
/// `-R` is deliberately NOT passed. It tells `less` to send ANSI sequences
/// through to the terminal, and the interactive path has already replaced
/// them - so `-R` could only ever matter for a sequence that slipped past
/// the filter, which is exactly the case where displaying it literally is
/// the safer outcome.
const DEFAULT_PAGER_ARGS: &[&str] = &["-F", "-X"];
/// Terminal height assumed when it cannot be measured.
const FALLBACK_HEIGHT: u16 = 24;
/// Terminal width assumed when it cannot be measured.
const FALLBACK_WIDTH: u16 = 80;
/// Tab stop width used when measuring how far a line reaches.
const TAB_WIDTH: usize = 8;
/// Stand-in for a control character that must not reach the terminal.
const CONTROL_REPLACEMENT: char = '\u{fffd}';

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

/// Write document text to stdout.
///
/// `interactive` must already encode the dual-state decision: true only for
/// a terminal stdout with neither `--json` nor `--raw`.
///
/// - `interactive == false`: the bytes go out VERBATIM. Nothing is added,
///   removed or replaced, because this is the data path.
/// - `interactive == true`: the text is prepared for display (see
///   [`for_display`]) and paged when it does not fit on one screen.
///
/// A pager that cannot be started is a convenience failure, not a command
/// failure: the content is written straight to stdout with a warning on
/// stderr, and the exit code stays 0.
pub fn write(text: &str, interactive: bool) -> Result<(), CliError> {
    if !interactive {
        return stdio::write_data(text);
    }
    let (display, replaced) = for_display(text);
    if replaced > 0 {
        stdio::write_diagnostic_line(&format!(
            "warning: the document contains {replaced} control character(s); \
             they were replaced for display - use --raw to get the bytes \
             unchanged"
        ));
    }
    let (width, height) = terminal_size();
    if !exceeds_screen(&display, height, width) {
        return stdio::write_data(&display);
    }
    let Some(pager) = Pager::from_env() else {
        return stdio::write_data(&display);
    };
    match run(&pager, &display) {
        Ok(()) => Ok(()),
        Err(reason) => {
            stdio::write_diagnostic_line(&format!(
                "warning: could not run the pager ({reason}); writing to stdout instead"
            ));
            stdio::write_data(&display)
        }
    }
}

/// Prepare document text for a terminal, returning it with the number of
/// characters that had to be replaced.
///
/// A document body is written by anyone who can edit the document, and a
/// terminal treats some of those bytes as COMMANDS, not text: `ESC ] 52` sets
/// the system clipboard, `ESC ] 8` makes any word a hyperlink to anywhere,
/// and plenty of sequences move the cursor or repaint what is already on
/// screen. None of that is markdown. On the display path every control
/// character other than a newline is therefore replaced with U+FFFD, which
/// keeps the text readable while making the substitution visible.
///
/// `\r` is dropped rather than replaced so that CRLF documents do not grow a
/// replacement marker on every line, and `\t` is expanded to spaces so
/// alignment survives without handing the terminal a control byte.
///
/// Bidirectional and zero-width formatting characters are left alone: they
/// are legitimate content in Arabic, Hebrew and Indic text, and unlike the
/// C0/C1 ranges they do not instruct the terminal to do anything.
fn for_display(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut replaced = 0_usize;
    for c in text.chars() {
        match c {
            '\n' => out.push('\n'),
            '\r' => {}
            '\t' => {
                // The tab stop is a TERMINAL column, so the text before it
                // has to be measured in columns too. Counting bytes puts
                // the stop in the wrong place after any non-ASCII text: a
                // CJK character is 3 bytes but 2 columns, a combining mark
                // is 2 bytes and 0 columns.
                let line_start = out.rfind('\n').map_or(0, |index| index + 1);
                let column = render::display_columns(&out[line_start..]);
                let pad = TAB_WIDTH - (column % TAB_WIDTH);
                out.extend(std::iter::repeat_n(' ', pad));
            }
            c if c.is_control() => {
                out.push(CONTROL_REPLACEMENT);
                replaced += 1;
            }
            c => out.push(c),
        }
    }
    if !out.is_empty() && !out.ends_with('\n') {
        // Display only: the shell prompt must not resume mid-line. The
        // verbatim path above never reaches this.
        out.push('\n');
    }
    (out, replaced)
}

/// Feed `text` to a pager on its stdin and wait for it to finish.
///
/// Returns `Err` when the pager did not do its job, so the caller can fall
/// back to writing the document to stdout.
///
/// The verdict is its EXIT STATUS, not how much it read. "How much it read"
/// looks like the sharper test - a pager the user quit has consumed part of
/// the document, one that never started has consumed none - but it cannot
/// be measured from here: a small document disappears into the pipe buffer
/// and every write "succeeds" whether or not anything on the other end ever
/// looked at it. `PAGER=false` on a short document is indistinguishable
/// from a pager that displayed it.
///
/// Status is unambiguous where it matters. Pagers exit 0 when the user
/// quits (`less`, `more`, `bat`, `most` all do), and non-zero when they
/// failed to run - so a non-zero status means the document may never have
/// been shown. The cost of being wrong is bounded and asymmetric: falling
/// back after a pager that did display the content prints it twice, which
/// is untidy; NOT falling back loses the document entirely while exiting 0.
/// The warning on the fallback path says which happened.
fn run(pager: &Pager, text: &str) -> Result<(), String> {
    let mut child = Command::new(&pager.program)
        .args(&pager.args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{}: {}", pager.program.to_string_lossy(), error.kind()))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A write failure here is expected when the user quits early: the
        // pager closed the pipe. The status below is what decides.
        let _ = stdin.write_all(text.as_bytes());
        let _ = stdin.flush();
    }
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "{} exited with {}",
            pager.program.to_string_lossy(),
            status
                .code()
                .map_or_else(|| "a signal".to_string(), |code| format!("status {code}"))
        )),
        Err(error) => Err(format!("cannot wait for the pager: {}", error.kind())),
    }
}

/// Whether `text` needs more than one screen of `height` rows at `width`
/// columns.
///
/// Counts SCREEN rows, not logical lines: a terminal wraps, so one 10,000
/// column line occupies 125 rows of an 80-column window and a document made
/// of one such line fills many screens. Counting `lines()` would call it a
/// single row and dump it past the top of the scrollback - the exact case
/// the pager exists for.
///
/// Width is measured in terminal columns via [`render::display_columns`], so
/// CJK (two columns per character) and combining marks (zero) are counted
/// the way the terminal counts them. Tabs have already been expanded by
/// [`for_display`].
///
/// One row is reserved for the shell prompt that follows the output, so
/// content exactly as tall as the terminal is still printed plainly.
pub fn exceeds_screen(text: &str, height: u16, width: u16) -> bool {
    let budget = usize::from(height).saturating_sub(1);
    // A zero width is not a one-column terminal, it is an unknown one:
    // treating it as 1 would page every document one character per row.
    let columns = usize::from(if width == 0 { FALLBACK_WIDTH } else { width });
    let mut rows = 0_usize;
    for line in text.lines() {
        rows += wrapped_rows(line, columns);
        // Stop as soon as the answer is settled: a huge document must not
        // cost a full measurement pass.
        if rows > budget {
            return true;
        }
    }
    rows > budget
}

/// How many terminal rows one logical line occupies at `columns` wide.
///
/// An empty line still occupies one row.
fn wrapped_rows(line: &str, columns: usize) -> usize {
    let width = render::display_columns(line);
    if width == 0 {
        return 1;
    }
    // Ceiling division: a line one column past the edge takes a second row.
    width.div_ceil(columns)
}

/// The terminal size in (columns, rows), with documented fallbacks.
fn terminal_size() -> (u16, u16) {
    match terminal_size::terminal_size() {
        Some((terminal_size::Width(columns), terminal_size::Height(rows)))
            if columns > 0 && rows > 0 =>
        {
            (columns, rows)
        }
        _ => (FALLBACK_WIDTH, FALLBACK_HEIGHT),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn unset_pager_falls_back_to_less() {
        let pager = Pager::parse(None).unwrap();
        assert_eq!(pager.program, OsString::from("less"));
        assert!(pager.args.contains(&OsString::from("-F")));
        // `-R` must NOT be passed: it would let `less` forward an escape
        // sequence that slipped past the display filter to the terminal.
        assert!(
            !pager.args.contains(&OsString::from("-R")),
            "the default pager must not be told to forward ANSI sequences"
        );
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
        assert!(!exceeds_screen(text, 24, 80));
        assert!(!exceeds_screen("", 24, 80));
    }

    #[test]
    fn content_taller_than_the_screen_needs_a_pager() {
        let text = "x\n".repeat(40);
        assert!(exceeds_screen(&text, 24, 80));
    }

    #[test]
    fn one_row_is_reserved_for_the_prompt() {
        let text = "x\n".repeat(23);
        assert!(
            !exceeds_screen(&text, 24, 80),
            "23 lines fit a 24-row screen"
        );
        let text = "x\n".repeat(24);
        assert!(exceeds_screen(&text, 24, 80), "24 lines do not");
    }

    #[test]
    fn a_single_long_line_that_wraps_past_the_screen_needs_a_pager() {
        // The regression this test exists for: one 10_000-column line is ONE
        // logical line but 125 rows of an 80-column terminal.
        let text = "a".repeat(10_000);
        assert!(
            exceeds_screen(&text, 24, 80),
            "a wrapped line was measured as one row"
        );
        // ... and the same line is fine on a terminal wide enough for it.
        assert!(!exceeds_screen(&text, 24, 20_000));
    }

    #[test]
    fn wrapping_is_measured_in_terminal_columns_not_characters() {
        // 60 CJK characters are 120 columns: two rows at width 80, not one.
        let line = "\u{4e2d}".repeat(60);
        assert_eq!(wrapped_rows(&line, 80), 2);
        // Combining marks add no width.
        assert_eq!(wrapped_rows("e\u{301}", 80), 1);
        // An empty line still occupies a row.
        assert_eq!(wrapped_rows("", 80), 1);
        // Exactly full, then one column over.
        assert_eq!(wrapped_rows(&"a".repeat(80), 80), 1);
        assert_eq!(wrapped_rows(&"a".repeat(81), 80), 2);
    }

    #[test]
    fn many_wrapped_lines_add_up() {
        // Twelve lines of 160 columns are 24 rows: past a 24-row screen.
        let text = format!("{}\n", "a".repeat(160)).repeat(12);
        assert!(exceeds_screen(&text, 24, 80));
        let text = format!("{}\n", "a".repeat(160)).repeat(11);
        assert!(!exceeds_screen(&text, 24, 80));
    }

    #[test]
    fn tiny_or_zero_heights_do_not_underflow() {
        assert!(exceeds_screen("a", 1, 80));
        assert!(exceeds_screen("a", 0, 80));
    }

    #[test]
    fn an_unknown_terminal_width_falls_back_instead_of_dividing_by_zero() {
        // Short content still fits, and a very long line still wraps: the
        // fallback width is used, not a one-column terminal.
        assert!(!exceeds_screen("a", 24, 0));
        assert!(exceeds_screen(&"a".repeat(10_000), 24, 0));
    }

    #[test]
    fn display_text_replaces_terminal_control_sequences() {
        // OSC 52 sets the system clipboard; OSC 8 forges a hyperlink. A
        // document body must never be able to issue either.
        let (display, replaced) = for_display("before\u{1b}]52;c;cGF5bG9hZA==\u{7}after");
        assert!(!display.contains('\u{1b}'), "ESC survived: {display:?}");
        assert!(!display.contains('\u{7}'), "BEL survived: {display:?}");
        assert_eq!(replaced, 2, "both control characters must be counted");
        assert!(display.starts_with("before"), "{display:?}");
        assert!(display.contains("after"), "{display:?}");
    }

    #[test]
    fn display_text_keeps_newlines_and_expands_tabs() {
        let (display, replaced) = for_display("a\tb\nc\n");
        assert_eq!(display, "a       b\nc\n");
        assert_eq!(replaced, 0, "a tab is not a smuggled control sequence");
    }

    #[test]
    fn tab_stops_are_measured_in_columns_not_bytes() {
        // U+4E2D is 3 bytes but 2 columns, so the next tab stop is at
        // column 8 and the tab must expand to 6 spaces. Counting bytes
        // would produce 5 and put everything after it in the wrong column -
        // and make the line measure as fitting the screen when it does not.
        let (display, _) = for_display("\u{4e2d}\tX\n");
        assert_eq!(display, "\u{4e2d}      X\n");
        assert_eq!(render::display_columns(display.trim_end()), 9);

        // A combining mark adds no columns, so the stop is unchanged from
        // the base letter alone.
        let (display, _) = for_display("e\u{301}\tX\n");
        assert_eq!(display, "e\u{301}       X\n");
    }

    #[test]
    fn a_tab_after_wide_text_is_counted_when_measuring_the_screen() {
        // End to end for the same bug: the line is 9 columns, so on an
        // 8-column terminal it wraps onto a second row and, with one row
        // reserved for the prompt, needs the pager.
        let (display, _) = for_display("\u{4e2d}\tX\n");
        assert!(exceeds_screen(&display, 2, 8));
    }

    #[test]
    fn each_line_gets_its_own_tab_stops() {
        let (display, _) = for_display("ab\tc\nabcdefghij\tk\n");
        assert_eq!(display, "ab      c\nabcdefghij      k\n");
    }

    #[test]
    fn display_text_drops_carriage_returns_without_marking_them() {
        // A CRLF document must not grow a marker on every line.
        let (display, replaced) = for_display("a\r\nb\r\n");
        assert_eq!(display, "a\nb\n");
        assert_eq!(replaced, 0);
    }

    #[test]
    fn display_text_leaves_bidi_and_zero_width_content_alone() {
        // These are legitimate content in Arabic, Hebrew and Indic text and
        // do not instruct the terminal to do anything.
        let raw = "\u{202b}\u{5e9}\u{5dc}\u{5d5}\u{5dd}\u{202c}\u{200d}\n";
        let (display, replaced) = for_display(raw);
        assert_eq!(display, raw);
        assert_eq!(replaced, 0);
    }

    #[test]
    fn display_text_ends_with_exactly_one_newline() {
        assert_eq!(for_display("a").0, "a\n");
        assert_eq!(for_display("a\n").0, "a\n");
        assert_eq!(for_display("").0, "");
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

    #[cfg(unix)]
    #[test]
    fn a_pager_that_fails_to_run_is_reported_so_the_document_is_not_lost() {
        // `PAGER=false` starts fine and exits 1 without showing anything.
        // Treating that as success made the whole document vanish behind
        // exit code 0.
        let pager = Pager {
            program: OsString::from("false"),
            args: Vec::new(),
        };
        let error = run(&pager, &"line\n".repeat(1000))
            .expect_err("a pager that exited non-zero must be reported");
        assert!(error.contains("status 1"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_pager_the_user_quit_early_is_normal_completion() {
        // `head -1` takes one line and exits 0 while the writer still has
        // data - the shape of quitting `less` with `q`. Exit 0, so no
        // fallback and no duplicated output.
        let pager = Pager {
            program: OsString::from("sh"),
            args: vec![OsString::from("-c"), OsString::from("head -1 > /dev/null")],
        };
        assert!(run(&pager, &"line\n".repeat(100_000)).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_signalled_pager_is_reported_rather_than_read_as_success() {
        let pager = Pager {
            program: OsString::from("sh"),
            args: vec![OsString::from("-c"), OsString::from("kill -TERM $$")],
        };
        let error = run(&pager, "short\n").expect_err("a signalled pager must be reported");
        assert!(error.contains("signal"), "{error}");
    }
}
