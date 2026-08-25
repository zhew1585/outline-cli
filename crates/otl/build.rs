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
const IR_SCHEMA_VERSION: u32 = 3;
/// JSON pointer prefix for local component-schema references.
const COMPONENTS_SCHEMAS_REF: &str = "#/components/schemas/";
/// The only request content type the generic engine can assemble.
const JSON_CONTENT_TYPE: &str = "application/json";
/// Maximum `$ref`/`allOf` expansion depth (guards against reference cycles).
const MAX_SCHEMA_DEPTH: usize = 8;

struct Op {
    name: String,
    path: String,
    summary: String,
    content_type: String,
    /// `engine::ir::BodyMode` variant name.
    body_mode: &'static str,
    params: Vec<Param>,
}

struct Param {
    name: String,
    /// `engine::ir::ParamType` variant name.
    ty: &'static str,
    required: bool,
    facets: Facets,
}

/// Schema constraint facets carried into the IR for local validation.
#[derive(Default)]
struct Facets {
    nullable: bool,
    enum_values: Vec<String>,
    minimum: Option<f64>,
    maximum: Option<f64>,
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
        .filter_map(|(path, item)| item.get("post").map(|post| compile_op(path, post, spec)))
        .collect();
    ops.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(pair) = ops.windows(2).find(|pair| pair[0].name == pair[1].name) {
        panic!("duplicate operation name in spec: {}", pair[0].name);
    }
    ops
}

/// Compile one POST operation into an IR entry.
///
/// Operations whose request body is not `application/json` are recorded
/// with their content type and marked unsupported: the generic engine
/// cannot assemble such a body, so they must fail at dispatch instead of
/// silently sending an empty JSON object.
fn compile_op(path: &str, post: &Value, spec: &Value) -> Op {
    let (content_type, schema) = request_body(post);
    let (params, root_union) = match schema {
        Some(schema) => extract_params(schema, spec),
        None => (Vec::new(), false),
    };
    let body_mode = if content_type.is_empty() || content_type == JSON_CONTENT_TYPE {
        if root_union {
            "RawJsonOnly"
        } else {
            "KeyValue"
        }
    } else {
        "Unsupported"
    };
    Op {
        name: path.trim_start_matches('/').to_string(),
        path: format!("{API_PATH_PREFIX}{path}"),
        summary: extract_summary(post),
        content_type,
        body_mode,
        params,
    }
}

