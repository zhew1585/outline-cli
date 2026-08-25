//! Helpers shared by the CLI integration tests.
//!
//! Not a test target of its own (it lives in a subdirectory), so each test
//! file includes it with `mod common;`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Environment variable that relocates the spec cache.
pub const CACHE_DIR_ENV: &str = "OTL_CACHE_DIR";

/// A cache directory guaranteed to hold no synced spec.
///
/// Story 4.2 made the effective operation table prefer a synced spec cache
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
