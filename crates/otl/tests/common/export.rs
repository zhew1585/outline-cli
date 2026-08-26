//! Shared fixtures for the `otl docs export` end-to-end tests.
//!
//! Split out so both export test files build the same mock collection and
//! read the same output tree; the two files differ in what they assert, not
//! in how they set it up.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path as request_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A syntactically valid collection id. The vendored spec declares
/// `collectionId` as `format: uuid`, so the engine rejects anything else
/// locally - before any request, which is the intended behaviour.
pub const COLLECTION: &str = "11111111-1111-4111-8111-111111111111";

/// One row of `documents.list`.
pub fn row(id: &str, title: &str, parent: Option<&str>) -> Value {
    let mut row = json!({ "id": id, "title": title, "updatedAt": "2026-08-01T00:00:00.000Z" });
    if let Some(parent) = parent {
        row["parentDocumentId"] = json!(parent);
    }
    row
}

/// Mount `documents.list` with one page of rows and `documents.info` for
/// each of them.
pub async fn server_with(rows: Vec<Value>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(request_path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": rows.clone(),
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;
    for row in rows {
        let id = row["id"].as_str().unwrap().to_string();
        let title = row["title"].as_str().unwrap().to_string();
        Mock::given(method("POST"))
            .and(request_path("/api/documents.info"))
            .and(body_partial_json(json!({ "id": id })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "id": id, "title": title, "text": format!("body of {id}\n") },
            })))
            .mount(&server)
            .await;
    }
    server
}

/// Every file path under `root`, relative and slash-separated.
pub fn tree(root: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path.strip_prefix(root).unwrap();
            found.insert(
                relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
    found
}
