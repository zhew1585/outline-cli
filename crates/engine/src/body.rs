//! Local request-body assembly and validation for `key=value` arguments.
//!
//! This is the local-validation half of the single request channel: every
//! `key=value` invocation flows through [`build_request_body`] before any
//! network activity, so unknown parameters, missing required parameters,
//! complex parameters, and malformed scalar values are all rejected
//! without a request being sent.

use serde_json::{Map, Number, Value};

use crate::error::EngineError;
use crate::ir::{OpSpec, ParamSpec, ParamType};

/// Assemble and validate the JSON request body for `op` from raw
/// `key=value` pairs.
///
/// Scalar values are coerced to their declared wire type (native JSON
/// integers/booleans/numbers, never strings-in-disguise). Purely local:
/// never touches the network.
pub fn build_request_body(op: &OpSpec, args: &[(String, String)]) -> Result<Value, EngineError> {
    let mut body = Map::new();
    for (key, raw) in args {
        let param = op.param(key).ok_or_else(|| unknown_param(op, key))?;
        if body.contains_key(key) {
            return Err(EngineError::InvalidParamValue {
                name: key.clone(),
                reason: "parameter given more than once".to_string(),
            });
        }
        body.insert(key.clone(), coerce_scalar(op, param, raw)?);
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

/// The first required parameter not present in the assembled body, if any.
fn first_missing_required<'a>(op: &'a OpSpec, body: &Map<String, Value>) -> Option<&'a ParamSpec> {
    op.params
        .iter()
        .find(|param| param.required && !body.contains_key(param.name.as_ref()))
}

/// Build the error for an argument that names no parameter of `op`.
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

/// Coerce one raw CLI value to the parameter's declared scalar type.
///
/// Complex (`Json`) parameters are rejected outright: they cannot be
/// expressed as a scalar `key=value` argument.
fn coerce_scalar(op: &OpSpec, param: &ParamSpec, raw: &str) -> Result<Value, EngineError> {
    let invalid = |reason: &str| EngineError::InvalidParamValue {
        name: param.name.to_string(),
        reason: reason.to_string(),
    };
    match param.ty {
        ParamType::String => Ok(Value::String(raw.to_string())),
        ParamType::Integer => raw
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| invalid("expected an integer")),
        ParamType::Boolean => match raw {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(invalid("expected a boolean (true or false)")),
        },
        ParamType::Number => raw
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| invalid("expected a finite number")),
        ParamType::Json => Err(EngineError::ComplexParam {
            operation: op.name.to_string(),
            name: param.name.to_string(),
        }),
    }
}
