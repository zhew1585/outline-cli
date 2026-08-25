//! End-to-end `otl spec sync` / `otl spec reset` (Story 4.2).
//!
//! Every case runs the real binary against a wiremock server and a
//! throwaway cache directory (`OTL_CACHE_DIR`): no test touches the real
//! network or the developer's cache. Output is asserted in `--json` mode,
//! which is also what a pipe gets by contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Path within the mock server serving the document.
const SPEC_PATH: &str = "/openapi/spec3.json";

/// A tiny OpenAPI document with one operation that the vendored spec does
/// NOT have, so "the new endpoint is usable" is unambiguous.
fn document_with(extra_op: &str) -> String {
    format!(
        r#"{{
          "openapi": "3.0.0",
          "paths": {{
            "/documents.info": {{
              "post": {{
                "summary": "Retrieve a document",
                "requestBody": {{"content": {{"application/json": {{"schema": {{
                  "type": "object", "required": ["id"],
                  "properties": {{"id": {{"type": "string"}}}}}}}}}}}}
              }}
            }},
            "/{extra_op}": {{
              "post": {{
                "summary": "Brand new endpoint",
                "requestBody": {{"content": {{"application/json": {{"schema": {{
                  "type": "object",
                  "properties": {{"note": {{"type": "string"}}}}}}}}}}}}
              }}
            }}
          }}
        }}"#
    )
}

async fn serve(body: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SPEC_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    server
}

/// The `otl` binary with an isolated cache directory and no Outline env.
fn otl(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env("OTL_CACHE_DIR", cache)
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY");
    cmd
}

/// Run `otl` off the async runtime; returns (stdout, stderr, exit code).
async fn run(cache: &Path, args: &[&str]) -> (String, String, i32) {
    let cache = cache.to_path_buf();
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let output = otl(&cache).args(&args).output().unwrap();
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.status.code().unwrap_or(-1),
        )
    })
    .await
    .unwrap()
}

