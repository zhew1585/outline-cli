//! `otl api <operation> [key=value...]` - the generic API escape hatch.
//!
//! Output format of this command is explicitly unstable (not covered by
//! semver, per the CLI contract).
//!
//! Two flags exist for the cases where the compiled spec or the CLI's own
//! caution gets in the way: `--no-validate` sends a request whose values
//! the vendored spec rejects (spec drift), and `--show-server-message`
//! restores the server's error text for a `--body` request, which is
//! withheld by default because it may quote the body.
//!
//! # Three jobs, three modules
//!
//! This module owns CALLING an operation. The other two are local-only and
//! answer the questions a caller has before it can call anything:
//!
//! - [`list`] - which operations exist ([`LIST_OPERATION`]);
//! - [`describe`] - what one operation takes and returns
//!   ([`DESCRIBE_OPERATION`]).
//!
//! Both are reserved first positionals rather than clap subcommands; see
//! [`reserved`] for why, and for what happens if a spec ever declares an
//! operation by one of those names.

mod describe;
mod list;
mod reserved;

use std::fs::File;
use std::io::Read;

use anyhow::anyhow;
use clap::Args;
use engine::{EngineError, ErrorDetail, Fetched, OpSpec, ValidationMode};
use serde_json::Value;

use crate::auth;
use crate::config::Overrides;
use crate::errors::map_engine_error_with_hint;
use crate::exit::CliError;
use crate::paging;
use crate::render::{self, OutputMode};
use crate::session::{self, UNCONFIRMED_OFFSET_NOTICE};
use crate::stdio;

/// Hint appended to validation errors that only a raw body can express.
const BODY_HINT: &str = "pass the whole request body as JSON with `--body @file.json`";

/// Hint appended when a server message was withheld for a `--body` call.
const SHOW_MESSAGE_HINT: &str =
    "pass --show-server-message to display it (it may echo your request body)";

/// Hint appended when an operation cannot be called generically at all.
const DEDICATED_COMMAND_HINT: &str =
    "it is not callable via `otl api`; a dedicated command is planned";

/// Reserved word: `otl api list` enumerates operations instead of calling
/// one.
pub(crate) const LIST_OPERATION: &str = "list";

/// Reserved word: `otl api describe <operation>` prints one operation's
/// full request/response contract instead of calling it.
pub(crate) const DESCRIBE_OPERATION: &str = "describe";

/// Maximum accepted size of a `--body` file.
///
/// The body is read into memory, parsed and copied, so an unbounded file
/// would turn into an out-of-memory abort instead of a clean usage error.
pub const MAX_BODY_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Arguments for `otl api`.
///
/// `disable_help_flag` plus the hand-rolled [`ApiArgs::help`] below is what
/// makes `otl api documents.info --help` describe THAT operation. clap's own
/// help flag is handled during parsing and prints the subcommand's help
/// whatever else is on the line, so `otl api documents.info --help` used to
/// print the generic `otl api` help - text that looks authoritative and says
/// nothing about `documents.info`. For an agent probing the CLI that is
/// worse than an error: an error makes it try something else, a plausible
/// wrong answer does not.
#[derive(Debug, Args)]
#[command(disable_help_flag = true)]
pub struct ApiArgs {
    /// API operation name, e.g. `documents.info`. Two reserved words:
    /// `list` enumerates every operation, `describe <operation>` prints one
    /// operation's parameters and response fields.
    // `Option` only so `otl api --help` can be answered without a name;
    // `required_unless_present` keeps it mandatory everywhere else, so every
    // path that needs a name below is guaranteed one by clap.
    #[arg(required_unless_present = "help")]
    pub operation: Option<String>,

    /// Request parameters as `key=value` pairs.
    #[arg(value_name = "KEY=VALUE")]
    pub args: Vec<String>,

    /// Describe the named operation (same as `otl api describe <operation>`),
    /// or print this help when no operation is named.
    #[arg(short, long, action = clap::ArgAction::SetTrue)]
    pub help: bool,

    /// Raw JSON request body from a file (`--body @file.json`), sent
    /// verbatim. Mutually exclusive with `key=value` arguments.
    #[arg(long, value_name = "@FILE")]
    pub body: Option<String>,

    /// Show the server's error message for a `--body` request. The server
    /// may quote your request body, so this can echo secrets it contains.
    #[arg(long)]
    pub show_server_message: bool,

    /// Skip local schema-facet checks (enum, numeric bounds, format) and
    /// send the request anyway. Use it when the vendored spec disagrees
    /// with your Outline instance.
    #[arg(long)]
    pub no_validate: bool,

