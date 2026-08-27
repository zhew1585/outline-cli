//! The CLI never reaches the network on its own.
//!
//! No update check, no spec check, no telemetry - a request happens only
//! because the user asked for one. Fetching a spec is the one non-API
//! network call in the codebase, so the guard here is about keeping it
//! confined to the one command that owns it.
//!
//! Two layers:
//!
//! 1. a source scan over `crates/*/src/**`: the document fetch and the
//!    upstream URL appear only where they belong, the HTTP client crate
//!    and raw sockets appear only in the two engine channels, each channel
//!    has exactly one request-sending call, the two channels do not call
//!    each other, and the document channel's public surface is the
//!    reviewed one;
//! 2. a behavioural check that a local command completes with every
//!    outbound connection pointed at a dead proxy.
//!
//! # What this is, and what it is not
//!
//! These are REGRESSION GUARDS over this codebase's source text, not a
//! proof that no request can be made. A string scan does not parse Rust:
//! a request expressed in a form none of the patterns below name - a
//! renamed import, a macro, a subprocess, a dependency doing it on our
//! behalf - would pass. What the guards do guarantee is that the shapes
//! this codebase actually uses cannot be added silently: a new send site,
//! a new public entry point in the fetch module, a raw socket, or a NAMED
//! HTTP/TLS crate (the list in `FORBIDDEN_CRATES`, which is the set worth
//! naming today rather than every client that exists) appearing in a
//! manifest or in `Cargo.lock` all fail here. Adding one means editing
//! this file - which is the review these tests stand in for.
//!
//! # The third channel
//!
//! `project-context.md` names two channels; there are three, and the third
//! is the documented exception rather than an oversight. RFC 6749 token,
//! RFC 7591/7592 registration and RFC 7009 revocation requests cannot go
//! through the engine's authenticated channel - they carry no bearer token,
//! they are form-encoded rather than JSON, they are not described by the
//! OpenAPI spec the engine dispatches from, and the whole point of one of
//! them is to obtain the credential the engine would need. So `otl::auth`
//! has its own HTTP client, and the rules below confine it the same way:
//!
//! - exactly ONE `.send()` in the crate (`auth/endpoint.rs`), so every
//!   OAuth request is subject to the same TLS check, the same
//!   `redirect::Policy::none()`, and the same error scrubbing;
//! - other `auth` modules may NAME `reqwest` (a `&Client` parameter, a
//!   `Url` parse) but cannot send, which is what the send-site rule
//!   enforces and what makes the wider `reqwest` allowlist safe.
//!
//! The invariant they support (not replace) is the one in
//! `project-context.md`, amended by the paragraph above: every outbound
//! request goes through one of the three channels, and only a command the
//! user typed causes one.

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
            // The OAuth channel (see "The third channel" above). These
            // modules name `reqwest` - a `&Client` they were handed, a
            // `Url` they parse - but only `endpoint.rs` may send, which the
            // next rule is what actually enforces.
            "crates/otl/src/auth/endpoint.rs",
            "crates/otl/src/auth/metadata.rs",
            "crates/otl/src/auth/oauth.rs",
            "crates/otl/src/auth/dcr.rs",
            "crates/otl/src/auth/login.rs",
            "crates/otl/src/auth/client_acquisition.rs",
            "crates/otl/src/auth/logout_remote.rs",
            "crates/otl/src/auth/source.rs",
            "crates/otl/src/auth/loopback.rs",
            "crates/otl/src/auth/callback_request.rs",
            "crates/otl/src/auth/transport.rs",
        ],
        why: "HTTP belongs to the engine's two channels and to the OAuth \
              channel documented above; no other module may speak to the \
              network, in any shape",
    },
    Confined {
        needle: ".send()",
        allowed: &[
            "crates/engine/src/client.rs",
            "crates/engine/src/fetch.rs",
            // The OAuth channel's single send site. This is the rule that
            // makes the `reqwest` allowlist above safe to be as wide as it
            // is: a new OAuth request has to go through this one function,
            // which applies the TLS check, disables redirects and scrubs
            // the error - or fail here.
            "crates/otl/src/auth/endpoint.rs",
        ],
        why: "all HTTP goes through one of the three channels: the \
              authenticated request channel, the plain-document fetch, and \
              the OAuth endpoint channel",
    },
    // Sockets. The OAuth redirect (RFC 8252) is caught on a loopback
    // listener, which is INBOUND: it originates no request and reaches no
    // remote host, so it is not the thing these rules exist to stop. It is
    // still confined, to the two modules that implement that one flow, so a
    // socket cannot appear anywhere else under cover of "the callback
    // server does it too".
    //
    // `TcpStream` is allowed in the same two files for two reasons: the
    // server reads the accepted connection (a stream is what `accept`
    // returns), and their `#[cfg(test)]` harnesses dial the listener to
    // reproduce the byte-trickle and slot-exhaustion attacks.
    //
    // The send-site rule above does NOT bound these: a `TcpStream` has no
    // `.send()`, so nothing there constrains an outbound `connect`. The
    // backstop is `every_outbound_connect_is_to_the_loopback_callback`
    // below, which requires every `TcpStream::connect` to name
    // `CALLBACK_HOST` - plus the file confinement here, which keeps the
    // question to two modules.
    Confined {
        needle: "std::net",
        allowed: &[
            "crates/otl/src/auth/loopback.rs",
            "crates/otl/src/auth/callback_request.rs",
            // `IpAddr` only: the transport rule has to know whether a host
            // is a loopback IP literal. No socket.
            "crates/otl/src/auth/transport.rs",
        ],
        why: "a raw socket would bypass the request channels entirely; the \
              loopback callback listener is inbound and is confined to the \
              OAuth redirect flow",
    },
    Confined {
        needle: "TcpListener",
        allowed: &["crates/otl/src/auth/loopback.rs"],
        why: "listening for connections belongs to the OAuth redirect flow \
              alone; anywhere else it would be a service this CLI has no \
              business running",
    },
    Confined {
        needle: "TcpStream",
        allowed: &[
            "crates/otl/src/auth/loopback.rs",
            "crates/otl/src/auth/callback_request.rs",
        ],
        why: "a raw socket would bypass the request channels entirely; the \
              accepted loopback connection is the one exception",
    },
    Confined {
        needle: "UdpSocket",
        allowed: &[],
        why: "a raw socket would bypass the request channels entirely",
    },
];

