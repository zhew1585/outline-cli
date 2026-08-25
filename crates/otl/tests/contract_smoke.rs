//! Contract smoke test against a real Outline workspace (Story 1.8).
//!
//! Ignored by default so `cargo test` never touches the network. The CI
//! contract job runs it with `-- --ignored` and injects credentials via the
//! `OUTLINE_TEST_URL` / `OUTLINE_TEST_API_KEY` secrets; when those env vars
//! are absent the test skips at runtime instead of failing. No credentials
//! or instance URLs live in the repository.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;

/// Read contract-test credentials from the environment, or `None` to skip.
fn contract_credentials() -> Option<(String, String)> {
    let url = std::env::var("OUTLINE_TEST_URL").ok()?;
    let key = std::env::var("OUTLINE_TEST_API_KEY").ok()?;
    if url.trim().is_empty() || key.trim().is_empty() {
        return None;
    }
    Some((url, key))
}

#[test]
#[ignore = "contract test: needs OUTLINE_TEST_URL / OUTLINE_TEST_API_KEY"]
fn auth_info_succeeds_against_real_workspace() {
    let Some((url, key)) = contract_credentials() else {
        eprintln!("skipping: OUTLINE_TEST_URL / OUTLINE_TEST_API_KEY not set");
        return;
    };

    let output = Command::cargo_bin("otl")
        .unwrap()
        .env("OUTLINE_URL", &url)
        .env("OUTLINE_API_KEY", &key)
        .args(["api", "auth.info"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "otl api auth.info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be JSON for jq consumption");
    assert!(
        payload.get("user").is_some() || payload.get("team").is_some(),
        "auth.info payload should describe the authenticated identity"
    );
}
