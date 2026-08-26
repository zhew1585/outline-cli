//! Auto-pagination: fetching every page, merging rows, and never
//! truncating silently (Story 1.6).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod paging_harness;

use std::borrow::Cow;

use engine::{Client, OffsetEcho, OpSpec, ParamSpec, ParamType};

use engine::{EngineError, PaginationSpec, TruncationCause, ValidationMode};
use paging_harness::*;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
