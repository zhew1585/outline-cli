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
    // Bytes first, then UTF-8. Reading straight into a `String` cannot tell
    // "over the cap" from "not text": a file one byte past the limit whose
    // last byte begins a multibyte character is cut mid-sequence, and the
    // read fails as invalid data - so the user is told their file is not
    // UTF-8 when the actual problem is its size. The bound is `take`, not
    // the metadata check above, which is only a fast path.
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(too_large());
    }
    String::from_utf8(bytes)
        .map_err(|_| CliError::usage(anyhow!("{label} file {:?} is not valid UTF-8", path)))
}
