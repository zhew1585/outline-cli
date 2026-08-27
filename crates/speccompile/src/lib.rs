//! Compile an OpenAPI document into a flat, versioned RPC IR.
//!
//! The output is a plain data table: one [`CompiledOp`] per operation,
//! carrying its request path and the scalar parameters of its JSON body.
//! There is deliberately no per-endpoint code generation.
//!
//! Two callers share this crate, which is the whole reason it exists:
//!
//! - a build script, which renders the table as Rust source compiled into
//!   the binary (the built-in IR);
//! - `spec sync` at run time, which compiles a freshly fetched document
//!   and caches the result.
//!
//! Both therefore agree by construction, and the runtime path parses a
//! spec exactly once - never at startup.
//!
//! The IR types here MIRROR `engine::ir` without depending on it (see
//! `Cargo.toml` for why). The mapping between the two is a single
//! exhaustive conversion in the `otl` crate, and a parity test asserts
//! that build-time and run-time compilation of the same document produce
//! identical tables.
//!
//! # Untrusted input
//!
//! A document fetched over the network reaches [`compile_json`] before
//! anything else looks at it, so this crate treats its input as hostile:
//! every failure is a typed error rather than a panic, recursion is
//! depth-bounded, and every compiled operation name and path is checked
//! against [`is_safe_op_name`]/[`is_safe_path`] - a path that could turn
//! `base_url + path` into a request to a different host would otherwise
//! leak the caller's bearer token.

#![forbid(unsafe_code)]

mod document;
mod rules;
mod schema;
mod text;

use serde_json::Value;
use thiserror::Error;

pub use document::MAX_PARSED_BYTES;
pub use rules::{is_safe_op_name, is_safe_path, MAX_OP_NAME_BYTES, MAX_PATH_BYTES};
pub use schema::MAX_SCHEMA_DEPTH;
pub use text::{
    is_display_safe, MAX_CONTENT_TYPE_BYTES, MAX_ENUM_VALUES, MAX_ENUM_VALUE_BYTES,
    MAX_FORMAT_BYTES, MAX_PARAM_NAME_BYTES, MAX_RESPONSE_FIELDS, MAX_SUMMARY_BYTES,
    MAX_SUMMARY_CHARS,
};

/// The only request content type a generic JSON RPC client can assemble.
pub const JSON_CONTENT_TYPE: &str = "application/json";

/// How the request body of an operation can be supplied. Mirrors
/// `engine::ir::BodyMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// A JSON object body assembled from `key=value` pairs.
    KeyValue,
    /// A JSON body constrained by a root-level `oneOf`/`anyOf` union;
    /// only a caller-supplied raw body can express it.
    RawJsonOnly,
    /// A content type this generic client cannot assemble.
    Unsupported,
}

/// The wire type of a single request parameter. Mirrors
/// `engine::ir::ParamType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    /// JSON string.
    String,
    /// JSON integer.
    Integer,
    /// JSON boolean.
    Boolean,
    /// JSON number (floating point).
    Number,
    /// Any complex JSON value (objects, arrays, unions).
    Json,
}

/// One request-body parameter with its constraint facets.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledParam {
    /// Parameter name as it appears in the JSON request body.
    pub name: String,
    /// Declared wire type.
    pub ty: ScalarKind,
    /// Whether the schema marks the parameter as required.
    pub required: bool,
    /// Whether the schema allows an explicit JSON `null`.
    pub nullable: bool,
    /// Allowed values when the schema is an enumeration; empty otherwise.
    pub enum_values: Vec<String>,
    /// Declared `format` (e.g. `uuid`), empty when absent.
    pub format: String,
    /// Inclusive lower bound for numeric parameters, if any.
    pub minimum: Option<f64>,
    /// Inclusive upper bound for numeric parameters, if any.
    pub maximum: Option<f64>,
    /// One-line prose from the source schema, already sanitized and capped;
    /// empty when the schema declares none.
    pub description: String,
}

/// One field of an operation's success response payload.
///
/// Mirrors `engine::ir::FieldSpec`. Declaration order is preserved (see
/// `Cargo.toml` for the `preserve_order` note): it is the schema's own
/// statement of which fields matter most, and a renderer ranks columns by
/// it.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledField {
    /// Field name as it appears in the JSON response.
    pub name: String,
    /// Declared wire type; [`ScalarKind::Json`] for anything that is not a
    /// single displayable value.
    pub ty: ScalarKind,
    /// Declared `format` (e.g. `uuid`), empty when absent.
    pub format: String,
    /// Whether the schema allows an explicit JSON `null`.
    pub nullable: bool,
    /// Whether the schema marks the field `readOnly`.
    pub read_only: bool,
}

