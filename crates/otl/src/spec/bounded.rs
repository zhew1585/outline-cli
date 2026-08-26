//! Framing and bounded coding of the cached operation table.
//!
//! Split out of `cache.rs` because it answers one question on its own: how
//! much can a cache file make this process allocate before it can be
//! rejected?
//!
//! # Why the table is framed instead of encoded as one value
//!
//! Encoding the whole table as a single `bincode` value put every nested
//! container - the parameter list of an operation, the enum values of a
//! parameter - out of reach. serde's `Vec` implementation caps its INITIAL
//! reservation at a megabyte's worth of elements and then grows by
//! doubling, so a container whose element count crosses that cap allocates
//! two megabytes from about forty kilobytes of input. A one-megabyte body
//! could stack about two dozen of those inside a single operation and
//! reach roughly forty-seven megabytes of heap - all of it before the
//! operation was complete enough to be measured, let alone rejected.
//!
//! Bounding that from the outside is not possible without owning the
//! decode of every nested type, which would mean mirroring `engine::ir`
//! here. Framing the table achieves the same result without the mirror:
//!
//! ```text
//! meta_len (u32 LE) | meta | op_count (u32 LE) | [ op_len (u32 LE) | op ]*
//! ```
//!
//! Every length is validated against a limit AND against the bytes that
//! actually remain before a single byte is decoded, and each record is
//! decoded from its own slice with its own byte limit. An operation
//! therefore cannot allocate more than one record's worth of amplification
//! (about a megabyte, the cap serde's own reservation logic imposes),
//! whatever it declares, and the table as a whole is bounded by the
//! footprint budget that is checked after every record.
//!
//! # The resulting bound
//!
//! For a cache file of at most [`super::cache::MAX_CACHE_FILE_BYTES`]
//! (1 MiB), decoding a hostile file can occupy:
//!
//! - the file contents in memory: at most that limit, 1 MiB;
//! - the operations accepted so far: at most [`MAX_DECODED_BYTES`], 4 MiB,
//!   charged with [`CONTAINER_SLACK`] so a container is counted at the
//!   capacity it can reach, not the length it reports;
//! - the record being decoded: under 2 MiB. A record holds at most
//!   [`MAX_OP_RECORD_BYTES`] (32 KiB) of bytes, so its satisfiable
//!   containers come to at most about 786 KiB (each byte can become one
//!   24-byte `Cow`), and a container whose declared length is a lie
//!   reserves at most serde's own cap (1 MiB) before running out of record
//!   and failing.
//!
//! Total: under about 8 MiB, transient, and then the file is discarded.
//!
//! This bound is stated because the previous one was WRONG. Charging
//! containers by length missed that `Vec` grows by doubling: a container
//! just past serde's reservation cap (43,690 `Cow<str>`s, about 43 KiB of
//! input) jumps to 87,380 slots, 2 MiB of heap, and two dozen of those fit
//! in a one-megabyte body - about 47 MiB, from a file that passed every
//! check until the budget was consulted. Framing is what removes the
//! construction; [`CONTAINER_SLACK`] is what stops the accounting from
//! lying about the rest.
//!
//! Both directions use the same rules: [`encode_table`] refuses to write
//! anything [`decode_table`] would refuse to read.

use std::mem::size_of;

use engine::ir::{OpSpec, ParamSpec};

use super::cache::CacheMeta;

/// Hard ceiling on the number of operations a cache may declare.
///
/// A byte limit alone does NOT bound a decoded table: a minimal `OpSpec`
/// encodes to six bytes and occupies well over a hundred once decoded.
/// (Chosen at roughly seventy times the vendored spec's operation count.)
pub const MAX_CACHED_OPS: usize = 8192;

/// Ceiling on the total decoded footprint of the operation table, checked
/// after every record.
pub const MAX_DECODED_BYTES: usize = 4 * 1024 * 1024;

