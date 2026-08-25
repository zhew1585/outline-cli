//! Compile the vendored OpenAPI spec into a static IR data table.
//!
//! This build script parses `spec/spec3.json`, compiles every operation in
//! it, and code-generates a `static OPS` table of `engine::ir::OpSpec`
//! values into `$OUT_DIR/ir_table.rs`. It produces a data table only -
//! never per-endpoint functions.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Outline URL convention: every RPC path lives under this prefix. This is
/// an Outline-specific rule, so it is applied here (the otl layer), never
/// in the engine - the engine joins `base_url + op.path` verbatim.
const API_PATH_PREFIX: &str = "/api";
/// Must match `engine::ir::IR_SCHEMA_VERSION`; asserted in generated code.
const IR_SCHEMA_VERSION: u32 = 2;
/// JSON pointer prefix for local component-schema references.
const COMPONENTS_SCHEMAS_REF: &str = "#/components/schemas/";

struct Op {
    name: String,
    path: String,
    summary: String,
    params: Vec<Param>,
}

struct Param {
    name: String,
    ty: &'static str,
    required: bool,
}

fn main() {
    println!("cargo:rerun-if-changed=spec/spec3.json");

    let manifest_dir = env_var("CARGO_MANIFEST_DIR");
    let spec_path = Path::new(&manifest_dir).join("spec/spec3.json");
    let spec = load_spec(&spec_path);
    let ops = extract_ops(&spec);
    if ops.is_empty() {
        panic!("no operations found in {}", spec_path.display());
    }

    let out_path = PathBuf::from(env_var("OUT_DIR")).join("ir_table.rs");
    if let Err(error) = fs::write(&out_path, render_table(&ops)) {
        panic!("failed to write {}: {error}", out_path.display());
    }
}

fn env_var(name: &str) -> String {
    match env::var(name) {
        Ok(value) => value,
        Err(error) => panic!("missing env var {name}: {error}"),
    }
}

fn load_spec(path: &Path) -> Value {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) => panic!("failed to read vendored spec {}: {error}", path.display()),
    };
    match serde_json::from_str(&raw) {
        Ok(spec) => spec,
        Err(error) => panic!(
            "vendored spec {} is not valid JSON: {error}",
            path.display()
        ),
    }
}

/// Collect every POST operation in the spec.
fn extract_ops(spec: &Value) -> Vec<Op> {
    let Some(paths) = spec.get("paths").and_then(Value::as_object) else {
        panic!("spec has no object at `paths`");
    };
    let mut ops: Vec<Op> = paths
        .iter()
        .filter_map(|(path, item)| {
            item.get("post").map(|post| Op {
                name: path.trim_start_matches('/').to_string(),
                path: format!("{API_PATH_PREFIX}{path}"),
                summary: extract_summary(post),
                params: extract_params(post, spec),
            })
        })
        .collect();
    ops.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(pair) = ops.windows(2).find(|pair| pair[0].name == pair[1].name) {
        panic!("duplicate operation name in spec: {}", pair[0].name);
    }
    ops
}

/// One-line summary: the spec `summary`, falling back to the first line of
/// `description`, whitespace-collapsed. Empty if the spec provides neither.
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

/// Extract scalar request-body parameters from one POST operation.
///
/// `allOf` compositions and local `$ref`s into `components.schemas` are
/// expanded. Only top-level scalar properties are typed; everything else
/// (objects, arrays, unions) is marked `Json`. Non-JSON bodies yield no
/// params.
fn extract_params(post: &Value, spec: &Value) -> Vec<Param> {
    let schema = post
        .pointer("/requestBody/content/application~1json/schema")
        .unwrap_or(&Value::Null);
    let mut params: Vec<Param> = Vec::new();
    let mut required: Vec<String> = Vec::new();
    collect_schema(schema, spec, 0, &mut params, &mut required);
    params
        .into_iter()
        .map(|param| Param {
            required: required.iter().any(|name| name == &param.name),
            ..param
        })
        .collect()
}

/// Maximum `$ref`/`allOf` expansion depth (guards against reference cycles).
const MAX_SCHEMA_DEPTH: usize = 8;

