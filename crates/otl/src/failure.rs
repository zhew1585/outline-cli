//! How a terminating error reaches the caller.
//!
//! # Why this is not just `eprintln!`
//!
//! The dual-state rule ("stdout is data, stderr is diagnostics") settled
//! where an error goes, and the exit-code table settled what it means. What
//! neither settled is the shape, and for a long time there was only one: a
//! prose line on stderr, in every state.
//!
//! For a human that is right. For the agents this CLI is built to be driven
//! by it is thin: `--json` produced an empty stdout and a sentence, so the
//! only machine-readable fact left was the process exit status - and a
//! caller that reads stdout through a pipe often does not have it, or has it
//! buried under a shell's own conventions. The nine codes were a published
//! contract that the structured output state did not publish.
//!
//! So `--json` gets the same fact as an object:
//!
//! ```json
//! { "error": { "exit_code": 2, "code": "usage", "message": "..." } }
//! ```
//!
//! on stderr, where diagnostics already live. `code` is
//! [`ExitCode::name`] - the same nine classes, not a second taxonomy that
//! would need its own document and its own drift.
//!
//! # What this does NOT cover
//!
//! clap's own usage errors (an unknown flag, a missing required argument)
//! are printed and exited by clap before any of this runs, so they stay
//! prose in both states. That is deliberate rather than unfinished: those
//! messages carry a usage synopsis and a suggestion, which is the useful
//! part, and their exit code is already 2. A caller must not assume every
//! failure is JSON just because `--json` was passed - the exit code remains
//! the thing that is always there.

use serde_json::json;

use crate::exit::CliError;
use crate::render::{self, OutputMode};
use crate::stdio;

/// Write a terminating error to stderr in the resolved output state.
pub fn report(error: &CliError, mode: OutputMode) {
    stdio::write_diagnostic_line(&rendered(error, mode));
}

/// The exact bytes [`report`] writes, so a test can ask about them.
///
/// The JSON form is SCRUBBED rather than verbatim: this object is authored
/// here, and its `message` interleaves this crate's prose with text from a
/// server, a filesystem or a config file. That is the same rule
/// `otl doctor` and `otl api describe` follow, and for the same reason -
/// nothing round-trips a diagnostic, so the round-trip exemption's premise
/// does not hold. `write_diagnostic_line` would scrub the line anyway; doing
/// it through the JSON renderer means the escaping happens before the
/// scrub, not after, so a hazard cannot ride out inside a `\u` escape.
pub fn rendered(error: &CliError, mode: OutputMode) -> String {
    match mode {
        OutputMode::Table => format!("error: {error}"),
        OutputMode::Json => {
            let payload = json!({
                "error": {
                    "exit_code": error.code as u8,
                    "code": error.code.name(),
                    "message": error.to_string(),
                }
            });
            // A failure to render the report must not replace the real exit
            // code with a panic, so the prose form is the fallback.
            render::render_json_scrubbed(&payload).unwrap_or_else(|_| format!("error: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::exit::ExitCode;
    use serde_json::Value;

    fn usage() -> CliError {
        CliError::usage(anyhow::anyhow!("OUTLINE_URL is not set."))
    }

    #[test]
    fn the_human_state_is_the_prose_line_it_has_always_been() {
        assert_eq!(
            rendered(&usage(), OutputMode::Table),
            "error: OUTLINE_URL is not set."
        );
    }

    #[test]
    fn the_json_state_carries_the_code_by_number_and_by_name() {
        let value: Value =
            serde_json::from_str(&rendered(&usage(), OutputMode::Json)).expect("json");
        assert_eq!(value["error"]["exit_code"], 2);
        assert_eq!(value["error"]["code"], "usage");
        assert_eq!(value["error"]["message"], "OUTLINE_URL is not set.");
    }

    /// Code 9 is the one whose meaning a caller most needs from the report
    /// itself: the output is real and incomplete, so "did it fail" has no
    /// yes/no answer that is not misleading.
    #[test]
    fn a_partial_failure_says_so_rather_than_reading_as_a_failure() {
        let error = CliError::partial(anyhow::anyhow!("3 of 40 documents were not written"));
        let value: Value = serde_json::from_str(&rendered(&error, OutputMode::Json)).expect("json");
        assert_eq!(value["error"]["exit_code"], 9);
        assert_eq!(value["error"]["code"], "partial");
    }

    #[test]
    fn every_code_has_a_name_and_no_two_share_one() {
        let codes = [
            ExitCode::Success,
            ExitCode::Failure,
            ExitCode::Usage,
            ExitCode::ApiRequest,
            ExitCode::Auth,
            ExitCode::NotFound,
            ExitCode::Server,
            ExitCode::RateLimited,
            ExitCode::Network,
            ExitCode::Partial,
        ];
        let mut names: Vec<&str> = codes.iter().map(|code| code.name()).collect();
        assert!(names.iter().all(|name| !name.is_empty()));
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two exit codes share a name");
    }

    /// The scrub has to happen on the way into the JSON, not after it: a
    /// message carrying an escape sequence must not arrive as a `\u` escape
    /// that a consumer un-escapes back into the terminal.
    #[test]
    fn a_hostile_message_is_scrubbed_rather_than_escaped_into_the_output() {
        let error = CliError::failure(anyhow::anyhow!("bad \u{1b}]52;c;cGF5bG9hZA==\u{7} value"));
        let text = rendered(&error, OutputMode::Json);
        assert!(!text.contains("\\u001b"), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
        let value: Value = serde_json::from_str(&text).expect("json");
        let message = value["error"]["message"].as_str().expect("message");
        assert!(message.contains("bad"), "{message}");
        assert!(message.contains("value"), "{message}");
    }
}
