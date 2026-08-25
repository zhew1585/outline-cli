//! `otl api <operation> [key=value...]` - the generic API escape hatch.
//!
//! Output format of this command is explicitly unstable (not covered by
//! semver, per the CLI contract).

use anyhow::anyhow;
use clap::Args;
use engine::Client;
use serde_json::Value;

use crate::config::Config;
use crate::exit::CliError;
use crate::ops;

/// Arguments for `otl api`.
#[derive(Debug, Args)]
pub struct ApiArgs {
    /// API operation name, e.g. `documents.info`.
    pub operation: String,

    /// Request parameters as `key=value` pairs.
    #[arg(value_name = "KEY=VALUE")]
    pub args: Vec<String>,
}

/// Run the `api` subcommand. Configuration and argument validation happen
/// before any network request.
pub fn run(cmd: &ApiArgs) -> Result<(), CliError> {
    let op = ops::find(&cmd.operation).ok_or_else(|| {
        CliError::usage(anyhow!(
            "unknown API operation {:?}; operation names follow the \
             `resource.method` form, e.g. `documents.info`",
            cmd.operation
        ))
    })?;
    let args = parse_key_value_args(&cmd.args)?;
    let config = Config::from_env().map_err(CliError::usage)?;

    let client = Client::new(&config.base_url, &config.api_key).map_err(CliError::usage)?;
    let response = client.execute(op, &args).map_err(CliError::failure)?;

    print_response(&response)
}

/// Parse raw `key=value` CLI arguments; reject malformed ones fail-fast.
fn parse_key_value_args(raw: &[String]) -> Result<Vec<(String, String)>, CliError> {
    raw.iter()
        .map(|arg| {
            arg.split_once('=')
                .filter(|(key, _)| !key.is_empty())
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .ok_or_else(|| {
                    CliError::usage(anyhow!("invalid argument {arg:?}: expected key=value form"))
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
