//! Bounded decoding of the cached operation table.
//!
//! Split out of `cache.rs` because it answers one question on its own: how
//! much can a cache file make this process allocate? See [`BoundedOps`].

use std::fmt;

use engine::ir::OpSpec;
use serde::{Deserialize, Serialize};

use super::cache::{MAX_CACHED_OPS, MAX_CACHE_BODY_BYTES, MAX_DECODED_BYTES, MIN_ENCODED_OP_BYTES};

/// The operation table, decoded under an explicit element and footprint
/// budget.
///
/// # Why a byte limit is not enough
///
/// bincode's `with_limit` counts the bytes the decoder CONSUMES. It says
/// nothing about what those bytes turn into: a minimal `OpSpec` encodes to
/// six bytes (four empty strings, a discriminant, an empty parameter list)
/// and occupies well over a hundred once decoded, and the serde path never
/// charges the decoder for a decoded structure. So a one-megabyte cache
/// could ask for a hundred thousand operations and get tens of megabytes
/// of heap - all of it allocated BEFORE any validation could discard the
/// file.
///
/// # What this bounds
///
/// The sequence is pulled element by element (bincode hands over a
/// pull-based `SeqAccess` and allocates nothing itself), and decoding stops
/// at the first element that breaks a rule:
///
/// - the declared element count must not exceed [`MAX_CACHED_OPS`], nor
///   what the remaining bytes could possibly encode;
/// - the running decoded footprint must stay under [`MAX_DECODED_BYTES`];
/// - capacity is reserved for what is plausible, never for what the file
///   claims.
///
/// One operation is still decoded whole before its footprint is counted,
/// so the peak is that budget plus one operation's worth - bounded by the
/// file limit, which is why that limit is small.
pub(super) struct BoundedOps(pub(super) Vec<OpSpec>);

impl Serialize for BoundedOps {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BoundedOps {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(OpsVisitor)
    }
}

struct OpsVisitor;

impl<'de> serde::de::Visitor<'de> for OpsVisitor {
    type Value = BoundedOps;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "at most {MAX_CACHED_OPS} operations")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let declared = seq.size_hint();
        if let Some(count) = declared {
            if count > MAX_CACHED_OPS {
                return Err(serde::de::Error::custom(format!(
                    "it declares {count} operations, more than the {MAX_CACHED_OPS} allowed"
                )));
            }
        }
        // Trust the smaller of "what it says" and "what could fit".
        let plausible = MAX_CACHE_BODY_BYTES / MIN_ENCODED_OP_BYTES;
        let capacity = declared.unwrap_or(0).min(MAX_CACHED_OPS).min(plausible);
        let mut ops: Vec<OpSpec> = Vec::with_capacity(capacity);
        let mut footprint = 0usize;
        while let Some(op) = seq.next_element::<OpSpec>()? {
            if ops.len() >= MAX_CACHED_OPS {
                return Err(serde::de::Error::custom(format!(
                    "it contains more than the {MAX_CACHED_OPS} operations allowed"
                )));
            }
            footprint = footprint.saturating_add(footprint_of(&op));
            if footprint > MAX_DECODED_BYTES {
                return Err(serde::de::Error::custom(format!(
                    "its operations decode to more than the {MAX_DECODED_BYTES} byte limit"
                )));
            }
            ops.push(op);
        }
        Ok(BoundedOps(ops))
    }
}

/// Rough heap footprint of one decoded operation: the struct itself, the
/// bytes of its owned strings, and its parameter and enum containers.
///
/// Approximate on purpose - it is a budget, not an accounting - but it
/// must never UNDER-count a field that an attacker can multiply, which is
/// why the containers are charged by element size and not just by length.
fn footprint_of(op: &OpSpec) -> usize {
    let text = op.name.len() + op.path.len() + op.summary.len() + op.content_type.len();
    let params: usize = op
        .params
        .iter()
        .map(|param| {
            std::mem::size_of::<engine::ir::ParamSpec>()
                + param.name.len()
                + param.format.len()
                + param.enum_values.len() * std::mem::size_of::<std::borrow::Cow<'static, str>>()
                + param
                    .enum_values
                    .iter()
                    .map(|value| value.len())
                    .sum::<usize>()
        })
        .sum();
    std::mem::size_of::<OpSpec>() + text + params
}
