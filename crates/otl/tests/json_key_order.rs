//! `--json` output preserves the server's key order (merge consequence).
//!
//! Before the spec compiler became a shared crate, `preserve_order` was a
//! BUILD-dependency feature: the build script needed the document's own
//! property order (the schema's statement of which response fields matter
//! most, which the table renderer ranks columns by), and resolver 2 kept
//! that out of the runtime, where `serde_json` sorted map keys.
//!
//! Now one crate does the parsing for both the build script and `otl spec
//! sync`, so the feature cannot differ between them: if it did, a synced
//! table and the built-in table would order fields differently, the column
//! ranking would depend on where the spec came from, and `spec_parity`
//! would fail. The feature therefore unifies into the runtime as well, and
//! response JSON now comes out in the order the server sent it rather than
//! sorted.
//!
//! # Why this is allowed on the curated commands too
//!
//! `otl api` output is explicitly unstable, so the change is free there.
//! The curated commands' `--json` shape IS a semver contract, and this
//! changes their output, so the reasoning has to be written down:
//!
//! - a JSON object is by definition an UNORDERED set of members (RFC 8259),
//!   so "shape" - which fields exist, with which types - does not cover the
//!   order they are serialized in. A consumer that depends on key order is
//!   depending on something JSON does not promise;
//! - the README's own statement of the contract is that `--json`
//!   "round-trips what the server sent". Preserving the server's order is
//!   MORE faithful to that promise than sorting was: the sorted output was
//!   the version that failed to round-trip;
//! - and the version is 0.x, where the README says breaking changes may
//!   land in a minor release.
//!
//! So: allowed, deliberate, and asserted here for both surfaces - the
//! unstable one and the contracted one - because a real change to a
//! protected surface should not exist only in a review thread.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{no_cache_dir, CACHE_DIR_ENV};

/// The unstable surface: `otl api`.
#[tokio::test(flavor = "multi_thread")]
async fn api_json_output_keeps_the_servers_key_order() {
    let server = MockServer::start().await;
    // Deliberately not alphabetical: "zebra" before "alpha".
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "zebra": 1, "middle": 2, "alpha": 3 }
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let stdout = tokio::task::spawn_blocking(move || {
        let output = Command::cargo_bin("otl")
            .unwrap()
            .env("OUTLINE_URL", &uri)
            .env("OUTLINE_API_KEY", "test-key")
            .env(CACHE_DIR_ENV, no_cache_dir())
            .env_remove("OUTLINE_PROFILE")
            .env("OUTLINE_CONFIG", "")
            .args(["api", "documents.info", "id=doc-1", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        String::from_utf8_lossy(&output.stdout).into_owned()
    })
    .await
    .unwrap();

    let zebra = stdout.find("zebra").expect("zebra in output");
    let middle = stdout.find("middle").expect("middle in output");
    let alpha = stdout.find("alpha").expect("alpha in output");
    assert!(
        zebra < middle && middle < alpha,
        "keys were reordered (sorted?) instead of kept as the server sent \
         them; see this file's header for why that matters:\n{stdout}"
    );
}

/// The CONTRACTED surface: a curated command. Same behaviour, and the one
/// that needed the argument above.
#[tokio::test(flavor = "multi_thread")]
async fn curated_json_output_keeps_the_servers_key_order() {
    let server = MockServer::start().await;
    // Deliberately not alphabetical, and nested one level down so this also
    // covers an object inside the payload.
    Mock::given(method("POST"))
        .and(path("/api/documents.search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "context": "ctx",
                "document": { "zebra": "z", "middle": "m", "alpha": "a" }
            }],
            "pagination": { "offset": 0, "limit": 100 },
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let stdout = tokio::task::spawn_blocking(move || {
        let output = Command::cargo_bin("otl")
            .unwrap()
            .env("OUTLINE_URL", &uri)
            .env("OUTLINE_API_KEY", "test-key")
            .env(CACHE_DIR_ENV, no_cache_dir())
            .env_remove("OUTLINE_PROFILE")
            .env("OUTLINE_CONFIG", "")
            .args(["docs", "search", "deploy", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    })
    .await
    .unwrap();

    let zebra = stdout.find("zebra").expect("zebra in output");
    let middle = stdout.find("middle").expect("middle in output");
    let alpha = stdout.find("alpha").expect("alpha in output");
    assert!(
        zebra < middle && middle < alpha,
        "a curated command reordered the server's keys; the contract this \
         does and does not cover is argued in this file's header:\n{stdout}"
    );
}
