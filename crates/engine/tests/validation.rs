//! Local parameter validation and body assembly (Story 1.3).
//!
//! Every validation failure must occur before any network request; the
//! closed-port tests prove it (a network attempt would surface as a
//! Transport error instead of the asserted validation error).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;

use engine::{build_request_body, Client, EngineError, OpSpec, ParamSpec, ParamType};
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
    }
}

/// A synthetic operation exercising every parameter type.
fn test_op() -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("things.update"),
        path: Cow::Borrowed("/api/things.update"),
        summary: Cow::Borrowed("Update a thing"),
        params: Cow::Owned(vec![
            param("id", ParamType::String, true),
            param("limit", ParamType::Integer, false),
            param("ok", ParamType::Boolean, false),
            param("score", ParamType::Number, false),
            param("filters", ParamType::Json, false),
            param("note", ParamType::String, false),
        ]),
    }
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
    let op = OpSpec {
        name: Cow::Borrowed("things.noop"),
        path: Cow::Borrowed("/api/things.noop"),
        summary: Cow::Borrowed(""),
        params: Cow::Borrowed(&[]),
    };
    let error = build_request_body(&op, &args(&[("x", "1")])).unwrap_err();
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