/// One compiled RPC operation.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledOp {
    /// Operation name in `resource.method` form.
    pub name: String,
    /// Request path, prefix included, joined verbatim onto a base URL.
    pub path: String,
    /// One-line summary from the source document (may be empty).
    pub summary: String,
    /// Request content type declared by the document, empty when the
    /// operation takes no request body.
    pub content_type: String,
    /// How the request body may be supplied.
    pub body_mode: BodyKind,
    /// Request-body parameters, in the order the document declares them
    /// (`preserve_order` is on). Deterministic either way, which is what
    /// the build-time/run-time parity rests on.
    pub params: Vec<CompiledParam>,
    /// Fields of one item of the success response payload, in the order the
    /// source schema declares them. Empty when the document describes no
    /// response shape.
    pub response_fields: Vec<CompiledField>,
    /// Whether the document marks the operation `deprecated`.
    ///
    /// Read here and deliberately NOT carried into the runtime IR: the IR is
    /// a dispatch table, a deprecation bit would change its schema version
    /// (invalidating every user's cache), and the one consumer that needs
    /// the flag - `otl doctor`, comparing the online API against the local
    /// table - compiles the fetched document itself and can read it from
    /// there. A deprecated operation still dispatches exactly as before.
    pub deprecated: bool,
}

/// A whole compiled document: operations sorted by name.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledSpec {
    /// Compiled operations, sorted by name and free of duplicates.
    pub ops: Vec<CompiledOp>,
}

/// Compilation knobs.
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Prefix prepended to every document path (e.g. `/api`), expressing a
    /// service's URL convention. Empty to use document paths verbatim.
    ///
    /// This is where a service-specific convention enters; the compiler
    /// itself knows no service.
    pub path_prefix: String,
    /// Property of a success response that holds the payload (e.g. `data`),
    /// for services that wrap their responses in an envelope. Empty when
    /// the response schema IS the payload.
    ///
    /// Also service-specific, and for the same reason it lives here rather
    /// than in the compiler: the compiler knows about OpenAPI, not about
    /// anybody's envelope.
    pub envelope_data_property: String,
}

impl CompileOptions {
    /// Options with the given path prefix and no response envelope.
    pub fn with_prefix(path_prefix: impl Into<String>) -> Self {
        Self {
            path_prefix: path_prefix.into(),
            envelope_data_property: String::new(),
        }
    }

    /// Same, for a service whose success payload sits under one property.
    #[must_use]
    pub fn with_envelope(self, data_property: impl Into<String>) -> Self {
        Self {
            envelope_data_property: data_property.into(),
            ..self
        }
    }
}

