//! JSON-schema walking: request-body parameters and their facets.
//!
//! Only top-level scalar properties are typed; everything else (objects,
//! arrays, unions) becomes [`ScalarKind::Json`] and is reachable through a
//! raw body only. `allOf` compositions and local `$ref`s into
//! `components.schemas` are expanded, depth-bounded so that a reference
//! cycle in an untrusted document is a typed error rather than a hang.

use std::collections::HashSet;

use serde_json::Value;

use crate::{CompileError, CompiledParam, ScalarKind};

/// JSON pointer prefix for local component-schema references.
const COMPONENTS_SCHEMAS_REF: &str = "#/components/schemas/";
/// JSON pointer to the component-schema map itself (no name appended, so
/// no escaping question arises for this part).
const COMPONENT_SCHEMAS_POINTER: &str = "/components/schemas";
/// Maximum `$ref`/`allOf` expansion depth (guards reference cycles).
pub const MAX_SCHEMA_DEPTH: usize = 8;

/// Extract request-body parameters from one JSON body schema.
///
/// The second return value reports a root-level `oneOf`/`anyOf`: such a
/// body cannot be assembled from flat `key=value` pairs.
pub(crate) fn extract_params(
    schema: &Value,
    document: &Value,
) -> Result<(Vec<CompiledParam>, bool), CompileError> {
    let mut walk = Walk {
        document,
        params: Vec::new(),
        seen: HashSet::new(),
        required: HashSet::new(),
        root_union: false,
    };
    walk.collect(schema, 0)?;
    let Walk {
        params,
        required,
        root_union,
        ..
    } = walk;
    let params = params
        .into_iter()
        .map(|param| CompiledParam {
            required: required.contains(&param.name),
            ..param
        })
        .collect();
    Ok((params, root_union))
}

/// Accumulator for one body-schema walk.
///
/// `seen` and `required` are hash sets, not lists, on purpose: an
/// untrusted document may declare tens of thousands of properties in one
/// schema, and the linear scans this replaces made compilation quadratic
/// in that count - a small download could pin a core for minutes.
/// `params` stays a `Vec` so the output keeps the parser's key order.
struct Walk<'a> {
    document: &'a Value,
    params: Vec<CompiledParam>,
    seen: HashSet<String>,
    required: HashSet<String>,
    root_union: bool,
}

impl Walk<'_> {
    /// Collect `properties` and `required` from a schema, expanding
    /// `allOf` branches and local `$ref`s. The first definition of a
    /// property name wins. A `oneOf`/`anyOf` anywhere on the root chain
    /// sets `root_union`.
    fn collect(&mut self, schema: &Value, depth: usize) -> Result<(), CompileError> {
        check_depth(depth)?;
        if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() {
            self.root_union = true;
        }
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            let resolved = resolve_ref(reference, self.document)?;
            return self.collect(resolved, depth + 1);
        }
        if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
            for branch in branches {
                self.collect(branch, depth + 1)?;
            }
        }
        if let Some(list) = schema.get("required").and_then(Value::as_array) {
            self.required
                .extend(list.iter().filter_map(Value::as_str).map(str::to_string));
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, prop) in properties {
                if !self.seen.insert(name.clone()) {
                    continue;
                }
                let param = self.compile_param(name, prop, depth)?;
                self.params.push(param);
            }
        }
        Ok(())
    }

    /// Compile one property into a parameter entry.
    fn compile_param(
        &self,
        name: &str,
        prop: &Value,
        depth: usize,
    ) -> Result<CompiledParam, CompileError> {
        let facets = self.facets(prop)?;
        Ok(CompiledParam {
            name: name.to_string(),
            ty: self.param_type(prop, depth)?,
            required: false,
            nullable: facets.nullable,
            enum_values: facets.enum_values,
            format: facets.format,
            minimum: facets.minimum,
            maximum: facets.maximum,
        })
    }

    /// Map one property schema to its wire type.
    ///
    /// `oneOf`/`anyOf` unions are always `Json`. A `$ref` or `allOf`
    /// wrapper around a scalar (the common enum idiom) resolves to that
    /// scalar.
    fn param_type(&self, prop: &Value, depth: usize) -> Result<ScalarKind, CompileError> {
        check_depth(depth)?;
        if prop.get("oneOf").is_some() || prop.get("anyOf").is_some() {
            return Ok(ScalarKind::Json);
        }
        if let Some(reference) = prop.get("$ref").and_then(Value::as_str) {
            let resolved = resolve_ref(reference, self.document)?;
            return self.param_type(resolved, depth + 1);
        }
        if let Some(branches) = prop.get("allOf").and_then(Value::as_array) {
            // A scalar wrapped in allOf stays a scalar; mixed or objecty
            // compositions fall through to Json.
            let mut kind = ScalarKind::Json;
            for branch in branches {
                let branch_kind = self.param_type(branch, depth + 1)?;
                if branch_kind != ScalarKind::Json {
                    kind = branch_kind;
                    break;
                }
            }
            return Ok(kind);
        }
        Ok(match prop.get("type").and_then(Value::as_str) {
            Some("string") => ScalarKind::String,
            Some("integer") => ScalarKind::Integer,
            Some("boolean") => ScalarKind::Boolean,
            Some("number") => ScalarKind::Number,
            _ => ScalarKind::Json,
        })
    }

    /// Collect the constraint facets of one property, following `$ref` and
    /// `allOf` wrappers (a nullable flag or enum is often one level down).
    ///
    /// `oneOf`/`anyOf` branches are deliberately not followed: their
    /// constraints are alternatives, not requirements.
    fn facets(&self, prop: &Value) -> Result<Facets, CompileError> {
        let mut facets = Facets::default();
        self.collect_facets(prop, 0, &mut facets)?;
        Ok(facets)
    }

    fn collect_facets(
        &self,
        schema: &Value,
        depth: usize,
        facets: &mut Facets,
    ) -> Result<(), CompileError> {
        check_depth(depth)?;
        merge_facets(schema, facets);
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            let resolved = resolve_ref(reference, self.document)?;
            self.collect_facets(resolved, depth + 1, facets)?;
        }
        if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
            for branch in branches {
                self.collect_facets(branch, depth + 1, facets)?;
            }
        }
        Ok(())
    }
}