/// Ceiling on one encoded operation record.
///
/// This is the number that bounds nested-container amplification: whatever
/// an operation declares inside itself, it only has this many bytes to
/// declare it with, and a reservation that outruns them fails immediately.
pub const MAX_OP_RECORD_BYTES: usize = 32 * 1024;

/// Ceiling on the encoded provenance record, which holds a handful of
/// short strings.
pub const MAX_META_RECORD_BYTES: usize = 4 * 1024;

/// Width of a framing length field.
const LEN_BYTES: usize = 4;

/// Fewest bytes one framed operation can possibly occupy: the length field
/// plus four zero-length strings, a body-mode discriminant and an empty
/// parameter list.
const MIN_FRAMED_OP_BYTES: usize = LEN_BYTES + 6;

/// Multiplier applied to container sizes when charging the footprint
/// budget.
///
/// A `Vec` that grows by doubling can hold up to twice the capacity its
/// length implies, and an allocator adds its own overhead. Charging the
/// logical size alone under-counts exactly the thing an attacker inflates,
/// so containers are charged for the worst case instead.
const CONTAINER_SLACK: usize = 2;

/// Why a table could not be framed, decoded, or accepted.
///
/// The resource limits are separate variants rather than one message: they
/// have different causes and different remedies, and reporting "too many
/// operations" for an operation with too many parameters sends the user
/// looking in the wrong place.
#[derive(Debug, thiserror::Error)]
#[error("{}", self.reason())]
pub enum TableError {
    /// The framing itself is wrong: truncated, a length that cannot fit,
    /// or bytes left over.
    Framing(String),
    /// A record's bytes are not a value this build understands.
    Decode(String),
    /// More operations than [`MAX_CACHED_OPS`].
    TooManyOperations { count: usize, limit: usize },
    /// One operation's encoded record exceeds [`MAX_OP_RECORD_BYTES`].
    OperationTooLarge {
        /// Position in the table (0-based), for a document with no usable
        /// name to report yet.
        index: usize,
        bytes: usize,
        limit: usize,
    },
    /// The table decodes to more memory than [`MAX_DECODED_BYTES`].
    TooMuchMemory { footprint: usize, limit: usize },
}

impl TableError {
    /// One sentence naming the cause and the actual numbers.
    pub fn reason(&self) -> String {
        match self {
            Self::Framing(reason) => reason.clone(),
            Self::Decode(reason) => format!("a record could not be decoded ({reason})"),
            Self::TooManyOperations { count, limit } => format!(
                "it declares {count} operations, more than the {limit} the cache format allows"
            ),
            Self::OperationTooLarge {
                index,
                bytes,
                limit,
            } => format!(
                "operation #{index} encodes to {bytes} bytes, more than the {limit} \
                 the cache format allows for one operation (too many parameters or \
                 enumerated values)"
            ),
            Self::TooMuchMemory { footprint, limit } => format!(
                "its operations would occupy about {footprint} bytes in memory, more \
                 than the {limit} the cache format allows"
            ),
        }
    }
}

impl TableError {
    /// What the user can actually do about it.
    ///
    /// One per cause: "trim the document" and "trim one operation's
    /// parameters" send someone to different places, and a corrupt file is
    /// not a size problem at all.
    pub fn remedy(&self) -> &'static str {
        match self {
            Self::Framing(_) | Self::Decode(_) => {
                "run `otl spec sync` to rebuild the cache, or `otl spec reset` to drop it"
            }
            Self::TooManyOperations { .. } => {
                "check that --url or --spec points at the intended document; a real \
                 API has far fewer operations than this, and a legitimately huge one \
                 has to be cut down to the operations you need"
            }
            Self::OperationTooLarge { .. } => {
                "one operation carries more parameters or enumerated values than the \
                 cache format holds; check the document, or remove that operation \
                 from it"
            }
            Self::TooMuchMemory { .. } => {
                "the document is too large to keep in the cache; check that --url or \
                 --spec points at the intended document, or cut it down to the \
                 operations you need"
            }
        }
    }
}