/// Why a document could not be compiled.
///
/// Every variant is content-free apart from names taken from the document
/// itself: the text of an error is safe to print.
#[derive(Debug, Error, PartialEq)]
pub enum CompileError {
    /// The document is not valid JSON.
    #[error("not a valid JSON document: {reason}")]
    NotJson {
        /// serde_json position message (no content).
        reason: String,
    },
    /// The document is valid JSON, and too large to parse within the
    /// budget.
    ///
    /// Separate from [`CompileError::NotJson`] on purpose: the input is
    /// well-formed, so telling the reader it is malformed sends them
    /// looking for a syntax error that does not exist.
    #[error(
        "it is {} of JSON that expands to more than the {} this parser will hold \
         (charged {} before stopping); check that the document is an API \
         description and not something else",
        document::human_bytes(*document_bytes),
        document::human_bytes(*limit),
        document::human_bytes(*charged)
    )]
    TooLarge {
        /// Size of the input document.
        document_bytes: usize,
        /// Estimated heap cost charged before the budget ran out.
        charged: usize,
        /// The budget.
        limit: usize,
    },
    /// The document has no `paths` object.
    #[error("document has no object at `paths`")]
    NoPaths,
    /// The document declares no usable operation.
    #[error("document declares no POST operations")]
    NoOperations,
    /// Two operations compiled to the same name.
    #[error("duplicate operation name {name:?}")]
    DuplicateOperation {
        /// The repeated operation name.
        name: String,
    },
    /// An operation declares a `requestBody` with no content types.
    #[error("operation {operation:?} declares a requestBody with no content types")]
    EmptyRequestBody {
        /// The offending operation.
        operation: String,
    },
    /// A `$ref` uses a form this compiler does not support.
    #[error("unsupported $ref {reference:?}: only local component schemas are handled")]
    UnsupportedRef {
        /// The offending reference.
        reference: String,
    },
    /// A `$ref` does not resolve within the document.
    #[error("$ref {reference:?} does not resolve in the document")]
    UnresolvedRef {
        /// The offending reference.
        reference: String,
    },
    /// Schema composition nests deeper than [`MAX_SCHEMA_DEPTH`], which a
    /// reference cycle also produces.
    #[error("schema $ref/allOf nesting exceeds {MAX_SCHEMA_DEPTH} levels (reference cycle?)")]
    DepthExceeded,
    /// An operation name or path is not safe to use.
    ///
    /// The hard case: a path that escapes its base URL (`//host`, `@host`,
    /// a scheme, a `..` segment) would send the caller's bearer token to
    /// another origin once joined onto the base URL.
    #[error("operation {operation:?} has an unusable {field}: {reason}")]
    UnsafeIdentifier {
        /// The offending operation, as named by the document.
        operation: String,
        /// Which field is at fault (`name` or `path`).
        field: &'static str,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A string with meaning (a parameter name, content type, format or
    /// enum value) carries characters a terminal would execute, or is
    /// implausibly long.
    ///
    /// Such text cannot be silently rewritten the way a summary can: it is
    /// matched against user input or sent on the wire, so the document is
    /// rejected instead. The offending value is deliberately NOT echoed -
    /// printing it is exactly the attack.
    #[error(
        "operation {operation:?} has an unusable {field}: {reason} \
         (the value is not shown: printing it is the attack)"
    )]
    UnsafeText {
        /// The offending operation.
        operation: String,
        /// Which kind of string is at fault.
        field: &'static str,
        /// Why it was rejected.
        reason: &'static str,
    },
}

/// Compile a JSON OpenAPI document.
///
/// The input is treated as untrusted throughout, starting with the parse:
/// only the parts this compiler reads are materialized, and they are
/// charged against a budget as they are built (see [`document`]). There is
/// deliberately no entry point that takes an already-parsed
/// `serde_json::Value` - that would be a way to skip the one limit that
/// bounds memory before any other limit exists.
pub fn compile_json(raw: &str, options: &CompileOptions) -> Result<CompiledSpec, CompileError> {
    let document = document::parse(raw, MAX_PARSED_BYTES)?;
    compile(&document, options)
}

/// Compile a parsed document.
fn compile(
    document: &document::Document,
    options: &CompileOptions,
) -> Result<CompiledSpec, CompileError> {
    let paths = document.paths.as_object().ok_or(CompileError::NoPaths)?;
    let mut ops: Vec<CompiledOp> = paths
        .iter()
        .filter_map(|(path, item)| item.get("post").map(|post| (path, post)))
        .map(|(path, post)| compile_op(path, post, &document.components, options))
        .collect::<Result<_, _>>()?;
    if ops.is_empty() {
        return Err(CompileError::NoOperations);
    }
    ops.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(pair) = ops.windows(2).find(|pair| pair[0].name == pair[1].name) {
        return Err(CompileError::DuplicateOperation {
            name: pair[0].name.clone(),
        });
    }
    Ok(CompiledSpec { ops })
}

/// Compile one POST operation into an IR entry.
///
/// An operation whose request body is not JSON keeps its content type and
/// is marked [`BodyKind::Unsupported`]: a generic client cannot assemble
/// such a body, so it must fail at dispatch rather than silently send an
/// empty JSON object.
fn compile_op(
    path: &str,
    post: &Value,
    components: &Value,
    options: &CompileOptions,
) -> Result<CompiledOp, CompileError> {
    let name = path.trim_start_matches('/').to_string();
    let (content_type, schema) = request_body(post, &name)?;
    let (params, root_union) = match schema {
        Some(schema) => schema::extract_params(schema, components)?,
        None => (Vec::new(), false),
    };
    let body_mode = body_kind(&content_type, root_union);
    let response_fields =
        schema::extract_response_fields(post, components, &options.envelope_data_property)?;
    let op = CompiledOp {
        path: format!("{}{path}", options.path_prefix),
        summary: extract_summary(post),
        content_type,
        body_mode,
        params,
        response_fields,
        deprecated: is_deprecated(post),
        name,
    };
    rules::check_identifiers(&op, options)?;
    rules::check_text(&op)?;
    Ok(op)
}

