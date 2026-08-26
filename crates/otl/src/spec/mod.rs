//! Spec lifecycle: where a spec comes from, and how a compiled one is
//! cached (Story 4.2).
//!
//! The CLI ships with the vendored spec compiled into the binary by
//! `build.rs`, and that stays the default. `otl spec sync` is the only
//! code path that fetches and parses a spec at run time; it writes the
//! compiled IR to a cache that later commands deserialize instead of
//! re-parsing anything. Startup therefore never touches an OpenAPI
//! document (see `tests/startup_guard.rs`), and the CLI never checks for
//! spec updates on its own (NFR4: no phone home).
//!
//! Everything Outline-specific about specs lives here - the upstream
//! source and the `/api` path convention - never in `engine` or
//! `spec-compile`.

mod bounded;
pub mod cache;
pub(crate) mod openfile;

use std::collections::HashSet;

use engine::ir::{BodyMode, OpSpec, ParamSpec, ParamType};
use spec_compile::{
    is_display_safe, BodyKind, CompiledOp, CompiledParam, CompiledSpec, ScalarKind,
};

/// Upstream source of the OpenAPI document, community-maintained.
///
/// This is the same document this crate vendors and compiles in at build
/// time; its provenance is recorded in the VENDOR note next to the
/// vendored copy. Fetched only when the user runs `otl spec sync` - never
/// on any other code path (NFR4).
pub const UPSTREAM_SPEC_URL: &str =
    "https://raw.githubusercontent.com/outline/openapi/main/spec3.json";

/// Outline URL convention: every RPC path lives under this prefix.
///
/// `build.rs` carries its own copy (a build script cannot import from the
/// crate it builds); `tests/spec_parity.rs` asserts the two agree by
/// compiling the vendored spec both ways.
pub const API_PATH_PREFIX: &str = "/api";

/// Compile options for an Outline spec.
pub fn compile_options() -> spec_compile::CompileOptions {
    spec_compile::CompileOptions::with_prefix(API_PATH_PREFIX)
}

/// Convert a compiled spec into engine IR.
///
/// The two type families are mirrors (see the `spec-compile` crate docs
/// for why it cannot depend on `engine`); this is the single place that
/// maps one to the other, and both matches below are exhaustive so a new
/// variant fails to compile rather than being silently defaulted.
pub fn to_ir(compiled: &CompiledSpec) -> Vec<OpSpec> {
    compiled.ops.iter().map(op_to_ir).collect()
}

fn op_to_ir(op: &CompiledOp) -> OpSpec {
    OpSpec {
        name: op.name.clone().into(),
        path: op.path.clone().into(),
        summary: op.summary.clone().into(),
        content_type: op.content_type.clone().into(),
        body_mode: match op.body_mode {
            BodyKind::KeyValue => BodyMode::KeyValue,
            BodyKind::RawJsonOnly => BodyMode::RawJsonOnly,
            BodyKind::Unsupported => BodyMode::Unsupported,
        },
        params: op.params.iter().map(param_to_ir).collect::<Vec<_>>().into(),
    }
}

fn param_to_ir(param: &CompiledParam) -> ParamSpec {
    ParamSpec {
        name: param.name.clone().into(),
        ty: match param.ty {
            ScalarKind::String => ParamType::String,
            ScalarKind::Integer => ParamType::Integer,
            ScalarKind::Boolean => ParamType::Boolean,
            ScalarKind::Number => ParamType::Number,
            ScalarKind::Json => ParamType::Json,
        },
        required: param.required,
        nullable: param.nullable,
        enum_values: param
            .enum_values
            .iter()
            .map(|value| value.clone().into())
            .collect::<Vec<_>>()
            .into(),
        format: param.format.clone().into(),
        minimum: param.minimum,
        maximum: param.maximum,
    }
}

