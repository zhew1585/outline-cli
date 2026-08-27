//! `otl api describe <operation>` and operation-level `--help`, end to end.
//!
//! Three things are asserted here that a unit test cannot reach: that the
//! command is answered without configuration, that it sends nothing even
//! when there IS something to send, and that a table installed by `otl spec
//! sync` is the one described.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::isolate;

/// `otl` with every machine-dependent input shut off, including the cache:
/// these assertions are about the spec compiled into the binary.
fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    isolate(&mut cmd)
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY");
    cmd
}

/// Run `otl` and return (stdout, stderr, exit code).
fn run(cmd: &mut Command, args: &[&str]) -> (String, String, i32) {
    let output = cmd.args(args).output().unwrap();
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// Describe an operation off the built-in table and parse the JSON.
fn describe(operation: &str) -> Value {
    let (stdout, stderr, code) = run(&mut otl(), &["api", "describe", operation]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {stdout}"))
}

#[test]
fn describe_prints_every_request_facet_without_configuration() {
    let contract = describe("documents.info");
    assert_eq!(contract["operation"], "documents.info");
    assert_eq!(contract["path"], "/api/documents.info");
    assert_eq!(contract["content_type"], "application/json");
    assert_eq!(contract["body_mode"], "key_value");
    assert_eq!(contract["callable"], true);
    assert_eq!(contract["source"], "built-in");
    assert_eq!(contract["summary"], "Retrieve a document");

    let params = contract["parameters"].as_array().expect("parameters");
    let id = params
        .iter()
        .find(|param| param["name"] == "id")
        .expect("documents.info takes an `id`");
    // The whole point of the command: a caller that has never seen the
    // Outline documentation can now name the parameter and its type.
    assert_eq!(id["type"], "string");
    for facet in [
        "required",
        "nullable",
        "enum_values",
        "format",
        "minimum",
        "maximum",
    ] {
        assert!(id.get(facet).is_some(), "facet {facet} missing: {id}");
    }
}

#[test]
fn describe_prints_the_response_shape() {
    let contract = describe("documents.info");
    let fields = contract["response_fields"].as_array().expect("fields");
    assert!(!fields.is_empty(), "no response fields: {contract}");
    let id = fields
        .iter()
        .find(|field| field["name"] == "id")
        .expect("the document response has an `id`");
    assert_eq!(id["format"], "uuid");
    assert_eq!(id["read_only"], true);
}

/// The facets `--no-validate` skips are exactly the ones a caller needs in
/// order to send a valid value in the first place.
#[test]
fn describe_prints_the_enumerations_local_validation_enforces() {
    let contract = describe("documents.list");
    let params = contract["parameters"].as_array().expect("parameters");
    let direction = params
        .iter()
        .find(|param| param["name"] == "direction")
        .expect("documents.list takes a `direction`");
    let values: Vec<&str> = direction["enum_values"]
        .as_array()
        .expect("enum_values")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(values, vec!["ASC", "DESC"], "{direction}");
}

/// `--limit` is refused on operations that do not paginate, so a caller has
/// to be able to tell them apart before it tries.
#[test]
fn describe_says_whether_an_operation_paginates() {
    assert_eq!(describe("documents.list")["paginates"], true);
    assert_eq!(describe("documents.info")["paginates"], false);
}

#[test]
fn describe_flags_an_operation_the_generic_client_cannot_call() {
    let contract = describe("documents.import");
    assert_eq!(contract["callable"], false, "{contract}");
    assert_eq!(contract["body_mode"], "unsupported", "{contract}");
    assert_eq!(
        contract["content_type"], "multipart/form-data",
        "{contract}"
    );
}

/// Gap 3: this used to print the generic `otl api` help - authoritative
/// looking text about something else entirely.
#[test]
fn operation_level_help_describes_that_operation() {
    let (from_help, _, code) = run(&mut otl(), &["api", "documents.info", "--help"]);
    assert_eq!(code, 0);
    let (from_describe, _, _) = run(&mut otl(), &["api", "describe", "documents.info"]);
    assert_eq!(
        from_help, from_describe,
        "`--help` and `describe` must be the same answer"
    );
    let contract: Value = serde_json::from_str(&from_help).expect("json");
    assert_eq!(contract["operation"], "documents.info");
    assert!(
        !from_help.contains("Usage: otl api"),
        "the generic help came back: {from_help:.200}"
    );
}

#[test]
fn the_short_help_flag_describes_too() {
    let (stdout, _, code) = run(&mut otl(), &["api", "documents.info", "-h"]);
    assert_eq!(code, 0);
    let contract: Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(contract["operation"], "documents.info");
}

/// Taking the help flag over must not lose the command's own help.
#[test]
fn command_level_help_still_works_and_names_both_reserved_words() {
    let (stdout, stderr, code) = run(&mut otl(), &["api", "--help"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("Usage: otl api"), "{stdout:.400}");
    assert!(stdout.contains("describe"), "{stdout:.400}");
    assert!(stdout.contains("list"), "{stdout:.400}");
    // Rendered from the real command tree, so the global flags are there.
    for flag in ["--json", "--profile", "--url", "--config"] {
        assert!(stdout.contains(flag), "{flag} missing from the help");
    }
}

/// A reserved word names no operation, so there is nothing to describe.
#[test]
fn help_on_a_reserved_word_prints_the_command_help() {
    for word in ["list", "describe"] {
        let (stdout, _, code) = run(&mut otl(), &["api", word, "--help"]);
        assert_eq!(code, 0, "{word}");
        assert!(stdout.contains("Usage: otl api"), "{word}: {stdout:.200}");
    }
}

/// An unknown name must not be answered with plausible text about
/// something else - that is the failure mode this story exists for.
#[test]
fn help_on_an_unknown_operation_is_an_error_not_a_generic_help() {
    let (stdout, stderr, code) = run(&mut otl(), &["api", "documents.inf", "--help"]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stdout.is_empty(), "printed something anyway: {stdout:.200}");
    assert!(stderr.contains("unknown API operation"), "{stderr}");
    assert!(stderr.contains("resource.method"), "{stderr}");
    assert!(stderr.contains("otl api list"), "{stderr}");
}

#[test]
fn describe_reports_an_unknown_operation_the_same_way_the_call_path_does() {
    let (_, from_describe, code) = run(&mut otl(), &["api", "describe", "documents.inf"]);
    assert_eq!(code, 2);
    let (_, from_call, code) = run(&mut otl(), &["api", "documents.inf"]);
    assert_eq!(code, 2);
    assert_eq!(from_describe, from_call, "two different messages");
}

#[test]
fn describe_needs_exactly_one_operation() {
    otl()
        .args(["api", "describe"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("needs an operation"));
    otl()
        .args(["api", "describe", "documents.info", "documents.list"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::is_empty());
}

#[test]
fn describe_rejects_request_flags() {
    for flag in ["--no-validate", "--show-server-message"] {
        otl()
            .args(["api", "describe", "documents.info", flag])
            .assert()
            .failure()
            .code(2)
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains("sends no request"));
    }
}

/// NFR4, the sharp version: a credential and an instance ARE configured, so
/// there is something to send. The mock still has to receive nothing.
///
/// The control call at the end is what makes this assertion sensitive: it
/// proves the fixture would have recorded a request had one been made.
#[tokio::test(flavor = "multi_thread")]
async fn discovery_sends_nothing_even_when_it_could() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/auth.info"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":{}}"#))
        .mount(&server)
        .await;
    let uri = server.uri();

    let local = ["describe documents.info", "list", "documents.info --help"];
    for invocation in local {
        let uri = uri.clone();
        let args: Vec<String> = invocation.split(' ').map(str::to_string).collect();
        let code = tokio::task::spawn_blocking(move || {
            let mut cmd = Command::cargo_bin("otl").unwrap();
            isolate(&mut cmd);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            cmd.env("OUTLINE_URL", &uri)
                .env("OUTLINE_API_KEY", "not-a-real-key")
                .arg("api")
                .args(&refs)
                .output()
                .unwrap()
                .status
                .code()
                .unwrap_or(-1)
        })
        .await
        .unwrap();
        assert_eq!(code, 0, "`otl api {invocation}` failed");
    }
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a discovery command reached the network"
    );

    // Control: the same environment DOES send a request when asked to.
    let uri = uri.clone();
    tokio::task::spawn_blocking(move || {
        let mut cmd = Command::cargo_bin("otl").unwrap();
        isolate(&mut cmd);
        cmd.env("OUTLINE_URL", &uri)
            .env("OUTLINE_API_KEY", "not-a-real-key")
            .args(["api", "auth.info"])
            .output()
            .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "the fixture never records anything: the assertion above was vacuous"
    );
}

// ---------------------------------------------------------------------------
// A synced table: `otl spec sync --spec <file>` compiles a local document and
// makes it the effective one, with no network involved.
// ---------------------------------------------------------------------------

/// Install `document` as the effective table and return its cache directory.
fn synced(document: &str) -> TempDir {
    let cache = TempDir::new().unwrap();
    let file = cache.path().join("document.json");
    std::fs::write(&file, document).unwrap();
    let (stdout, stderr, code) = run(
        &mut synced_otl(cache.path()),
        &["spec", "sync", "--spec", file.to_str().unwrap()],
    );
    assert_eq!(code, 0, "sync failed: {stdout}{stderr}");
    cache
}

/// `otl` pointed at a specific cache directory, everything else isolated.
fn synced_otl(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    isolate(&mut cmd)
        .env("OTL_CACHE_DIR", cache)
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY");
    cmd
}

/// One operation, with a summary and one enumerated parameter.
fn document(operation: &str, summary: &str, enum_value: &str) -> String {
    format!(
        r#"{{"openapi":"3.0.0","paths":{{"/{operation}":{{"post":{{
          "summary":"{summary}",
          "requestBody":{{"content":{{"application/json":{{"schema":{{
            "type":"object","required":["mode"],
            "properties":{{"mode":{{"type":"string","enum":["ok","{enum_value}"]}}}}
          }}}}}}}}}}}}}}}}"#
    )
}