/// Classify how the request body of an operation may be supplied.
fn body_kind(content_type: &str, root_union: bool) -> BodyKind {
    if !content_type.is_empty() && content_type != JSON_CONTENT_TYPE {
        return BodyKind::Unsupported;
    }
    if root_union {
        BodyKind::RawJsonOnly
    } else {
        BodyKind::KeyValue
    }
}

/// The operation's request content type and its JSON schema.
///
/// Returns an empty content type when the operation takes no request
/// body, and no schema when the body is not JSON.
fn request_body<'a>(
    post: &'a Value,
    operation: &str,
) -> Result<(String, Option<&'a Value>), CompileError> {
    let Some(content) = post
        .pointer("/requestBody/content")
        .and_then(Value::as_object)
    else {
        return Ok((String::new(), None));
    };
    if let Some(entry) = content.get(JSON_CONTENT_TYPE) {
        return Ok((JSON_CONTENT_TYPE.to_string(), entry.get("schema")));
    }
    let content_type = content
        .keys()
        .next()
        .ok_or_else(|| CompileError::EmptyRequestBody {
            operation: operation.to_string(),
        })?;
    Ok((content_type.clone(), None))
}

/// One-line summary: the document `summary`, falling back to the first
/// line of `description`. Empty if neither exists.
///
/// The result is SANITIZED, not validated: control characters (terminal
/// escapes, row-forging tabs and newlines, bidi overrides) are dropped and
/// the length is capped. Nothing dispatches on a summary, so dropping
/// characters cannot change behaviour - whereas rejecting the whole
/// document because one description contains a stray byte would.
fn extract_summary(post: &Value) -> String {
    let raw = post
        .get("summary")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| post.get("description").and_then(Value::as_str))
        .unwrap_or_default();
    let first_line = raw.lines().next().unwrap_or_default();
    text::sanitize_display(first_line)
}

