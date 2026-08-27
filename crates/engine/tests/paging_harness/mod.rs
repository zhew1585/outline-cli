//! Shared fixtures for the pagination suites.
//!
//! `pagination` covers the core fetch-and-merge behaviour; `pagination_echo`
//! covers what the engine does when a server's offset echo, page-size hint
//! or descriptor is missing, wrong, or hostile. Both drive the same
//! wiremock scaffolding, which lives here.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;

use engine::{
    BodyMode, Client, EngineError, OffsetEcho, OpSpec, PaginationSpec, ParamSpec, ParamType,
    ValidationMode,
};
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const OFFSET_PARAM: &str = "from";
pub const LIMIT_PARAM: &str = "count";

/// A parameter spec with no schema facets
/// concern; these tests are about paging).
pub const fn param(name: &'static str, ty: ParamType) -> ParamSpec {
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

pub const PARAMS: &[ParamSpec] = &[
    param(OFFSET_PARAM, ParamType::Integer),
    param(LIMIT_PARAM, ParamType::Integer),
    param("query", ParamType::String),
];

pub fn op() -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("things.list"),
        path: Cow::Borrowed("/rpc/things.list"),
        summary: Cow::Borrowed("List things"),
        content_type: Cow::Borrowed("application/json"),
        body_mode: BodyMode::KeyValue,
        response_fields: Cow::Borrowed(&[]),
        params: Cow::Borrowed(PARAMS),
    }
}

/// The test API's convention: rows nested two levels deep, paging hints in
/// a separate metadata object.
pub fn spec() -> PaginationSpec {
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

pub fn items(range: std::ops::Range<u64>) -> Vec<Value> {
    range.map(|n| json!({ "id": format!("row-{n}") })).collect()
}

/// One well-behaved page: rows plus honest `page_size`/`from` hints.
pub async fn mount_page(server: &MockServer, offset: u64, page_size: u64, data: Vec<Value>) {
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
pub async fn mount_hintless_page(server: &MockServer, offset: u64, data: Vec<Value>) {
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
pub async fn run_paged(
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
