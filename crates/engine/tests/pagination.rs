//! Story 1.6: caller-described auto-pagination in the request channel.
//!
//! These tests deliberately use a NON-Outline wire vocabulary
//! (`from`/`count`, rows under `/result/rows`, hints under `/meta/*`) to
//! prove the engine carries no vendor convention of its own.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;

use engine::{
    BodyMode, Client, EngineError, OffsetEcho, OpSpec, PaginationSpec, ParamSpec, ParamType,
    TruncationCause, ValidationMode,
};
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OFFSET_PARAM: &str = "from";
const LIMIT_PARAM: &str = "count";

/// A parameter spec with no schema facets (facets are story 1.2/1.3's
/// concern; these tests are about paging).
const fn param(name: &'static str, ty: ParamType) -> ParamSpec {
    ParamSpec {
        name: Cow::Borrowed(name),
        ty,
        required: false,
        nullable: false,
        enum_values: Cow::Borrowed(&[]),
        format: Cow::Borrowed(""),
        minimum: None,
        maximum: None,
    }
}

const PARAMS: &[ParamSpec] = &[
    param(OFFSET_PARAM, ParamType::Integer),
    param(LIMIT_PARAM, ParamType::Integer),
    param("query", ParamType::String),
];

fn op() -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("things.list"),
        path: Cow::Borrowed("/rpc/things.list"),
        summary: Cow::Borrowed("List things"),
        content_type: Cow::Borrowed("application/json"),
        body_mode: BodyMode::KeyValue,
        params: Cow::Borrowed(PARAMS),
    }
}

/// The test API's convention: rows nested two levels deep, paging hints in
/// a separate metadata object.
fn spec() -> PaginationSpec {
    PaginationSpec {
        offset_param: Cow::Borrowed(OFFSET_PARAM),
        limit_param: Cow::Borrowed(LIMIT_PARAM),
        items_pointer: Cow::Borrowed("/result/rows"),
        page_size_pointer: Some(Cow::Borrowed("/meta/page_size")),
        offset_echo: OffsetEcho::Required {
            pointer: Cow::Borrowed("/meta/from"),
        },
        stale_metadata_pointer: Some(Cow::Borrowed("/meta")),
        page_size: 100,
        max_pages: 100,
    }
}

fn items(range: std::ops::Range<u64>) -> Vec<Value> {
    range.map(|n| json!({ "id": format!("row-{n}") })).collect()
}

/// One well-behaved page: rows plus honest `page_size`/`from` hints.
async fn mount_page(server: &MockServer, offset: u64, page_size: u64, data: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .and(body_partial_json(json!({ OFFSET_PARAM: offset })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": data },
            "meta": { "from": offset, "page_size": page_size },
            "ok": true,
        })))
        .expect(1)
        .mount(server)
        .await;
}

/// A page that confirms the offset but omits the page-size hint.
async fn mount_hintless_page(server: &MockServer, offset: u64, data: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .and(body_partial_json(json!({ OFFSET_PARAM: offset })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": data },
            "meta": { "from": offset },
        })))
        .expect(1)
        .mount(server)
        .await;
}

