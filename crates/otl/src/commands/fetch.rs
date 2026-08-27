//! Unified resource lookup, mirroring Outline's MCP `fetch` tool.

use anyhow::anyhow;
use clap::{Args, ValueEnum};
use serde_json::{json, Value};
use url::Url;

use crate::config::Overrides;
use crate::exit::CliError;
use crate::render::OutputMode;
use crate::session::Session;

use super::output;

const DOCUMENT_INFO: &str = "documents.info";
const COLLECTION_INFO: &str = "collections.info";
const COLLECTION_DOCUMENTS: &str = "collections.documents";
const USER_INFO: &str = "users.info";
const CURRENT_USER_INFO: &str = "auth.info";
const ATTACHMENT_REDIRECT: &str = "attachments.redirect";

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Resource {
    Document,
    Collection,
    User,
    Attachment,
}

/// Fetch one Outline resource by ID or URL.
#[derive(Debug, Args)]
#[command(after_long_help = "API contracts:
  document -> documents.info; collection -> collections.info plus
  collections.documents for the tree; user -> users.info, or auth.info for
  current_user; attachment -> attachments.redirect, whose signed Location is
  returned without being followed.

  Inspect them with:
    otl api describe documents.info --json
    otl api describe collections.info --json
    otl api describe collections.documents --json
    otl api describe users.info --json
    otl api describe attachments.redirect --json")]
pub struct FetchArgs {
    /// Resource kind to retrieve.
    #[arg(value_enum)]
    resource: Resource,

    /// UUID, urlId, or full Outline URL. For users, current_user/self/me
    /// retrieves the authenticated user.
    id: String,
}

pub fn run(args: &FetchArgs, mode: OutputMode, overrides: &Overrides) -> Result<(), CliError> {
    let id = extract_id(&args.id)?;
    let session = Session::open(overrides)?;
    let value = match args.resource {
        Resource::Document => session.call_data(DOCUMENT_INFO, &[pair("id", &id)])?,
        Resource::Collection => {
            let collection = session.call_data(COLLECTION_INFO, &[pair("id", &id)])?;
            let documents = session.call_data(COLLECTION_DOCUMENTS, &[pair("id", &id)])?;
            json!({ "collection": collection, "documents": documents })
        }
        Resource::User if is_current_user(&id) => {
            let auth = session.call_data(CURRENT_USER_INFO, &[])?;
            auth.get("user").cloned().unwrap_or(Value::Null)
        }
        Resource::User => session.call_data(USER_INFO, &[pair("id", &id)])?,
        Resource::Attachment => {
            let signed_url =
                session.call_redirect_location(ATTACHMENT_REDIRECT, &[pair("id", &id)])?;
            json!({ "id": id, "signedUrl": signed_url })
        }
    };
    output::emit(&value, mode)
}

fn pair(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

fn is_current_user(id: &str) -> bool {
    matches!(
        id.to_ascii_lowercase().as_str(),
        "current_user" | "self" | "me"
    )
}

/// Match Outline MCP's URL handling: `?id=` wins, otherwise the last
/// non-empty path segment is the model identifier/urlId.
fn extract_id(raw: &str) -> Result<String, CliError> {
    if !raw.starts_with("http://") && !raw.starts_with("https://") {
        return Ok(raw.to_string());
    }
    let url = Url::parse(raw)
        .map_err(|_| CliError::usage(anyhow!("the resource URL is not a valid HTTP(S) URL")))?;
    if let Some(id) = url
        .query_pairs()
        .find_map(|(key, value)| (key == "id" && !value.is_empty()).then(|| value.into_owned()))
    {
        return Ok(id);
    }
    url.path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| CliError::usage(anyhow!("the resource URL contains no identifier")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ids_pass_through() {
        assert_eq!(extract_id("doc-1").unwrap(), "doc-1");
    }

    #[test]
    fn full_urls_yield_the_last_segment() {
        assert_eq!(
            extract_id("https://docs.example.com/doc/title-urlId/").unwrap(),
            "title-urlId"
        );
    }

    #[test]
    fn query_id_takes_precedence() {
        assert_eq!(
            extract_id("https://docs.example.com/path?id=attachment-id").unwrap(),
            "attachment-id"
        );
    }
}
