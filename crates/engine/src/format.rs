//! Validation of schema `format` facets.
//!
//! Only formats with an unambiguous, cheaply checkable definition are
//! enforced; every other `format` value passes through untouched, so a
//! vendor-specific or unknown tag can never make an operation uncallable.
//!
//! The checks are hand-rolled on purpose: no regex engine, no extra
//! dependency, no measurable binary-size cost.

use serde_json::Value;

use crate::error::EngineError;
use crate::ir::ParamSpec;

/// Number of hex digits in each dash-separated UUID group.
const UUID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];

/// Check `value` against the parameter's declared format.
///
/// Unknown formats and non-string values are accepted unchanged.
pub(crate) fn check(param: &ParamSpec, value: &Value) -> Result<(), EngineError> {
    let (Some(text), format) = (value.as_str(), param.format.as_ref()) else {
        return Ok(());
    };
    let valid = match format {
        "uuid" => is_uuid(text),
        "date-time" => is_rfc3339_date_time(text),
        "email" => is_email(text),
        "uri" => is_uri(text),
        // Unlisted formats (and no format at all) are not enforced.
        _ => true,
    };
    if valid {
        return Ok(());
    }
    Err(EngineError::InvalidParamValue {
        name: param.name.to_string(),
        reason: format!("expected a valid {format} value"),
    })
}

/// `8-4-4-4-12` lowercase-or-uppercase hex groups.
fn is_uuid(text: &str) -> bool {
    let mut groups = text.split('-');
    let sized = UUID_GROUPS.iter().all(|length| {
        groups
            .next()
            .is_some_and(|group| group.len() == *length && is_all(group, u8::is_ascii_hexdigit))
    });
    sized && groups.next().is_none()
}

/// RFC 3339 date-time: `YYYY-MM-DDThh:mm:ss[.frac](Z|±hh:mm)`.
fn is_rfc3339_date_time(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || !matches!(bytes[10], b'T' | b't') {
        return false;
    }
    is_date(&text[..10]) && is_time_with_offset(&text[11..])
}

/// `YYYY-MM-DD` with in-range month and day (calendar length not checked).
fn is_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    let [year, month, day] = parts[..] else {
        return false;
    };
    is_number_in(year, 4, 0, 9999) && is_number_in(month, 2, 1, 12) && is_number_in(day, 2, 1, 31)
}

/// `hh:mm:ss[.frac]` followed by `Z`, `z` or `±hh:mm`.
fn is_time_with_offset(text: &str) -> bool {
    let (time, offset) = match text.find(['Z', 'z', '+']) {
        Some(index) => text.split_at(index),
        // A `-` inside the time is impossible, so the last one starts the
        // negative offset.
        None => match text.rfind('-') {
            Some(index) => text.split_at(index),
            None => return false,
        },
    };
    is_clock(time) && is_offset(offset)
}

/// `hh:mm:ss` with an optional `.frac` suffix.
fn is_clock(text: &str) -> bool {
    let (clock, fraction) = match text.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (text, None),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    let [hour, minute, second] = parts[..] else {
        return false;
    };
    let fraction_ok = fraction
        .is_none_or(|digits| !digits.is_empty() && is_all(digits, |byte| byte.is_ascii_digit()));
    fraction_ok
        && is_number_in(hour, 2, 0, 23)
        && is_number_in(minute, 2, 0, 59)
        // 60 is allowed for leap seconds.
        && is_number_in(second, 2, 0, 60)
}

/// `Z`, `z`, or a signed `hh:mm` UTC offset.
fn is_offset(text: &str) -> bool {
    if matches!(text, "Z" | "z") {
        return true;
    }
    let Some(digits) = text.strip_prefix('+').or_else(|| text.strip_prefix('-')) else {
        return false;
    };
    let Some((hour, minute)) = digits.split_once(':') else {
        return false;
    };
    is_number_in(hour, 2, 0, 23) && is_number_in(minute, 2, 0, 59)
}

