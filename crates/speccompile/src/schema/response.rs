//! Response-field walking: the bounded pre-order list `api describe`
//! rebuilds a tree from.
//!
//! Split out of the parameter walk it shares [`Walk`] with, because the two
//! answer different questions. A request object stays FLAT - `key=value`
//! can only address top-level properties - while a response object is
//! walked recursively, so this half is where nesting, the depth limit and
//! recursive models are decided.
//!
//! Nothing recursive reaches the IR: the tree is stored as a flat list in
//! pre-order, each entry carrying its depth. That keeps decoding an
//! untrusted cache non-recursive, and it is why every rule this file
//! maintains (a child is exactly one level deeper than its parent, that
//! parent is a container, and a field whose properties are missing says so)
//! is re-checked by `rules::check_text` before the list is used.

use std::collections::HashSet;

use serde_json::Value;

use super::{check_depth, resolve_ref, Walk, MAX_SCHEMA_DEPTH};
use crate::{CompileError, CompiledField, FieldContainer};

/// JSON pointer to the success response schema of an operation. `~1` is
/// the pointer escape for the `/` in `application/json`.
const SUCCESS_SCHEMA_POINTER: &str = "/responses/200/content/application~1json/schema";

/// Fields of one item of an operation's success payload.
///
/// The envelope (`{"data": ...}`, named by the caller) and the
/// list-vs-object distinction are resolved here; an operation whose
/// document describes no success schema, or whose payload is not an
/// object, yields no fields and leaves the renderer to fall back on the
/// data it receives.
pub(crate) fn extract_response_fields(
    post: &Value,
    components: &Value,
    envelope_property: &str,
) -> Result<Vec<CompiledField>, CompileError> {
    let Some(schema) = post.pointer(SUCCESS_SCHEMA_POINTER) else {
        return Ok(Vec::new());
    };
    let walk = Walk {
        components,
        params: Vec::new(),
        seen: HashSet::new(),
        required: HashSet::new(),
        root_union: false,
    };
    let Some(item) = walk.response_item_schema(schema, envelope_property, 0)? else {
        return Ok(Vec::new());
    };
    let mut fields = Vec::new();
    let mut seen = HashSet::new();
    let mut active_refs = HashSet::new();
    walk.collect_fields(item, 0, 0, &mut fields, &mut seen, &mut active_refs)?;
    Ok(fields)
}

/// Whether a response subtree reached the list in full.
///
/// Only one thing cuts a subtree short: a `$ref` that is already open on the
/// current branch, i.e. a recursive model. The flag travels up to the field
/// that owns the subtree, which records it as
/// [`CompiledField::children_omitted`] - the alternative is a field that
/// looks exactly like an object with no properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Emitted {
    /// Everything the schema declares for this subtree is in the list.
    Whole,
    /// Nothing was emitted here: the schema repeats a model already open.
    Cut,
}