/// Re-check operations that arrive from outside the compiler.
///
/// A cache file is a separate trust boundary from the document it was
/// compiled from: it can be truncated, bit-flipped, left by another
/// version, or written by something else entirely. Everything the compiler
/// guarantees is therefore re-established here, on the way in.
///
/// Three classes of rule, each for a different attack:
///
/// 1. **Names and paths** - the request URL is `base_url + op.path`
///    concatenated as text, so a path that starts a new authority would
///    send the bearer token to another host.
/// 2. **The name/path binding** - a path may be well formed and still be
///    the WRONG one. Nothing above stops a cache from mapping
///    `documents.search` to `/api/documents.delete`: same origin, same
///    token, destructive endpoint. The compiler derives one from the other
///    (`path == "/api/" + name` by construction), so that invariant is
///    asserted rather than assumed.
/// 3. **Text** - summaries, content types, parameter names, formats and
///    enum values are all printed to a terminal, which executes some byte
///    sequences instead of showing them.
pub fn validate_ops(ops: &[OpSpec]) -> Result<(), String> {
    if ops.is_empty() {
        return Err("it contains no operations".to_string());
    }
    // The resource ceilings (how many operations, how much memory they
    // come to) live with the framing that enforces them, in
    // `bounded::check_table`, so that one place owns them and one message
    // explains them. This function owns the SAFETY rules below.
    let mut seen: HashSet<&str> = HashSet::with_capacity(ops.len());
    for op in ops {
        check_identity(op)?;
        check_text(op)?;
        if !seen.insert(op.name.as_ref()) {
            return Err(format!("operation {:?} appears twice", op.name));
        }
    }
    Ok(())
}

/// Rules 1 and 2: the name is usable, the path is a plain absolute path,
/// and the two agree.
fn check_identity(op: &OpSpec) -> Result<(), String> {
    if !spec_compile::is_safe_op_name(&op.name) {
        return Err("it contains an operation with an unusable name".to_string());
    }
    if !spec_compile::is_safe_path(&op.path) {
        return Err(format!(
            "operation {:?} has a request path that is not a plain absolute path",
            op.name
        ));
    }
    // The invariant every compiled table satisfies by construction. An
    // operation whose path names a DIFFERENT operation is a redirection of
    // the caller's credentials, even though both fields look harmless on
    // their own.
    let expected = format!("{API_PATH_PREFIX}/{}", op.name);
    if op.path != expected {
        return Err(format!(
            "operation {:?} does not dispatch to its own endpoint (it points \
             somewhere else, which would send credentials to an operation the \
             user did not ask for)",
            op.name
        ));
    }
    Ok(())
}

