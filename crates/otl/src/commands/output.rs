//! Output helpers for the stable commands, and the one decision they force.
//!
//! There are two functions because the `--json` rule has two halves, and
//! which half applies is a property of the VALUE, not of the command:
//!
//! - a SERVER PAYLOAD round-trips byte for byte. That is the contract a
//!   caller diffs, replays and verifies against, and scrubbing it would
//!   quietly change the server's own answer.
//! - an object `otl` AUTHORED is scrubbed. Nothing round-trips it, and the
//!   foreign text interpolated into it - an id from a command line, a name
//!   from a document - is exactly what the scrubber exists for.
//!
//! Naming the kind at every call site is what keeps that reviewable. It
//! also puts this module's two `render` calls where `tests/authored_json.rs`
//! can register them: `render::render` never names `render_json`, so the
//! guard could not see this door until it was asked to scan for it.

use anyhow::anyhow;
use serde_json::Value;

use crate::exit::CliError;
use crate::render::{self, OutputMode};
use crate::stdio;

/// Emit a payload that came from the server, verbatim.
pub fn emit_server(value: &Value, mode: OutputMode) -> Result<(), CliError> {
    let rendered = render::render(value, mode, &[])
        .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
    stdio::write_data_line(&rendered)
}

/// Emit an object this CLI built, with every string scrubbed.
///
/// Scrubbed in BOTH output states, unlike [`emit_server`]: an authored
/// object is small and prints as JSON either way, and the hazard - a
/// terminal-control or bidi sequence in an interpolated value - is the same
/// on a terminal as in a pipe.
pub fn emit_authored(value: &Value, mode: OutputMode) -> Result<(), CliError> {
    let _ = mode;
    stdio::write_data_line(&authored(value)?)
}

/// The exact bytes [`emit_authored`] writes, so a test can ask about them.
fn authored(value: &Value) -> Result<String, CliError> {
    render::render_json_scrubbed(value)
        .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The inner defence for an authored object: whatever a caller manages
    /// to interpolate into one, it reaches the terminal inert.
    ///
    /// Today's only such object is `otl fetch attachment`'s `{id,
    /// signedUrl}`, and its `id` is additionally refused by strict
    /// validation before any request (pinned in `tests/mcp_parity.rs`). This
    /// test is what keeps the guarantee when the next authored object
    /// arrives without that second gate.
    #[test]
    fn an_authored_object_reaches_the_terminal_inert() {
        let hostile = "id\u{1b}]52;c;x\u{7}\u{202e}\u{200f}\u{00ad}";
        let rendered = authored(&serde_json::json!({
            "id": hostile,
            "signedUrl": "https://storage.example/private/file?signature=secret",
        }))
        .expect("renders");
        for hazard in ['\u{1b}', '\u{7}', '\u{202e}', '\u{200f}', '\u{00ad}'] {
            assert!(
                !rendered.contains(hazard),
                "{hazard:?} survived: {rendered}"
            );
        }
        // Still a usable document, and the safe parts are untouched.
        assert!(rendered.contains("signature=secret"), "{rendered}");
        assert!(rendered.contains("\"id\""), "{rendered}");
    }

    /// The other half of the pair is verbatim ON PURPOSE: a server payload
    /// has to round-trip. Asserted so that "fix" the two functions into one
    /// fails here.
    #[test]
    fn a_server_payload_is_not_rewritten() {
        let value = serde_json::json!({ "text": "a\u{202e}b" });
        let rendered = render::render(&value, OutputMode::Json, &[]).expect("renders");
        assert!(
            rendered.contains('\u{202e}'),
            "the verbatim emitter rewrote a server payload: {rendered}"
        );
    }
}