/// The operation's request content type and its JSON schema.
///
/// Returns an empty content type when the operation takes no request body,
/// and no schema when the body is not `application/json`.
fn request_body(post: &Value) -> (String, Option<&Value>) {
    let Some(content) = post
        .pointer("/requestBody/content")
        .and_then(Value::as_object)
    else {
        return (String::new(), None);
    };
    if let Some(entry) = content.get(JSON_CONTENT_TYPE) {
        return (JSON_CONTENT_TYPE.to_string(), entry.get("schema"));
    }
    let content_type = match content.keys().next() {
        Some(first) => first.clone(),
        None => panic!("operation declares a requestBody with no content types"),
    };
    (content_type, None)
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

/// Extract request-body parameters from one JSON body schema.
///
/// `allOf` compositions and local `$ref`s into `components.schemas` are
/// expanded. Only top-level scalar properties are typed; everything else
/// (objects, arrays, unions) is marked `Json`.
///
/// The second return value reports a root-level `oneOf`/`anyOf`: such a
/// body cannot be assembled from flat `key=value` pairs, so the operation
/// becomes raw-body-only.
fn extract_params(schema: &Value, spec: &Value) -> (Vec<Param>, bool) {
    let mut params: Vec<Param> = Vec::new();
    let mut required: Vec<String> = Vec::new();
    let mut root_union = false;
    collect_schema(schema, spec, 0, &mut params, &mut required, &mut root_union);
    let params = params
        .into_iter()
        .map(|param| Param {
            required: required.iter().any(|name| name == &param.name),
            ..param
        })
        .collect();
    (params, root_union)
}

/// Recursively collect `properties` and `required` from a schema,
/// expanding `allOf` branches and local `$ref`s. Later duplicate property
/// names are ignored (first definition wins). A `oneOf`/`anyOf` anywhere
/// on the root chain sets `root_union`.
fn collect_schema(
    schema: &Value,
    spec: &Value,
    depth: usize,
    params: &mut Vec<Param>,
    required: &mut Vec<String>,
    root_union: &mut bool,
) {
    if depth > MAX_SCHEMA_DEPTH {
        panic!("schema $ref/allOf nesting exceeds {MAX_SCHEMA_DEPTH} levels (cycle?)");
    }
    if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() {
        *root_union = true;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let resolved = resolve_ref(reference, spec);
        collect_schema(resolved, spec, depth + 1, params, required, root_union);
        return;
    }
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            collect_schema(branch, spec, depth + 1, params, required, root_union);
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
                    facets: extract_facets(prop, spec),
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

/// Collect the constraint facets of one property, following `$ref` and
/// `allOf` wrappers (a nullable flag or enum is often one level down).
///
/// `oneOf`/`anyOf` branches are deliberately not followed: their
/// constraints are alternatives, not requirements.
///
/// TODO: `format` and `pattern` are not compiled yet (see `ParamSpec`).
fn extract_facets(prop: &Value, spec: &Value) -> Facets {
    let mut facets = Facets::default();
    collect_facets(prop, spec, 0, &mut facets);
    facets
}

fn collect_facets(schema: &Value, spec: &Value, depth: usize, facets: &mut Facets) {
    if depth > MAX_SCHEMA_DEPTH {
        panic!("schema $ref/allOf nesting exceeds {MAX_SCHEMA_DEPTH} levels (cycle?)");
    }
    if schema.get("nullable").and_then(Value::as_bool) == Some(true) {
        facets.nullable = true;
    }
    if facets.enum_values.is_empty() {
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            facets.enum_values = values.iter().map(enum_literal).collect();
        }
    }
    facets.minimum = facets
        .minimum
        .or_else(|| schema.get("minimum").and_then(Value::as_f64));
    facets.maximum = facets
        .maximum
        .or_else(|| schema.get("maximum").and_then(Value::as_f64));
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        collect_facets(resolve_ref(reference, spec), spec, depth + 1, facets);
    }
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            collect_facets(branch, spec, depth + 1, facets);
        }
    }
}

/// Render one enum entry as the text a `key=value` argument must match.
fn enum_literal(value: &Value) -> String {
    match value.as_str() {
        Some(text) => text.to_string(),
        None => value.to_string(),
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
        render_op(&mut out, op);
    }
    let _ = writeln!(out, "];");
    out
}

fn render_op(out: &mut String, op: &Op) {
    let _ = writeln!(out, "    engine::ir::OpSpec {{");
    for (field, value) in [
        ("name", &op.name),
        ("path", &op.path),
        ("summary", &op.summary),
        ("content_type", &op.content_type),
    ] {
        let _ = writeln!(
            out,
            "        {field}: ::std::borrow::Cow::Borrowed({value:?}),"
        );
    }
    let _ = writeln!(
        out,
        "        body_mode: engine::ir::BodyMode::{},",
        op.body_mode
    );
    let _ = writeln!(out, "        params: ::std::borrow::Cow::Borrowed(&[");
    for param in &op.params {
        render_param(out, param);
    }
    let _ = writeln!(out, "        ]),");
    let _ = writeln!(out, "    }},");
}

fn render_param(out: &mut String, param: &Param) {
    let enum_values: String = param
        .facets
        .enum_values
        .iter()
        .map(|value| format!("::std::borrow::Cow::Borrowed({value:?}), "))
        .collect();
    let _ = writeln!(out, "            engine::ir::ParamSpec {{");
    let _ = writeln!(
        out,
        "                name: ::std::borrow::Cow::Borrowed({:?}),",
        param.name
    );
    let _ = writeln!(
        out,
        "                ty: engine::ir::ParamType::{}, required: {},",
        param.ty, param.required
    );
    let _ = writeln!(
        out,
        "                nullable: {}, enum_values: ::std::borrow::Cow::Borrowed(&[{enum_values}]),",
        param.facets.nullable
    );
    let _ = writeln!(
        out,
        "                minimum: {}, maximum: {},",
        render_bound(param.facets.minimum),
        render_bound(param.facets.maximum)
    );
    let _ = writeln!(out, "            }},");
}

/// Render an optional numeric bound as a Rust literal.
fn render_bound(bound: Option<f64>) -> String {
    match bound {
        Some(value) if value.is_finite() => format!("::core::option::Option::Some({value:?}_f64)"),
        _ => "::core::option::Option::None".to_string(),
    }
}
