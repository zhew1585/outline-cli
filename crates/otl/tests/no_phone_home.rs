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
        needle: "get_text",
        allowed: &[
            "crates/engine/src/fetch.rs",
            "crates/otl/src/commands/spec.rs",
        ],
        why: "the same rule for the fetcher's method form, which a caller \
              could otherwise use to bypass the free function",
    },
    Confined {
        needle: "UPSTREAM_SPEC_URL",
        allowed: &[
            "crates/otl/src/spec/mod.rs",
            "crates/otl/src/commands/spec.rs",
        ],
        why: "the upstream spec source may only be read by the sync command",
    },
    // The HTTP stack itself is confined, not just the shapes of a call.
    // `.send()` alone was too weak a guard: `reqwest::blocking::get(url)`,
    // `Client::execute(request)`, or a `.send()` split across lines all
    // pass it. Naming the CRATE catches every form of every API it has,
    // and reaching for a different HTTP client or a raw socket means
    // adding a dependency, which the rules below also catch.
    Confined {
        needle: "reqwest",
        allowed: &[
            "crates/engine/src/client.rs",
            "crates/engine/src/error.rs",
            "crates/engine/src/fetch.rs",
            // One doc comment explaining why transport errors are not
            // printed; no code.
            "crates/otl/src/exit.rs",
        ],
        why: "HTTP belongs to the engine's two channels; no other module may \
              speak to the network, in any shape",
    },
    Confined {
        needle: ".send()",
        allowed: &["crates/engine/src/client.rs", "crates/engine/src/fetch.rs"],
        why: "all HTTP goes through the engine's two channels: the \
              authenticated request channel and the plain-document fetch",
    },
    Confined {
        needle: "std::net",
        allowed: &[],
        why: "a raw socket would bypass the request channels entirely",
    },
    Confined {
        needle: "TcpStream",
        allowed: &[],
        why: "a raw socket would bypass the request channels entirely",
    },
    Confined {
        needle: "UdpSocket",
        allowed: &[],
        why: "a raw socket would bypass the request channels entirely",
    },
];

/// Rules whose needle is expected to be absent everywhere, so the
/// "not vacuous" check below must not demand a hit for them.
const EXPECTED_ABSENT: &[&str] = &["std::net", "TcpStream", "UdpSocket"];

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
    // A renamed symbol would make a rule pass trivially. The socket rules
    // are exempt: their whole point is that nothing matches them.
    let sources = runtime_sources();
    for rule in CONFINED {
        if EXPECTED_ABSENT.contains(&rule.needle) {
            continue;
        }
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

/// Every allowlisted file must exist. A rename would otherwise turn an
/// entry into a permanent hole in whichever rule it belongs to.
#[test]
fn allowlisted_files_all_exist() {
    let root = workspace_root();
    for rule in CONFINED {
        for allowed in rule.allowed {
            assert!(
                root.join(allowed).is_file(),
                "{allowed} is allowlisted for {:?} but does not exist",
                rule.needle
            );
        }
    }
}

/// No new dependency may bring a second HTTP stack or TLS client in
/// through the back door, which would make the source rules above moot.
#[test]
fn no_second_http_stack_is_declared() {
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml")).unwrap();
    for forbidden in [
        "ureq",
        "curl",
        "hyper =",
        "isahc",
        "attohttpc",
        "surf",
        "native-tls",
        "openssl",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "{forbidden:?} appears in the workspace manifest: a second HTTP/TLS \
             stack would sidestep the single-channel rule"
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
