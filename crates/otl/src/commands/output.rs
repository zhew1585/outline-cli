//! Small output helpers for stable commands that return arbitrary JSON.

use anyhow::anyhow;
use serde_json::Value;

use crate::exit::CliError;
use crate::render::{self, OutputMode};
use crate::stdio;

/// Render a server-derived value in the ordinary dual output state.
pub fn emit(value: &Value, mode: OutputMode) -> Result<(), CliError> {
    let rendered = render::render(value, mode, &[])
        .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
    stdio::write_data_line(&rendered)
}
