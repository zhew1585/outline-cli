//! End-to-end CLI tests for Story 1.6 (auto-pagination + `--limit`) and
//! Story 1.7 (429 backoff, rate-limit exit code), via assert_cmd against
//! wiremock.
//!
//! Diagnostics printed to stderr are compared against golden files; the
//! server origin and measured wait times are normalized away first, since
//! the mock port and timings vary per run.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{no_cache_dir, CACHE_DIR_ENV};

/// `otl` command with Outline env scrubbed for deterministic tests.
fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    cmd.env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        // Pagination descriptors come from the built-in spec.
        .env(CACHE_DIR_ENV, no_cache_dir());
    cmd
}

fn items(range: std::ops::Range<u64>) -> Vec<Value> {
    range.map(|n| json!({ "id": format!("doc-{n}") })).collect()
}

/// Replace the run-dependent parts of a diagnostic (mock origin, measured
/// wait) so stderr can be compared against a golden file.
fn normalize(stderr: &str, origin: &str) -> String {
    let text = stderr.replace(origin, "<ORIGIN>");
    // Rebuild left to right so the substituted marker is never rescanned.
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find("waiting ") {
        let after = &rest[start + "waiting ".len()..];
        let end = after.find('s').map_or(after.len(), |offset| offset + 1);
        out.push_str(&rest[..start]);
        out.push_str("waiting <WAIT>");
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Mount one `documents.list` page keyed on the request offset.
async fn mount_page(server: &MockServer, offset: u64, applied_limit: u64, data: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/api/documents.list"))
        .and(body_partial_json(json!({ "offset": offset })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": data,
            "pagination": { "offset": offset, "limit": applied_limit },
            "ok": true,
        })))
        .expect(1)
        .mount(server)
        .await;
}

