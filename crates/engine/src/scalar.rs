//! Scalar `key=value` coercion: raw CLI text to a typed JSON value.
//!
//! Coercion is exact by construction: a value that cannot be represented
//! faithfully in JSON is rejected locally rather than silently rounded on
//! the wire.

use serde_json::{Number, Value};

use crate::error::EngineError;
use crate::ir::{ParamSpec, ParamType};

/// The literal a nullable parameter accepts as an explicit JSON `null`.
pub(crate) const NULL_LITERAL: &str = "null";

/// Whether `raw` requests an explicit JSON `null` for `param`.
///
/// Works for every type, complex ones included: clearing a nullable field
/// is the one thing a `key=value` argument can say about it.
pub(crate) fn is_null_literal(param: &ParamSpec, raw: &str) -> bool {
    param.nullable && raw == NULL_LITERAL
}

/// Coerce one raw CLI value to the parameter's declared scalar type.
///
/// Complex (`Json`) parameters are rejected by the caller before this
/// point; the arm below only exists to keep the match exhaustive.
pub(crate) fn coerce(param: &ParamSpec, raw: &str) -> Result<Value, EngineError> {
    if is_null_literal(param, raw) {
        return Ok(Value::Null);
    }
    match param.ty {
        ParamType::String | ParamType::Json => Ok(Value::String(raw.to_string())),
        ParamType::Boolean => match raw {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(invalid(param, "expected a boolean (true or false)")),
        },
        ParamType::Integer => raw
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| invalid(param, "expected an integer")),
        ParamType::Number => parse_number(param, raw),
    }
}

/// Parse a JSON number without losing precision.
///
/// Integers are taken as `i64`, then `u64`; anything else goes through
/// `f64` and is accepted only when the shortest round-trip rendering of
/// the parsed double denotes the same decimal value as the input.
fn parse_number(param: &ParamSpec, raw: &str) -> Result<Value, EngineError> {
    if let Ok(value) = raw.parse::<i64>() {
        return Ok(Value::from(value));
    }
    if let Ok(value) = raw.parse::<u64>() {
        return Ok(Value::from(value));
    }
    let parsed = raw
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .ok_or_else(|| invalid(param, "expected a finite number"))?;
    // Strict: an input whose decimal form cannot be normalized is treated
    // as inexact rather than assumed equivalent.
    let exact = match (decimal_parts(raw), decimal_parts(&parsed.to_string())) {
        (Some(input), Some(round_tripped)) => input == round_tripped,
        _ => false,
    };
    if !exact {
        return Err(EngineError::InexactNumber {
            name: param.name.to_string(),
            reason: "the value is not exactly representable as a JSON number".to_string(),
        });
    }
    Ok(Value::Number(parsed))
}

/// Normalize a decimal literal to `(negative, significant digits, exponent)`
/// so that two spellings of the same value compare equal (e.g. `1.25e3`
/// and `1250.0`). Returns `None` for input this comparison cannot handle.
fn decimal_parts(raw: &str) -> Option<(bool, String, i64)> {
    let (negative, unsigned) = match raw.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, raw.strip_prefix('+').unwrap_or(raw)),
    };
    let (mantissa, exponent) = split_exponent(unsigned)?;
    let (integral, fractional) = match mantissa.split_once('.') {
        Some((integral, fractional)) => (integral, fractional),
        None => (mantissa, ""),
    };
    let all_digits = integral
        .bytes()
        .chain(fractional.bytes())
        .all(|byte| byte.is_ascii_digit());
    if !all_digits || (integral.is_empty() && fractional.is_empty()) {
        return None;
    }
    let digits = format!("{integral}{fractional}");
    let scale = i64::try_from(fractional.len()).ok()?;
    let trimmed = digits.trim_start_matches('0').trim_end_matches('0');
    if trimmed.is_empty() {
        // Every spelling of zero normalizes to the same value.
        return Some((false, String::new(), 0));
    }
    let trailing_zeros =
        i64::try_from(digits.trim_start_matches('0').len() - trimmed.len()).ok()?;
    Some((
        negative,
        trimmed.to_string(),
        exponent - scale + trailing_zeros,
    ))
}

/// Split a decimal literal into its mantissa and base-10 exponent.
fn split_exponent(unsigned: &str) -> Option<(&str, i64)> {
    match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => Some((mantissa, exponent.parse::<i64>().ok()?)),
        None => Some((unsigned, 0)),
    }
}

/// Build an invalid-value error for `param`.
fn invalid(param: &ParamSpec, reason: &str) -> EngineError {
    EngineError::InvalidParamValue {
        name: param.name.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::decimal_parts;

    #[test]
    fn equivalent_spellings_normalize_equally() {
        assert_eq!(decimal_parts("1.25e3"), decimal_parts("1250.0"));
        assert_eq!(decimal_parts("0.5"), decimal_parts("5e-1"));
        assert_eq!(decimal_parts("-0.0"), decimal_parts("0"));
        assert_eq!(decimal_parts("2"), decimal_parts("2.000"));
    }

    #[test]
    fn different_values_normalize_differently() {
        assert_ne!(
            decimal_parts("9007199254740993.0"),
            decimal_parts("9007199254740992")
        );
        assert_ne!(decimal_parts("1.5"), decimal_parts("1.6"));
        assert_ne!(decimal_parts("1e2"), decimal_parts("1e3"));
        assert_ne!(decimal_parts("-1"), decimal_parts("1"));
    }

    #[test]
    fn unparseable_input_yields_none() {
        assert_eq!(decimal_parts("inf"), None);
        assert_eq!(decimal_parts("NaN"), None);
        assert_eq!(decimal_parts(""), None);
    }
}