/// Bincode configuration for one record: fixed, and limited to the record
/// it is decoding.
fn record_config(limit_bytes: usize) -> impl bincode::config::Config {
    // The generic parameter has to be a constant, so the two record sizes
    // are expressed as the same maximum; the slice handed to the decoder is
    // what actually bounds each one.
    let _ = limit_bytes;
    bincode::config::standard().with_limit::<MAX_OP_RECORD_BYTES>()
}

/// Frame a table into the body of a cache file.
///
/// Applies every rule [`decode_table`] applies, so a table that is written
/// can always be read back.
pub(super) fn encode_table(meta: &CacheMeta, ops: &[OpSpec]) -> Result<Vec<u8>, TableError> {
    check_table(ops)?;
    let mut body = Vec::new();
    let meta_record = encode_record(meta, MAX_META_RECORD_BYTES)?;
    push_record(&mut body, &meta_record);
    push_len(&mut body, ops.len());
    for (index, op) in ops.iter().enumerate() {
        let record = encode_record(op, MAX_OP_RECORD_BYTES)?;
        if record.len() > MAX_OP_RECORD_BYTES {
            return Err(TableError::OperationTooLarge {
                index,
                bytes: record.len(),
                limit: MAX_OP_RECORD_BYTES,
            });
        }
        push_record(&mut body, &record);
    }
    Ok(body)
}

/// The rules that do not depend on the encoding: how many operations, and
/// how much memory they come to.
pub(super) fn check_table(ops: &[OpSpec]) -> Result<(), TableError> {
    if ops.len() > MAX_CACHED_OPS {
        return Err(TableError::TooManyOperations {
            count: ops.len(),
            limit: MAX_CACHED_OPS,
        });
    }
    let footprint = ops
        .iter()
        .fold(0usize, |total, op| total.saturating_add(footprint_of(op)));
    if footprint > MAX_DECODED_BYTES {
        return Err(TableError::TooMuchMemory {
            footprint,
            limit: MAX_DECODED_BYTES,
        });
    }
    Ok(())
}

/// Decode a framed table, refusing anything that would cost more than the
/// budgets allow.
pub(super) fn decode_table(body: &[u8]) -> Result<(CacheMeta, Vec<OpSpec>), TableError> {
    let mut cursor = 0usize;
    let meta_record = take_record(body, &mut cursor, MAX_META_RECORD_BYTES)?;
    let meta: CacheMeta = decode_record(meta_record)?;

    let count = take_len(body, &mut cursor)?;
    if count > MAX_CACHED_OPS {
        return Err(TableError::TooManyOperations {
            count,
            limit: MAX_CACHED_OPS,
        });
    }
    // Against the bytes that ACTUALLY remain, not against the format's
    // maximum: a short body declaring thousands of operations is a lie, and
    // reserving for it would be the allocation the count check exists to
    // prevent.
    let remaining = body.len().saturating_sub(cursor);
    if count > remaining / MIN_FRAMED_OP_BYTES {
        return Err(TableError::Framing(format!(
            "it declares {count} operations but only {remaining} bytes follow"
        )));
    }

    let mut ops: Vec<OpSpec> = Vec::with_capacity(count);
    let mut footprint = 0usize;
    for _ in 0..count {
        let record = take_record(body, &mut cursor, MAX_OP_RECORD_BYTES)?;
        let op: OpSpec = decode_record(record)?;
        footprint = footprint.saturating_add(footprint_of(&op));
        if footprint > MAX_DECODED_BYTES {
            return Err(TableError::TooMuchMemory {
                footprint,
                limit: MAX_DECODED_BYTES,
            });
        }
        ops.push(op);
    }
    if cursor != body.len() {
        return Err(TableError::Framing(format!(
            "it carries {} unexpected trailing bytes",
            body.len() - cursor
        )));
    }
    Ok((meta, ops))
}

