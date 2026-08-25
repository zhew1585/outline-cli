//! `otl api <operation> [key=value...]` - the generic API escape hatch.
//!
//! Output format of this command is explicitly unstable (not covered by
//! semver, per the CLI contract).

use std::fs::File;
use std::io::Read;

use anyhow::anyhow;
use clap::Args;
use engine::{BodyMode, Client, EngineError};
use serde_json::Value;

use crate::config::Config;
use crate::exit::CliError;
use crate::ops;

/// Hint appended to validation errors that only a raw body can express.
const BODY_HINT: &str = "pass the whole request body as JSON with `--body @file.json`";

/// Hint appended when an operation cannot be called generically at all.
const DEDICATED_COMMAND_HINT: &str =
    "it is not callable via `otl api`; a dedicated command is planned";

/// Marker appended in `otl api list` to operations that cannot be called.
const NOT_CALLABLE_MARKER: &str = "[not callable via api";

/// Reserved word: `otl api list` enumerates operations instead of calling
/// one. Safe because real operation names always contain a `.`.
const LIST_OPERATION: &str = "list";

/// Maximum accepted size of a `--body` file.
///
/// The body is read into memory, parsed and copied, so an unbounded file
/// would turn into an out-of-memory abort instead of a clean usage error.
pub const MAX_BODY_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Arguments for `otl api`.
#[derive(Debug, Args)]
pub struct ApiArgs {
    /// API operation name, e.g. `documents.info` (or `list` to enumerate
    /// all operations).
    pub operation: String,

    /// Request parameters as `key=value` pairs.
    #[arg(value_name = "KEY=VALUE")]
    pub args: Vec<String>,

    /// Raw JSON request body from a file (`--body @file.json`), sent
    /// verbatim. Mutually exclusive with `key=value` arguments.
    #[arg(long, value_name = "@FILE")]
    pub body: Option<String>,
}

/// How the request body is supplied.
enum Payload {
    /// `key=value` pairs, validated and coerced against the IR.
    KeyValue(Vec<(String, String)>),
    /// Raw JSON text, passed through verbatim.
    Raw(String),
}

/// Run the `api` subcommand. Configuration and argument validation happen
/// before any network request.
pub fn run(cmd: &ApiArgs) -> Result<(), CliError> {
    if cmd.operation == LIST_OPERATION {
        return run_list(cmd);
    }
    let op = ops::find(&cmd.operation).ok_or_else(|| {
        CliError::usage(anyhow!(
            "unknown API operation {:?}; operation names follow the \
             `resource.method` form, e.g. `documents.info` \
             (run `otl api list` to see all operations)",
            cmd.operation
        ))
    })?;
    let payload = build_payload(cmd)?;
    let config = Config::from_env().map_err(CliError::usage)?;

    let client = Client::new(&config.base_url, &config.api_key).map_err(client_error)?;
    let response = match &payload {
        Payload::KeyValue(args) => client.execute(op, args),
        Payload::Raw(body) => client.execute_raw(op, body),
    }
    .map_err(execute_error)?;

    print_response(&response)
}

/// Resolve the request payload from CLI arguments: either `key=value`
/// pairs or a `--body @file.json` passthrough, never both.
fn build_payload(cmd: &ApiArgs) -> Result<Payload, CliError> {
    let Some(body) = &cmd.body else {
        return Ok(Payload::KeyValue(parse_key_value_args(&cmd.args)?));
    };
    if !cmd.args.is_empty() {
        return Err(CliError::usage(anyhow!(
            "--body cannot be combined with key=value arguments"
        )));
    }
    load_body_file(body).map(Payload::Raw)
}

/// Read and pre-validate a `--body @file.json` argument.
///
/// The content must be at most [`MAX_BODY_FILE_BYTES`] and parse as JSON
/// (fail fast with the file named, before any network request) but is
/// later sent verbatim, byte-for-byte.
fn load_body_file(value: &str) -> Result<String, CliError> {
    let Some(path) = value.strip_prefix('@') else {
        return Err(CliError::usage(anyhow!(
            "--body expects `@` followed by a file path, e.g. --body @file.json"
        )));
    };
    let raw = read_capped(path)?;
    // Validate without materializing a Value tree: the bytes are sent as
    // they are, so only well-formedness matters here.
    if let Err(error) = serde_json::from_str::<serde::de::IgnoredAny>(&raw) {
        return Err(CliError::usage(anyhow!(
            "--body file {path:?} is not valid JSON: {error}"
        )));
    }
    Ok(raw)
}

