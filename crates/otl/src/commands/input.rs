//! Bounded reads for user-selected command input files.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::anyhow;

use crate::exit::CliError;

/// Read a UTF-8 file without allowing an unbounded allocation.
pub fn read_utf8(path: &Path, label: &str, limit: u64) -> Result<String, CliError> {
    let io_error = |error: std::io::Error| {
        CliError::usage(anyhow!(
            "cannot read {label} file {:?}: {}",
            path,
            error.kind()
        ))
    };
    let too_large = || {
        CliError::usage(anyhow!(
            "{label} file {:?} is too large: the limit is {limit} bytes",
            path
        ))
    };
    let file = File::open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if metadata.is_file() && metadata.len() > limit {
        return Err(too_large());
    }
    let mut text = String::new();
    let read = file
        .take(limit + 1)
        .read_to_string(&mut text)
        .map_err(io_error)?;
    if read as u64 > limit {
        return Err(too_large());
    }
    Ok(text)
}