/// Run one paged execution against `uri` on a blocking thread.
async fn run_paged(
    uri: String,
    args: Vec<(String, String)>,
    max_items: Option<u64>,
) -> Result<engine::Fetched, EngineError> {
    tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute_paged(&op(), &args, ValidationMode::Strict, &spec(), max_items)
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn fetches_all_pages_and_merges_rows() {
    let server = MockServer::start().await;
    mount_page(&server, 0, 3, items(0..3)).await;
    mount_page(&server, 3, 3, items(3..6)).await;
    mount_page(&server, 6, 3, items(6..7)).await;

    let fetched = run_paged(
        server.uri(),
        vec![("query".to_string(), "x".to_string())],
        None,
    )
    .await
    .unwrap();

    let rows = fetched.value["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 7);
    assert_eq!(rows[0]["id"], "row-0");
    assert_eq!(rows[6]["id"], "row-6");
    assert_eq!(fetched.value["ok"], true, "envelope fields must survive");
    assert!(fetched.truncation.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn merged_envelope_drops_stale_page_metadata() {
    // Finding 9: page 1's paging echo must not describe the merged result.
    let server = MockServer::start().await;
    mount_page(&server, 0, 2, items(0..2)).await;
    mount_page(&server, 2, 2, items(2..3)).await;

    let fetched = run_paged(server.uri(), vec![], None).await.unwrap();

    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 3);
    assert!(
        fetched.value.get("meta").is_none(),
        "stale page metadata retained: {}",
        fetched.value
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_page_size_hint_keeps_paging_until_empty_page() {
    // Finding 2: without a hint, a short page proves nothing - the server
    // may have clamped it. Paging must continue until an empty page.
    let server = MockServer::start().await;
    mount_hintless_page(&server, 0, items(0..2)).await;
    mount_hintless_page(&server, 2, items(2..3)).await;
    mount_hintless_page(&server, 3, vec![]).await;

    let fetched = run_paged(server.uri(), vec![], None).await.unwrap();

    assert_eq!(
        fetched.value["result"]["rows"].as_array().unwrap().len(),
        3,
        "rows silently dropped: {}",
        fetched.value
    );
    assert!(fetched.truncation.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_page_size_hint_keeps_paging() {
    // A non-numeric or zero hint is unusable, not a licence to stop.
    for hint in [json!("2"), json!(0), json!(-1), json!(null)] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ OFFSET_PARAM: 0 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": { "rows": items(0..2) },
                "meta": { "from": 0, "page_size": hint },
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ OFFSET_PARAM: 2 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": { "rows": items(2..3) },
                "meta": { "from": 2 },
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ OFFSET_PARAM: 3 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": { "rows": [] },
                "meta": { "from": 3 },
            })))
            .expect(1)
            .mount(&server)
            .await;

        let fetched = run_paged(server.uri(), vec![], None).await.unwrap();
        assert_eq!(
            fetched.value["result"]["rows"].as_array().unwrap().len(),
            3,
            "hint {hint} caused a silent short read"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn server_returning_more_rows_than_requested_still_advances_correctly() {
    let server = MockServer::start().await;
    // Asked for 100, hint says 2, but 4 rows arrive: advance by received.
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ OFFSET_PARAM: 0 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..4) },
            "meta": { "from": 0, "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;
    mount_page(&server, 4, 2, items(4..5)).await;

    let fetched = run_paged(server.uri(), vec![], None).await.unwrap();
    let rows = fetched.value["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[4]["id"], "row-4");
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicting_server_offset_hint_is_an_error() {
    // Finding 2: a server that ignores the requested offset would make
    // "advance by received" skip or duplicate rows.
    let server = MockServer::start().await;
    mount_page(&server, 0, 2, items(0..2)).await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ OFFSET_PARAM: 2 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "from": 0, "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_paged(server.uri(), vec![], None).await.unwrap_err();
    match error {
        EngineError::Pagination { reason } => {
            assert!(reason.contains('0') && reason.contains('2'), "{reason}");
        }
        other => panic!("expected Pagination error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_envelope_mid_stream_is_an_error_not_a_clean_success() {
    // Finding 3 (reviewer PoC): page 2 answers with a non-array.
    let server = MockServer::start().await;
    mount_page(&server, 0, 2, items(0..2)).await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ OFFSET_PARAM: 2 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": "not-an-array" },
            "meta": { "from": 2, "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_paged(server.uri(), vec![], None).await.unwrap_err();
    match error {
        EngineError::Pagination { reason } => {
            assert!(reason.contains("/result/rows"), "{reason}");
        }
        other => panic!("expected Pagination error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn max_items_caps_fetch_and_reports_definite_truncation() {
    let server = MockServer::start().await;
    // Cap 2 asks for 3 rows; receiving 3 proves more rows exist.
    mount_page(&server, 0, 3, items(0..3)).await;

    let fetched = run_paged(server.uri(), vec![], Some(2)).await.unwrap();

    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 2);
    let truncation = fetched.truncation.expect("must report truncation");
    assert_eq!(truncation.fetched, 2);
    assert_eq!(truncation.cause, TruncationCause::MaxItems);
    assert!(truncation.cause.is_definite());
}

#[tokio::test(flavor = "multi_thread")]
async fn max_items_of_one_is_respected() {
    let server = MockServer::start().await;
    mount_page(&server, 0, 2, items(0..2)).await;

    let fetched = run_paged(server.uri(), vec![], Some(1)).await.unwrap();

    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 1);
    assert_eq!(fetched.truncation.unwrap().cause, TruncationCause::MaxItems);
}

#[tokio::test(flavor = "multi_thread")]
async fn max_items_equal_to_result_size_is_not_truncation() {
    let server = MockServer::start().await;
    // 2 rows exist; the cap-probe request (count 3) comes back short.
    mount_page(&server, 0, 3, items(0..2)).await;

    let fetched = run_paged(server.uri(), vec![], Some(2)).await.unwrap();

    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 2);
    assert!(fetched.truncation.is_none(), "false truncation warning");
}

#[tokio::test(flavor = "multi_thread")]
async fn max_items_larger_than_result_is_not_truncation() {
    let server = MockServer::start().await;
    mount_page(&server, 0, 2, items(0..2)).await;
    mount_page(&server, 2, 2, items(2..3)).await;

    let fetched = run_paged(server.uri(), vec![], Some(1_000)).await.unwrap();

    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 3);
    assert!(fetched.truncation.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn page_limit_reports_possible_not_definite_truncation() {
    // Finding 8: the data may have ended exactly at the cap.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..1) },
            "meta": { "page_size": 1 },
        })))
        .mount(&server)
        .await;

    let capped = PaginationSpec {
        max_pages: 2,
        // This fixture answers every offset with the same body, so it
        // cannot echo the requested offset; the page cap is the subject.
        offset_echo: OffsetEcho::Ignored,
        ..spec()
    };
    let uri = server.uri();
    let fetched = tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute_paged(&op(), &[], ValidationMode::Strict, &capped, None)
    })
    .await
    .unwrap()
    .unwrap();

    let truncation = fetched.truncation.expect("page cap must be reported");
    assert_eq!(truncation.cause, TruncationCause::PageLimit);
    assert_eq!(truncation.fetched, 2);
    assert!(
        !truncation.cause.is_definite(),
        "page cap cannot prove truncation"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_limit_arg_fetches_one_full_page_and_warns() {
    // Finding 4: a caller-pinned page size means manual paging, but a full
    // page must still be reported as possibly truncated.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ LIMIT_PARAM: 2 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "from": 0, "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let fetched = run_paged(
        server.uri(),
        vec![(LIMIT_PARAM.to_string(), "2".to_string())],
        None,
    )
    .await
    .unwrap();

    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 2);
    let truncation = fetched.truncation.expect("full manual page must warn");
    assert_eq!(truncation.cause, TruncationCause::ManualPage);
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_limit_arg_with_short_page_does_not_warn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ LIMIT_PARAM: 5 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "from": 0, "page_size": 5 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let fetched = run_paged(
        server.uri(),
        vec![(LIMIT_PARAM.to_string(), "5".to_string())],
        None,
    )
    .await
    .unwrap();

    assert!(fetched.truncation.is_none(), "false warning");
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_offset_arg_starts_pagination_there() {
    let server = MockServer::start().await;
    mount_page(&server, 5, 3, items(5..8)).await;
    mount_page(&server, 8, 3, items(8..9)).await;

    let fetched = run_paged(
        server.uri(),
        vec![(OFFSET_PARAM.to_string(), "5".to_string())],
        None,
    )
    .await
    .unwrap();

    let rows = fetched.value["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["id"], "row-5");
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_offset_arg_is_rejected_before_any_request() {
    // Finding 5: a bad offset must fail fast, not silently become 0.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": [] },
        })))
        .expect(0)
        .mount(&server)
        .await;

    for bad in ["abc", "-1", "1.5", ""] {
        let error = run_paged(
            server.uri(),
            vec![(OFFSET_PARAM.to_string(), bad.to_string())],
            None,
        )
        .await
        .unwrap_err();
        match error {
            EngineError::InvalidParamValue { name, reason } => {
                assert_eq!(name, OFFSET_PARAM);
                assert!(
                    !reason.contains(bad) || bad.is_empty(),
                    "echoed input: {reason}"
                );
            }
            other => panic!("expected InvalidParamValue for {bad:?}, got {other:?}"),
        }
    }
}

/// The same operation with a string-typed offset parameter. An IR that
/// declares the offset as an integer caps it at `i64::MAX` during
/// coercion, so this is how the u64 offset arithmetic gets exercised.
const STRING_OFFSET_PARAMS: &[ParamSpec] = &[
    param(OFFSET_PARAM, ParamType::String),
    param(LIMIT_PARAM, ParamType::Integer),
];

fn string_offset_op() -> OpSpec {
    OpSpec {
        params: Cow::Borrowed(STRING_OFFSET_PARAMS),
        ..op()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn offset_at_u64_max_cannot_overflow() {
    // Finding 5: advancing past u64::MAX must not panic or wrap to 0.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..1) },
            "meta": { "from": u64::MAX, "page_size": 1 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let fetched = tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute_paged(
            &string_offset_op(),
            &[(OFFSET_PARAM.to_string(), u64::MAX.to_string())],
            ValidationMode::Strict,
            &spec(),
            None,
        )
    })
    .await
    .unwrap()
    .unwrap();

    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 1);
    let truncation = fetched.truncation.unwrap();
    assert_eq!(truncation.cause, TruncationCause::OffsetSpaceExhausted);
    // Re-review finding 5: failing to build the next offset does not prove
    // the server had more rows.
    assert!(
        !truncation.cause.is_definite(),
        "offset exhaustion cannot prove truncation"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_list_first_response_is_an_error_not_a_silent_success() {
    // Re-review finding 3: the caller chose this descriptor, so a first
    // page with no array at the items pointer is a protocol mismatch - not
    // a complete result that happens to have an unknown shape.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .expect(1)
        .mount(&server)
        .await;

    // No offset hint configured, so the items pointer is the only check.
    let items_only = PaginationSpec {
        offset_echo: OffsetEcho::Ignored,
        ..spec()
    };
    let uri = server.uri();
    let error = tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute_paged(&op(), &[], ValidationMode::Strict, &items_only, None)
    })
    .await
    .unwrap()
    .unwrap_err();
    match error {
        EngineError::Pagination { reason } => assert!(reason.contains("/result/rows"), "{reason}"),
        other => panic!("expected Pagination error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_without_a_descriptor_makes_exactly_one_request() {
    // Auto-pagination is opt-in: plain execute never pages, even for an
    // operation that happens to declare paging parameters.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..3) },
            "meta": { "from": 0, "page_size": 3 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let value = tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute(&op(), &[], ValidationMode::Strict)
    })
    .await
    .unwrap()
    .unwrap();

    assert_eq!(value["result"]["rows"].as_array().unwrap().len(), 3);
}

#[test]
fn errors_propagate_from_any_page() {
    let client = Client::new("http://127.0.0.1:9", "token").unwrap();
    let error = client
        .execute_paged(&op(), &[], ValidationMode::Strict, &spec(), None)
        .unwrap_err();
    assert!(matches!(error, EngineError::Transport { .. }), "{error:?}");
}

// --- Re-review findings ---------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn contradicting_offset_echo_is_always_an_error() {
    // Re-review finding 1 (PoC): the server ignores the requested offset
    // and echoes a non-numeric hint, so "advance by received" cannot be
    // trusted. Merging [0,1] with [0] would silently duplicate a row.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ OFFSET_PARAM: 0 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "from": "0", "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_paged(server.uri(), vec![], None).await.unwrap_err();
    match error {
        EngineError::Pagination { reason } => {
            assert!(reason.contains("/meta/from"), "{reason}");
            assert!(reason.contains("cannot be confirmed"), "{reason}");
        }
        other => panic!("expected Pagination error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn absent_offset_echo_is_an_error_only_when_required() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_paged(server.uri(), vec![], None).await.unwrap_err();
    assert!(matches!(error, EngineError::Pagination { .. }), "{error:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ignored_offset_echo_means_the_engine_trusts_its_counter() {
    // None keeps its meaning: the API reports no offset, so no check.
    let server = MockServer::start().await;
    for (offset, rows) in [(0, items(0..2)), (2, items(2..3))] {
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ OFFSET_PARAM: offset })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": { "rows": rows },
                "meta": { "page_size": 2 },
            })))
            .expect(1)
            .mount(&server)
            .await;
    }

    let hintless = PaginationSpec {
        offset_echo: OffsetEcho::Ignored,
        ..spec()
    };
    let uri = server.uri();
    let fetched = tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute_paged(&op(), &[], ValidationMode::Strict, &hintless, None)
    })
    .await
    .unwrap()
    .unwrap();

    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_page_warns_when_the_server_clamps_below_the_request() {
    // Re-review finding 2 PoC A: limit=1000, server applies 25 and says so.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ LIMIT_PARAM: 1000 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..25) },
            "meta": { "from": 0, "page_size": 25 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let fetched = run_paged(
        server.uri(),
        vec![(LIMIT_PARAM.to_string(), "1000".to_string())],
        None,
    )
    .await
    .unwrap();

    let truncation = fetched
        .truncation
        .expect("a clamped full page must be reported");
    assert_eq!(truncation.cause, TruncationCause::ManualPage);
    assert_eq!(truncation.fetched, 25);
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_page_warns_when_no_applied_size_hint_is_available() {
    // Re-review finding 2 PoC B / part (c): with no trustworthy applied
    // size, silence would be a silent truncation - warn conservatively.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ LIMIT_PARAM: 100 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..1) },
            "meta": { "from": 0 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let fetched = run_paged(
        server.uri(),
        vec![(LIMIT_PARAM.to_string(), "100".to_string())],
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        fetched.truncation.map(|t| t.cause),
        Some(TruncationCause::ManualPage),
        "no hint must warn conservatively"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_limit_arguments_are_rejected_before_any_request() {
    // The body builder rejects a repeated parameter outright, so the
    // engine can never send one value while judging fullness by another.
    // (paginate's own lookups take the last occurrence as defense in depth.)
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let error = run_paged(
        server.uri(),
        vec![
            (LIMIT_PARAM.to_string(), "100".to_string()),
            (LIMIT_PARAM.to_string(), "1".to_string()),
        ],
        None,
    )
    .await
    .unwrap_err();

    match error {
        EngineError::InvalidParamValue { name, reason } => {
            assert_eq!(name, LIMIT_PARAM);
            assert!(reason.contains("more than once"), "{reason}");
        }
        other => panic!("expected InvalidParamValue, got {other:?}"),
    }
}

/// Probe descriptor validation without any server.
fn validate(spec: &PaginationSpec) -> Result<(), EngineError> {
    let client = Client::new("http://127.0.0.1:9", "token").unwrap();
    match client.execute_paged(&op(), &[], ValidationMode::Strict, spec, None) {
        // A valid descriptor gets as far as the (closed) network.
        Err(EngineError::Transport { .. }) => Ok(()),
        Err(other) => Err(other),
        Ok(_) => panic!("unexpected success against a closed port"),
    }
}

#[test]
fn invalid_json_pointers_are_rejected_before_any_request() {
    // Re-review finding 3: strict RFC 6901, checked locally.
    let cases = [
        "result/rows",    // no leading slash
        "/result/~2rows", // invalid escape
        "/result/rows~",  // dangling escape
        "~0",             // no leading slash
    ];
    for bad in cases {
        let spec = PaginationSpec {
            items_pointer: Cow::Owned(bad.to_string()),
            ..spec()
        };
        match validate(&spec) {
            Err(EngineError::InvalidPaginationSpec { reason }) => {
                assert!(reason.contains("items_pointer"), "{reason}");
            }
            other => panic!("expected InvalidPaginationSpec for {bad:?}, got {other:?}"),
        }
    }
}

#[test]
fn invalid_hint_pointers_are_rejected_too() {
    for spec in [
        PaginationSpec {
            page_size_pointer: Some(Cow::Borrowed("meta/page_size")),
            ..spec()
        },
        PaginationSpec {
            offset_echo: OffsetEcho::Required {
                pointer: Cow::Borrowed("/meta/~9from"),
            },
            ..spec()
        },
        PaginationSpec {
            stale_metadata_pointer: Some(Cow::Borrowed("meta")),
            ..spec()
        },
    ] {
        match validate(&spec) {
            Err(EngineError::InvalidPaginationSpec { .. }) => {}
            other => panic!("expected InvalidPaginationSpec, got {other:?}"),
        }
    }
}

#[test]
fn valid_pointers_including_escapes_and_root_are_accepted() {
    for pointer in ["", "/result/rows", "/a~1b/~0rows"] {
        let spec = PaginationSpec {
            items_pointer: Cow::Owned(pointer.to_string()),
            // Keep the stale pointer clear of the items pointer.
            stale_metadata_pointer: None,
            ..spec()
        };
        assert!(
            validate(&spec).is_ok(),
            "valid pointer {pointer:?} was rejected"
        );
    }
}

#[test]
fn stale_metadata_pointer_overlapping_the_items_pointer_is_rejected() {
    // Re-review finding 4: metadata removal must never be able to delete
    // the merged rows. Equal, ancestor and descendant all overlap.
    for stale in ["/result/rows", "/result", "", "/result/rows/0"] {
        let spec = PaginationSpec {
            stale_metadata_pointer: Some(Cow::Owned(stale.to_string())),
            ..spec()
        };
        match validate(&spec) {
            Err(EngineError::InvalidPaginationSpec { reason }) => {
                assert!(reason.contains("overlaps"), "{reason}");
            }
            other => panic!("expected InvalidPaginationSpec for stale {stale:?}, got {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_metadata_removal_follows_rfc_6901_array_indexes() {
    // Re-review finding 4: array indexes must not be parsed leniently -
    // `00` is not a valid RFC 6901 index and must remove nothing.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..1) },
            "meta": { "from": 0, "page_size": 2 },
            "notes": ["keep-me", "drop-me"],
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let padded = PaginationSpec {
        stale_metadata_pointer: Some(Cow::Borrowed("/notes/00")),
        ..spec()
    };
    let fetched = tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute_paged(&op(), &[], ValidationMode::Strict, &padded, None)
    })
    .await
    .unwrap()
    .unwrap();

    assert_eq!(
        fetched.value["notes"],
        json!(["keep-me", "drop-me"]),
        "a non-RFC-6901 index must not remove an element"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_metadata_removal_accepts_a_real_array_index() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..1) },
            "meta": { "from": 0, "page_size": 2 },
            "notes": ["drop-me", "keep-me"],
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let indexed = PaginationSpec {
        stale_metadata_pointer: Some(Cow::Borrowed("/notes/0")),
        ..spec()
    };
    let fetched = tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute_paged(&op(), &[], ValidationMode::Strict, &indexed, None)
    })
    .await
    .unwrap()
    .unwrap();

    assert_eq!(fetched.value["notes"], json!(["keep-me"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_page_branch_also_enforces_the_offset_hint() {
    // Round-3 finding (PoC): the manual single-page branch must not be a
    // hole in the offset invariant. Requested offset 5, server echoes a
    // non-numeric offset, so the row it returned cannot be trusted.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..1) },
            "meta": { "from": "0", "page_size": 1 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_paged(
        server.uri(),
        vec![
            (OFFSET_PARAM.to_string(), "5".to_string()),
            (LIMIT_PARAM.to_string(), "1".to_string()),
        ],
        None,
    )
    .await
    .unwrap_err();

    match error {
        EngineError::Pagination { reason } => {
            assert!(reason.contains("cannot be confirmed"), "{reason}");
            assert!(reason.contains('5'), "requested offset missing: {reason}");
        }
        other => panic!("expected Pagination error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_page_branch_also_enforces_the_items_pointer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": "not-an-array" },
            "meta": { "from": 0, "page_size": 1 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_paged(
        server.uri(),
        vec![(LIMIT_PARAM.to_string(), "1".to_string())],
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, EngineError::Pagination { .. }), "{error:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_page_keeps_its_rows_and_its_own_paging_echo() {
    // Taking rows out for the check must not lose them, and a single
    // page's paging echo is accurate for that page (unlike a merge).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "from": 0, "page_size": 5 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let fetched = run_paged(
        server.uri(),
        vec![(LIMIT_PARAM.to_string(), "5".to_string())],
        None,
    )
    .await
    .unwrap();

    let rows = fetched.value["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "rows lost by the acceptance check");
    assert_eq!(rows[1]["id"], "row-1");
    assert_eq!(fetched.value["meta"]["page_size"], 5);
    assert!(fetched.truncation.is_none());
}

// --- OffsetEcho: the three descriptor states -----------------------------

/// The descriptor with a chosen offset-echo mode.
fn spec_with(echo: OffsetEcho) -> PaginationSpec {
    PaginationSpec {
        offset_echo: echo,
        ..spec()
    }
}

/// Fetch with an explicit offset-echo mode.
async fn run_with_echo(uri: String, echo: OffsetEcho) -> Result<engine::Fetched, EngineError> {
    tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute_paged(&op(), &[], ValidationMode::Strict, &spec_with(echo), None)
    })
    .await
    .unwrap()
}

/// A two-page collection whose pages omit the offset echo entirely.
async fn mount_echoless_pages(server: &MockServer) {
    for (offset, rows, size) in [(0, items(0..2), 2), (2, items(2..3), 2)] {
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ OFFSET_PARAM: offset })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": { "rows": rows },
                "meta": { "page_size": size },
            })))
            .expect(1)
            .mount(server)
            .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn required_echo_rejects_an_absent_offset() {
    let server = MockServer::start().await;
    // Page one alone: the fetch must abort on it, so no page two is asked
    // for and none is mounted.
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_with_echo(
        server.uri(),
        OffsetEcho::Required {
            pointer: Cow::Borrowed("/meta/from"),
        },
    )
    .await
    .unwrap_err();

    match error {
        EngineError::Pagination { reason } => assert!(reason.contains("no offset"), "{reason}"),
        other => panic!("expected Pagination error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_if_present_tolerates_an_absent_offset_but_flags_it() {
    // A spec can be wrong or drift: an endpoint that sends no envelope must
    // still be usable. The rows come back merged, marked unconfirmed.
    let server = MockServer::start().await;
    mount_echoless_pages(&server).await;

    let fetched = run_with_echo(
        server.uri(),
        OffsetEcho::ValidateIfPresent {
            pointer: Cow::Borrowed("/meta/from"),
        },
    )
    .await
    .unwrap();

    let rows = fetched.value["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "rows dropped: {}", fetched.value);
    assert_eq!(rows[2]["id"], "row-2");
    assert!(
        fetched.offset_unconfirmed,
        "an absent echo must be reported, never silent"
    );
    assert!(fetched.truncation.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_if_present_still_rejects_a_contradicting_offset() {
    // Tolerating absence must not tolerate a server contradicting itself:
    // that path produces wrong rows.
    let server = MockServer::start().await;
    mount_page(&server, 0, 2, items(0..2)).await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ OFFSET_PARAM: 2 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "from": 0, "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_with_echo(
        server.uri(),
        OffsetEcho::ValidateIfPresent {
            pointer: Cow::Borrowed("/meta/from"),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(error, EngineError::Pagination { .. }), "{error:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_if_present_still_rejects_an_unusable_offset() {
    // Present but not a number: comparable to nothing, so it cannot be
    // waved through as "absent".
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "from": "0", "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_with_echo(
        server.uri(),
        OffsetEcho::ValidateIfPresent {
            pointer: Cow::Borrowed("/meta/from"),
        },
    )
    .await
    .unwrap_err();

    match error {
        EngineError::Pagination { reason } => {
            assert!(reason.contains("unusable offset"), "{reason}");
        }
        other => panic!("expected Pagination error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_null_offset_counts_as_absent_not_as_a_contradiction() {
    // JSON null is the wire's way of saying "no value".
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rpc/things.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..1) },
            "meta": { "from": null, "page_size": 2 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let fetched = run_with_echo(
        server.uri(),
        OffsetEcho::ValidateIfPresent {
            pointer: Cow::Borrowed("/meta/from"),
        },
    )
    .await
    .unwrap();
    assert!(fetched.offset_unconfirmed);
    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn ignored_echo_never_flags_anything() {
    let server = MockServer::start().await;
    mount_echoless_pages(&server).await;

    let fetched = run_with_echo(server.uri(), OffsetEcho::Ignored)
        .await
        .unwrap();
    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 3);
    assert!(
        !fetched.offset_unconfirmed,
        "nothing was asked for, so nothing is unconfirmed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn confirmed_pages_are_not_flagged() {
    let server = MockServer::start().await;
    mount_page(&server, 0, 2, items(0..2)).await;
    mount_page(&server, 2, 2, items(2..3)).await;

    let fetched = run_with_echo(
        server.uri(),
        OffsetEcho::ValidateIfPresent {
            pointer: Cow::Borrowed("/meta/from"),
        },
    )
    .await
    .unwrap();
    assert!(!fetched.offset_unconfirmed);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_manual_branch_flags_an_absent_offset_too() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ LIMIT_PARAM: 5 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "rows": items(0..2) },
            "meta": { "page_size": 5 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uri = server.uri();
    let echo = OffsetEcho::ValidateIfPresent {
        pointer: Cow::Borrowed("/meta/from"),
    };
    let fetched = tokio::task::spawn_blocking(move || {
        let client = Client::new(&uri, "token")?;
        client.execute_paged(
            &op(),
            &[(LIMIT_PARAM.to_string(), "5".to_string())],
            ValidationMode::Strict,
            &spec_with(echo),
            None,
        )
    })
    .await
    .unwrap()
    .unwrap();

    assert!(fetched.offset_unconfirmed);
    assert_eq!(fetched.value["result"]["rows"].as_array().unwrap().len(), 2);
}