/// Read a file, refusing anything over [`MAX_BODY_FILE_BYTES`].
///
/// The metadata size is checked first (cheap rejection) and the read is
/// bounded as well, so a file that grows between the two - or one whose
/// reported size is unreliable, such as a pipe - cannot exhaust memory.
fn read_capped(path: &str) -> Result<String, CliError> {
    let io_error = |error: std::io::Error| {
        CliError::usage(anyhow!("cannot read --body file {path:?}: {error}"))
    };
    let too_large = || {
        CliError::usage(anyhow!(
            "--body file {path:?} is too large: the limit is {MAX_BODY_FILE_BYTES} bytes"
        ))
    };
    let file = File::open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if metadata.is_file() && metadata.len() > MAX_BODY_FILE_BYTES {
        return Err(too_large());
    }
    let mut raw = String::new();
    let read = file
        .take(MAX_BODY_FILE_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(io_error)?;
    if read as u64 > MAX_BODY_FILE_BYTES {
        return Err(too_large());
    }
    Ok(raw)
}

/// Map an engine error from the execute path to a CLI error.
///
/// Local validation errors are usage errors (exit code 2); those a raw
/// body can fix gain the `--body` hint, and an operation the generic
/// client cannot call at all is reported as awaiting a dedicated command.
/// Everything else (transport, server) is a generic failure (exit code 1).
fn execute_error(error: EngineError) -> CliError {
    if !error.is_validation() {
        return CliError::failure(error);
    }
    if matches!(error, EngineError::UnsupportedBodyType { .. }) {
        return CliError::usage(anyhow!("{error}; {DEDICATED_COMMAND_HINT}"));
    }
    if error.suggests_raw_body() {
        return CliError::usage(anyhow!("{error}; {BODY_HINT}"));
    }
    CliError::usage(error)
}

/// Print every compiled operation as `name<TAB>summary`, one per line.
/// Purely local: needs no configuration and touches no network.
///
/// Operations the generic client cannot call are listed too, but flagged
/// with the content type they need.
fn run_list(cmd: &ApiArgs) -> Result<(), CliError> {
    if !cmd.args.is_empty() || cmd.body.is_some() {
        return Err(CliError::usage(anyhow!(
            "`otl api list` takes no further arguments"
        )));
    }
    let mut out = String::new();
    for op in ops::OPS {
        out.push_str(&op.name);
        out.push('\t');
        out.push_str(&op.summary);
        if op.body_mode == BodyMode::Unsupported {
            out.push(' ');
            out.push_str(NOT_CALLABLE_MARKER);
            out.push_str(": requires ");
            out.push_str(&op.content_type);
            out.push(']');
        }
        out.push('\n');
    }
    print!("{out}");
    Ok(())
}

/// A bad base URL is a configuration mistake (exit code 2); anything else
/// while constructing the client is a generic failure (exit code 1).
fn client_error(error: EngineError) -> CliError {
    match error {
        EngineError::InvalidBaseUrl { .. } => CliError::usage(error),
        _ => CliError::failure(error),
    }
}

/// Parse raw `key=value` CLI arguments; reject malformed ones fail-fast.
///
/// A malformed argument is reported by POSITION only. Its text is never
/// echoed: a user who forgets the key (or the `=`) would otherwise put a
/// secret straight into the error message, its Debug and the source chain.
fn parse_key_value_args(raw: &[String]) -> Result<Vec<(String, String)>, CliError> {
    raw.iter()
        .enumerate()
        .map(|(index, arg)| {
            arg.split_once('=')
                .filter(|(key, _)| !key.is_empty())
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .ok_or_else(|| {
                    CliError::usage(anyhow!(
                        "invalid argument #{}: expected key=value form (the argument text is \
                         omitted here in case it contains a secret)",
                        index + 1
                    ))
                })
        })
        .collect()
}

/// Pretty-print the `data` field (or the whole envelope if absent) to stdout.
fn print_response(response: &Value) -> Result<(), CliError> {
    let payload = response.get("data").unwrap_or(response);
    let rendered = serde_json::to_string_pretty(payload)
        .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
    println!("{rendered}");
    Ok(())
}
