//! Guard: how many ways there are to obtain a credential, and where they are.
//!
//! "A credential must not cross instances" is enforced on four paths: the
//! read path, the write path, `otl auth logout`'s revocation anchor, and
//! `otl auth info`'s live identity check. Each rule is stated once and
//! shared, so no path resolves on its own.
//!
//! This file answers the question - *what else can obtain a credential
//! without going through the gate?* - as a test rather than as an
//! assurance. There are THREE credential classes, they have
//! different correct anchors, and each has exactly one entrance:
//!
//! | class | what it authenticates | entrance | anchor |
//! |---|---|---|---|
//! | fixed API key | `Authorization` on the request channel | `config::Config::release` | the resolved profile/URL binding |
//! | OAuth session | `Authorization` on the request channel | `CredentialProvider::for_session` | `check_binding`, plus the session's own recorded origin |
//! | OAuth endpoint secrets (refresh token, client secret, registration access token) | token / registration / revocation endpoints | `auth::endpoint`'s single `.send()` | each credential's OWN recorded origin |
//!
//! # Why not one entrance for all three
//!
//! Because the third class must work when there is no configuration at all.
//! `otl auth logout` has to be able to revoke a token and delete a dynamic
//! registration on a machine whose `OUTLINE_URL` is wrong, missing, or points
//! somewhere new - that is precisely when a user reaches for it - so its
//! anchor is the origin each credential recorded for itself, not the resolved
//! instance. Routing it through `Config::release` would demand a usable
//! instance in order to clean up after an unusable one. And the second class
//! cannot be a `Config` either: a `Config` holds one fixed string, while a
//! session rotates on every refresh and needs a lock to do it.
//!
//! So the invariant is not "one entrance", it is "one entrance PER CLASS,
//! and no path that skips its class's entrance". The rules below pin that,
//! and `no_phone_home.rs` pins the third class's single `.send()`.
//!
//! # What this is, and what it is not
//!
//! A source scan, like `no_phone_home.rs` and `startup_guard.rs`. It does
//! not parse Rust, so a renamed import or a value passed through a helper
//! would pass it. What it guarantees is that the shapes this codebase uses
//! cannot be added silently: a second call to the gate, a second provider
//! constructor, a second place that builds an authenticated channel, or the
//! environment variable being named anywhere under `auth` all fail here, and
//! failing here means editing this file - which is the review it stands in
//! for.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

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

/// One construct, the files that may use it, and why nothing else may.
struct Confined {
    needle: &'static str,
    allowed: &'static [&'static str],
    why: &'static str,
}

const CONFINED: &[Confined] = &[
    // ---- class 1: the fixed API key ---------------------------------------
    Confined {
        needle: "Config::release",
        allowed: &["crates/otl/src/auth/mod.rs"],
        why: "the release gate has ONE caller, `auth::resolve_credential`. A \
              second one is a second place where the file-vs-environment \
              decision and the profile binding are applied, which is how they \
              come to disagree",
    },
    Confined {
        needle: "StoredCredential::new",
        allowed: &["crates/otl/src/auth/mod.rs"],
        why: "the credential file's key is handed to the gate from one place; \
              anywhere else would be a caller deciding for itself what the \
              file offers",
    },
    Confined {
        needle: "select_credential_source",
        allowed: &["crates/otl/src/config/mod.rs", "crates/otl/src/auth/mod.rs"],
        why: "which store supplies a fixed key is decided once, next to the \
              release that acts on the decision",
    },
    // `auth` reading this variable at all is the bug:
    // config scopes an environment key to the selected profile
    // (`OUTLINE_API_KEY_<PROFILE>`) and refuses to fall back to the global
    // one, and a module that names the global variable is a module that can
    // fall back.
    Confined {
        needle: "ENV_API_KEY",
        allowed: &[
            // The definition, and the profile-scoped variable's derivation.
            "crates/otl/src/config/mod.rs",
            // The one reader: the gate's own token source.
            "crates/otl/src/config/secret.rs",
            // Diagnostics that name the variable so a user can set it.
            "crates/otl/src/config/error.rs",
            "crates/otl/src/errors.rs",
            // The "no credentials" message, which lists all three ways in.
            "crates/otl/src/auth/mod.rs",
            // A hygiene observation for `auth info`: PRESENCE only, never the
            // value, and never used to decide anything. Documented as such at
            // the call site.
            "crates/otl/src/auth/report.rs",
        ],
        why: "the environment is config's store, reachable only through the \
              release gate. `otl auth info` read it directly and sent a \
              global key to a profile's instance that `otl api` refused on \
              the same configuration",
    },
    Confined {
        needle: "\"OUTLINE_API_KEY\"",
        allowed: &["crates/otl/src/config/mod.rs"],
        why: "spelled once, where the constant is defined. A literal \
              elsewhere is the same read with the constant's guard removed",
    },
    // ---- class 2: the OAuth session ---------------------------------------
    Confined {
        needle: "for_session",
        allowed: &[
            // The definition, and its unit tests.
            "crates/otl/src/auth/source.rs",
            // `resolve_credential`, the single credential path.
            "crates/otl/src/auth/mod.rs",
            // The identity call that labels a session `auth login` just
            // wrote. Deliberately session-only: a stored or exported key
            // would answer as a different principal.
            "crates/otl/src/auth/login.rs",
        ],
        why: "the only constructor of the renewing provider, and the only \
              thing that can turn a stored session into a bearer token. It \
              runs `check_binding` unconditionally, which is the property \
              every caller inherits",
    },
    // ---- both classes: building an authenticated channel ------------------
    Confined {
        needle: "with_credentials(",
        allowed: &[
            "crates/engine/src/client.rs",
            "crates/otl/src/auth/mod.rs",
            "crates/otl/src/auth/login.rs",
        ],
        why: "a renewing channel is built from a provider, and a provider \
              comes from `for_session` alone",
    },
    Confined {
        needle: "Client::new(",
        allowed: &["crates/otl/src/auth/mod.rs"],
        why: "a fixed-key channel is built in one place, from the string the \
              release gate returned. Anywhere else would be a key that \
              reached the wire without the gate having seen the settings",
    },
];

