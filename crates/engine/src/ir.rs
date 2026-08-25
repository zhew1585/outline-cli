//! Versioned intermediate representation (IR) for RPC operations.
//!
//! The IR is a plain static data table: one [`OpSpec`] per operation, each
//! carrying its request parameters. It is produced at build time from an
//! OpenAPI spec and interpreted at runtime by a single generic dispatcher.
//! There is deliberately no per-endpoint generated code.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// Schema version of the IR data structures.
///
/// Any serialized IR (e.g. a future on-disk cache) must embed this version;
/// a mismatch invalidates the whole artifact and forces a rebuild.
pub const IR_SCHEMA_VERSION: u32 = 1;

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

/// A single request-body parameter of an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Parameter name as it appears in the JSON request body.
    pub name: Cow<'static, str>,
    /// Declared wire type.
    pub ty: ParamType,
    /// Whether the spec marks this parameter as required.
    pub required: bool,
}

/// A single RPC operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpSpec {
    /// Operation name in `resource.method` form (e.g. `things.info`).
    pub name: Cow<'static, str>,
    /// URL path joined verbatim onto the client base URL (e.g.
    /// `/rpc/things.info`). Any service-specific prefix convention is
    /// applied by the spec compiler that emits the IR, never by the engine.
    pub path: Cow<'static, str>,
    /// Request-body parameters.
    pub params: Cow<'static, [ParamSpec]>,
}

impl OpSpec {
    /// Look up a parameter spec by name.
    pub fn param(&self, name: &str) -> Option<&ParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }
}
