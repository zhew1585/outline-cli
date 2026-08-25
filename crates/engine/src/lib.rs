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
//! - [`fetch`]: the plain-document channel (one unauthenticated GET), used
//!   to retrieve a spec; separate from the credential-carrying request
//!   channel on purpose (see the module docs).
//! - [`sanitize`]: the credential-hygiene pipeline every piece of
//!   server-provided text passes through at error construction time.
//! - [`paginate`]: auto-pagination driven by a caller-supplied
//!   [`PaginationSpec`] (the engine knows no wire vocabulary of its own).
//! - [`retry`]: 429 backoff policy (Retry-After aware).
//! - [`throttle`]: token-bucket rate limiting over a shared handle.

#![forbid(unsafe_code)]

pub mod body;
pub mod client;
pub mod error;
pub mod fetch;
mod format;
pub mod ir;
pub mod paginate;
pub mod retry;
pub mod sanitize;
mod scalar;
pub mod throttle;

pub use body::build_request_body;
pub use client::{base_url_origin, is_valid_base_url, Client, ErrorDetail, DEFAULT_TIMEOUT};
pub use error::{EngineError, TransportKind};
pub use ir::{BodyMode, OpSpec, ParamSpec, ParamType, ValidationMode, IR_SCHEMA_VERSION};
pub use paginate::{Fetched, OffsetEcho, PaginationSpec, Truncation, TruncationCause};
pub use retry::RetryPolicy;
pub use throttle::{Throttle, TokenBucket};
