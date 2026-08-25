//! Generic OpenAPI RPC engine.
//!
//! This crate is deliberately service-agnostic: it knows nothing about any
//! particular API vendor. It provides:
//!
//! - [`ir`]: the versioned intermediate representation (IR) describing RPC
//!   operations compiled from an OpenAPI spec at build time.
//! - [`body`]: local request-body assembly and validation for `key=value`
//!   arguments (schema-driven type coercion).
//! - [`client`]: the single request channel through which every HTTP call
//!   must flow.
//! - [`error`]: typed engine errors.

#![forbid(unsafe_code)]

pub mod body;
pub mod client;
pub mod error;
pub mod ir;
mod scalar;

pub use body::{build_request_body, MIN_SENSITIVE_VALUE_CHARS};
pub use client::{base_url_origin, is_valid_base_url, Client};
pub use error::{EngineError, TransportKind};
pub use ir::{BodyMode, OpSpec, ParamSpec, ParamType, IR_SCHEMA_VERSION};