/// Merge the facets declared directly on one schema node. First
/// declaration wins for enum and format; bounds and nullable accumulate.
fn merge_facets(schema: &Value, facets: &mut Facets) {
    if schema.get("nullable").and_then(Value::as_bool) == Some(true) {
        facets.nullable = true;
    }
    if facets.enum_values.is_empty() {
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            facets.enum_values = values.iter().map(enum_literal).collect();
        }
    }
    if facets.format.is_empty() {
        if let Some(format) = schema.get("format").and_then(Value::as_str) {
            facets.format = format.to_string();
        }
    }
    facets.minimum = facets
        .minimum
        .or_else(|| schema.get("minimum").and_then(Value::as_f64));
    facets.maximum = facets
        .maximum
        .or_else(|| schema.get("maximum").and_then(Value::as_f64));
}

/// Schema constraint facets carried into the IR for local validation.
///
/// `pattern` is deliberately not compiled: validating it needs a regex
/// engine, roughly a megabyte of binary, for the two `pattern` constraints
/// in the whole vendored document.
#[derive(Default)]
struct Facets {
    nullable: bool,
    enum_values: Vec<String>,
    format: String,
    minimum: Option<f64>,
    maximum: Option<f64>,
}

fn check_depth(depth: usize) -> Result<(), CompileError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(CompileError::DepthExceeded);
    }
    Ok(())
}

/// Resolve a local `#/components/schemas/...` reference.
///
/// The text after the prefix is one JSON Pointer *token*, so it arrives
/// already escaped per RFC 6901: `~1` stands for `/` and `~0` for `~`. It
/// therefore has to be UNescaped to recover the schema name - escaping it
/// again (or indexing with it raw) fails to find any schema whose name
/// contains either character. Unescaping is done by hand and the map is
/// indexed directly, which also keeps `/` inside a name from being read as
/// pointer structure.
fn resolve_ref<'a>(reference: &str, document: &'a Value) -> Result<&'a Value, CompileError> {
    let token = reference
        .strip_prefix(COMPONENTS_SCHEMAS_REF)
        .ok_or_else(|| CompileError::UnsupportedRef {
            reference: reference.to_string(),
        })?;
    // Order matters (RFC 6901): `~1` first, then `~0`, so that the literal
    // sequence `~01` decodes to `~1` rather than to `/`.
    let name = token.replace("~1", "/").replace("~0", "~");
    document
        .pointer(COMPONENT_SCHEMAS_POINTER)
        .and_then(|schemas| schemas.get(&name))
        .ok_or_else(|| CompileError::UnresolvedRef {
            reference: reference.to_string(),
        })
}

