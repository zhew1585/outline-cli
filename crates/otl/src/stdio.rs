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
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    match writeln!(handle, "{text}").and_then(|()| handle.flush()) {
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
pub fn write_diagnostic_line(text: &str) {
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    let _ = writeln!(handle, "{text}");
    let _ = handle.flush();
}