/// Rule 3: nothing that reaches a terminal may carry control characters,
/// and no field may be implausibly long.
fn check_text(op: &OpSpec) -> Result<(), String> {
    let unsafe_text = |field: &str| {
        Err(format!(
            "operation {:?} has a {field} that is too long or contains control \
             characters (the value is not shown: printing it is the attack)",
            op.name
        ))
    };
    if !is_display_safe(&op.summary, spec_compile::MAX_SUMMARY_BYTES) {
        return unsafe_text("summary");
    }
    if !is_display_safe(&op.content_type, spec_compile::MAX_CONTENT_TYPE_BYTES) {
        return unsafe_text("content type");
    }
    for param in op.params.iter() {
        if param.name.is_empty()
            || !is_display_safe(&param.name, spec_compile::MAX_PARAM_NAME_BYTES)
        {
            return unsafe_text("parameter name");
        }
        if !is_display_safe(&param.format, spec_compile::MAX_FORMAT_BYTES) {
            return unsafe_text("parameter format");
        }
        if param.enum_values.len() > spec_compile::MAX_ENUM_VALUES {
            return unsafe_text("parameter enum");
        }
        if !param
            .enum_values
            .iter()
            .all(|value| is_display_safe(value, spec_compile::MAX_ENUM_VALUE_BYTES))
        {
            return unsafe_text("parameter enum value");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn ops(name: &str, path: &str) -> Vec<OpSpec> {
        vec![OpSpec {
            name: name.to_string().into(),
            path: path.to_string().into(),
            summary: String::new().into(),
            content_type: String::new().into(),
            body_mode: BodyMode::KeyValue,
            params: Vec::new().into(),
        }]
    }

    #[test]
    fn converts_a_compiled_spec_to_ir() {
        let raw = r#"{"paths":{"/things.info":{"post":{"summary":"S",
            "requestBody":{"content":{"application/json":{"schema":{
                "type":"object","required":["id"],
                "properties":{"id":{"type":"string","format":"uuid"}}}}}}}}}}"#;
        let compiled = spec_compile::compile_json(raw, &compile_options()).expect("compiles");
        let ir = to_ir(&compiled);
        assert_eq!(ir.len(), 1);
        assert_eq!(ir[0].name, "things.info");
        assert_eq!(ir[0].path, "/api/things.info");
        assert_eq!(ir[0].body_mode, BodyMode::KeyValue);
        let param = ir[0].param("id").expect("id param");
        assert_eq!(param.ty, ParamType::String);
        assert!(param.required);
        assert_eq!(param.format, "uuid");
        assert!(validate_ops(&ir).is_ok());
    }

    #[test]
    fn rejects_an_empty_table() {
        assert!(validate_ops(&[]).is_err());
    }

    #[test]
    fn rejects_a_path_that_escapes_the_base_url() {
        // The exact shape a tampered cache would use to exfiltrate the
        // bearer token: `https://host` + `@evil.example/x` is a request to
        // evil.example with `host` as userinfo.
        for path in ["@evil.example/x", "//evil.example/x", "/a/../../b", "x"] {
            assert!(
                validate_ops(&ops("things.info", path)).is_err(),
                "{path:?} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_an_unusable_operation_name() {
        assert!(validate_ops(&ops("things info", "/api/things.info")).is_err());
    }

    /// The same-origin remap: both fields are individually well formed, but
    /// the path belongs to a different (destructive) operation. Calling
    /// `documents.search` must never dispatch to `documents.delete`.
    #[test]
    fn rejects_a_path_that_names_another_operation() {
        let table = ops("documents.search", "/api/documents.delete");
        let error = validate_ops(&table).expect_err("must be rejected");
        assert!(
            error.contains("does not dispatch to its own endpoint"),
            "{error}"
        );

        // Also rejected: a prefix that is not the API prefix at all, and a
        // path with extra segments appended.
        for path in [
            "/documents.search",
            "/api/v2/documents.search",
            "/api/documents.search/extra",
            "/api/documents.search.",
        ] {
            assert!(
                validate_ops(&ops("documents.search", path)).is_err(),
                "{path:?} must be rejected"
            );
        }
        // The compiled shape is of course accepted.
        assert!(validate_ops(&ops("documents.search", "/api/documents.search")).is_ok());
    }

    #[test]
    fn rejects_a_duplicated_operation() {
        let mut table = ops("things.info", "/api/things.info");
        table.push(table[0].clone());
        assert!(validate_ops(&table).is_err());
    }

    /// A cached summary or identifier carrying terminal escapes must not be
    /// printed: `api list` writes summaries verbatim.
    #[test]
    fn rejects_text_that_a_terminal_would_execute() {
        let with_summary = |summary: &str| {
            vec![OpSpec {
                summary: summary.to_string().into(),
                ..ops("things.info", "/api/things.info")[0].clone()
            }]
        };
        for hostile in [
            "\u{1b}]52;c;cGF3bmVk\u{7}",
            "two\nlines",
            "a\tb",
            "bidi\u{202e}flip",
        ] {
            assert!(
                validate_ops(&with_summary(hostile)).is_err(),
                "{hostile:?} must be rejected"
            );
        }
        assert!(validate_ops(&with_summary("Retrieve a document")).is_ok());
        assert!(validate_ops(&with_summary(&"x".repeat(5000))).is_err());

        let with_param = |name: &str, format: &str, enums: Vec<&str>| {
            vec![OpSpec {
                params: vec![ParamSpec {
                    name: name.to_string().into(),
                    ty: ParamType::String,
                    required: false,
                    nullable: false,
                    enum_values: enums
                        .into_iter()
                        .map(|value| value.to_string().into())
                        .collect::<Vec<_>>()
                        .into(),
                    format: format.to_string().into(),
                    minimum: None,
                    maximum: None,
                }]
                .into(),
                ..ops("things.info", "/api/things.info")[0].clone()
            }]
        };
        assert!(validate_ops(&with_param("id", "uuid", vec!["a", "b"])).is_ok());
        assert!(validate_ops(&with_param("id\u{1b}[31m", "", vec![])).is_err());
        assert!(validate_ops(&with_param("", "", vec![])).is_err());
        assert!(validate_ops(&with_param("id", "uu\nid", vec![])).is_err());
        assert!(validate_ops(&with_param("id", "", vec!["ok", "bad\u{7}"])).is_err());
    }

    #[test]
    fn accepts_a_hostile_content_type_only_when_printable() {
        let with_type = |content_type: &str| {
            vec![OpSpec {
                content_type: content_type.to_string().into(),
                ..ops("things.info", "/api/things.info")[0].clone()
            }]
        };
        assert!(validate_ops(&with_type("multipart/form-data")).is_ok());
        assert!(validate_ops(&with_type("text/x\u{1b}[31m")).is_err());
    }

    /// The whole built-in table must satisfy the rules a cache is held to;
    /// otherwise the loader would reject a table the binary itself ships.
    #[test]
    fn the_built_in_table_satisfies_every_rule() {
        validate_ops(crate::ops::OPS).expect("built-in table is valid");
    }
}