/// Render one enum entry as the text a `key=value` argument must match.
fn enum_literal(value: &Value) -> String {
    match value.as_str() {
        Some(text) => text.to_string(),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::{compile_json, CompileOptions};

    fn compile(paths: &str, components: &str) -> Result<crate::CompiledSpec, CompileError> {
        let raw = format!(r##"{{"paths":{paths},"components":{{"schemas":{components}}}}}"##);
        compile_json(&raw, &CompileOptions::with_prefix("/api"))
    }

    fn body(schema: &str) -> String {
        format!(
            r##"{{"/things.info":{{"post":{{"requestBody":{{"content":{{
                "application/json":{{"schema":{schema}}}}}}}}}}}}}"##
        )
    }

    #[test]
    fn expands_refs_and_all_of() {
        let compiled = compile(
            &body(
                r##"{"allOf":[{"$ref":"#/components/schemas/Base"},
                {"type":"object","properties":{"extra":{"type":"boolean"}}}]}"##,
            ),
            r##"{"Base":{"type":"object","required":["id"],
                "properties":{"id":{"type":"string"}}}}"##,
        )
        .expect("compiles");
        let params = &compiled.ops[0].params;
        assert_eq!(params.len(), 2);
        assert!(params.iter().any(|p| p.name == "id" && p.required));
        assert!(params
            .iter()
            .any(|p| p.name == "extra" && p.ty == ScalarKind::Boolean));
    }

    #[test]
    fn a_reference_cycle_is_an_error_not_a_hang() {
        let error = compile(
            &body(r##"{"$ref":"#/components/schemas/Loop"}"##),
            r##"{"Loop":{"allOf":[{"$ref":"#/components/schemas/Loop"}]}}"##,
        )
        .expect_err("cycle must be rejected");
        assert_eq!(error, CompileError::DepthExceeded);
    }

    #[test]
    fn an_unresolved_ref_is_an_error() {
        let error = compile(&body(r##"{"$ref":"#/components/schemas/Missing"}"##), "{}")
            .expect_err("must be rejected");
        assert!(matches!(error, CompileError::UnresolvedRef { .. }));
    }

    #[test]
    fn an_external_ref_is_rejected() {
        let error = compile(&body(r##"{"$ref":"https://evil.example/s.json"}"##), "{}")
            .expect_err("must be rejected");
        assert!(matches!(error, CompileError::UnsupportedRef { .. }));
    }

    #[test]
    fn collects_facets_through_a_ref() {
        let compiled = compile(
            &body(
                r##"{"type":"object","properties":{
                    "mode":{"allOf":[{"$ref":"#/components/schemas/Mode"}]},
                    "size":{"type":"integer","minimum":1,"maximum":100},
                    "note":{"type":"string","nullable":true,"format":"uuid"}}}"##,
            ),
            r##"{"Mode":{"type":"string","enum":["read","write"]}}"##,
        )
        .expect("compiles");
        let params = &compiled.ops[0].params;
        let mode = params.iter().find(|p| p.name == "mode").expect("mode");
        assert_eq!(mode.ty, ScalarKind::String);
        assert_eq!(mode.enum_values, ["read", "write"]);
        let size = params.iter().find(|p| p.name == "size").expect("size");
        assert_eq!((size.minimum, size.maximum), (Some(1.0), Some(100.0)));
        let note = params.iter().find(|p| p.name == "note").expect("note");
        assert!(note.nullable);
        assert_eq!(note.format, "uuid");
    }

    /// A schema name containing `/` or `~` is referenced with the RFC 6901
    /// escapes `~1` / `~0`; the compiler has to decode them, not re-encode
    /// them.
    #[test]
    fn resolves_refs_whose_schema_name_needs_pointer_escaping() {
        for (name, escaped) in [
            ("A/B", "A~1B"),
            ("A~B", "A~0B"),
            ("A~1B", "A~01B"),
            ("a/b/c", "a~1b~1c"),
        ] {
            let compiled = compile(
                &body(&format!(r##"{{"$ref":"#/components/schemas/{escaped}"}}"##)),
                &format!(
                    r#"{{"{name}":{{"type":"object","required":["id"],
                        "properties":{{"id":{{"type":"string"}}}}}}}}"#
                ),
            )
            .unwrap_or_else(|error| panic!("{escaped} must resolve to {name:?}: {error}"));
            let params = &compiled.ops[0].params;
            assert_eq!(params.len(), 1, "{escaped}");
            assert!(params[0].required, "{escaped}");
        }
    }

    /// A wide schema must compile in linear time. Before the sets below
    /// were sets, this was two quadratic scans: 64k properties took seconds
    /// of pure CPU on a release build, and the input for that is a few
    /// megabytes of JSON - well inside the download limit.
    #[test]
    fn a_very_wide_schema_compiles_in_linear_time() {
        const WIDTH: usize = 20_000;
        let properties: String = (0..WIDTH)
            .map(|index| format!(r#""p{index}":{{"type":"string"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let required: String = (0..WIDTH)
            .map(|index| format!(r#""p{index}""#))
            .collect::<Vec<_>>()
            .join(",");
        let schema =
            format!(r#"{{"type":"object","required":[{required}],"properties":{{{properties}}}}}"#);

        let started = std::time::Instant::now();
        let compiled = compile(&body(&schema), "{}").expect("compiles");
        let elapsed = started.elapsed();

        assert_eq!(compiled.ops[0].params.len(), WIDTH);
        assert!(compiled.ops[0].params.iter().all(|param| param.required));
        // Linear work is milliseconds even in a debug build; the quadratic
        // version needed seconds at this width. A wide margin keeps this
        // from being load-flaky while still catching a regression.
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "compiling {WIDTH} properties took {elapsed:?}: quadratic again?"
        );
    }

    #[test]
    fn complex_properties_become_json() {
        let compiled = compile(
            &body(
                r##"{"type":"object","properties":{
                    "items":{"type":"array","items":{"type":"string"}},
                    "either":{"oneOf":[{"type":"string"},{"type":"integer"}]}}}"##,
            ),
            "{}",
        )
        .expect("compiles");
        for param in &compiled.ops[0].params {
            assert_eq!(param.ty, ScalarKind::Json, "{}", param.name);
        }
        // A union on a PROPERTY does not make the whole body raw-only.
        assert_eq!(compiled.ops[0].body_mode, crate::BodyKind::KeyValue);
    }
}