/// Approximate heap footprint of one decoded operation.
///
/// A budget, not an accounting - but one that must never UNDER-count what
/// an attacker can multiply, which is why containers are charged for their
/// worst-case capacity ([`CONTAINER_SLACK`]) rather than their length.
fn footprint_of(op: &OpSpec) -> usize {
    let text = op.name.len() + op.path.len() + op.summary.len() + op.content_type.len();
    let params: usize = op.params.iter().map(footprint_of_param).sum();
    size_of::<OpSpec>()
        + text
        + CONTAINER_SLACK * (op.params.len() * size_of::<ParamSpec>())
        + params
}

fn footprint_of_param(param: &ParamSpec) -> usize {
    let values: usize = param.enum_values.iter().map(|value| value.len()).sum();
    param.name.len()
        + param.format.len()
        + values
        + CONTAINER_SLACK * (param.enum_values.len() * size_of::<std::borrow::Cow<'static, str>>())
}

/// Encode one record, refusing a value that does not fit its own limit.
fn encode_record<T: serde::Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, TableError> {
    let encoded = bincode::serde::encode_to_vec(value, record_config(limit))
        .map_err(|error| TableError::Decode(error.to_string()))?;
    if encoded.len() > limit {
        return Err(TableError::Framing(format!(
            "a record encodes to {} bytes, over its {limit} byte limit",
            encoded.len()
        )));
    }
    Ok(encoded)
}

/// Decode one record from exactly its own bytes.
fn decode_record<T: serde::de::DeserializeOwned>(record: &[u8]) -> Result<T, TableError> {
    let (value, consumed) =
        bincode::serde::decode_from_slice::<T, _>(record, record_config(record.len()))
            .map_err(|error| TableError::Decode(error.to_string()))?;
    if consumed != record.len() {
        return Err(TableError::Framing(format!(
            "a record carries {} unexpected trailing bytes",
            record.len() - consumed
        )));
    }
    Ok(value)
}

fn push_len(body: &mut Vec<u8>, value: usize) {
    // Every length written here has already been bounded well below u32.
    let narrowed = u32::try_from(value).unwrap_or(u32::MAX);
    body.extend_from_slice(&narrowed.to_le_bytes());
}

fn push_record(body: &mut Vec<u8>, record: &[u8]) {
    push_len(body, record.len());
    body.extend_from_slice(record);
}

/// Read a length field, advancing the cursor.
fn take_len(body: &[u8], cursor: &mut usize) -> Result<usize, TableError> {
    let end = cursor
        .checked_add(LEN_BYTES)
        .ok_or_else(|| TableError::Framing("its framing overflows".to_string()))?;
    let bytes = body
        .get(*cursor..end)
        .ok_or_else(|| TableError::Framing("it is truncated inside a length field".to_string()))?;
    let mut field = [0u8; LEN_BYTES];
    field.copy_from_slice(bytes);
    *cursor = end;
    Ok(u32::from_le_bytes(field) as usize)
}

/// Read a length-prefixed record, advancing the cursor.
///
/// The length is checked against its limit AND against the bytes that
/// remain, before the slice is taken - so a forged length can neither
/// allocate nor read past the end.
fn take_record<'a>(
    body: &'a [u8],
    cursor: &mut usize,
    limit: usize,
) -> Result<&'a [u8], TableError> {
    let len = take_len(body, cursor)?;
    if len > limit {
        return Err(TableError::Framing(format!(
            "a record declares {len} bytes, over its {limit} byte limit"
        )));
    }
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| TableError::Framing("its framing overflows".to_string()))?;
    let record = body
        .get(*cursor..end)
        .ok_or_else(|| TableError::Framing("it is truncated inside a record".to_string()))?;
    *cursor = end;
    Ok(record)
}
