//! Local request-body assembly and validation for `key=value` arguments.
//!
//! This is the local-validation half of the single request channel: every
//! `key=value` invocation flows through [`build_request_body`] before any
//! network activity, so operations that cannot be called generically,
//! unknown parameters, missing required parameters, complex parameters,
//! and values violating their schema facets are all rejected without a
//! request being sent.

use std::borrow::Cow;

use serde_json::{Map, Value};

use crate::error::EngineError;
use crate::ir::{BodyMode, OpSpec, ParamSpec, ParamType};
use crate::scalar;

/// Minimum length of a request-body string value considered sensitive.
///
/// Shorter values (ids, short flags, single words) are left alone so that
/// redaction cannot swallow whole server messages.
pub const MIN_SENSITIVE_VALUE_CHARS: usize = 8;

/// Assemble and validate the JSON request body for `op` from raw
/// `key=value` pairs.
///
/// Scalar values are coerced to their declared wire type (native JSON
/// integers/booleans/numbers, never strings-in-disguise) and checked
/// against the schema facets carried by the IR. Purely local: never
/// touches the network.
pub fn build_request_body(op: &OpSpec, args: &[(String, String)]) -> Result<Value, EngineError> {
    ensure_dispatchable(op)?;
    if op.body_mode == BodyMode::RawJsonOnly {
        return Err(EngineError::UnionBody {
            operation: op.name.to_string(),
        });
    }
    let mut body = Map::new();
    for (key, raw) in args {
        let param = op.param(key).ok_or_else(|| unknown_param(op, key))?;
        if body.contains_key(key) {
            return Err(EngineError::InvalidParamValue {
                name: key.clone(),
                reason: "parameter given more than once".to_string(),
            });
        }
        body.insert(key.clone(), typed_value(op, param, raw)?);
    }
    if let Some(missing) = first_missing_required(op, &body) {
        return Err(EngineError::MissingParam {
            operation: op.name.to_string(),
            name: missing.name.to_string(),
            ty: missing.ty,
        });
    }
    Ok(Value::Object(body))
}

/// Reject operations this generic client cannot call at all.
///
/// Applies to both `key=value` and raw-body requests: a non-JSON content
/// type cannot be produced from either.
pub(crate) fn ensure_dispatchable(op: &OpSpec) -> Result<(), EngineError> {
    if op.body_mode == BodyMode::Unsupported {
        return Err(EngineError::UnsupportedBodyType {
            operation: op.name.to_string(),
            content_type: op.content_type.to_string(),
        });
    }
    Ok(())
}

/// Collect the string leaves of a request body that are long enough to be
/// treated as potentially sensitive.
///
/// Used to redact caller-supplied secrets that a server (or proxy) echoes
/// back in an error response.
pub(crate) fn sensitive_values(body: &Value) -> Vec<String> {
    let mut found = Vec::new();
    collect_sensitive(body, &mut found);
    found
}

fn collect_sensitive(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) if text.chars().count() >= MIN_SENSITIVE_VALUE_CHARS => {
            found.push(text.clone());
        }
        Value::Array(items) => items.iter().for_each(|item| collect_sensitive(item, found)),
        Value::Object(entries) => entries
            .values()
            .for_each(|entry| collect_sensitive(entry, found)),
        _ => {}
    }
}

/// The first required parameter not present in the assembled body, if any.
fn first_missing_required<'a>(op: &'a OpSpec, body: &Map<String, Value>) -> Option<&'a ParamSpec> {
    op.params
        .iter()
        .find(|param| param.required && !body.contains_key(param.name.as_ref()))
}

/// Build the error for an argument that names no parameter of `op`.
///
/// Deliberately carries the key only, never the value: a mistyped key can
/// still be paired with a secret.
fn unknown_param(op: &OpSpec, key: &str) -> EngineError {
    let valid = if op.params.is_empty() {
        "this operation takes no key=value parameters".to_string()
    } else {
        let names: Vec<&str> = op.params.iter().map(|p| p.name.as_ref()).collect();
        format!("valid parameters: {}", names.join(", "))
    };
    EngineError::UnknownParam {
        operation: op.name.to_string(),
        name: key.to_string(),
        valid,
    }
}

/// Coerce one raw value and check it against the parameter's facets.
fn typed_value(op: &OpSpec, param: &ParamSpec, raw: &str) -> Result<Value, EngineError> {
    // Clearing a nullable field is expressible as key=value whatever the
    // parameter's type, so this precedes the complex-type rejection.
    if scalar::is_null_literal(param, raw) {
        return Ok(Value::Null);
    }
    if param.ty == ParamType::Json {
        return Err(EngineError::ComplexParam {
            operation: op.name.to_string(),
            name: param.name.to_string(),
        });
    }
    let value = scalar::coerce(param, raw)?;
    check_enum(param, &value)?;
    check_bounds(param, &value)?;
    Ok(value)
}

/// Reject a value outside the parameter's declared enumeration.
fn check_enum(param: &ParamSpec, value: &Value) -> Result<(), EngineError> {
    if param.enum_values.is_empty() || value.is_null() {
        return Ok(());
    }
    let candidate = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    if param
        .enum_values
        .iter()
        .any(|allowed| *allowed == candidate)
    {
        return Ok(());
    }
    let allowed: Vec<&str> = param.enum_values.iter().map(Cow::as_ref).collect();
    Err(EngineError::InvalidParamValue {
        name: param.name.to_string(),
        reason: format!("allowed values are: {}", allowed.join(", ")),
    })
}

/// Reject a numeric value outside the parameter's declared bounds.
fn check_bounds(param: &ParamSpec, value: &Value) -> Result<(), EngineError> {
    let Some(number) = value.as_f64() else {
        return Ok(());
    };
    let out_of_range = |reason: String| EngineError::InvalidParamValue {
        name: param.name.to_string(),
        reason,
    };
    if let Some(minimum) = param.minimum.filter(|minimum| number < *minimum) {
        return Err(out_of_range(format!("must be at least {minimum}")));
    }
    if let Some(maximum) = param.maximum.filter(|maximum| number > *maximum) {
        return Err(out_of_range(format!("must be at most {maximum}")));
    }
    Ok(())
}