fn parse(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout is JSON")
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_makes_a_new_endpoint_available_to_api_list() {
    let server = serve(document_with("things.brandNew")).await;
    let cache = TempDir::new().unwrap();
    let url = format!("{}{SPEC_PATH}", server.uri());

    // Before the sync the operation does not exist at all.
    let (_, stderr, code) = run(cache.path(), &["api", "things.brandNew"]).await;
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("unknown API operation"), "{stderr}");

    let (stdout, _, code) = run(cache.path(), &["spec", "sync", "--url", &url, "--json"]).await;
    assert_eq!(code, 0, "{stdout}");
    let report = parse(&stdout);
    assert_eq!(report["changed"], Value::Bool(true));
    assert_eq!(report["operations"], Value::from(2));
    assert_eq!(report["added"][0], Value::from("things.brandNew"));
    assert_eq!(report["spec_hash"].as_str().unwrap().len(), 64);

    // After the sync it is listed...
    let (stdout, _, code) = run(cache.path(), &["api", "list"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("things.brandNew"), "{stdout}");
    assert!(stdout.contains("Brand new endpoint"), "{stdout}");

    // ...and dispatchable: it fails on missing configuration, not on an
    // unknown operation, which proves the lookup went through the cache.
    let (_, stderr, code) = run(cache.path(), &["api", "things.brandNew"]).await;
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("OUTLINE_URL"), "{stderr}");
    assert!(!stderr.contains("unknown API operation"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_synced_spec_replaces_the_built_in_one_entirely() {
    let server = serve(document_with("things.brandNew")).await;
    let cache = TempDir::new().unwrap();
    let url = format!("{}{SPEC_PATH}", server.uri());
    run(cache.path(), &["spec", "sync", "--url", &url]).await;

    let (stdout, _, _) = run(cache.path(), &["api", "list"]).await;
    let listed = stdout.lines().count();
    assert_eq!(listed, 2, "the whole table comes from the cache: {stdout}");
    // An operation the vendored spec has but this document does not is
    // gone: a sync is a replacement, not a merge.
    assert!(!stdout.contains("collections.list"), "{stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_sync_of_the_same_document_changes_nothing() {
    let server = serve(document_with("things.brandNew")).await;
    let cache = TempDir::new().unwrap();
    let url = format!("{}{SPEC_PATH}", server.uri());
    run(cache.path(), &["spec", "sync", "--url", &url]).await;

    let (stdout, _, code) = run(cache.path(), &["spec", "sync", "--url", &url, "--json"]).await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(parse(&stdout)["changed"], Value::Bool(false));

    // --force rewrites it anyway.
    let (stdout, _, code) = run(
        cache.path(),
        &["spec", "sync", "--url", &url, "--json", "--force"],
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(parse(&stdout)["changed"], Value::Bool(true));
}

/// The story's second acceptance criterion: a damaged cache must not brick
/// any command. Every command still works, on the built-in spec, and says
/// so on stderr - stdout stays clean data.
#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_cache_falls_back_to_the_built_in_spec() {
    let server = serve(document_with("things.brandNew")).await;
    let cache = TempDir::new().unwrap();
    let url = format!("{}{SPEC_PATH}", server.uri());
    run(cache.path(), &["spec", "sync", "--url", &url]).await;

    let file = cache.path().join("ir-cache.bin");
    let mut raw = std::fs::read(&file).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 0xff;
    std::fs::write(&file, &raw).unwrap();

    let (stdout, stderr, code) = run(cache.path(), &["api", "list"]).await;
    assert_eq!(
        code, 0,
        "a damaged cache must not fail the command: {stderr}"
    );
    assert!(stderr.contains("damaged"), "{stderr}");
    assert!(stderr.contains("spec sync"), "no remedy offered: {stderr}");
    // Built-in table again: the synced operation is gone, the vendored
    // ones are back.
    assert!(!stdout.contains("things.brandNew"), "{stdout}");
    assert!(stdout.contains("documents.info"), "{stdout}");

    // Truncation is the other damage mode, and `--help` must not care at
    // all (it never resolves a table).
    std::fs::write(&file, &raw[..20]).unwrap();
    let (_, _, code) = run(cache.path(), &["api", "list"]).await;
    assert_eq!(code, 0);
    let (_, stderr, code) = run(cache.path(), &["--help"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "--help touched the cache: {stderr}");

    // An empty file, a foreign file, and a directory in place of the file:
    // none of them may panic (exit 101) or fail the command.
    std::fs::write(&file, b"").unwrap();
    assert_eq!(run(cache.path(), &["api", "list"]).await.2, 0);
    std::fs::write(&file, b"not a cache at all").unwrap();
    assert_eq!(run(cache.path(), &["api", "list"]).await.2, 0);
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    let (_, stderr, code) = run(cache.path(), &["api", "list"]).await;
    assert_eq!(code, 0, "{stderr}");
}

/// A sync over a damaged cache heals it rather than reporting it up to
/// date: the hash comparison must not trust an unusable file.
#[tokio::test(flavor = "multi_thread")]
async fn sync_heals_a_damaged_cache() {
    let server = serve(document_with("things.brandNew")).await;
    let cache = TempDir::new().unwrap();
    let url = format!("{}{SPEC_PATH}", server.uri());
    run(cache.path(), &["spec", "sync", "--url", &url]).await;

    let file = cache.path().join("ir-cache.bin");
    let raw = std::fs::read(&file).unwrap();
    std::fs::write(&file, &raw[..raw.len() / 2]).unwrap();

    let (stdout, _, code) = run(cache.path(), &["spec", "sync", "--url", &url, "--json"]).await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(parse(&stdout)["changed"], Value::Bool(true));
    let (stdout, stderr, _) = run(cache.path(), &["api", "list"]).await;
    assert!(stdout.contains("things.brandNew"), "{stdout}");
    assert!(stderr.is_empty(), "still warning after a heal: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_returns_to_the_built_in_spec() {
    let server = serve(document_with("things.brandNew")).await;
    let cache = TempDir::new().unwrap();
    let url = format!("{}{SPEC_PATH}", server.uri());
    run(cache.path(), &["spec", "sync", "--url", &url]).await;

    let (stdout, _, code) = run(cache.path(), &["spec", "reset", "--json"]).await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(parse(&stdout)["removed"], Value::Bool(true));

    let (stdout, stderr, code) = run(cache.path(), &["api", "list"]).await;
    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "reset should be silent afterwards: {stderr}"
    );
    assert!(!stdout.contains("things.brandNew"), "{stdout}");
    assert!(stdout.contains("collections.list"), "{stdout}");

    // Resetting again is not an error.
    let (stdout, _, code) = run(cache.path(), &["spec", "reset", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(parse(&stdout)["removed"], Value::Bool(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_document_can_be_compiled_without_any_network() {
    let cache = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("local.json");
    std::fs::write(&file, document_with("things.localOnly")).unwrap();

    let (stdout, _, code) = run(
        cache.path(),
        &[
            "spec",
            "sync",
            "--json",
            "--spec",
            &file.display().to_string(),
        ],
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    let report = parse(&stdout);
    assert_eq!(report["source"], Value::from("local file"));
    // The provenance record must not carry the file path.
    assert!(
        !stdout.contains(&dir.path().display().to_string()),
        "path leaked into the report: {stdout}"
    );

    let (stdout, _, _) = run(cache.path(), &["api", "list"]).await;
    assert!(stdout.contains("things.localOnly"), "{stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_that_is_not_a_spec_is_rejected_and_the_cache_kept() {
    let server = serve("{\"nope\": true}".to_string()).await;
    let cache = TempDir::new().unwrap();
    let good = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SPEC_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(document_with("things.brandNew")))
        .mount(&good)
        .await;
    run(
        cache.path(),
        &[
            "spec",
            "sync",
            "--url",
            &format!("{}{SPEC_PATH}", good.uri()),
        ],
    )
    .await;

    let (_, stderr, code) = run(
        cache.path(),
        &[
            "spec",
            "sync",
            "--url",
            &format!("{}{SPEC_PATH}", server.uri()),
        ],
    )
    .await;
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("cannot be used"), "{stderr}");

    // The previous, good cache survived the failed sync.
    let (stdout, _, _) = run(cache.path(), &["api", "list"]).await;
    assert!(stdout.contains("things.brandNew"), "{stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hostile_document_is_refused() {
    // A path that would turn `base_url + path` into a request to another
    // host: the bearer token must never follow it.
    let hostile = r#"{"paths":{"/@evil.example/x":{"post":{"summary":"nope"}}}}"#;
    let server = serve(hostile.to_string()).await;
    let cache = TempDir::new().unwrap();
    let (_, stderr, code) = run(
        cache.path(),
        &[
            "spec",
            "sync",
            "--url",
            &format!("{}{SPEC_PATH}", server.uri()),
        ],
    )
    .await;
    assert_eq!(code, 1, "{stderr}");
    assert!(!cache.path().join("ir-cache.bin").exists(), "cache written");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_local_file_is_a_usage_error() {
    let cache = TempDir::new().unwrap();
    let (_, stderr, code) = run(
        cache.path(),
        &["spec", "sync", "--spec", "/nonexistent/spec.json"],
    )
    .await;
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("cannot read"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_source_is_a_network_error() {
    let cache = TempDir::new().unwrap();
    let (_, stderr, code) = run(
        cache.path(),
        &["spec", "sync", "--url", "http://127.0.0.1:1/spec.json"],
    )
    .await;
    assert_eq!(code, 7, "{stderr}");
    assert!(!cache.path().join("ir-cache.bin").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_source_url_is_rejected_before_any_request() {
    let cache = TempDir::new().unwrap();
    for url in ["file:///etc/passwd", "not-a-url"] {
        let (_, stderr, code) = run(cache.path(), &["spec", "sync", "--url", url]).await;
        assert_eq!(code, 2, "{url}: {stderr}");
    }
}

#[test]
fn url_and_spec_cannot_be_combined() {
    let cache = TempDir::new().unwrap();
    otl(cache.path())
        .args([
            "spec",
            "sync",
            "--url",
            "https://example.com/s.json",
            "--spec",
            "s.json",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}
