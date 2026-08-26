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
//! That is within contract - `otl api` output is explicitly unstable - and
//! arguably better, since it shows the API's own ordering. This test exists
//! so the change is a decision with a name on it rather than a surprise,
//! and so that flipping it back would fail somewhere visible.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{no_cache_dir, CACHE_DIR_ENV};

#[tokio::test(flavor = "multi_thread")]
async fn json_output_keeps_the_servers_key_order() {
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
