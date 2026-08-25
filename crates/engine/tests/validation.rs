//! Local parameter validation and body assembly (Story 1.3).
//!
//! Every validation failure must occur before any network request; the
//! closed-port tests prove it (a network attempt would surface as a
//! Transport error instead of the asserted validation error).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;

use engine::{build_request_body, BodyMode, Client, EngineError, OpSpec, ParamSpec, ParamType};
use serde_json::json;
use wiremock::matchers::{body_json, body_string, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Nothing listens here; reaching the network turns into a Transport error.
const CLOSED_PORT_URL: &str = "http://127.0.0.1:9";

fn param(name: &'static str, ty: ParamType, required: bool) -> ParamSpec {
    ParamSpec {
        name: Cow::Borrowed(name),
        ty,
        required,
        nullable: false,
        enum_values: Cow::Borrowed(&[]),
        minimum: None,
        maximum: None,
    }
}

/// An `application/json` operation with the given parameters.
fn json_op(params: Vec<ParamSpec>) -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("things.update"),
        path: Cow::Borrowed("/api/things.update"),
        summary: Cow::Borrowed("Update a thing"),
        content_type: Cow::Borrowed("application/json"),
        body_mode: BodyMode::KeyValue,
        params: Cow::Owned(params),
    }
}

/// A synthetic operation exercising every parameter type.
fn test_op() -> OpSpec {
    json_op(vec![
        param("id", ParamType::String, true),
        param("limit", ParamType::Integer, false),
        param("ok", ParamType::Boolean, false),
        param("score", ParamType::Number, false),
        param("filters", ParamType::Json, false),
        param("note", ParamType::String, false),
    ])
}

