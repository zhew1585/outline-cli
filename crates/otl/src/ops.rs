//! The effective IR table.
//!
//! Two sources, in order:
//!
//! 1. the cache written by `otl spec sync`, if one exists and passes every
//!    check ([`crate::spec::cache`]);
//! 2. otherwise the built-in table `build.rs` compiled from the vendored
//!    spec into the binary.
//!
//! Resolution is lazy and happens at most once per process: `otl --help`,
//! `otl --version` and any clap usage error exit before an operation is
//! ever looked up, so the startup budget pays nothing for the cache. When
//! a table IS needed the cost is one `stat` plus one read and decode of a
//! few hundred kilobytes - no OpenAPI document is parsed at run time, ever
//! (the sole exception being the `spec sync` command itself).
//!
//! A cache that cannot be used is DISCARDED, never repaired: the built-in
//! table takes over and a one-line diagnostic on stderr says so, with the
//! command that fixes it. Failing here would mean a damaged cache file
//! bricks every command, which is exactly what must not happen.

use std::sync::OnceLock;

use engine::ir::OpSpec;

use crate::stdio;

// Defines `pub static OPS: &[engine::ir::OpSpec]`, the built-in table.
include!(concat!(env!("OUT_DIR"), "/ir_table.rs"));

/// The resolved table, computed once per process.
static TABLE: OnceLock<&'static [OpSpec]> = OnceLock::new();

/// Built-in definitions that stable commands must not lose to spec drift.
const CURATED_OPERATIONS: &[&str] = &[
    "collections.archive",
    "comments.resolve",
    "comments.unresolve",
    "comments.list",
];

/// The operations this process uses: the synced table if one is usable,
/// the built-in table otherwise.
pub fn table() -> &'static [OpSpec] {
    TABLE.get_or_init(resolve)
}

/// Whether the effective table came from a cache rather than the built-in
/// spec. Resolves the table if it has not been resolved yet.
pub fn is_synced() -> bool {
    !std::ptr::eq(table(), OPS)
}

/// Look up an operation by its `resource.method` name.
pub fn find(name: &str) -> Option<&'static OpSpec> {
    table().iter().find(|op| op.name == name)
}

/// Resolve a stable command's contract.
///
/// The generic API surface remains an exact view of a synced table. Stable
/// commands use the vetted built-in definition for known upstream omissions,
/// so a sync cannot remove or weaken those commands.
pub(crate) fn find_curated(name: &str) -> Option<&'static OpSpec> {
    if CURATED_OPERATIONS.contains(&name) {
        return OPS.iter().find(|op| op.name == name);
    }
    find(name)
}

/// Resolve the effective table, warning about (and ignoring) a cache that
/// cannot be used.
fn resolve() -> &'static [OpSpec] {
    match crate::spec::cache::load() {
        // No cache at all: the normal, silent case.
        Ok(None) => OPS,
        // Leaked on purpose: the table lives as long as the process, and a
        // `&'static` keeps every caller signature unchanged whether the
        // operations came from the binary or from disk.
        Ok(Some(cached)) => Box::leak(cached.ops.into_boxed_slice()),
        Err(error) => {
            let kind = if error.is_stale() {
                "outdated"
            } else {
                "damaged"
            };
            stdio::write_diagnostic_line(&format!(
                "warning: ignoring {kind} spec cache, using the built-in spec instead: \
                 {error}; {}",
                error.remedy()
            ));
            OPS
        }
    }
}
