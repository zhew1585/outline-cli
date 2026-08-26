//! Shared helpers for the curated-command end-to-end tests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod export;

use assert_cmd::Command;

/// An `otl` command with every environment variable the CLI reads scrubbed,
/// so a test never depends on the developer's shell.
pub fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        .env_remove("PAGER")
        .env_remove("BROWSER");
    cmd
}

/// [`otl`] pointed at a mock server with a test API key.
pub fn otl_at(uri: &str) -> Command {
    let mut cmd = otl();
    cmd.env("OUTLINE_URL", uri)
        .env("OUTLINE_API_KEY", "test-key");
    cmd
}

/// Run a blocking closure off the async test runtime.
pub async fn blocking<F, T>(work: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work).await.unwrap()
}
