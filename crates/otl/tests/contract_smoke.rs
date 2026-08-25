//! Contract smoke test against a real Outline workspace (Story 1.8).
//!
//! Ignored by default so `cargo test` never touches the network. The CI
//! contract job runs it with `-- --ignored` and injects credentials via the
//! `OUTLINE_TEST_URL` / `OUTLINE_TEST_API_KEY` secrets; when those env vars
//! are absent the test skips at runtime instead of failing. No credentials
//! or instance URLs live in the repository.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;

mod common;
use common::{no_cache_dir, CACHE_DIR_ENV};

/// Read contract-test credentials from the environment, or `None` to skip.
fn contract_credentials() -> Option<(String, String)> {
    let url = std::env::var("OUTLINE_TEST_URL").ok()?;
    let key = std::env::var("OUTLINE_TEST_API_KEY").ok()?;
    if url.trim().is_empty() || key.trim().is_empty() {
        return None;
    }
    Some((url, key))
}

// The smoke uses `documents.search` because the current IR table only
// compiles the documents.* subset (Story 1.1 MVP slice). Once the
// full-endpoint IR lands (Story 1.2), switch this to `auth.info` for a
// cheaper, side-effect-free identity check.
#[test]
#[ignore = "contract test: needs OUTLINE_TEST_URL / OUTLINE_TEST_API_KEY"]
fn documents_search_succeeds_against_real_workspace() {
    let Some((url, key)) = contract_credentials() else {
        eprintln!("skipping: OUTLINE_TEST_URL / OUTLINE_TEST_API_KEY not set");
        return;
    };

    let output = Command::cargo_bin("otl")
        .unwrap()
        .env("OUTLINE_URL", &url)
        .env("OUTLINE_API_KEY", &key)
        // A contract test checks the VENDORED spec against the live API;
        // a synced cache on the runner would test something else.
        .env(CACHE_DIR_ENV, no_cache_dir())
        .args(["api", "documents.search", "query=contract-smoke"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "otl api documents.search failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be JSON for jq consumption");
    // `otl api` prints the `data` field of the Outline envelope; for
    // documents.search that is an array of search results (possibly empty).
    assert!(
        payload.is_array(),
        "documents.search data payload should be an array, got: {payload}"
    );
}