impl Walk<'_> {
    /// Walk from a response schema to the schema of one payload item.
    fn response_item_schema<'a>(
        &'a self,
        schema: &'a Value,
        envelope_property: &str,
        depth: usize,
    ) -> Result<Option<&'a Value>, CompileError> {
        check_depth(depth)?;
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            let resolved = resolve_ref(reference, self.components)?;
            return self.response_item_schema(resolved, envelope_property, depth + 1);
        }
        let payload = if envelope_property.is_empty() {
            Some(schema)
        } else {
            schema.pointer(&format!("/properties/{envelope_property}"))
        };
        match payload {
            Some(payload) => self.unwrap_array(payload, depth).map(Some),
            None => Ok(None),
        }
    }

    /// The item schema of an array, or the schema itself when it is not one.
    fn unwrap_array<'a>(
        &'a self,
        schema: &'a Value,
        depth: usize,
    ) -> Result<&'a Value, CompileError> {
        check_depth(depth)?;
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            let resolved = resolve_ref(reference, self.components)?;
            return self.unwrap_array(resolved, depth + 1);
        }
        Ok(match schema.get("items") {
            Some(items) if schema.get("type").and_then(Value::as_str) == Some("array") => items,
            _ => schema,
        })
    }

    /// Collect response fields in declaration order, expanding `$ref` and
    /// `allOf`. The first definition of a name wins.
    fn collect_fields(
        &self,
        schema: &Value,
        schema_depth: usize,
        field_depth: u8,
        out: &mut Vec<CompiledField>,
        seen: &mut HashSet<String>,
        active_refs: &mut HashSet<String>,
    ) -> Result<Emitted, CompileError> {
        check_depth(schema_depth)?;
        if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() {
            // Alternatives are not one guaranteed response shape. The
            // parent still records `Union`, but inventing shared children
            // here would teach callers paths that may not exist. `Whole`,
            // not `Cut`: a union's children are not omitted, they do not
            // exist as one shape, and `FieldContainer::Union` says so.
            return Ok(Emitted::Whole);
        }
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            return self.collect_ref_fields(
                reference,
                schema_depth,
                field_depth,
                out,
                seen,
                active_refs,
            );
        }
        if schema.get("type").and_then(Value::as_str) == Some("array") {
            return self.collect_item_fields(
                schema,
                schema_depth,
                field_depth,
                out,
                seen,
                active_refs,
            );
        }
        let emitted =
            self.collect_all_of(schema, schema_depth, field_depth, out, seen, active_refs)?;
        self.collect_properties(schema, field_depth, out, seen, active_refs)?;
        Ok(emitted)
    }

    /// Collect every `allOf` branch of one schema.
    ///
    /// One cut branch cuts the composition: `allOf: [$ref Self]` is exactly
    /// how a recursive field is written in a real document, and the
    /// properties the object contributes alongside it are then only part of
    /// what it declares.
    fn collect_all_of(
        &self,
        schema: &Value,
        schema_depth: usize,
        field_depth: u8,
        out: &mut Vec<CompiledField>,
        seen: &mut HashSet<String>,
        active_refs: &mut HashSet<String>,
    ) -> Result<Emitted, CompileError> {
        let Some(branches) = schema.get("allOf").and_then(Value::as_array) else {
            return Ok(Emitted::Whole);
        };
        let mut emitted = Emitted::Whole;
        for branch in branches {
            if self.collect_fields(
                branch,
                schema_depth + 1,
                field_depth,
                out,
                seen,
                active_refs,
            )? == Emitted::Cut
            {
                emitted = Emitted::Cut;
            }
        }
        Ok(emitted)
    }

    /// Collect the fields of one array item. An array without a declared
    /// `items` schema describes no reachable path, so it contributes nothing.
    fn collect_item_fields(
        &self,
        schema: &Value,
        schema_depth: usize,
        field_depth: u8,
        out: &mut Vec<CompiledField>,
        seen: &mut HashSet<String>,
        active_refs: &mut HashSet<String>,
    ) -> Result<Emitted, CompileError> {
        match schema.get("items") {
            Some(items) => {
                self.collect_fields(items, schema_depth + 1, field_depth, out, seen, active_refs)
            }
            None => Ok(Emitted::Whole),
        }
    }

    /// Follow one response `$ref`, stopping a recursive model at the finite
    /// prefix already emitted on this branch.
    fn collect_ref_fields(
        &self,
        reference: &str,
        schema_depth: usize,
        field_depth: u8,
        out: &mut Vec<CompiledField>,
        seen: &mut HashSet<String>,
        active_refs: &mut HashSet<String>,
    ) -> Result<Emitted, CompileError> {
        if !active_refs.insert(reference.to_string()) {
            // The finite prefix already emitted on this branch is useful;
            // repeating it to the depth limit is neither useful nor stable.
            // Reported as `Cut` so the field that owns this subtree can say
            // its properties are not listed, rather than passing for an
            // object that has none.
            return Ok(Emitted::Cut);
        }
        let result = match resolve_ref(reference, self.components) {
            Ok(resolved) => self.collect_fields(
                resolved,
                schema_depth + 1,
                field_depth,
                out,
                seen,
                active_refs,
            ),
            Err(error) => Err(error),
        };
        active_refs.remove(reference);
        result
    }

    /// Emit one object's properties and then their bounded descendants.
    fn collect_properties(
        &self,
        schema: &Value,
        field_depth: u8,
        out: &mut Vec<CompiledField>,
        seen: &mut HashSet<String>,
        active_refs: &mut HashSet<String>,
    ) -> Result<(), CompileError> {
        let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
            return Ok(());
        };
        for (name, prop) in properties {
            if !seen.insert(name.clone()) {
                continue;
            }
            let facets = self.facets(prop)?;
            let container = self.field_container(prop, 0)?;
            let position = out.len();
            out.push(CompiledField {
                name: name.clone(),
                ty: self.param_type(prop, 0)?,
                format: facets.format,
                nullable: facets.nullable,
                read_only: facets.read_only,
                depth: field_depth,
                container,
                children_omitted: false,
            });
            out[position].children_omitted =
                self.collect_children(prop, field_depth, container, out, active_refs)?;
        }
        Ok(())
    }

    /// Collect one field's own properties, and report whether any were left
    /// out.
    ///
    /// The two ways that happens are deliberately not distinguished in the
    /// output: a recursive model (reported by [`Emitted::Cut`]) and a field
    /// at the depth limit. Both mean the same thing to a caller - the
    /// properties exist and are not in this list - and both are answered by
    /// looking at the model this field repeats.
    fn collect_children(
        &self,
        prop: &Value,
        field_depth: u8,
        container: FieldContainer,
        out: &mut Vec<CompiledField>,
        active_refs: &mut HashSet<String>,
    ) -> Result<bool, CompileError> {
        if usize::from(field_depth) >= MAX_SCHEMA_DEPTH {
            // The limit, not the schema. Only a container can have
            // properties to leave out (a union's are not listed at any
            // depth, by design), and only one that DECLARES some: claiming
            // omitted properties for an empty object would send a caller
            // looking for a shape that does not exist, which is the same
            // class of lie the flag exists to prevent.
            return Ok(
                matches!(container, FieldContainer::Object | FieldContainer::Array)
                    && self.declares_properties(prop, 0)?,
            );
        }
        let mut child_seen = HashSet::new();
        let emitted =
            self.collect_fields(prop, 0, field_depth + 1, out, &mut child_seen, active_refs)?;
        Ok(emitted == Emitted::Cut)
    }

    /// Whether a schema declares any property at all, following `$ref`,
    /// `allOf` and `items` the way the walk itself does.
    ///
    /// Asked only at the depth limit, where the walk stops before it can
    /// find out by walking: the answer decides whether that field says its
    /// properties were left out. A structural question, so it looks for a
    /// non-empty `properties` map and nothing else - it never emits a field
    /// and cannot recurse past [`check_depth`].
    fn declares_properties(&self, schema: &Value, depth: usize) -> Result<bool, CompileError> {
        check_depth(depth)?;
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            return self.declares_properties(resolve_ref(reference, self.components)?, depth + 1);
        }
        if let Some(items) = schema.get("items") {
            if schema.get("type").and_then(Value::as_str) == Some("array") {
                return self.declares_properties(items, depth + 1);
            }
        }
        if schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| !properties.is_empty())
        {
            return Ok(true);
        }
        if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
            for branch in branches {
                if self.declares_properties(branch, depth + 1)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Classify a response property without changing its scalar wire type.
    /// Keeping this separate from `ParamType` avoids making request parsing
    /// pretend it can assemble nested objects from flat arguments.
    fn field_container(
        &self,
        schema: &Value,
        depth: usize,
    ) -> Result<FieldContainer, CompileError> {
        check_depth(depth)?;
        if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() {
            return Ok(FieldContainer::Union);
        }
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            return self.field_container(resolve_ref(reference, self.components)?, depth + 1);
        }
        if schema.get("type").and_then(Value::as_str) == Some("array") {
            return Ok(FieldContainer::Array);
        }
        if schema.get("type").and_then(Value::as_str) == Some("object")
            || schema.get("properties").is_some()
        {
            return Ok(FieldContainer::Object);
        }
        if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
            for branch in branches {
                let container = self.field_container(branch, depth + 1)?;
                if container != FieldContainer::None {
                    return Ok(container);
                }
            }
        }
        Ok(FieldContainer::None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use crate::{compile_json, CompileOptions, FieldContainer};

    fn opts() -> CompileOptions {
        CompileOptions::with_prefix("/api")
    }

    #[test]
    fn response_fields_recurse_through_objects_refs_and_array_items() {
        let raw = serde_json::json!({
            "paths": {"/things.list": {"post": {"responses": {"200": {"content": {
                "application/json": {"schema": {"type": "object", "properties": {
                    "result": {"$ref": "#/components/schemas/Result"},
                    "choice": {"oneOf": [{"type": "string"}, {"type": "integer"}]}
                }}}
            }}}}}},
            "components": {"schemas": {"Result": {"type": "object", "properties": {
                "id": {"type": "string", "format": "uuid"},
                "items": {"type": "array", "items": {"type": "object", "properties": {
                    "code": {"type": "integer"}
                }}}
            }}}}
        })
        .to_string();
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        let fields = &compiled.ops[0].response_fields;
        let shape: Vec<(&str, u8, FieldContainer)> = fields
            .iter()
            .map(|field| (field.name.as_str(), field.depth, field.container))
            .collect();
        assert_eq!(
            shape,
            [
                ("result", 0, FieldContainer::Object),
                ("id", 1, FieldContainer::None),
                ("items", 1, FieldContainer::Array),
                ("code", 2, FieldContainer::None),
                ("choice", 0, FieldContainer::Union),
            ]
        );
    }

    /// A recursive model has no finite expansion, so the walk stops - and
    /// the field that owns the cut subtree must SAY so. Without the flag it
    /// is indistinguishable from an object with no properties, which denies
    /// a path (`manager.manager.id`) that the API really serves.
    #[test]
    fn a_recursive_model_marks_the_field_whose_children_it_cut() {
        let raw = serde_json::json!({
            "paths": {"/things.info": {"post": {"responses": {"200": {"content": {
                "application/json": {"schema": {"type": "object", "properties": {
                    "node": {"$ref": "#/components/schemas/Node"}
                }}}
            }}}}}},
            "components": {"schemas": {"Node": {"type": "object", "properties": {
                "id": {"type": "string"},
                "manager": {"$ref": "#/components/schemas/Node"}
            }}}}
        })
        .to_string();
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        let fields = &compiled.ops[0].response_fields;
        let shape: Vec<(&str, u8, FieldContainer, bool)> = fields
            .iter()
            .map(|field| {
                (
                    field.name.as_str(),
                    field.depth,
                    field.container,
                    field.children_omitted,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("node", 0, FieldContainer::Object, false),
                ("id", 1, FieldContainer::None, false),
                // The recursion stops here, and the field admits it.
                ("manager", 1, FieldContainer::Object, true),
            ]
        );
    }

    /// `allOf: [$ref Self, {props}]` - a recursive model with extra fields
    /// of its own - is where "children omitted" and "children listed" are
    /// both true of one field. An earlier validator rejected exactly this
    /// document, so the shape is pinned here as well as accepted.
    #[test]
    fn a_recursive_model_with_extra_properties_lists_them_and_marks_the_rest() {
        let raw = serde_json::json!({
            "paths": {"/things.info": {"post": {"responses": {"200": {"content": {
                "application/json": {"schema": {"type": "object", "properties": {
                    "node": {"$ref": "#/components/schemas/Node"}
                }}}
            }}}}}},
            "components": {"schemas": {"Node": {"type": "object", "properties": {
                "id": {"type": "string"},
                "manager": {"allOf": [
                    {"$ref": "#/components/schemas/Node"},
                    {"type": "object", "properties": {"note": {"type": "string"}}}
                ]}
            }}}}
        })
        .to_string();
        let compiled = compile_json(&raw, &opts()).expect("a legitimate document");
        let fields = &compiled.ops[0].response_fields;
        let shape: Vec<(&str, u8, bool)> = fields
            .iter()
            .map(|field| (field.name.as_str(), field.depth, field.children_omitted))
            .collect();
        assert_eq!(
            shape,
            [
                ("node", 0, false),
                ("id", 1, false),
                // Both at once: `note` is listed below it, the recursive
                // half is not, and the flag reports the half that is gone.
                ("manager", 1, true),
                ("note", 2, false),
            ]
        );
    }

    /// A cut inside an ARRAY item marks the array, not something else: the
    /// children of an array field describe one item, so that is the field
    /// whose shape is incomplete.
    #[test]
    fn a_recursive_model_inside_an_array_marks_the_array_field() {
        let raw = serde_json::json!({
            "paths": {"/things.info": {"post": {"responses": {"200": {"content": {
                "application/json": {"schema": {"type": "object", "properties": {
                    "node": {"$ref": "#/components/schemas/Node"}
                }}}
            }}}}}},
            "components": {"schemas": {"Node": {"type": "object", "properties": {
                "children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}}
            }}}}
        })
        .to_string();
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        let fields = &compiled.ops[0].response_fields;
        let shape: Vec<(&str, u8, FieldContainer, bool)> = fields
            .iter()
            .map(|field| {
                (
                    field.name.as_str(),
                    field.depth,
                    field.container,
                    field.children_omitted,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("node", 0, FieldContainer::Object, false),
                ("children", 1, FieldContainer::Array, true),
            ]
        );
    }

    /// The flag is a claim about the SCHEMA, so an object that declares no
    /// properties must not carry it - not even at the depth limit, where
    /// the walk stops before it could find out by walking. Claiming omitted
    /// properties for an empty object sends a caller looking for a shape
    /// that does not exist.
    #[test]
    fn an_object_with_no_properties_never_claims_omitted_children() {
        let mut schema = serde_json::json!({"type": "object", "properties": {
            "empty": {"type": "object"},
            "loose": {"type": "array"}
        }});
        // Nest the pair past the depth limit, so the same assertion covers
        // the limit branch as well as the ordinary one.
        for _ in 0..(crate::MAX_SCHEMA_DEPTH + 2) {
            schema = serde_json::json!({"type": "object", "properties": {"down": schema}});
        }
        let raw = serde_json::json!({
            "paths": {"/things.info": {"post": {"responses": {"200": {"content": {
                "application/json": {"schema": schema}
            }}}}}}
        })
        .to_string();
        let compiled = compile_json(&raw, &opts()).expect("compiles");
        let fields = &compiled.ops[0].response_fields;
        assert!(
            fields
                .iter()
                .any(|field| field.depth as usize == crate::MAX_SCHEMA_DEPTH),
            "the fixture never reaches the depth limit: {:?}",
            fields
                .iter()
                .map(|f| (f.name.as_str(), f.depth))
                .collect::<Vec<_>>()
        );
        for field in fields.iter() {
            let empty_container = matches!(field.name.as_str(), "empty" | "loose");
            assert!(
                !(empty_container && field.children_omitted),
                "{} declares no properties yet claims omitted children",
                field.name
            );
        }
    }
}