/// Rules whose needle is expected to be absent everywhere, so the
/// "not vacuous" check below must not demand a hit for them.
const EXPECTED_ABSENT: &[&str] = &["UdpSocket"];

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

/// The one constant an outbound `connect` may aim at.
const LOOPBACK_HOST_CONST: &str = "CALLBACK_HOST";

#[test]
fn every_outbound_connect_is_to_the_loopback_callback() {
    // Relaxing `std::net`/`TcpStream` for the OAuth redirect flow opened a
    // hole the other rules do not close: `accept()` is inbound, but
    // `TcpStream::connect` is outbound and has no `.send()` for the send-site
    // rule to count. So it gets its own rule, and it is a rule about the
    // ARGUMENT rather than the file: a connect may only aim at the loopback
    // callback constant.
    //
    // Every current call is a test harness dialling its own listener. The
    // rule is written so that a runtime one would have to name the same
    // constant, which is a loopback IP literal (never the NAME `localhost`,
    // which a resolver can point elsewhere - see `auth::transport`).
    let mut offenders = Vec::new();
    for (path, source) in runtime_sources() {
        for (index, line) in source.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if !line.contains("TcpStream::connect") && !line.contains("UdpSocket::bind") {
                continue;
            }
            if !line.contains(LOOPBACK_HOST_CONST) {
                offenders.push(format!("  {path}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these outbound connections do not aim at {LOOPBACK_HOST_CONST}:\n{}\n\
         A socket that leaves this machine bypasses all three request \
         channels and everything they enforce.",
        offenders.join("\n")
    );

    // Not vacuous: there ARE connects, and the constant they name really is
    // a loopback address rather than a hostname.
    let connects: usize = runtime_sources()
        .iter()
        .map(|(_, source)| source.matches("TcpStream::connect").count())
        .sum();
    assert!(
        connects > 0,
        "no outbound connect found at all: the rule is looking at nothing"
    );
    assert_eq!(
        otl::auth::loopback::CALLBACK_HOST,
        "127.0.0.1",
        "the callback host is no longer a loopback IP literal; a name can be \
         resolved to another host, which is the whole reason this is pinned"
    );
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

/// HTTP clients and TLS backends that must not appear anywhere in the
/// dependency graph, manifests and lock file alike.
const FORBIDDEN_CRATES: &[&str] = &[
    "ureq",
    "curl",
    "isahc",
    "attohttpc",
    "surf",
    "minreq",
    "native-tls",
    "openssl",
    "openssl-sys",
];

/// Crates that legitimately appear in `Cargo.lock` because reqwest brings
/// them, but that this workspace must not depend on DIRECTLY.
///
/// A lock-file scan cannot tell "pulled in by reqwest" from "used by us",
/// so the distinction is made where it is visible: our own manifests.
const INDIRECT_ONLY_CRATES: &[&str] = &["hyper", "rustls", "h2", "tokio-rustls"];

/// Whether one manifest line declares `crate_name` as a dependency, in
/// any of the forms TOML allows.
///
/// Four shapes, all real declarations:
///
/// ```toml
/// hyper = "1"                      # inline
/// "hyper" = "1"                    # inline, quoted key (also 'hyper')
/// [dependencies.hyper]             # table (also [target."...".dependencies.hyper])
/// h = { package = "hyper", ... }   # renamed - the KEY says nothing
/// ```
///
/// The renamed form is why this cannot just look at keys: `package = "..."`
/// is what actually pulls the crate in, whatever the key is called. A
/// feature string inside an array (`"rustls"` in reqwest's own feature
/// list) is none of these and must keep passing - choosing the TLS backend
/// that way is the point.
fn is_dependency_declaration(line: &str, crate_name: &str) -> bool {
    // Comments are not declarations. The caller filters them too; doing it
    // here as well means this function is correct on its own, which is
    // what its unit test then actually establishes.
    if line.trim_start().starts_with('#') {
        return false;
    }
    // Quotes are stripped wherever they appear in the key: `"hyper"` and
    // `"hyper".workspace` are both this crate, and a crate name can never
    // contain a quote itself.
    let key: String = line
        .split('=')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|character| !matches!(character, '"' | '\''))
        .collect();
    let key = key.trim();
    let inline =
        line.contains('=') && (key == crate_name || key.starts_with(&format!("{crate_name}.")));
    let table = line.starts_with('[')
        && line.ends_with(']')
        && line
            .trim_matches(['[', ']'])
            .rsplit('.')
            .next()
            .is_some_and(|last| last.trim_matches(['"', '\'']) == crate_name);
    // `package = "hyper"` renames the crate; the key is then irrelevant.
    let renamed = ["package = ", "package=", "package =\""]
        .iter()
        .any(|prefix| match line.split_once(prefix) {
            Some((_, rest)) => {
                rest.trim_start()
                    .trim_start_matches(['"', '\''])
                    .starts_with(&format!("{crate_name}\""))
                    || rest
                        .trim_start()
                        .trim_start_matches(['"', '\''])
                        .starts_with(&format!("{crate_name}'"))
            }
            None => false,
        });
    inline || table || renamed
}

/// No new dependency may bring a second HTTP stack or TLS client in
/// through the back door, which would make the source rules above moot.
///
/// Every manifest in the workspace is checked, not just the root one - a
/// member crate can declare its own dependencies - and so is `Cargo.lock`,
/// which is where anything that actually got resolved shows up, including
/// transitively.
///
/// Limits: the forbidden list is the set of stacks worth naming today, not
/// every HTTP client that exists. A crate nobody listed here would pass -
/// but it would still have to appear in a manifest diff, and its use would
/// still have to get past the source rules above.
#[test]
fn no_second_http_stack_reaches_the_dependency_graph() {
    let root = workspace_root();
    let mut manifests = vec![root.join("Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let manifest = entry.unwrap().path().join("Cargo.toml");
        if manifest.is_file() {
            manifests.push(manifest);
        }
    }
    assert!(manifests.len() >= 4, "found only {manifests:?}");
    for manifest in &manifests {
        let text = std::fs::read_to_string(manifest).unwrap();
        for forbidden in FORBIDDEN_CRATES {
            assert!(
                !text.contains(forbidden),
                "{forbidden:?} appears in {}: a second HTTP/TLS stack would \
                 sidestep the single-channel rule",
                manifest.display()
            );
        }
        // reqwest's own dependencies are fine in the lock file and not
        // fine as a direct dependency of ours: using one directly means
        // speaking HTTP or TLS outside the two channels.
        for indirect in INDIRECT_ONLY_CRATES {
            // Two ways TOML declares a dependency, and only these two are
            // a declaration:
            //
            //   inline:  `hyper = "1"`, `hyper.workspace = true`
            //   table:   `[dependencies.hyper]`, `[target."cfg(unix)".dependencies.hyper]`
            //
            // A feature string inside an array (`"rustls"` in reqwest's
            // own feature list) is neither, and must keep passing:
            // choosing the TLS backend that way is the point.
            let declared = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.starts_with('#'))
                .any(|line| is_dependency_declaration(line, indirect));
            assert!(
                !declared,
                "{indirect:?} is declared directly in {}: it may only arrive \
                 as a dependency of reqwest",
                manifest.display()
            );
        }
    }

    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
    for forbidden in FORBIDDEN_CRATES {
        assert!(
            !lock.contains(&format!("name = \"{forbidden}\"")),
            "{forbidden:?} is in Cargo.lock: something pulled a second \
             HTTP/TLS stack into the graph"
        );
    }
}

/// Other ways reqwest can be told to perform a request.
///
/// Forbidden INSIDE the two channel files, where a reqwest client is in
/// scope: each channel sends with `.send()` and nothing else, so that
/// counting the sends means something. Elsewhere `.execute(` is the
/// ENGINE's own dispatcher (`client.execute(op, ...)`), which is how a
/// command is supposed to call the API - and reqwest cannot be reached
/// from there anyway, because the `reqwest` rule confines it to these two
/// files.
const CHANNEL_SEND_APIS: &[&str] = &[
    ".execute(",
    "blocking::get(",
    "reqwest::get(",
    ".send_with(",
    "Request::new(",
];

/// Shapes that name reqwest outright, forbidden everywhere outside the two
/// channels (the `reqwest` confinement rule covers these too; they are
/// listed for a clearer failure message).
const NAMED_REQWEST_SENDS: &[&str] = &["blocking::get(", "reqwest::get("];

/// The dependency detector, tested against the forms it has to catch and
/// the ones it must not. Without this, the guard is only ever exercised
/// against manifests that happen to be clean - which is how the table form
/// slipped through before.
#[test]
fn the_dependency_detector_knows_every_toml_form() {
    for declaration in [
        "hyper = \"1\"",
        "hyper=\"1\"",
        "hyper.workspace = true",
        "\"hyper\" = \"1\"",
        "'hyper' = \"1\"",
        "\"hyper\".workspace = true",
        "[dependencies.hyper]",
        "[dev-dependencies.hyper]",
        "[target.\"cfg(unix)\".dependencies.hyper]",
        "[dependencies.\"hyper\"]",
        // Renamed: the key says nothing, `package` says everything.
        "h = { package = \"hyper\", version = \"1\" }",
        "anything = { version = \"1\", package = \"hyper\" }",
        "package = \"hyper\"",
    ] {
        assert!(
            is_dependency_declaration(declaration.trim(), "hyper"),
            "{declaration:?} is a declaration"
        );
    }
    for innocent in [
        "\"hyper\",", // a feature string in an array
        "features = [\"hyper\"]",
        "# hyper = \"1\"",    // commented out (filtered earlier)
        "hyperlocal = \"1\"", // a different crate
        "[dependencies.hyperlocal]",
        "reqwest = { features = [\"hyper\"] }",
        "h = { package = \"hyperlocal\" }", // a different crate, renamed
        "# package = \"hyper\"",
    ] {
        assert!(
            !is_dependency_declaration(innocent.trim(), "hyper"),
            "{innocent:?} is not a declaration of `hyper`"
        );
    }
}

/// One request-sending call per channel, and no other way of sending.
///
/// Counting is what a file-wide allowlist cannot do: it makes a SECOND
/// request site inside an already-allowlisted file fail. The alternative
/// APIs above are forbidden outright, because they would not change the
/// count.
///
/// Limits (see the module docs): this counts a literal `.send()` on a
/// line. A call split across lines, or built through a renamed alias,
/// would not be recognised - the count would then fail closed on the
/// EXPECTED side (a channel with zero recognised sends is also an error),
/// which is why the expected number is asserted exactly rather than as a
/// maximum.
#[test]
fn each_channel_has_exactly_one_send() {
    for (file, expected) in [
        ("crates/engine/src/client.rs", 1),
        ("crates/engine/src/fetch.rs", 1),
        // The OAuth channel. Counted here for the same reason as the other
        // two: the value of "one send site" is that the TLS check, the
        // disabled redirects and the error scrubbing are applied by
        // construction rather than remembered at each call.
        ("crates/otl/src/auth/endpoint.rs", 1),
    ] {
        let source = std::fs::read_to_string(workspace_root().join(file)).unwrap();
        let code: Vec<&str> = source
            .lines()
            // Skip doc comments: they discuss the rule.
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        let sends = code.iter().filter(|line| line.contains(".send()")).count();
        assert_eq!(
            sends, expected,
            "{file} has {sends} `.send()` calls, expected {expected}: each \
             channel must have exactly one place where a request is made"
        );
        for api in CHANNEL_SEND_APIS {
            let hits: Vec<&&str> = code.iter().filter(|line| line.contains(api)).collect();
            assert!(
                hits.is_empty(),
                "{file} uses {api:?} ({hits:?}): a channel sends with `.send()` \
                 and nothing else, so that counting it means something"
            );
        }
    }

    // And nowhere else at all.
    const SENDERS: &[&str] = &[
        "crates/engine/src/client.rs",
        "crates/engine/src/fetch.rs",
        "crates/otl/src/auth/endpoint.rs",
    ];
    for (path, source) in runtime_sources() {
        if SENDERS.contains(&path.as_str()) {
            continue;
        }
        for api in NAMED_REQWEST_SENDS {
            assert!(
                !source.contains(api),
                "{path} uses {api:?}: only the three channels may send"
            );
        }
    }
}

/// The two channels may not call each other.
///
/// The document channel must not reach for the credential-carrying client
/// (that would send the bearer token to a document host, the very thing
/// the split exists to prevent), and the RPC channel must not fetch
/// documents.
#[test]
fn the_two_channels_do_not_compose() {
    // Code only: the doc comments in both files discuss the other channel
    // on purpose, which is how the split stays explained.
    let code = |file: &str| -> String {
        std::fs::read_to_string(workspace_root().join(file))
            .unwrap()
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let fetch = code("crates/engine/src/fetch.rs");
    // Note: `reqwest::blocking::Client` is not the engine's client; the
    // needles below name the engine's own type and module.
    for forbidden in [
        "crate::client",
        "client::Client",
        "crate::Client",
        "execute(",
    ] {
        assert!(
            !fetch.contains(forbidden),
            "{forbidden:?} in fetch.rs: the document channel must not use the \
             credential-carrying client"
        );
    }
    let client = code("crates/engine/src/client.rs");
    for forbidden in ["crate::fetch", "fetch::", "DocumentFetch", "get_text"] {
        assert!(
            !client.contains(forbidden),
            "{forbidden:?} in client.rs: the RPC channel must not fetch documents"
        );
    }
}

/// The document channel's public surface is pinned.
///
/// This is what closes the "add a new entry point inside an allowlisted
/// file" route: a new public item there is a new way to make a request,
/// and it has to be added here deliberately - which is exactly the review
/// this module is standing in for.
///
/// Both directions are checked: nothing unexpected is exported, and
/// everything expected is still exported (so renaming an item cannot
/// quietly empty the list). `pub use` is included in the scan, because a
/// re-export is an entry point too.
#[test]
fn the_document_channel_exports_only_what_is_reviewed() {
    const EXPECTED: &[&str] = &[
        "pub const MAX_DOCUMENT_BYTES",
        "pub enum FetchError",
        "pub struct FetchedDocument",
        "pub struct DocumentFetch",
        "pub fn new",
        "pub fn with_retry_policy",
        "pub fn with_throttle",
        "pub fn with_max_bytes",
        "pub fn get_text",
        "pub fn fetch_document",
        "pub fn document_origin",
    ];
    let source =
        std::fs::read_to_string(workspace_root().join("crates/engine/src/fetch.rs")).unwrap();
    let exported: Vec<String> = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        // Every form a public item can start with, `pub use` and
        // `pub async fn` included. Struct and enum FIELDS are `pub` too,
        // but they are not entry points, so they are filtered out below by
        // requiring one of these keywords.
        .filter(|line| {
            [
                "pub fn ",
                "pub async fn ",
                "pub const ",
                "pub struct ",
                "pub enum ",
                "pub use ",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        })
        .map(|line| {
            // Keep the declaration head: everything up to the first
            // delimiter that starts parameters, bounds or a body.
            let head: String = line
                .chars()
                .take_while(|character| !matches!(character, '(' | '<' | '{' | ':' | ';' | '='))
                .collect();
            head.trim().to_string()
        })
        .collect();

    for item in &exported {
        assert!(
            EXPECTED.contains(&item.as_str()),
            "fetch.rs exports {item:?}, which is not in the reviewed list: a \
             new public item in the document channel is a new way to make a \
             request"
        );
    }
    for expected in EXPECTED {
        assert!(
            exported.iter().any(|item| item == expected),
            "fetch.rs no longer exports {expected:?}: the reviewed list is \
             stale, so it no longer constrains anything"
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
