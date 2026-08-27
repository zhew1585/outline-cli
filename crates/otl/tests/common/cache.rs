//! Fixtures for the spec-cache tests.
//!
//! Shared by `spec_cache.rs` (the cache works) and `spec_cache_rejects.rs`
//! (the cache refuses), which are separate files because they answer
//! separate questions - and because one file of both was over the
//! 800-line limit.
//!
//! Building cache files by hand is the point: it is what lets a test
//! declare something the encoder would never write - an impossible
//! operation count, a record that lies about its length - which is exactly
//! what a hostile cache does. The layout pinned here is:
//!
//! ```text
//! magic(8) | layout version(4 LE) | sha256(body)(32) | body
//! body = meta_len(4 LE) | meta | op_count(4 LE) | [ op_len(4 LE) | op ]*
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use engine::ir::{BodyMode, OpSpec};
use otl::spec::cache::CacheMeta;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

pub const MAGIC: [u8; 8] = *b"OTL-IRC\x00";
pub const FORMAT_VERSION: u32 = 2;

/// A table as the framing layer sees it.
pub struct Body {
    pub meta: CacheMeta,
    pub ops: Vec<OpSpec>,
}

/// Encode one record the way the cache does.
pub fn record<T: Serialize>(value: &T) -> Vec<u8> {
    bincode::serde::encode_to_vec(value, bincode::config::standard().with_limit::<32_768>())
        .unwrap()
}

pub fn push_record(body: &mut Vec<u8>, bytes: &[u8]) {
    body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    body.extend_from_slice(bytes);
}

/// Frame a body from parts, bypassing every check `store_at` makes.
pub fn frame(body: &Body) -> Vec<u8> {
    frame_with_count(body, body.ops.len() as u32)
}

/// [`frame`], but with a declared operation count of the caller's choosing
/// - so a test can claim thousands of operations in a handful of bytes.
pub fn frame_with_count(body: &Body, declared: u32) -> Vec<u8> {
    let mut out = Vec::new();
    push_record(&mut out, &record(&body.meta));
    out.extend_from_slice(&declared.to_le_bytes());
    for op in &body.ops {
        push_record(&mut out, &record(op));
    }
    out
}

pub fn op(name: &str, path: &str) -> OpSpec {
    OpSpec {
        name: name.to_string().into(),
        path: path.to_string().into(),
        summary: "summary".to_string().into(),
        content_type: "application/json".to_string().into(),
        body_mode: BodyMode::KeyValue,
        params: Vec::new().into(),
        response_fields: Vec::new().into(),
    }
}

pub fn meta() -> CacheMeta {
    CacheMeta::new("a".repeat(64), "https://spec.example".to_string())
}

/// Write a cache file with a valid header around an arbitrary body.
pub fn write_body(file: &Path, magic: [u8; 8], version: u32, body: &[u8]) {
    let mut raw = Vec::new();
    raw.extend_from_slice(&magic);
    raw.extend_from_slice(&version.to_le_bytes());
    raw.extend_from_slice(&Sha256::digest(body));
    raw.extend_from_slice(body);
    fs::write(file, raw).unwrap();
}

/// Write a cache file from a framed table.
pub fn write_raw(file: &Path, magic: [u8; 8], version: u32, body: &Body) {
    write_body(file, magic, version, &frame(body));
}

pub fn temp_cache() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("ir-cache.bin");
    (dir, file)
}