/// A single `@`, a non-empty local part, and a dotted domain - the shape
/// every mail transport agrees on, without pretending to be RFC 5322.
fn is_email(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    let plain = |part: &str| {
        !part.is_empty() && is_all(part, |byte| !byte.is_ascii_whitespace() && *byte != b'@')
    };
    let labels: Vec<&str> = domain.split('.').collect();
    plain(local) && labels.len() >= 2 && labels.iter().all(|label| plain(label))
}

/// An absolute URI: an alphabetic-led scheme, a `:`, and no whitespace.
fn is_uri(text: &str) -> bool {
    let Some((scheme, rest)) = text.split_once(':') else {
        return false;
    };
    let scheme_ok = scheme
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && is_all(scheme, |byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
        });
    scheme_ok && !rest.is_empty() && is_all(text, |byte| !byte.is_ascii_whitespace())
}

/// Whether every byte of `text` satisfies `predicate`.
fn is_all(text: &str, predicate: impl Fn(&u8) -> bool) -> bool {
    !text.is_empty() && text.bytes().all(|byte| predicate(&byte))
}

/// Whether `text` is exactly `width` digits denoting a value in range.
fn is_number_in(text: &str, width: usize, low: u32, high: u32) -> bool {
    text.len() == width
        && is_all(text, |byte| byte.is_ascii_digit())
        && text
            .parse::<u32>()
            .is_ok_and(|value| (low..=high).contains(&value))
}

#[cfg(test)]
mod tests {
    use super::{is_email, is_rfc3339_date_time, is_uri, is_uuid};

    #[test]
    fn uuid_shape_is_enforced() {
        assert!(is_uuid("d8f7a1b2-3c4d-5e6f-7081-92a3b4c5d6e7"));
        assert!(is_uuid("D8F7A1B2-3C4D-5E6F-7081-92A3B4C5D6E7"));
        for bad in [
            "not-a-uuid",
            "d8f7a1b2-3c4d-5e6f-7081-92a3b4c5d6e", // short group
            "d8f7a1b2-3c4d-5e6f-7081-92a3b4c5d6e77", // long group
            "d8f7a1b2-3c4d-5e6f-7081-92a3b4c5d6e7-1", // extra group
            "d8f7a1b23c4d5e6f708192a3b4c5d6e7",    // unhyphenated
            "g8f7a1b2-3c4d-5e6f-7081-92a3b4c5d6e7", // non-hex
            "",
        ] {
            assert!(!is_uuid(bad), "accepted {bad:?}");
        }
    }

    #[test]
    fn date_time_shape_is_enforced() {
        for good in [
            "2026-08-25T12:34:56Z",
            "2026-08-25t12:34:56z",
            "2026-08-25T12:34:56.789Z",
            "2026-08-25T12:34:56+02:00",
            "2026-08-25T12:34:56.1-05:30",
            "2026-12-31T23:59:60Z",
        ] {
            assert!(is_rfc3339_date_time(good), "rejected {good:?}");
        }
        for bad in [
            "2026-08-25",
            "2026-08-25 12:34:56Z",
            "2026-13-25T12:34:56Z",
            "2026-08-32T12:34:56Z",
            "2026-08-25T24:00:00Z",
            "2026-08-25T12:34:56",
            "2026-08-25T12:34:56+2:00",
            "2026-08-25T12:34:56.Z",
            "yesterday",
            "",
        ] {
            assert!(!is_rfc3339_date_time(bad), "accepted {bad:?}");
        }
    }

    #[test]
    fn email_and_uri_shapes_are_enforced() {
        assert!(is_email("jane.doe@example.com"));
        for bad in ["jane", "jane@", "@example.com", "jane@example", "a b@c.d"] {
            assert!(!is_email(bad), "accepted {bad:?}");
        }
        assert!(is_uri("https://example.com/x"));
        assert!(is_uri("mailto:jane@example.com"));
        for bad in [
            "example.com",
            "://example.com",
            "1http://x",
            "http:",
            "a b:c",
        ] {
            assert!(!is_uri(bad), "accepted {bad:?}");
        }
    }
}
