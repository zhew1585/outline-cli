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

mod schema;

use serde_json::Value;
use thiserror::Error;

pub use schema::MAX_SCHEMA_DEPTH;

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
    /// Request-body parameters, in the order `serde_json` yields object
    /// keys (alphabetical, since `preserve_order` is off). Deterministic
    /// either way, which is what the build-time/run-time parity rests on.
    pub params: Vec<CompiledParam>,
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
}

impl CompileOptions {
    /// Options with the given path prefix.
    pub fn with_prefix(path_prefix: impl Into<String>) -> Self {
        Self {
            path_prefix: path_prefix.into(),
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
}

/// Compile a JSON OpenAPI document.
///
/// The input is treated as untrusted; see the crate docs.
pub fn compile_json(raw: &str, options: &CompileOptions) -> Result<CompiledSpec, CompileError> {
    let document: Value = serde_json::from_str(raw).map_err(|error| CompileError::NotJson {
        reason: error.to_string(),
    })?;
    compile(&document, options)
}

/// Compile an already-parsed JSON OpenAPI document.
pub fn compile(document: &Value, options: &CompileOptions) -> Result<CompiledSpec, CompileError> {
    let paths = document
        .get("paths")
        .and_then(Value::as_object)
        .ok_or(CompileError::NoPaths)?;
    let mut ops: Vec<CompiledOp> = paths
        .iter()
        .filter_map(|(path, item)| item.get("post").map(|post| (path, post)))
        .map(|(path, post)| compile_op(path, post, document, options))
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
    document: &Value,
    options: &CompileOptions,
) -> Result<CompiledOp, CompileError> {
    let name = path.trim_start_matches('/').to_string();
    let (content_type, schema) = request_body(post, &name)?;
    let (params, root_union) = match schema {
        Some(schema) => schema::extract_params(schema, document)?,
        None => (Vec::new(), false),
    };
    let body_mode = body_kind(&content_type, root_union);
    let op = CompiledOp {
        path: format!("{}{path}", options.path_prefix),
        summary: extract_summary(post),
        content_type,
        body_mode,
        params,
        name,
    };
    check_identifiers(&op)?;
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

/// Reject an operation whose name or path cannot be used safely.
fn check_identifiers(op: &CompiledOp) -> Result<(), CompileError> {
    let unsafe_id = |field, reason| CompileError::UnsafeIdentifier {
        operation: op.name.clone(),
        field,
        reason,
    };
    if !is_safe_op_name(&op.name) {
        return Err(unsafe_id("name", SAFE_NAME_RULE));
    }
    if let Err(reason) = check_path(&op.path) {
        return Err(unsafe_id("path", reason));
    }
    Ok(())
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
/// line of `description`, whitespace-collapsed. Empty if neither exists.
fn extract_summary(post: &Value) -> String {
    let raw = post
        .get("summary")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| post.get("description").and_then(Value::as_str))
        .unwrap_or_default();
    let first_line = raw.lines().next().unwrap_or_default();
    first_line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Why an operation name is rejected, as one sentence.
const SAFE_NAME_RULE: &str =
    "an operation name must be a non-empty run of ASCII letters, digits, `.`, `_` or `-`";

/// Whether an operation name is safe to expose as a CLI argument and to
/// look up in the IR table.
///
/// Anything else could carry whitespace, control characters or shell
/// metacharacters into help output and error messages.
pub fn is_safe_op_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_OP_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Longest accepted operation name. Real names are `resource.method`.
pub const MAX_OP_NAME_BYTES: usize = 128;
/// Longest accepted request path.
pub const MAX_PATH_BYTES: usize = 256;

/// Whether a request path is safe to join onto a base URL.
///
/// See [`check_path`] for the rules; this is the boolean form, used to
/// re-check paths that arrive from outside the compiler (a cache file).
pub fn is_safe_path(path: &str) -> bool {
    check_path(path).is_ok()
}

/// Check a request path, returning why it was rejected.
///
/// The rules exist for one reason: the request URL is `base_url + path`,
/// concatenated as text. A path that starts a new authority (`//host`),
/// injects userinfo (`@host`), names a scheme, or walks up with `..` would
/// silently retarget the request - and send the bearer token to whoever
/// owns that host. Only an absolute path of conservative characters is
/// accepted; percent escapes are rejected rather than decoded.
fn check_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() || !path.starts_with('/') {
        return Err("a request path must start with `/`");
    }
    if path.len() > MAX_PATH_BYTES {
        return Err("a request path must be at most 256 bytes");
    }
    let allowed =
        |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/');
    if !path.bytes().all(allowed) {
        return Err(
            "a request path may only contain ASCII letters, digits, `.`, `_`, `-` and `/` \
             (no userinfo, scheme, query, escape or control characters)",
        );
    }
    if path.split('/').skip(1).any(|segment| segment.is_empty()) {
        return Err("a request path must not contain an empty segment (`//`)");
    }
    if path
        .split('/')
        .any(|segment| segment == ".." || segment == ".")
    {
        return Err("a request path must not contain a `.` or `..` segment");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        // Parameter order follows serde_json's object-key order.
        assert_eq!(op.params.len(), 2);
        assert_eq!(op.params[0].name, "count");
        assert_eq!(op.params[0].ty, ScalarKind::Integer);
        assert!(!op.params[0].required);
        assert_eq!(op.params[1].name, "id");
        assert_eq!(op.params[1].ty, ScalarKind::String);
        assert!(op.params[1].required);
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

    #[test]
    fn op_name_checks_reject_metacharacters() {
        for bad in ["", "a b", "a/b", "a;b", "a\u{7f}b", "\u{4e2d}"] {
            assert!(!is_safe_op_name(bad), "{bad:?} must be rejected");
        }
        assert!(is_safe_op_name("documents.info"));
    }
}