/// Run `otl api ...` against `uri` on a blocking thread.
async fn run(uri: String, args: Vec<&'static str>) -> std::process::Output {
    tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_URL", uri)
            .env("OUTLINE_API_KEY", "test-key")
            .arg("api")
            .args(args)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn list_operation_fetches_all_pages() {
    let server = MockServer::start().await;
    mount_page(&server, 0, 2, items(0..2)).await;
    mount_page(&server, 2, 2, items(2..4)).await;
    mount_page(&server, 4, 2, items(4..5)).await;

    let output = run(server.uri(), vec!["documents.list"]).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let rows: Value = serde_json::from_str(&stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 5, "all pages must be merged: {stdout}");
    assert_eq!(rows[4]["id"], "doc-4");
    assert_eq!(stderr, "", "unexpected diagnostics: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_flag_caps_results_and_warns_on_stderr() {
    let server = MockServer::start().await;
    // --limit 2 asks for 3 rows (cap + 1); a full page proves more exist.
    mount_page(&server, 0, 3, items(0..3)).await;

    let uri = server.uri();
    let output = run(uri.clone(), vec!["--limit", "2", "documents.list"]).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let rows: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2, "cap not applied");
    // Golden file: the whole warning, byte for byte.
    assert_eq!(
        normalize(&stderr, &uri),
        include_str!("golden/warn_limit_truncated.txt")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_flag_without_truncation_stays_silent() {
    let server = MockServer::start().await;
    // Exactly 2 rows exist; the probe (limit 3) comes back short.
    mount_page(&server, 0, 3, items(0..2)).await;

    let output = run(server.uri(), vec!["--limit", "2", "documents.list"]).await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stderr, "", "false warning: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_pagination_hint_does_not_stop_early() {
    // Reviewer PoC (finding 2): the server omits its pagination echo and
    // returns fewer rows than requested. Paging must continue.
    let server = MockServer::start().await;
    for (offset, rows) in [(0, items(0..2)), (2, items(2..3)), (3, vec![])] {
        Mock::given(method("POST"))
            .and(path("/api/documents.list"))
            .and(body_partial_json(json!({ "offset": offset })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": rows,
                // Offset echoed (Outline always does), page size omitted.
                "pagination": { "offset": offset },
            })))
            .expect(1)
            .mount(&server)
            .await;
    }

    let output = run(server.uri(), vec!["documents.list"]).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let rows: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        rows.as_array().unwrap().len(),
        3,
        "rows silently dropped: {stdout}"
    );
    // The offset IS echoed here (only the page size is missing), so the
    // result is fully confirmed and there is nothing to report.
    assert_eq!(stderr, "", "unexpected diagnostics: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_page_two_fails_instead_of_reporting_success() {
    // Reviewer PoC (finding 3).
    let server = MockServer::start().await;
    mount_page(&server, 0, 2, items(0..2)).await;
    Mock::given(method("POST"))
        .and(path("/api/documents.list"))
        .and(body_partial_json(json!({ "offset": 2 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": "not-an-array" })))
        .expect(1)
        .mount(&server)
        .await;

    let output = run(server.uri(), vec!["documents.list"]).await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("pagination failed"), "stderr: {stderr}");
    assert_eq!(stdout, "", "partial data printed as success: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_limit_arg_fetches_one_page_and_warns() {
    // Reviewer PoC (finding 4): a raw limit= must not silently return one
    // page as if it were everything.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.list"))
        .and(body_partial_json(json!({ "limit": 2 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": items(0..2),
            "pagination": { "offset": 0, "limit": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = run(uri.clone(), vec!["documents.list", "limit=2"]).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // Golden file: the whole warning, byte for byte.
    assert_eq!(
        normalize(&stderr, &uri),
        include_str!("golden/warn_manual_page.txt")
    );
}

#[test]
fn limit_flag_with_raw_limit_arg_is_a_usage_error() {
    // Finding 4: the two mean different things; refuse rather than guess.
    otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "--limit", "2", "documents.list", "limit=25"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be combined"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn limit_flag_on_a_non_list_operation_is_a_usage_error() {
    // A cap that cannot apply must be refused, not silently ignored.
    otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "--limit", "2", "documents.info", "id=doc-1"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("list operations"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn invalid_offset_arg_is_a_usage_error_without_network() {
    // Finding 5: a bad offset must never silently become 0. Port 9 is
    // closed, so any request would surface as a transport error instead.
    for bad in ["offset=abc", "offset=-1", "offset=1.5"] {
        otl()
            .env("OUTLINE_URL", "http://127.0.0.1:9")
            .env("OUTLINE_API_KEY", "test-key")
            .args(["api", "documents.list", bad])
            .assert()
            .failure()
            .code(2)
            .stderr(predicate::str::contains("offset"))
            .stdout(predicate::str::is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limited_request_retries_and_succeeds_with_stderr_notice() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "doc-1", "title": "Recovered" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = run(uri.clone(), vec!["documents.info", "id=doc-1"]).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    // Data stays on stdout; the wait notice goes to stderr only.
    assert!(stdout.contains("Recovered"), "stdout: {stdout}");
    assert!(!stdout.contains("429"), "notice leaked to stdout: {stdout}");
    // Golden file: the whole retry notice, byte for byte.
    assert_eq!(
        normalize(&stderr, &uri),
        include_str!("golden/notice_rate_limited_retry.txt")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exhausted_retries_exit_with_the_rate_limit_code() {
    // Finding 6: rate-limit exhaustion has its own documented exit code.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .mount(&server)
        .await;

    let uri = server.uri();
    let output = run(uri.clone(), vec!["documents.info", "id=doc-1"]).await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(8), "stderr: {stderr}");
    assert!(stderr.contains("rate limited"), "stderr: {stderr}");
    assert!(stderr.contains("429"), "stderr: {stderr}");
    assert_eq!(stdout, "", "stdout must stay data-only: {stdout}");
}

#[test]
fn limit_flag_rejects_zero() {
    otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "--limit", "0", "documents.list"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--limit"));
}

// --- Re-review findings ---------------------------------------------------

#[test]
fn duplicate_argument_keys_are_a_usage_error() {
    // Re-review finding 2 PoC B: only one value can reach the request
    // body, so a repeated key must be refused rather than silently
    // resolved. Port 9 is closed: any request would fail differently.
    otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "documents.list", "limit=100", "limit=1"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("more than once"))
        .stdout(predicate::str::is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn server_clamped_manual_page_warns() {
    // Re-review finding 2 PoC A: limit=1000, Outline applies 25 and says
    // so, and more rows exist. Staying silent would be silent truncation.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.list"))
        .and(body_partial_json(json!({ "limit": 1000 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": items(0..25),
            "pagination": { "offset": 0, "limit": 25 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = run(server.uri(), vec!["documents.list", "limit=1000"]).await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stderr.contains("may be truncated"),
        "clamped page not reported: {stderr:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn contradicting_offset_echo_fails_instead_of_merging_wrong_rows() {
    // The server ignores the requested offset and says so by echoing 0
    // again: page 2 would duplicate row 0 into the result.
    let server = MockServer::start().await;
    for offset in [0, 2] {
        Mock::given(method("POST"))
            .and(path("/api/documents.list"))
            .and(body_partial_json(json!({ "offset": offset })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": items(0..2),
                "pagination": { "offset": 0, "limit": 2 },
            })))
            .mount(&server)
            .await;
    }

    let output = run(server.uri(), vec!["documents.list"]).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("pagination failed"), "stderr: {stderr}");
    assert_eq!(stdout, "", "wrong rows printed as success: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unusable_offset_echo_still_fails() {
    // Present but not a number: nothing to compare, so it cannot be waved
    // through as merely absent.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": items(0..2),
            "pagination": { "offset": "0", "limit": 2 },
        })))
        .mount(&server)
        .await;

    let output = run(server.uri(), vec!["documents.list"]).await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("pagination failed"), "stderr: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "unverifiable rows printed as success"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn absent_offset_echo_succeeds_with_a_notice_and_correct_rows() {
    // An endpoint that sends no pagination envelope at all (spec drift)
    // must still be usable: correct merged rows, exit 0, and one notice.
    let server = MockServer::start().await;
    for (offset, rows) in [(0, items(0..2)), (2, items(2..3)), (3, vec![])] {
        Mock::given(method("POST"))
            .and(path("/api/documents.list"))
            .and(body_partial_json(json!({ "offset": offset })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": rows })))
            .expect(1)
            .mount(&server)
            .await;
    }

    let output = run(server.uri(), vec!["--json", "documents.list"]).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let rows: Value = serde_json::from_str(&stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3, "rows lost or duplicated: {stdout}");
    assert_eq!(rows[0]["id"], "doc-0");
    assert_eq!(rows[2]["id"], "doc-2");
    // Never silent, and said once - not once per page.
    assert_eq!(
        stderr.matches("did not echo the pagination offset").count(),
        1,
        "notice missing or repeated: {stderr:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn offset_space_exhausted_is_reported_as_possible_truncation() {
    // Re-review finding 5: the data may have ended exactly there.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": items(0..1),
            "pagination": { "offset": u64::MAX, "limit": 1 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let offset_arg: &'static str = Box::leak(format!("offset={}", u64::MAX).into_boxed_str());
    let output = run(server.uri(), vec!["documents.list", offset_arg]).await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stderr.contains("may be truncated"),
        "must not claim definite truncation: {stderr:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_page_with_unconfirmed_offset_fails() {
    // Round-3 PoC: `documents.list offset=5 limit=1` against a server that
    // ignores the offset and echoes a non-numeric one must not return the
    // wrong row as a success.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": items(0..1),
            "pagination": { "offset": "0", "limit": 1 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = run(server.uri(), vec!["documents.list", "offset=5", "limit=1"]).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("pagination failed"), "stderr: {stderr}");
    assert_eq!(stdout, "", "wrong row printed as success: {stdout}");
}
