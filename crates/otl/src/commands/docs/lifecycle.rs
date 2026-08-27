//! Move, reorder, archive, and trash documents.

use anyhow::anyhow;
use clap::Args;

use crate::commands::output;
use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;
use crate::session::Session;

#[derive(Debug, Args)]
#[command(after_long_help = "API contract:
  This command uses documents.move.

  Inspect it with:
    otl api describe documents.move --json")]
pub struct MoveArgs {
    /// Document UUID or urlId.
    id: String,

    /// Move to this collection.
    #[arg(long, value_name = "ID")]
    collection: Option<String>,

    /// Nest beneath this parent document.
    #[arg(long, value_name = "ID")]
    parent: Option<String>,

    /// Position within the destination siblings.
    #[arg(long)]
    index: Option<u64>,
}

#[derive(Debug, Args)]
#[command(after_long_help = "API contracts:
  Without --archive this command uses documents.delete (to the trash); with
  it, documents.archive.

  Inspect them with:
    otl api describe documents.delete --json
    otl api describe documents.archive --json")]
pub struct DeleteArgs {
    /// Document UUID or urlId.
    id: String,

    /// Archive instead of moving the document to trash.
    #[arg(long)]
    archive: bool,
}

pub fn move_document(
    args: &MoveArgs,
    mode: OutputMode,
    overrides: &Overrides,
) -> Result<(), CliError> {
    if args.collection.is_none() && args.parent.is_none() && args.index.is_none() {
        return Err(CliError::usage(anyhow!(
            "nothing to move: pass --collection, --parent, and/or --index"
        )));
    }
    let mut request = vec![("id".to_string(), args.id.clone())];
    if let Some(value) = &args.collection {
        request.push(("collectionId".to_string(), value.clone()));
    }
    if let Some(value) = &args.parent {
        request.push(("parentDocumentId".to_string(), value.clone()));
    }
    if let Some(value) = args.index {
        request.push(("index".to_string(), value.to_string()));
    }
    let result = Session::open(overrides)?.call_data("documents.move", &request)?;
    output::emit(&result, mode)
}

pub fn delete_document(
    args: &DeleteArgs,
    mode: OutputMode,
    overrides: &Overrides,
) -> Result<(), CliError> {
    let operation = if args.archive {
        "documents.archive"
    } else {
        "documents.delete"
    };
    let result =
        Session::open(overrides)?.call_data(operation, &[("id".to_string(), args.id.clone())])?;
    output::emit(&result, mode)
}