    /// Cap the total number of rows fetched by auto-pagination.
    ///
    /// List operations fetch every page by default; with `--limit` the
    /// fetch stops at N rows and a truncation warning goes to stderr if
    /// more rows were available. Cannot be combined with a raw `limit=`
    /// argument, which pins the server page size instead.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub limit: Option<u64>,
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
///
/// `overrides` carries the command-line configuration layer, which outranks
/// the environment and the user config file key by key. `root` builds the
/// whole clap command on demand, and is only called to render `otl api
/// --help`: passing the builder rather than the built command keeps the
/// cost off every other invocation, and rendering from the REAL command
/// tree is what keeps that help text identical to the one clap used to
/// print (global flags included) instead of a second, drifting copy.
///
/// Three of the four paths below are purely local - `list`, `describe` and
/// any form of `--help` need no configuration, no credential and no
/// network - so they are resolved before [`auth::client`] is ever built.
pub fn run(
    cmd: &ApiArgs,
    mode: OutputMode,
    overrides: &Overrides,
    root: fn() -> clap::Command,
) -> Result<(), CliError> {
    match reserved::dispatch(cmd, mode, root)? {
        reserved::Next::Done => Ok(()),
        reserved::Next::Call(op) => call(cmd, op, mode, overrides),
    }
}

/// Call one operation over the network and print its response.
fn call(
    cmd: &ApiArgs,
    op: &'static OpSpec,
    mode: OutputMode,
    overrides: &Overrides,
) -> Result<(), CliError> {
    let payload = build_payload(cmd)?;
    // A raw --body is sent verbatim and once, so pagination never applies.
    let pagination = match &payload {
        Payload::KeyValue(_) => paging::spec_for(op),
        Payload::Raw(_) => None,
    };
    check_limit_usage(cmd, op, &payload, pagination.is_some())?;
    // One place resolves configuration AND the credential for every command,
    // and hands the request channel a source that renews itself. `otl api`
    // must not build its own client: renewal, the transport rule and the
    // instance binding all live behind this call (see `crate::auth`).
    let client = auth::client(overrides)?;
    let detail = error_detail(cmd);
    let fetched = match (&payload, &pagination) {
        (Payload::KeyValue(args), Some(spec)) => {
            client.execute_paged(op, args, validation_mode(cmd), spec, cmd.limit)
        }
        (Payload::KeyValue(args), None) => client
            .execute(op, args, validation_mode(cmd))
            .map(Fetched::complete),
        (Payload::Raw(body), _) => client.execute_raw(op, body, detail).map(Fetched::complete),
    }
    .map_err(|error| execute_error(error, detail))?;

    if fetched.offset_unconfirmed {
        // Once per command, not once per page: the rows are usable, the
        // boundaries between them just could not be verified.
        stdio::write_diagnostic_line(UNCONFIRMED_OFFSET_NOTICE);
    }
    if let Some(truncation) = &fetched.truncation {
        session::warn_truncated(truncation);
    }
    print_response(&fetched.value, mode, &op.response_fields)
}

/// Reject `--limit` where it cannot mean anything, before any request.
///
/// `--limit` (a total cap) and a raw `limit=` argument (a per-page size)
/// mean different things and would silently fight each other, so asking
/// for both is a usage error rather than a guess. `--limit` on a `--body`
/// call or on an operation that does not paginate would be a silent no-op.
fn check_limit_usage(
    cmd: &ApiArgs,
    op: &OpSpec,
    payload: &Payload,
    paginated: bool,
) -> Result<(), CliError> {
    if cmd.limit.is_none() {
        return Ok(());
    }
    if let Payload::KeyValue(args) = payload {
        if args.iter().any(|(key, _)| key == paging::LIMIT_PARAM) {
            return Err(CliError::usage(anyhow!(
                "--limit and a `{param}=` argument cannot be combined: --limit \
                 caps the total rows fetched across pages, while `{param}=` \
                 pins the server page size and fetches a single page; keep one",
                param = paging::LIMIT_PARAM
            )));
        }
    }
    if !paginated {
        return Err(CliError::usage(anyhow!(
            "--limit applies only to list operations called with key=value \
             arguments, and {:?} does not paginate here; drop --limit",
            op.name
        )));
    }
    Ok(())
}

/// How strictly to validate locally: `--no-validate` keeps the structural
/// checks but skips the schema facets (the spec-drift escape hatch).
fn validation_mode(cmd: &ApiArgs) -> ValidationMode {
    if cmd.no_validate {
        ValidationMode::SkipFacets
    } else {
        ValidationMode::Strict
    }
}

/// How much of a server error may be shown.
///
/// A `--body` request is the one case where server text can quote values
/// the CLI never saw as arguments, so its free-form message is withheld
/// unless the user opts in with `--show-server-message`.
fn error_detail(cmd: &ApiArgs) -> ErrorDetail {
    if cmd.body.is_some() && !cmd.show_server_message {
        ErrorDetail::CodeOnly
    } else {
        ErrorDetail::Full
    }
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
/// Exit-code classification is owned by `crate::errors` (the documented
/// 0..7 table); this function only adds the `otl api`-specific way out of
/// each situation: the `--body` hint for a value no `key=value` argument
/// can express, the dedicated-command note for an operation the generic
/// client cannot call, and the `--show-server-message` opt-in when the
/// server's own text was withheld.
fn execute_error(error: EngineError, detail: ErrorDetail) -> CliError {
    let hint = hint_for(&error, detail);
    map_engine_error_with_hint(error, hint)
}

/// Pick the actionable hint for one engine error, if any.
fn hint_for(error: &EngineError, detail: ErrorDetail) -> Option<&'static str> {
    if !error.is_validation() {
        let withheld = detail == ErrorDetail::CodeOnly && matches!(error, EngineError::Api { .. });
        return withheld.then_some(SHOW_MESSAGE_HINT);
    }
    if matches!(error, EngineError::UnsupportedBodyType { .. }) {
        return Some(DEDICATED_COMMAND_HINT);
    }
    if error.suggests_raw_body() {
        return Some(BODY_HINT);
    }
    None
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

/// Print the `data` field (or the whole envelope if absent) to stdout in
/// the resolved output mode (raw JSON, or a table for list-shaped data).
///
/// `schema` is the operation's compiled response shape, which drives table
/// column selection; the same `data` convention is applied here and by the
/// build pipeline that extracted it.
fn print_response(
    response: &Value,
    mode: OutputMode,
    schema: &[engine::FieldSpec],
) -> Result<(), CliError> {
    let payload = response.get("data").unwrap_or(response);
    let rendered = render::render(payload, mode, schema)
        .map_err(|error| CliError::failure(anyhow!("failed to render response: {error}")))?;
    // Never `println!` on the data path: a consumer that closes the pipe
    // early must not turn into a panic and exit code 101.
    stdio::write_data_line(&rendered)
}