/// If `describe` answered from the built-in table while the call path
/// dispatched from the synced one, it would be handing out a contract for
/// an operation that is not the one about to be called.
#[test]
fn describe_answers_from_the_effective_table_not_the_built_in_one() {
    let cache = synced(&document("things.info", "Brand new endpoint", "also-ok"));
    let (stdout, stderr, code) = run(
        &mut synced_otl(cache.path()),
        &["api", "describe", "things.info"],
    );
    assert_eq!(code, 0, "{stderr}");
    let contract: Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(contract["source"], "synced", "{contract}");
    assert_eq!(contract["summary"], "Brand new endpoint");
    let mode = contract["parameters"][0].clone();
    assert_eq!(mode["name"], "mode");
    assert_eq!(mode["required"], true, "{mode}");

    // And an operation only the BUILT-IN table has is gone, exactly as it is
    // for the call path: a sync replaces the table, it does not merge.
    let (_, stderr, code) = run(
        &mut synced_otl(cache.path()),
        &["api", "describe", "documents.info"],
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("unknown API operation"), "{stderr}");
}

/// A document this CLI did not write may legally declare an operation named
/// `describe`. The reserved word wins - discovery must not disappear -
/// but it says so rather than shadowing silently.
#[test]
fn an_operation_named_like_a_reserved_word_is_reported_not_hidden() {
    let cache = synced(&document(
        "describe",
        "A resource called describe",
        "also-ok",
    ));
    let (stdout, stderr, code) = run(
        &mut synced_otl(cache.path()),
        &["api", "describe", "describe"],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("reserved") && stderr.contains("describe"),
        "no collision warning: {stderr:?}"
    );
    // The reserved command still ran, and it can even describe the
    // operation it is shadowing.
    let contract: Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(contract["operation"], "describe");

    // `list` is the other reserved word and shares the policy, so it is
    // asserted rather than assumed - a shared helper is exactly the kind of
    // thing that gets called from one branch and not the other.
    let listed = synced(&document("list", "A resource called list", "also-ok"));
    let (stdout, stderr, code) = run(&mut synced_otl(listed.path()), &["api", "list"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("reserved") && stderr.contains("list"),
        "no collision warning: {stderr:?}"
    );
    let rows: Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(rows[0]["name"], "list", "{rows}");

    // The ordinary case says nothing: no warning without a collision.
    let quiet = synced(&document("things.info", "Ordinary", "also-ok"));
    for args in [vec!["api", "describe", "things.info"], vec!["api", "list"]] {
        let (_, stderr, code) = run(&mut synced_otl(quiet.path()), &args);
        assert_eq!(code, 0);
        assert!(
            stderr.is_empty(),
            "{args:?}: unexpected warning: {stderr:?}"
        );
    }
}

/// Spec text is third-party text, and this output is aimed at a program
/// that will feed it to a language model. `U+200F` (RIGHT-TO-LEFT MARK)
/// passes the compiler's own check, so if the sink did not scrub it, it
/// would arrive verbatim in both states.
#[test]
fn third_party_text_reaches_stdout_scrubbed_in_both_paths() {
    let hostile = "\u{200f}reordered";
    let cache = synced(&document(
        "things.info",
        "safe \\u200f summary",
        "bad\\u200fvalue",
    ));
    for args in [
        vec!["api", "describe", "things.info"],
        vec!["api", "list"],
        vec!["api", "things.info", "--help"],
    ] {
        let (stdout, stderr, code) = run(&mut synced_otl(cache.path()), &args);
        assert_eq!(code, 0, "{args:?}: {stderr}");
        assert!(
            !stdout.contains('\u{200f}'),
            "{args:?} passed a bidi mark through: {stdout:?}"
        );
        assert!(!stdout.contains(hostile), "{args:?}: {stdout:?}");
    }
    // Control, and the whole reason this test means anything: the mark
    // really is in the compiled table on disk. Without this the assertions
    // above would pass just as well against a document that never carried
    // it - which is how a scrub test goes quiet.
    let compiled = std::fs::read(cache.path().join("ir-cache.bin")).unwrap();
    let mark = '\u{200f}'.to_string().into_bytes();
    assert!(
        compiled
            .windows(mark.len())
            .any(|window| window == mark.as_slice()),
        "the compiler dropped U+200F itself: this test proves nothing about the sink"
    );
}
