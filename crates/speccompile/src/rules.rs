//! What a compiled operation may contain.
//!
//! Split out of `lib.rs` because it is one concern with one reason to
//! exist: every rule here exists because the value it checks ends up
//! somewhere it can do harm - a request path joined onto a base URL, a name
//! written into a shell completion script, a summary or a column header
//! printed to a terminal.
//!
//! The path rules are the load-bearing ones. A request URL is
//! `base_url + path` concatenated as text, so a path that starts a new
//! authority (`//host`, `@host`), names a scheme, or walks up with `..`
//! would send the caller's bearer token to whoever owns that host.

use crate::text::{
    is_display_safe, MAX_CONTENT_TYPE_BYTES, MAX_ENUM_VALUES, MAX_ENUM_VALUE_BYTES,
    MAX_FORMAT_BYTES, MAX_PARAM_NAME_BYTES, MAX_RESPONSE_FIELDS,
};
use crate::{CompileError, CompileOptions, CompiledOp};

/// Reason text for a rejected string with meaning.
const UNSAFE_TEXT_REASON: &str =
    "it contains control or direction-changing characters, or exceeds its length limit";
/// Reason text for an over-long enum.
const TOO_MANY_ENUM_VALUES: &str = "it declares more enumerated values than the limit";
/// Reason text for an over-long response field list.
const TOO_MANY_RESPONSE_FIELDS: &str = "it declares more response fields than the limit";

/// Reject an operation carrying text a terminal would execute.
///
/// The summary is not checked here: [`extract_summary`] sanitizes it
/// instead, because display-only text can be rewritten without changing
/// what any command does. Everything checked below is compared against
/// user input or sent on the wire, where rewriting WOULD change behaviour.
pub(crate) fn check_text(op: &CompiledOp) -> Result<(), CompileError> {
    let unsafe_text = |field, reason| CompileError::UnsafeText {
        operation: op.name.clone(),
        field,
        reason,
    };
    if !is_display_safe(&op.content_type, MAX_CONTENT_TYPE_BYTES) {
        return Err(unsafe_text("content type", UNSAFE_TEXT_REASON));
    }
    for param in &op.params {
        if param.name.is_empty() || !is_display_safe(&param.name, MAX_PARAM_NAME_BYTES) {
            return Err(unsafe_text("parameter name", UNSAFE_TEXT_REASON));
        }
        if !is_display_safe(&param.format, MAX_FORMAT_BYTES) {
            return Err(unsafe_text("parameter format", UNSAFE_TEXT_REASON));
        }
        if param.enum_values.len() > MAX_ENUM_VALUES {
            return Err(unsafe_text("parameter enum", TOO_MANY_ENUM_VALUES));
        }
        if !param
            .enum_values
            .iter()
            .all(|value| is_display_safe(value, MAX_ENUM_VALUE_BYTES))
        {
            return Err(unsafe_text("parameter enum value", UNSAFE_TEXT_REASON));
        }
    }
    // Response field names and formats are printed too - as table column
    // HEADERS, which is about as good a place to inject an escape as
    // exists - so they are held to the same rule as everything else that
    // reaches a terminal.
    if op.response_fields.len() > MAX_RESPONSE_FIELDS {
        return Err(unsafe_text("response field list", TOO_MANY_RESPONSE_FIELDS));
    }
    for field in &op.response_fields {
        if field.name.is_empty() || !is_display_safe(&field.name, MAX_PARAM_NAME_BYTES) {
            return Err(unsafe_text("response field name", UNSAFE_TEXT_REASON));
        }
        if !is_display_safe(&field.format, MAX_FORMAT_BYTES) {
            return Err(unsafe_text("response field format", UNSAFE_TEXT_REASON));
        }
    }
    Ok(())
}

/// Reject an operation whose name or path cannot be used safely, or whose
/// two identifiers disagree.
pub(crate) fn check_identifiers(
    op: &CompiledOp,
    options: &CompileOptions,
) -> Result<(), CompileError> {
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
    // The binding between the two, established HERE rather than assumed by
    // the consumer. Name and path are derived from the same document path,
    // so they must agree - but only if that document path began with `/`.
    // A path written `things.delete` (no leading slash) would otherwise be
    // swallowed by the prefix into `/apithings.delete`, which passes the
    // character rules above while dispatching somewhere nobody asked for.
    if op.path != format!("{}/{}", options.path_prefix, op.name) {
        return Err(unsafe_id("path", PATH_BINDING_RULE));
    }
    Ok(())
}

/// Why an operation path/name binding is rejected.
const PATH_BINDING_RULE: &str =
    "the request path must be the configured prefix followed by `/` and the \
     operation name, which requires the document path to start with `/`";

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