fn args(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn build_request_body_coerces_scalars_to_native_json_types() {
    let body = build_request_body(
        &test_op(),
        &args(&[
            ("id", "doc-1"),
            ("limit", "5"),
            ("ok", "true"),
            ("score", "1.5"),
            ("note", "42"),
        ]),
    )
    .unwrap();
    assert_eq!(
        body,
        json!({
            "id": "doc-1",
            "limit": 5,
            "ok": true,
            "score": 1.5,
            // A string-typed param stays a string even if it looks numeric.
            "note": "42",
        })
    );
}

#[test]
fn optional_params_omitted_stay_out_of_the_body() {
    let body = build_request_body(&test_op(), &args(&[("id", "doc-1")])).unwrap();
    assert_eq!(body, json!({ "id": "doc-1" }));
}

#[test]
fn missing_required_param_names_param_and_type() {
    let error = build_request_body(&test_op(), &[]).unwrap_err();
    match &error {
        EngineError::MissingParam { name, ty, .. } => {
            assert_eq!(name, "id");
            assert_eq!(*ty, ParamType::String);
        }
        other => panic!("expected MissingParam, got {other:?}"),
    }
    let rendered = format!("{error}");
    assert!(rendered.contains("\"id\""), "no param name: {rendered}");
    assert!(rendered.contains("string"), "no type: {rendered}");
}

#[test]
fn unknown_param_lists_valid_params() {
    let error = build_request_body(&test_op(), &args(&[("bogus", "1")])).unwrap_err();
    assert!(matches!(error, EngineError::UnknownParam { .. }));
    let rendered = format!("{error}");
    assert!(rendered.contains("\"bogus\""), "no bad name: {rendered}");
    for valid in ["id", "limit", "ok", "score", "filters", "note"] {
        assert!(rendered.contains(valid), "missing {valid}: {rendered}");
    }
}

#[test]
fn unknown_param_on_parameterless_op_says_so() {
    let error = build_request_body(&json_op(vec![]), &args(&[("x", "1")])).unwrap_err();
    let rendered = format!("{error}");
    assert!(
        rendered.contains("no key=value parameters"),
        "unhelpful message: {rendered}"
    );
}

#[test]
fn complex_param_as_key_value_is_rejected() {
    let error =
        build_request_body(&test_op(), &args(&[("id", "x"), ("filters", "[]")])).unwrap_err();
    match &error {
        EngineError::ComplexParam { name, .. } => assert_eq!(name, "filters"),
        other => panic!("expected ComplexParam, got {other:?}"),
    }
}

#[test]
fn scalar_coercion_failures_are_rejected_with_expected_type() {
    for (key, value, expected) in [
        ("limit", "abc", "integer"),
        ("limit", "1.5", "integer"),
        ("ok", "yes", "boolean"),
        ("score", "fast", "number"),
    ] {
        let error =
            build_request_body(&test_op(), &args(&[("id", "x"), (key, value)])).unwrap_err();
        assert!(
            matches!(error, EngineError::InvalidParamValue { .. }),
            "{key}={value}: got {error:?}"
        );
        let rendered = format!("{error}");
        assert!(
            rendered.contains(expected),
            "{key}={value}: type missing from {rendered}"
        );
    }
}

#[test]
fn duplicate_param_is_rejected() {
    let error = build_request_body(&test_op(), &args(&[("id", "a"), ("id", "b")])).unwrap_err();
    assert!(
        matches!(error, EngineError::InvalidParamValue { .. }),
        "got {error:?}"
    );
    assert!(format!("{error}").contains("more than once"));
}

#[test]
fn validation_variants_are_flagged_as_validation_errors() {
    let op = test_op();
    let cases = [
        build_request_body(&op, &[]).unwrap_err(),
        build_request_body(&op, &args(&[("bogus", "1")])).unwrap_err(),
        build_request_body(&op, &args(&[("id", "x"), ("filters", "[]")])).unwrap_err(),
        build_request_body(&op, &args(&[("id", "x"), ("limit", "abc")])).unwrap_err(),
        build_request_body(&union_op(), &[]).unwrap_err(),
        build_request_body(&unsupported_op(), &[]).unwrap_err(),
    ];
    for error in cases {
        assert!(error.is_validation(), "not validation: {error:?}");
    }
    // Server/transport errors are not usage errors.
    let api_error = EngineError::Api {
        status: 500,
        message: "boom".to_string(),
    };
    assert!(!api_error.is_validation());
}

#[test]
fn execute_fails_validation_before_any_network_request() {
    let client = Client::new(CLOSED_PORT_URL, "token").unwrap();
    let error = client.execute(&test_op(), &[]).unwrap_err();
    assert!(
        matches!(error, EngineError::MissingParam { .. }),
        "expected local MissingParam, got {error:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_sends_coerced_native_types_on_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.update"))
        .and(body_json(json!({
            "id": "doc-1",
            "limit": 5,
            "ok": true,
            "score": 1.5,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?;
        client.execute(
            &test_op(),
            &args(&[
                ("id", "doc-1"),
                ("limit", "5"),
                ("ok", "true"),
                ("score", "1.5"),
            ]),
        )
    })
    .await
    .unwrap();

    assert_eq!(result.unwrap()["data"]["ok"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_raw_sends_the_body_verbatim() {
    // Key order and fields unknown to the op spec must pass through
    // untouched: byte-for-byte equality on the wire.
    let raw_body = r#"{"z": 1, "a": {"nested": [true, null]}}"#;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.update"))
        .and(body_string(raw_body.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?;
        client.execute_raw(&test_op(), raw_body)
    })
    .await
    .unwrap();

    assert_eq!(result.unwrap()["data"]["ok"], true);
}

#[test]
fn execute_raw_rejects_invalid_json_before_any_network_request() {
    let client = Client::new(CLOSED_PORT_URL, "token").unwrap();
    let error = client.execute_raw(&test_op(), "{not json").unwrap_err();
    assert!(
        matches!(error, EngineError::InvalidRequestBody { .. }),
        "expected InvalidRequestBody, got {error:?}"
    );
    assert!(error.is_validation());
}

// --- Finding 1: operations whose body is not application/json ------------

/// An operation the generic client cannot assemble a body for.
fn unsupported_op() -> OpSpec {
    OpSpec {
        content_type: Cow::Borrowed("multipart/form-data"),
        body_mode: BodyMode::Unsupported,
        params: Cow::Borrowed(&[]),
        ..test_op()
    }
}

#[test]
fn unsupported_body_type_is_rejected_by_build_request_body() {
    let error = build_request_body(&unsupported_op(), &[]).unwrap_err();
    match &error {
        EngineError::UnsupportedBodyType { content_type, .. } => {
            assert_eq!(content_type, "multipart/form-data");
        }
        other => panic!("expected UnsupportedBodyType, got {other:?}"),
    }
    assert!(format!("{error}").contains("multipart/form-data"));
}

#[test]
fn unsupported_body_type_is_rejected_before_any_network_request() {
    let client = Client::new(CLOSED_PORT_URL, "token").unwrap();
    for error in [
        client.execute(&unsupported_op(), &[]).unwrap_err(),
        client
            .execute_raw(&unsupported_op(), r#"{"a":1}"#)
            .unwrap_err(),
    ] {
        assert!(
            matches!(error, EngineError::UnsupportedBodyType { .. }),
            "expected local UnsupportedBodyType, got {error:?}"
        );
        assert!(error.is_validation());
    }
}

// --- Finding 2: root-level oneOf/anyOf operations ------------------------

/// An operation whose body is constrained by a root-level JSON union, so
/// only a raw body can satisfy it.
fn union_op() -> OpSpec {
    OpSpec {
        body_mode: BodyMode::RawJsonOnly,
        params: Cow::Owned(vec![
            param("documentId", ParamType::String, false),
            param("collectionId", ParamType::String, false),
        ]),
        ..test_op()
    }
}

#[test]
fn union_body_op_refuses_key_value_args_even_when_empty() {
    for supplied in [vec![], args(&[("documentId", "a")])] {
        let error = build_request_body(&union_op(), &supplied).unwrap_err();
        assert!(
            matches!(error, EngineError::UnionBody { .. }),
            "expected UnionBody, got {error:?}"
        );
        assert!(error.is_validation());
    }
}

#[test]
fn union_body_op_refuses_key_value_before_any_network_request() {
    let client = Client::new(CLOSED_PORT_URL, "token").unwrap();
    let error = client.execute(&union_op(), &[]).unwrap_err();
    assert!(
        matches!(error, EngineError::UnionBody { .. }),
        "expected local UnionBody, got {error:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn union_body_op_accepts_a_raw_body() {
    let raw_body = r#"{"documentId":"doc-1"}"#;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.update"))
        .and(body_string(raw_body.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })))
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?;
        client.execute_raw(&union_op(), raw_body)
    })
    .await
    .unwrap();

    assert_eq!(result.unwrap()["data"]["ok"], true);
}

// --- Finding 3: enum, nullable and numeric bounds ------------------------

/// An operation with a string enum, a nullable string, and a bounded int.
fn faceted_op() -> OpSpec {
    json_op(vec![
        ParamSpec {
            enum_values: Cow::Borrowed(&[
                Cow::Borrowed("read"),
                Cow::Borrowed("read_write"),
                Cow::Borrowed("admin"),
            ]),
            ..param("permission", ParamType::String, false)
        },
        ParamSpec {
            nullable: true,
            ..param("collectionId", ParamType::String, false)
        },
        ParamSpec {
            minimum: Some(0.0),
            maximum: Some(100.0),
            ..param("size", ParamType::Integer, false)
        },
        param("id", ParamType::String, false),
    ])
}

#[test]
fn enum_param_accepts_declared_variants() {
    let body = build_request_body(&faceted_op(), &args(&[("permission", "read_write")])).unwrap();
    assert_eq!(body, json!({ "permission": "read_write" }));
}

#[test]
fn enum_param_rejects_unknown_value_listing_allowed_values() {
    let error =
        build_request_body(&faceted_op(), &args(&[("permission", "definitely-not")])).unwrap_err();
    assert!(
        matches!(error, EngineError::InvalidParamValue { .. }),
        "got {error:?}"
    );
    let rendered = format!("{error}");
    for allowed in ["read", "read_write", "admin"] {
        assert!(rendered.contains(allowed), "missing {allowed}: {rendered}");
    }
}

#[test]
fn numeric_bounds_are_enforced_locally() {
    for value in ["-1", "101"] {
        let error = build_request_body(&faceted_op(), &args(&[("size", value)])).unwrap_err();
        assert!(
            matches!(error, EngineError::InvalidParamValue { .. }),
            "size={value}: got {error:?}"
        );
    }
    let body = build_request_body(&faceted_op(), &args(&[("size", "0")])).unwrap();
    assert_eq!(body, json!({ "size": 0 }));
}

#[test]
fn nullable_param_maps_the_null_literal_to_json_null() {
    let body = build_request_body(&faceted_op(), &args(&[("collectionId", "null")])).unwrap();
    assert_eq!(body, json!({ "collectionId": null }));
    // A real value still passes through as a string.
    let body = build_request_body(&faceted_op(), &args(&[("collectionId", "abc")])).unwrap();
    assert_eq!(body, json!({ "collectionId": "abc" }));
}

#[test]
fn nullable_complex_param_accepts_the_null_literal() {
    // Clearing a nullable object/array field is the one thing key=value
    // can express about a complex parameter.
    let op = json_op(vec![ParamSpec {
        nullable: true,
        ..param("preferences", ParamType::Json, false)
    }]);
    let body = build_request_body(&op, &args(&[("preferences", "null")])).unwrap();
    assert_eq!(body, json!({ "preferences": null }));
    // Any other value for it still points at --body.
    let error = build_request_body(&op, &args(&[("preferences", "{}")])).unwrap_err();
    assert!(
        matches!(error, EngineError::ComplexParam { .. }),
        "got {error:?}"
    );
}

#[test]
fn non_nullable_param_keeps_the_null_literal_as_a_string() {
    let body = build_request_body(&faceted_op(), &args(&[("id", "null")])).unwrap();
    assert_eq!(body, json!({ "id": "null" }));
}

// --- Finding 6: exact JSON numbers --------------------------------------

#[test]
fn large_integers_keep_full_precision() {
    // 2^53 + 1 is exactly representable as i64 but not as f64.
    let body = build_request_body(
        &test_op(),
        &args(&[("id", "x"), ("score", "9007199254740993")]),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        r#"{"id":"x","score":9007199254740993}"#
    );
}

#[test]
fn numbers_beyond_u64_and_inexact_decimals_are_rejected() {
    for value in [
        "99999999999999999999999999",  // beyond u64: would round
        "9007199254740993.0",          // not representable in f64
        "0.1234567890123456789012345", // more digits than f64 can hold
    ] {
        let error =
            build_request_body(&test_op(), &args(&[("id", "x"), ("score", value)])).unwrap_err();
        assert!(
            matches!(error, EngineError::InexactNumber { .. }),
            "score={value}: got {error:?}"
        );
        assert!(error.is_validation());
    }
    // Non-finite values are plain invalid, not merely inexact.
    for value in ["1e400", "NaN", "fast"] {
        let error =
            build_request_body(&test_op(), &args(&[("id", "x"), ("score", value)])).unwrap_err();
        assert!(
            matches!(error, EngineError::InvalidParamValue { .. }),
            "score={value}: got {error:?}"
        );
    }
}

#[test]
fn exactly_representable_decimals_are_accepted() {
    for (value, expected) in [("1.25e3", 1250.0), ("-0.5", -0.5), ("2", 2.0)] {
        let body = build_request_body(&test_op(), &args(&[("id", "x"), ("score", value)])).unwrap();
        assert_eq!(
            body["score"].as_f64(),
            Some(expected),
            "score={value} mismatched"
        );
    }
}

// --- Finding 4: secrets inside a raw body ------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn secrets_echoed_from_a_raw_body_are_redacted_from_errors() {
    let secret = "BODY-SECRET-9f3d";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.update"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "message": format!("server echoed {{\"password\":\"{secret}\"}}")
        })))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?;
        client.execute_raw(&test_op(), &format!(r#"{{"password":"{secret}"}}"#))
    })
    .await
    .unwrap();

    let error = result.unwrap_err();
    // Display, Debug and the whole source chain must be secret-free.
    let rendered = format!("{error} / {error:?}");
    assert_eq!(
        rendered.matches("BODY-SECRET").count(),
        0,
        "body secret leaked: {rendered}"
    );
    assert!(rendered.contains("***"), "no redaction marker: {rendered}");
}

#[tokio::test(flavor = "multi_thread")]
async fn short_body_values_do_not_blank_out_error_messages() {
    // Only values at or above the minimum length are treated as sensitive,
    // so ordinary short field values cannot swallow the server's message.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.update"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "message": "document abc was not found"
        })))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?;
        client.execute_raw(&test_op(), r#"{"id":"abc"}"#)
    })
    .await
    .unwrap();

    match result.unwrap_err() {
        EngineError::Api { message, .. } => {
            assert_eq!(message, "document abc was not found");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_and_array_body_secrets_are_redacted() {
    let secret = "NESTED-SECRET-1234";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/things.update"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "message": format!("rejected {secret}") })),
        )
        .mount(&server)
        .await;

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let client = Client::new(&base_url, "token")?;
        client.execute_raw(
            &test_op(),
            &format!(r#"{{"outer":{{"inner":["{secret}"]}}}}"#),
        )
    })
    .await
    .unwrap();

    let error = result.unwrap_err();
    let rendered = format!("{error} / {error:?}");
    assert_eq!(
        rendered.matches("NESTED-SECRET").count(),
        0,
        "nested body secret leaked: {rendered}"
    );
}