/// Lines of `source` that USE `needle`, with 1-based line numbers.
///
/// Comment lines are skipped: a mention in prose is not a call, and a guard
/// that forbids explaining itself makes the code less reviewable, which is
/// the opposite of the point. A trailing comment on a real line still leaves
/// the code before it, so it is still matched.
fn uses(source: &str, needle: &str) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| line.contains(needle))
        .map(|(index, _)| index + 1)
        .collect()
}

#[test]
fn every_way_to_obtain_a_credential_is_where_it_is_declared_to_be() {
    for rule in CONFINED {
        let offenders: Vec<String> = runtime_sources()
            .into_iter()
            .filter(|(path, _)| !rule.allowed.contains(&path.as_str()))
            .flat_map(|(path, source)| {
                uses(&source, rule.needle)
                    .into_iter()
                    .map(move |line| format!("  {path}:{line}"))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "{:?} is used at\n{}\nwhich may not use it: {}",
            rule.needle,
            offenders.join("\n"),
            rule.why
        );
    }
}

#[test]
fn no_rule_passes_by_looking_at_nothing() {
    // A rename would make a rule vacuous: it would stop matching anything
    // and keep reporting green. Every needle above must still be present
    // somewhere, and in at least one of its allowlisted files.
    let sources = runtime_sources();
    for rule in CONFINED {
        let hits: Vec<&String> = sources
            .iter()
            .filter(|(_, source)| !uses(source, rule.needle).is_empty())
            .map(|(path, _)| path)
            .collect();
        assert!(
            !hits.is_empty(),
            "{:?} no longer appears in any runtime source: the rule is stale",
            rule.needle
        );
        assert!(
            hits.iter()
                .any(|path| rule.allowed.contains(&path.as_str())),
            "{:?} appears only outside its allowlist ({hits:?}): the \
             allowlist names files that no longer use it",
            rule.needle
        );
    }
}

#[test]
fn every_allowlisted_file_exists() {
    // A path typo would silently exempt nothing and forbid everything, or -
    // worse, if the real file were renamed - forbid nothing.
    for rule in CONFINED {
        for file in rule.allowed {
            assert!(
                workspace_root().join(file).is_file(),
                "{file:?} is allowlisted for {:?} but does not exist",
                rule.needle
            );
        }
    }
}

#[test]
fn no_module_under_auth_reads_the_process_environment_for_a_credential() {
    // The narrower form of the ENV_API_KEY rule, and the one that survives a
    // rename: `auth` may read the environment for things that are not
    // credentials (where the config directory is, whether a warning is
    // silenced), and for nothing else. Registered per file with the variable
    // it reads, so a new read has to say which variable it wants.
    const ALLOWED_READS: &[(&str, &str)] = &[
        // `OUTLINE_CONFIG_DIR`: where the credential file lives. Not a
        // credential.
        ("crates/otl/src/auth/paths.rs", "env::var(name)"),
        // `OUTLINE_NO_KEY_WARNING`: whether to print a warning. Not a
        // credential.
        (
            "crates/otl/src/auth/selection.rs",
            "env::var(ENV_NO_KEY_WARNING)",
        ),
        // Presence of a plaintext key in the environment, for the hygiene
        // report. The VALUE is never read and never decides anything.
        (
            "crates/otl/src/auth/report.rs",
            "std::env::var(crate::config::ENV_API_KEY)",
        ),
    ];
    let mut offenders = Vec::new();
    for (path, source) in runtime_sources() {
        if !path.starts_with("crates/otl/src/auth/") {
            continue;
        }
        for (index, line) in source.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if !(line.contains("env::var") || line.contains("var_os")) {
                continue;
            }
            let registered = ALLOWED_READS
                .iter()
                .any(|(file, context)| path == *file && line.contains(context));
            if !registered {
                offenders.push(format!("  {path}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these environment reads under `auth` are not registered:\n{}\n\
         A credential comes from the config gate, never from the environment \
         read here - that is the [N1] bug. If the variable is genuinely not a \
         credential, add it to ALLOWED_READS with the reason.",
        offenders.join("\n")
    );
}
