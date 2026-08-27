//! Request pre-signed attachment uploads.

use clap::{Args, Subcommand};

use crate::commands::output;
use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;
use crate::session::Session;

#[derive(Debug, Args)]
pub struct AttachmentsArgs {
    #[command(subcommand)]
    command: AttachmentsCommand,
}

#[derive(Debug, Subcommand)]
enum AttachmentsCommand {
    /// Request a pre-signed upload URL and attachment URL.
    Create(CreateArgs),
}

/// Arguments for `otl attachments create`.
#[derive(Debug, Args)]
#[command(after_long_help = "API contract:
  This command uses attachments.create, which returns the pre-signed upload
  form. It uploads nothing itself.

  Inspect it with:
    otl api describe attachments.create --json")]
struct CreateArgs {
    /// Filename including extension.
    #[arg(long)]
    name: String,
    /// MIME content type, for example image/png.
    #[arg(long)]
    content_type: String,
    /// File size in bytes.
    #[arg(long)]
    size: u64,
    /// Associate the attachment with this document.
    #[arg(long, value_name = "ID")]
    document: Option<String>,
}

pub fn run(
    args: &AttachmentsArgs,
    mode: OutputMode,
    overrides: &Overrides,
) -> Result<(), CliError> {
    match &args.command {
        AttachmentsCommand::Create(args) => create(args, mode, overrides),
    }
}

fn create(args: &CreateArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let mut request = vec![
        ("name".to_string(), args.name.clone()),
        ("contentType".to_string(), args.content_type.clone()),
        ("size".to_string(), args.size.to_string()),
    ];
    if let Some(document) = &args.document {
        request.push(("documentId".to_string(), document.clone()));
    }
    let result = Session::open(overrides)?.call_data("attachments.create", &request)?;
    output::emit_server(&result, mode)
}
