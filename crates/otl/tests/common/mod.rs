//! Helpers shared by the CLI integration tests.
//!
//! Not a test target of its own (it lives in a subdirectory), so each test
//! file includes it with `mod common;`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod cache;
pub mod completions;
pub mod doctor;
pub mod export;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use assert_cmd::Command;
use tempfile::TempDir;

/// An empty config directory, shared by every test in this binary.
///
/// Commands build their request channel through `otl::auth`, which looks for
/// a credential file in the user's config directory. Without this, a test run
/// would read the developer's real `credentials.toml` - and a stored session
/// bound to a different instance is refused, so these tests would pass or
/// fail depending on whose machine they ran on. Leaked in the process's
/// lifetime on purpose: it has to outlive every child command, and the OS
/// reclaims it when the test binary exits.
///
/// A real (empty) directory rather than an absent path, unlike
/// [`no_cache_dir`]: an absent credential file and an unreadable one take
/// different code paths, and "absent" is the one these tests want.
pub fn isolated_config_dir() -> &'static Path {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().unwrap()).path()
}

/// Shut off every machine-dependent input, for a suite that builds its own
/// [`Command`] rather than starting from [`otl`].
///
/// Same list as [`otl`] minus the instance and credential variables, which
/// those suites set themselves.
pub fn isolate(cmd: &mut Command) -> &mut Command {
    cmd.env_remove("OUTLINE_PROFILE")
        .env("OUTLINE_CONFIG", "")
        .env("OUTLINE_CONFIG_DIR", isolated_config_dir())
        .env("OUTLINE_NO_KEY_WARNING", "1")
        .env(CACHE_DIR_ENV, no_cache_dir())
}

/// Environment variable that relocates the spec cache.
pub const CACHE_DIR_ENV: &str = "OTL_CACHE_DIR";

/// An `otl` command with every environment variable the CLI reads scrubbed,
/// so a test never depends on the developer's shell.
///
/// "Every" is meant literally, and it is the whole value of this helper: a
/// variable that is missed here does not make a test fail, it makes it
/// depend on the machine. Three kinds of state have to be shut off:
///
/// - credentials and instance (`OUTLINE_URL`, `OUTLINE_API_KEY`);
/// - the user config file and profile selection (`OUTLINE_CONFIG` empty
///   means "read no file at all");
/// - the CREDENTIAL file (`OUTLINE_CONFIG_DIR`): every command
///   now builds its request channel through `otl::auth`, which looks there
///   before it will send anything, so a developer with a stored session
///   would get different results from these tests than CI does;
/// - the synced spec cache, which decides which operations exist at all
///   - pointed at a directory that cannot contain one;
/// - and the output environment (`PAGER`, `BROWSER`).
pub fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE")
        .env("OUTLINE_CONFIG", "")
        .env(CACHE_DIR_ENV, no_cache_dir())
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

/// A cache directory guaranteed to hold no synced spec.
///
/// The effective operation table prefers a synced spec cache
/// over the one compiled into the binary. Every test that runs the binary
/// therefore has to say which of the two it means, or its assertions
/// silently depend on whether the machine happens to have run `otl spec
/// sync` - which changes operation names, paths, `api list` output and
/// stderr.
///
/// This path deliberately DOES NOT EXIST: a missing cache directory is
/// exactly the "no cache, use the built-in spec" case that the assertions
/// in these files are written against, and nothing has to be created or
/// cleaned up to get it. Tests that want a real cache create a `TempDir`
/// and point the variable there instead (see `spec_sync_e2e.rs`).
///
/// The name is unique per process and per call, and its absence is
/// asserted before it is handed out. A fixed shared path would be a
/// promise anyone could break by dropping a compatible cache there, which
/// would quietly re-couple all of these tests to external state.
pub fn no_cache_dir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = format!(
        "otl-tests-absent-spec-cache-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    );
    let path = std::env::temp_dir().join(unique);
    assert!(
        !path.exists(),
        "{} exists: these tests require a cache directory that cannot \
         contain a synced spec",
        path.display()
    );
    path
}
