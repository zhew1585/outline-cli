//! NFR4: the CLI never reaches the network on its own.
//!
//! No update check, no spec check, no telemetry - a request happens only
//! because the user asked for one. Story 4.2 adds the first non-API
//! network call to the codebase (fetching a spec), so the guard here is
//! about keeping it confined to the one command that owns it.
//!
//! Two layers:
//!
//! 1. a source scan proving that only `spec sync` can reach the document
//!    fetch, and that HTTP lives in exactly two known places in `engine`;
//! 2. a behavioural check that a local command completes with every
//!    outbound connection pointed at a dead proxy.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

/// Every `crates/*/src/**/*.rs`, as (path relative to the workspace root,
/// contents).
fn runtime_sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    collect(&workspace_root().join("crates"), &mut files);
    assert!(!files.is_empty(), "no runtime sources found");
    files
}

fn collect(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            // Only sources under a crate's `src/`; build scripts and tests
            // are not the runtime.
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            collect(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path
                .strip_prefix(workspace_root())
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if relative.contains("/src/") {
                out.push((relative, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
}

/// Which files may mention a given construct, and why nothing else may.
struct Confined {
    needle: &'static str,
    allowed: &'static [&'static str],
    why: &'static str,
}

const CONFINED: &[Confined] = &[
    Confined {
        needle: "fetch_document",
        allowed: &[
            "crates/engine/src/fetch.rs",
            "crates/otl/src/commands/spec.rs",
        ],
        why: "fetching a document is `spec sync`'s privilege alone; any other \
              caller would be a network request the user did not ask for",
    },
    Confined {
        needle: "UPSTREAM_SPEC_URL",
        allowed: &[
            "crates/otl/src/spec/mod.rs",
            "crates/otl/src/commands/spec.rs",
        ],
        why: "the upstream spec source may only be read by the sync command",
    },
    Confined {
        needle: ".send()",
        allowed: &["crates/engine/src/client.rs", "crates/engine/src/fetch.rs"],
        why: "all HTTP goes through the engine's two channels: the \
              authenticated request channel and the plain-document fetch",
    },
];

#[test]
fn network_entry_points_stay_confined() {
    for rule in CONFINED {
        let offenders: Vec<String> = runtime_sources()
            .into_iter()
            .filter(|(path, source)| {
                source.contains(rule.needle) && !rule.allowed.contains(&path.as_str())
            })
            .map(|(path, _)| path)
            .collect();
        assert!(
            offenders.is_empty(),
            "{:?} appears in {offenders:?}, which may not use it: {}",
            rule.needle,
            rule.why
        );
    }
}

#[test]
fn the_confinement_rules_are_not_vacuous() {
    // A renamed symbol would make every rule above pass trivially.
    let sources = runtime_sources();
    for rule in CONFINED {
        let hits = sources
            .iter()
            .filter(|(_, source)| source.contains(rule.needle))
            .count();
        assert!(
            hits > 0,
            "{:?} no longer appears in any runtime source: the rule is stale",
            rule.needle
        );
    }
}

/// A local command must not need the network at all. Both proxy variables
/// point at a dead port, so any outbound HTTP(S) attempt that honours them
/// fails; the command still has to succeed.
#[test]
fn a_local_command_works_with_every_outbound_route_dead() {
    let cache = TempDir::new().unwrap();
    for args in [vec!["api", "list"], vec!["--help"], vec!["--version"]] {
        Command::cargo_bin("otl")
            .unwrap()
            .env("OTL_CACHE_DIR", cache.path())
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .args(&args)
            .assert()
            .success();
    }
}
