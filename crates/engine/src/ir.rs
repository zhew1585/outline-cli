//! Versioned intermediate representation (IR) for RPC operations.
//!
//! The IR is a plain static data table: one [`OpSpec`] per operation, each
//! carrying its request parameters. It is produced at build time from an
//! OpenAPI spec and interpreted at runtime by a single generic dispatcher.
//! There is deliberately no per-endpoint generated code.

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Schema version of the IR data structures.
///
/// Any serialized IR (e.g. a future on-disk cache) must embed this version;
/// a mismatch invalidates the whole artifact and forces a rebuild.
///
/// Version history: 2 added `OpSpec::summary`; 3 added
/// `OpSpec::content_type`, `OpSpec::body_mode` and the `ParamSpec`
/// constraint facets (`nullable`, `enum_values`, `minimum`, `maximum`);
/// 4 added `ParamSpec::format`.
pub const IR_SCHEMA_VERSION: u32 = 4;

/// How strictly a request is validated against the IR before being sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ValidationMode {
    /// Enforce every compiled schema facet (enum, bounds, format).
    #[default]
    Strict,
    /// Skip the facet checks, keeping type coercion and the structural
    /// checks (unknown/missing/complex parameters).
    ///
    /// The escape hatch for a spec that disagrees with the live server:
    /// without it a stale or wrong constraint would make an operation
    /// uncallable.
    SkipFacets,
}

/// The wire type of a single request parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamType {
    /// JSON string.
    String,
    /// JSON integer.
    Integer,
    /// JSON boolean.
    Boolean,
    /// JSON number (floating point).
    Number,
    /// Any complex JSON value (objects, arrays, unions).
    Json,
}

impl fmt::Display for ParamType {
    /// Lowercase JSON-schema-style type name, used in error messages.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::Number => "number",
            Self::Json => "json",
        };
        f.write_str(text)
    }
}

/// How the request body of an operation can be supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyMode {
    /// A JSON object body that can be assembled from `key=value` pairs.
    KeyValue,
    /// A JSON body constrained by a root-level `oneOf`/`anyOf` union: the
    /// choice between branches cannot be expressed as flat `key=value`
    /// pairs, so only a caller-supplied raw JSON body is accepted.
    RawJsonOnly,
    /// The body uses a content type this generic client cannot assemble
    /// (e.g. `multipart/form-data`). Not callable; a service-specific
    /// command has to handle it.
    Unsupported,
}

/// A single request-body parameter of an operation.
///
/// Constraint facets mirror the source schema so that invalid values are
/// rejected locally, before any request is sent.
///
/// TODO: `pattern` is deliberately not compiled. Validating it needs a
/// regex engine, roughly a megabyte of binary against a 5 MB budget, to
/// serve the two `pattern` constraints in the whole vendored spec; such
/// values are left for the server to reject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Parameter name as it appears in the JSON request body.
    pub name: Cow<'static, str>,
    /// Declared wire type.
    pub ty: ParamType,
    /// Whether the spec marks this parameter as required.
    pub required: bool,
    /// Whether the schema allows an explicit JSON `null`.
    ///
    /// For such a parameter the literal `key=null` is sent as JSON `null`.
    /// The consequence is that the four-character *string* `"null"` cannot
    /// be expressed as a `key=value` argument; callers who need it must
    /// supply a raw JSON body instead.
    pub nullable: bool,
    /// Allowed values, when the schema constrains the parameter to an
    /// enumeration. Empty means unconstrained.
    pub enum_values: Cow<'static, [Cow<'static, str>]>,
    /// Declared `format` (e.g. `uuid`, `date-time`), empty when absent.
    ///
    /// Only formats with an unambiguous definition are enforced; any other
    /// value is carried for diagnostics but passed through unchecked.
    pub format: Cow<'static, str>,
    /// Inclusive lower bound for numeric parameters, if any.
    pub minimum: Option<f64>,
    /// Inclusive upper bound for numeric parameters, if any.
    pub maximum: Option<f64>,
}

/// A single RPC operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpSpec {
    /// Operation name in `resource.method` form (e.g. `things.info`).
    pub name: Cow<'static, str>,
    /// URL path joined verbatim onto the client base URL (e.g.
    /// `/rpc/things.info`). Any service-specific prefix convention is
    /// applied by the spec compiler that emits the IR, never by the engine.
    pub path: Cow<'static, str>,
    /// One-line human-readable summary from the source spec (may be empty).
    pub summary: Cow<'static, str>,
    /// Request content type declared by the spec, empty when the operation
    /// takes no request body.
    pub content_type: Cow<'static, str>,
    /// How the request body may be supplied.
    pub body_mode: BodyMode,
    /// Request-body parameters.
    pub params: Cow<'static, [ParamSpec]>,
}

impl OpSpec {
    /// Look up a parameter spec by name.
    pub fn param(&self, name: &str) -> Option<&ParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }
}
