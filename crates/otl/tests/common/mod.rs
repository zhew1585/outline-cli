//! Shared helpers for the curated-command end-to-end tests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod export;

use std::path::Path;
use std::sync::OnceLock;

use assert_cmd::Command;
use tempfile::TempDir;

/// An empty config directory, shared by every test in this binary.
///
/// The curated commands build their request channel through `otl::auth`,
/// which looks for a credential file in the user's config directory. Without
/// this, a test run would read the developer's real `credentials.toml` - and
/// a stored session bound to a different instance is refused, so these tests
/// would pass or fail depending on whose machine they ran on. Leaked in the
/// process's lifetime on purpose: it has to outlive every child command, and
/// the OS reclaims it when the test binary exits.
fn isolated_config_dir() -> &'static Path {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().unwrap()).path()
}

/// An `otl` command with every environment variable the CLI reads scrubbed,
/// so a test never depends on the developer's shell.
pub fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE")
        .env_remove("OUTLINE_CONFIG")
        .env_remove("PAGER")
        .env_remove("BROWSER")
        .env("OUTLINE_CONFIG_DIR", isolated_config_dir())
        // These tests authenticate with the environment key, and the notice
        // about where a plaintext variable ends up is not what any of them
        // is about. `auth_api_key.rs` owns asserting that it IS printed.
        .env("OUTLINE_NO_KEY_WARNING", "1");
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