/// Whether the document marks this operation `deprecated`.
///
/// Strictly the JSON boolean `true`: OpenAPI defines the field as a
/// boolean, and treating any truthy-looking value (`"false"`, `0`, `{}`) as
/// deprecation would let a document deprecate an operation by accident. A
/// missing field means "not deprecated", which is the OpenAPI default.
fn is_deprecated(post: &Value) -> bool {
    post.get("deprecated") == Some(&Value::Bool(true))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn opts() -> CompileOptions {
        CompileOptions::with_prefix("/api")
    }

    fn doc(paths: &str) -> String {
        format!(r#"{{"paths":{paths}}}"#)
    }

    #[test]
    fn compiles_a_minimal_operation() {
        let raw = doc(r#"{"/things.info":{"post":{"summary":"Info about a thing",
                "requestBody":{"content":{"application/json":{"schema":{
                    "type":"object","required":["id"],
                    "properties":{"id":{"type":"string"},"count":{"type":"integer"}}}}}}}}}"#);
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        assert_eq!(compiled.ops.len(), 1);
        let op = &compiled.ops[0];
        assert_eq!(op.name, "things.info");
        assert_eq!(op.path, "/api/things.info");
        assert_eq!(op.summary, "Info about a thing");
        assert_eq!(op.content_type, JSON_CONTENT_TYPE);
        assert_eq!(op.body_mode, BodyKind::KeyValue);
        // DECLARATION order, not alphabetical: `preserve_order` is on,
        // because the document's own ordering is what a renderer ranks
        // columns by. (`id` is declared first here even though `count`
        // sorts before it.)
        assert_eq!(op.params.len(), 2);
        assert_eq!(op.params[0].name, "id");
        assert_eq!(op.params[0].ty, ScalarKind::String);
        assert!(op.params[0].required);
        assert_eq!(op.params[1].name, "count");
        assert_eq!(op.params[1].ty, ScalarKind::Integer);
        assert!(!op.params[1].required);
    }

    /// The `deprecated` flag is read per operation, strictly as the JSON
    /// boolean `true`. `otl doctor` reports it, so a document must not be
    /// able to deprecate an operation with a truthy-looking value.
    #[test]
    fn reads_the_deprecated_flag_of_an_operation() {
        let raw = doc(r#"{"/things.old":{"post":{"deprecated":true}},
                "/things.new":{"post":{}},
                "/things.stringy":{"post":{"deprecated":"true"}},
                "/things.zero":{"post":{"deprecated":0}}}"#);
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        let flag = |name: &str| {
            compiled
                .ops
                .iter()
                .find(|op| op.name == name)
                .expect("operation is compiled")
                .deprecated
        };
        assert!(flag("things.old"));
        assert!(!flag("things.new"));
        assert!(!flag("things.stringy"), "a string is not the boolean true");
        assert!(!flag("things.zero"), "a number is not the boolean true");
    }

    #[test]
    fn rejects_a_path_that_would_retarget_the_request() {
        // `base_url + "/api" + "/@evil.example"` puts everything after the
        // `@` in the authority: the token would go to another host.
        let raw = doc(r#"{"/@evil.example/x":{"post":{}}}"#);
        let error = compile_json(&raw, &opts()).expect_err("must be rejected");
        assert!(
            matches!(error, CompileError::UnsafeIdentifier { field, .. } if field == "name"),
            "unexpected error: {error}"
        );
    }

    /// A document path without a leading slash gets swallowed by the
    /// prefix: `documents.delete` becomes `/apidocuments.delete`, which
    /// passes every character rule while dispatching to an endpoint nobody
    /// named. The compiler must establish the name/path binding itself, not
    /// leave it to whoever consumes the table.
    #[test]
    fn rejects_a_document_path_without_a_leading_slash() {
        let raw = doc(r#"{"documents.delete":{"post":{}}}"#);
        let error = compile_json(&raw, &opts()).expect_err("must be rejected");
        assert!(
            matches!(error, CompileError::UnsafeIdentifier { field, .. } if field == "path"),
            "unexpected error: {error}"
        );
        assert!(error.to_string().contains("start with `/`"), "{error}");
    }

    /// Every compiled operation satisfies `path == prefix + "/" + name`,
    /// with any prefix.
    #[test]
    fn the_name_path_binding_holds_for_every_prefix() {
        for prefix in ["", "/api", "/v1/rpc"] {
            let options = CompileOptions::with_prefix(prefix);
            let raw = doc(r#"{"/things.info":{"post":{}},"/things.list":{"post":{}}}"#);
            let compiled = compile_json(&raw, &options).expect("compiles");
            for op in &compiled.ops {
                assert_eq!(
                    op.path,
                    format!("{prefix}/{}", op.name),
                    "prefix {prefix:?}"
                );
            }
        }
    }

    #[test]
    fn rejects_a_traversal_path() {
        let raw = doc(r#"{"/../secrets":{"post":{}}}"#);
        assert!(compile_json(&raw, &opts()).is_err());
    }

    #[test]
    fn rejects_an_empty_document() {
        assert_eq!(
            compile_json(r#"{"paths":{}}"#, &opts()),
            Err(CompileError::NoOperations)
        );
        assert_eq!(compile_json("{}", &opts()), Err(CompileError::NoPaths));
        assert!(matches!(
            compile_json("not json", &opts()),
            Err(CompileError::NotJson { .. })
        ));
    }

    #[test]
    fn marks_a_non_json_body_unsupported() {
        let raw = doc(r#"{"/things.import":{"post":{"requestBody":{"content":{
                "multipart/form-data":{"schema":{"type":"object"}}}}}}}"#);
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        assert_eq!(compiled.ops[0].body_mode, BodyKind::Unsupported);
        assert_eq!(compiled.ops[0].content_type, "multipart/form-data");
        assert!(compiled.ops[0].params.is_empty());
    }

    #[test]
    fn marks_a_root_union_body_raw_only() {
        let raw = doc(r#"{"/things.act":{"post":{"requestBody":{"content":{
                "application/json":{"schema":{"oneOf":[
                    {"type":"object","properties":{"a":{"type":"string"}}},
                    {"type":"object","properties":{"b":{"type":"string"}}}]}}}}}}}"#);
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        assert_eq!(compiled.ops[0].body_mode, BodyKind::RawJsonOnly);
    }

    #[test]
    fn falls_back_to_the_first_description_line() {
        let raw = doc(r#"{"/things.info":{"post":{"description":"First  line\nSecond"}}}"#);
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        assert_eq!(compiled.ops[0].summary, "First line");
    }

    #[test]
    fn empty_prefix_keeps_document_paths() {
        let raw = doc(r#"{"/things.info":{"post":{}}}"#);
        let compiled = compile_json(&raw, &CompileOptions::default()).expect("compiles");
        assert_eq!(compiled.ops[0].path, "/things.info");
    }

    #[test]
    fn sorts_operations_by_name() {
        let raw = doc(r#"{"/b.op":{"post":{}},"/a.op":{"post":{}}}"#);
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        let names: Vec<&str> = compiled.ops.iter().map(|op| op.name.as_str()).collect();
        assert_eq!(names, ["a.op", "b.op"]);
    }

    #[test]
    fn path_checks_reject_authority_and_escapes() {
        for bad in [
            "",
            "things.info",
            "//evil.example/x",
            "/x@evil.example",
            "/x?y=1",
            "/x#y",
            "/%2e%2e/x",
            "/x\ny",
            "/a/../b",
            "/a//b",
            "/http:/x",
        ] {
            assert!(!is_safe_path(bad), "{bad:?} must be rejected");
        }
        assert!(is_safe_path("/api/things.info"));
        assert!(is_safe_path("/a/b-c_d.e"));
    }

    /// A summary is display-only, so hostile characters are dropped rather
    /// than making the whole document unusable - but they must be gone
    /// before the string reaches the IR, because `api list` prints it
    /// verbatim in a line-oriented format.
    #[test]
    fn a_hostile_summary_is_sanitized_not_propagated() {
        let raw = doc(r#"{"/things.info":{"post":{"summary":
                "safe \u001b]52;c;cGF3bmVk\u0007 \u001b[31mred\u001b[0m\ttab"}}}"#);
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        let summary = &compiled.ops[0].summary;
        for forbidden in ['\u{1b}', '\u{7}', '\t', '\n', '\r'] {
            assert!(
                !summary.contains(forbidden),
                "{forbidden:?} survived: {summary:?}"
            );
        }
        assert!(summary.contains("red"), "{summary:?}");
        assert!(is_display_safe(summary, MAX_SUMMARY_CHARS), "{summary:?}");
    }

    #[test]
    fn an_enormous_summary_is_capped() {
        let raw = doc(&format!(
            r#"{{"/things.info":{{"post":{{"summary":"{}"}}}}}}"#,
            "x".repeat(200_000)
        ));
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        assert_eq!(compiled.ops[0].summary.chars().count(), MAX_SUMMARY_CHARS);
    }

    /// Text with meaning cannot be silently rewritten (it is matched
    /// against user input or sent on the wire), so a document that puts
    /// terminal control sequences there is rejected whole.
    #[test]
    fn hostile_text_with_meaning_is_rejected() {
        let cases = [
            // parameter name
            r#"{"/things.info":{"post":{"requestBody":{"content":{"application/json":
                {"schema":{"type":"object","properties":{"id\u001b[31m":{"type":"string"}}}}}}}}}"#,
            // content type
            r#"{"/things.info":{"post":{"requestBody":{"content":{"text/x\u001b[31m":
                {"schema":{"type":"object"}}}}}}}"#,
            // enum value
            r#"{"/things.info":{"post":{"requestBody":{"content":{"application/json":
                {"schema":{"type":"object","properties":{"mode":{"type":"string",
                "enum":["ok","bad\u0007"]}}}}}}}}}"#,
            // format
            r#"{"/things.info":{"post":{"requestBody":{"content":{"application/json":
                {"schema":{"type":"object","properties":{"id":{"type":"string",
                "format":"uu\nid"}}}}}}}}}"#,
        ];
        for case in cases {
            let error = compile_json(&doc(case), &opts()).expect_err("must be rejected");
            assert!(
                matches!(error, CompileError::UnsafeText { .. }),
                "unexpected error: {error}"
            );
            // The error must not print the value it just rejected.
            let text = format!("{error}");
            assert!(!text.contains('\u{1b}'), "escape echoed: {text:?}");
            assert!(!text.contains('\u{7}'), "control echoed: {text:?}");
        }
    }

    #[test]
    fn an_unbounded_enum_is_rejected() {
        let values: String = (0..MAX_ENUM_VALUES + 1)
            .map(|index| format!(r#""v{index}""#))
            .collect::<Vec<_>>()
            .join(",");
        let raw = doc(&format!(
            r#"{{"/things.info":{{"post":{{"requestBody":{{"content":{{"application/json":
                {{"schema":{{"type":"object","properties":{{"mode":{{"type":"string",
                "enum":[{values}]}}}}}}}}}}}}}}}}}}"#
        ));
        let error = compile_json(&raw, &opts()).expect_err("must be rejected");
        assert!(
            matches!(error, CompileError::UnsafeText { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn op_name_checks_reject_metacharacters() {
        for bad in ["", "a b", "a/b", "a;b", "a\u{7f}b", "\u{4e2d}"] {
            assert!(!is_safe_op_name(bad), "{bad:?} must be rejected");
        }
        assert!(is_safe_op_name("documents.info"));
    }
}