/// Recursively collect `properties` and `required` from a schema,
/// expanding `allOf` branches and local `$ref`s. Later duplicate property
/// names are ignored (first definition wins).
fn collect_schema(
    schema: &Value,
    spec: &Value,
    depth: usize,
    params: &mut Vec<Param>,
    required: &mut Vec<String>,
) {
    if depth > MAX_SCHEMA_DEPTH {
        panic!("schema $ref/allOf nesting exceeds {MAX_SCHEMA_DEPTH} levels (cycle?)");
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        collect_schema(
            resolve_ref(reference, spec),
            spec,
            depth + 1,
            params,
            required,
        );
        return;
    }
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            collect_schema(branch, spec, depth + 1, params, required);
        }
    }
    if let Some(list) = schema.get("required").and_then(Value::as_array) {
        required.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, prop) in properties {
            if params.iter().all(|existing| existing.name != *name) {
                params.push(Param {
                    name: name.clone(),
                    ty: param_type(prop, spec, depth),
                    required: false,
                });
            }
        }
    }
}

/// Resolve a local `#/components/schemas/...` reference.
fn resolve_ref<'a>(reference: &str, spec: &'a Value) -> &'a Value {
    let Some(name) = reference.strip_prefix(COMPONENTS_SCHEMAS_REF) else {
        panic!("unsupported $ref {reference:?}: only {COMPONENTS_SCHEMAS_REF}* is handled");
    };
    match spec.pointer(&format!("/components/schemas/{name}")) {
        Some(resolved) => resolved,
        None => panic!("$ref {reference:?} does not resolve in the vendored spec"),
    }
}

/// Map one property schema to a `ParamType` variant name.
///
/// `oneOf`/`anyOf` unions are always `Json` (complex; k=v is rejected at
/// runtime). `$ref`s and `allOf` wrappers around a scalar (a common enum
/// idiom, e.g. `allOf: [$ref: SomeStringEnum]`) resolve to that scalar.
fn param_type(prop: &Value, spec: &Value, depth: usize) -> &'static str {
    if depth > MAX_SCHEMA_DEPTH {
        panic!("schema $ref/allOf nesting exceeds {MAX_SCHEMA_DEPTH} levels (cycle?)");
    }
    if prop.get("oneOf").is_some() || prop.get("anyOf").is_some() {
        return "Json";
    }
    if let Some(reference) = prop.get("$ref").and_then(Value::as_str) {
        return param_type(resolve_ref(reference, spec), spec, depth + 1);
    }
    if let Some(branches) = prop.get("allOf").and_then(Value::as_array) {
        // A scalar wrapped in allOf stays a scalar; mixed/objecty
        // compositions fall through to Json.
        return branches
            .iter()
            .map(|branch| param_type(branch, spec, depth + 1))
            .find(|ty| *ty != "Json")
            .unwrap_or("Json");
    }
    match prop.get("type").and_then(Value::as_str) {
        Some("string") => "String",
        Some("integer") => "Integer",
        Some("boolean") => "Boolean",
        Some("number") => "Number",
        _ => "Json",
    }
}

/// Render the static table as Rust source referencing `engine::ir` types.
fn render_table(ops: &[Op]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// @generated by build.rs from spec/spec3.json - do not edit."
    );
    let _ = writeln!(
        out,
        "const _: () = assert!(engine::ir::IR_SCHEMA_VERSION == {IR_SCHEMA_VERSION});"
    );
    let _ = writeln!(out, "/// Static IR table of all compiled operations.");
    let _ = writeln!(out, "pub static OPS: &[engine::ir::OpSpec] = &[");
    for op in ops {
        let _ = writeln!(out, "    engine::ir::OpSpec {{");
        let _ = writeln!(
            out,
            "        name: ::std::borrow::Cow::Borrowed({:?}),",
            op.name
        );
        let _ = writeln!(
            out,
            "        path: ::std::borrow::Cow::Borrowed({:?}),",
            op.path
        );
        let _ = writeln!(
            out,
            "        summary: ::std::borrow::Cow::Borrowed({:?}),",
            op.summary
        );
        let _ = writeln!(out, "        params: ::std::borrow::Cow::Borrowed(&[");
        for param in &op.params {
            let _ = writeln!(
                out,
                "            engine::ir::ParamSpec {{ name: ::std::borrow::Cow::Borrowed({:?}), \
                 ty: engine::ir::ParamType::{}, required: {} }},",
                param.name, param.ty, param.required
            );
        }
        let _ = writeln!(out, "        ]),");
        let _ = writeln!(out, "    }},");
    }
    let _ = writeln!(out, "];");
    out
}
