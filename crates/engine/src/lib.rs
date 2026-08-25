//! Generic OpenAPI RPC engine.
//!
//! This crate is deliberately service-agnostic: it knows nothing about any
//! particular API vendor. It provides:
//!
//! - [`ir`]: the versioned intermediate representation (IR) describing RPC
//!   operations compiled from an OpenAPI spec at build time.
//! - [`client`]: the single request channel through which every HTTP call
//!   must flow.
//! - [`error`]: typed engine errors.

#![forbid(unsafe_code)]

pub mod client;
pub mod error;
pub mod ir;

pub use client::{base_url_origin, is_valid_base_url, Client};
pub use error::{EngineError, TransportKind};
pub use ir::{OpSpec, ParamSpec, ParamType, IR_SCHEMA_VERSION};
