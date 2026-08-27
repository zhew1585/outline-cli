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
//! decoded from its own slice with its own byte limit. What that bounds,
//! precisely:
//!
//! - the number of records, and the bytes any one of them may declare;
//! - the total decoded footprint of the records ACCEPTED so far.
//!
//! What it does not bound, and the mechanism worth remembering: bincode's
//! serde bridge charges its byte budget for what it CONSUMES and never
//! calls `claim_container_read`, while serde's `Vec` reserves
//! `min(declared, 1 MiB / size_of::<T>())` of its own accord. Those two
//! are independent, so a container that lies about its length reserves up
//! to serde's cap regardless of how small the record is.
//!
//! How many such reservations can be live at once is bounded by the DEPTH
//! of the path being decoded, not by how many containers the type has.
//! `OpSpec` has three (`params`, a parameter's `enum_values`, and
//! `response_fields`), but a container that lies never completes, so the
//! container AFTER it is never reached: the deepest reachable pair is the
//! parameter list and one parameter's enum list. Both shapes are measured
//! in `crates/otl/tests/memory_bounds.rs` - all three lying, and a
//! complete parameter list followed by a lying field list - and the larger
//! of the two is the number that matters.
//!
//! This is written as reasoning rather than a constant because the IR gains
//! fields: `response_fields` arrived after this accounting did. When the
//! next one arrives, charge it in [`footprint_of`] and add its lie to that
//! fixture; the two have to move together or the budget stops seeing what
//! the decoder allocates.
//!
//! That is a fact about the shape of the IR, not something framing can fix
//! from the outside; bounding it would mean owning the decode of every
//! nested type. What framing does is keep it to a small constant instead
//! of letting it scale with the file. The measured figures - for that
//! exact shape, and for it inside a full table - are in
//! `crates/otl/tests/memory_bounds.rs`.
//!
//! # The resulting bound
//!
//! Deliberately NOT stated as a number here. Twice this module carried an
//! arithmetic bound in prose, and twice an independent measurement showed
//! it too low - once by a factor of six, once by a whole missing term. A
//! wrong bound is worse than none: it stops the next reader from checking.
//!
//! What is stated instead is what ENFORCES the bound, each with its own
//! constant, so a reader can find the check rather than trust a sum:
//!
//! - the file never exceeds [`super::cache::MAX_CACHE_FILE_BYTES`] and is
//!   read into a buffer reserved from its stat'd size (no doubling);
//! - the table's own `Vec` is charged against the budget BEFORE it is
//!   reserved, and its length cannot exceed [`MAX_CACHED_OPS`] nor what
//!   the remaining bytes could encode;
//! - the accepted operations are charged as they arrive, against
//!   [`MAX_DECODED_BYTES`], with containers charged at their worst-case
//!   capacity ([`CONTAINER_SLACK`]);
//! - the record being decoded is confined to [`MAX_OP_RECORD_BYTES`] and
//!   decoded with a byte budget of its own, so a container that lies about
//!   its length has only a record's worth of bytes to lie with.
//!
//! The actual peaks are MEASURED, with a heap profiler, in
//! `crates/otl/tests/memory_bounds.rs` - including the worst record shape
//! and a cache declaring the maximum number of operations. That test is
//! the bound; this list is how it is achieved.
//!
//! Both directions use the same rules: [`encode_table`] refuses to write
//! anything [`decode_ops`] would refuse to read.

use std::mem::size_of;

use engine::ir::{FieldSpec, OpSpec, ParamSpec};

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
        /// Position in the table (0-based). Always available, and the
        /// only identifier a cache being DECODED has: the name is inside
        /// the record that was just refused.
        index: usize,
        /// The operation's name, when it is known (encoding a table the
        /// caller already validated). Safe to print: it has been through
        /// the text rules.
        name: Option<String>,
        /// Bytes the record came to.
        bytes: usize,
        /// The limit it exceeded.
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
                name,
                bytes,
                limit,
            } => {
                let which = match name {
                    Some(name) => format!("operation {name:?}"),
                    None => format!("operation #{index}"),
                };
                format!(
                    "{which} encodes to {bytes} bytes, more than the {limit} the cache \
                     format allows for one operation (too many parameters or \
                     enumerated values)"
                )
            }
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

/// How much bincode's own byte budget may exceed a record's size cap.
///
/// The budget counts consumed bytes PLUS the claims the decoder makes for
/// containers, so it has to be larger than the record itself or a
/// legitimate record at the cap would be refused: a real 32,008-byte
/// operation with 4000 parameters fails at a 32 KiB budget and decodes at
/// four times that. Any multiple works for a legitimate record; what
/// matters is that the budget stays a small multiple of the record, so a
/// forged container length is still refused before it can reserve.
const RECORD_BUDGET_FACTOR: usize = 4;

/// Bincode configuration for an operation record.
fn op_record_config() -> impl bincode::config::Config {
    bincode::config::standard().with_limit::<{ MAX_OP_RECORD_BYTES * RECORD_BUDGET_FACTOR }>()
}

/// Bincode configuration for the provenance record, which is far smaller
/// and gets a budget to match rather than sharing the operation one.
fn meta_record_config() -> impl bincode::config::Config {
    bincode::config::standard().with_limit::<{ MAX_META_RECORD_BYTES * RECORD_BUDGET_FACTOR }>()
}

/// Frame a table into the body of a cache file.
///
/// Applies every rule [`decode_ops`] applies, so a table that is written
/// can always be read back.
pub(super) fn encode_table(meta: &CacheMeta, ops: &[OpSpec]) -> Result<Vec<u8>, TableError> {
    check_table(ops)?;
    let mut body = Vec::new();
    let meta_record = encode_record(meta, meta_record_config())?;
    if meta_record.len() > MAX_META_RECORD_BYTES {
        return Err(TableError::Framing(format!(
            "the provenance record encodes to {} bytes, over its {MAX_META_RECORD_BYTES} \
             byte limit",
            meta_record.len()
        )));
    }
    push_record(&mut body, &meta_record);
    push_len(&mut body, ops.len());
    for (index, op) in ops.iter().enumerate() {
        let record = encode_record(op, op_record_config())?;
        // Classified as what it is - one operation carrying more than the
        // format holds - so the remedy can point at that operation's
        // parameters instead of at the document's size.
        if record.len() > MAX_OP_RECORD_BYTES {
            return Err(TableError::OperationTooLarge {
                index,
                name: Some(op.name.to_string()),
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
    // Same accounting as the decoder, the table's own Vec included.
    let footprint = ops.iter().fold(
        ops.len().saturating_mul(size_of::<OpSpec>()),
        |total, op| total.saturating_add(footprint_of(op)),
    );
    if footprint > MAX_DECODED_BYTES {
        return Err(TableError::TooMuchMemory {
            footprint,
            limit: MAX_DECODED_BYTES,
        });
    }
    Ok(())
}

/// Decode the provenance record of a framed table, returning it and the
/// cursor just after it.
///
/// The provenance record and the operations are decoded through SEPARATE
/// entry points, and the split is not cosmetic: the operation records are
/// `bincode`, which is positional and carries no field names, so decoding a
/// table written for another `IR_SCHEMA_VERSION` reads one struct's bytes as
/// another's. It usually errors - and would then be reported as a DAMAGED
/// cache rather than an outdated one, which is a lie about what happened and
/// sends the user looking for corruption - but with the right byte alignment
/// it could also decode into a table that is merely wrong.
///
/// The provenance record is the first thing in the body and says which
/// version wrote it, so the caller checks that BEFORE calling
/// [`decode_ops`]. `crates/otl/tests/ir_upgrade.rs` drives a real v5 cache
/// through that order.
///
/// This record is self-delimiting and length-bounded, so it can be read from
/// a body whose remaining records this build may not understand at all.
pub(super) fn decode_meta(body: &[u8]) -> Result<(CacheMeta, usize), TableError> {
    let mut cursor = 0usize;
    let meta_record = take_record(body, &mut cursor, MAX_META_RECORD_BYTES)?;
    let meta: CacheMeta = decode_record(meta_record, meta_record_config())?;
    Ok((meta, cursor))
}

/// Decode the operation records that follow the provenance record.
pub(super) fn decode_ops(body: &[u8], mut cursor: usize) -> Result<Vec<OpSpec>, TableError> {
    let count = take_len(body, &mut cursor)?;
    let mut footprint = check_declared_count(count, body.len().saturating_sub(cursor))?;
    let mut ops: Vec<OpSpec> = Vec::with_capacity(count);
    for index in 0..count {
        let record = take_op_record(body, &mut cursor, index)?;
        let op: OpSpec = decode_record(record, op_record_config())?;
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
    Ok(ops)
}

/// Vet a declared operation count before anything is reserved for it, and
/// return what reserving it will cost.
///
/// Three ways a count can be refused, and all three happen before the
/// allocation: more than the format allows, more than the bytes that
/// ACTUALLY remain could encode (a short body declaring thousands of
/// operations is simply lying), and more memory than the budget allows for
/// the table's own `Vec` - 8192 slots of `OpSpec` is a megabyte, and it
/// used to be invisible to the very budget that exists to bound it.
fn check_declared_count(count: usize, remaining: usize) -> Result<usize, TableError> {
    if count > MAX_CACHED_OPS {
        return Err(TableError::TooManyOperations {
            count,
            limit: MAX_CACHED_OPS,
        });
    }
    if count > remaining / MIN_FRAMED_OP_BYTES {
        return Err(TableError::Framing(format!(
            "it declares {count} operations but only {remaining} bytes follow"
        )));
    }
    let footprint = count.saturating_mul(size_of::<OpSpec>());
    if footprint > MAX_DECODED_BYTES {
        return Err(TableError::TooMuchMemory {
            footprint,
            limit: MAX_DECODED_BYTES,
        });
    }
    Ok(footprint)
}

/// Approximate heap footprint of one decoded operation.
///
/// A budget, not an accounting - but one that must never UNDER-count what
/// an attacker can multiply, which is why containers are charged for their
/// worst-case capacity ([`CONTAINER_SLACK`]) rather than their length.
fn footprint_of(op: &OpSpec) -> usize {
    let text = op.name.len() + op.path.len() + op.summary.len() + op.content_type.len();
    let params: usize = op.params.iter().map(footprint_of_param).sum();
    // `response_fields` is a container too, and one that arrived later than
    // this accounting did: an uncharged container is exactly the hole this
    // budget exists to close, whoever adds it.
    let fields: usize = op
        .response_fields
        .iter()
        .map(|field| field.name.len() + field.format.len())
        .sum();
    text + CONTAINER_SLACK * (op.params.len() * size_of::<ParamSpec>())
        + params
        + CONTAINER_SLACK * (op.response_fields.len() * size_of::<FieldSpec>())
        + fields
}

fn footprint_of_param(param: &ParamSpec) -> usize {
    let values: usize = param.enum_values.iter().map(|value| value.len()).sum();
    param.name.len()
        + param.format.len()
        + values
        + CONTAINER_SLACK * (param.enum_values.len() * size_of::<std::borrow::Cow<'static, str>>())
}

/// Encode one record.
///
/// Size is NOT checked here: the caller knows which record this is and can
/// say so ("operation #7 is too large" rather than "a record is too
/// large"), which decides what the user is told to do about it.
fn encode_record<T: serde::Serialize>(
    value: &T,
    config: impl bincode::config::Config,
) -> Result<Vec<u8>, TableError> {
    bincode::serde::encode_to_vec(value, config)
        .map_err(|error| TableError::Decode(error.to_string()))
}

/// Decode one record from exactly its own bytes.
fn decode_record<T: serde::de::DeserializeOwned>(
    record: &[u8],
    config: impl bincode::config::Config,
) -> Result<T, TableError> {
    let (value, consumed) = bincode::serde::decode_from_slice::<T, _>(record, config)
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

/// Read one operation record, reporting an over-long one as what it is.
///
/// The generic [`take_record`] would call it a framing problem, which is
/// true but useless: "operation #7 is bigger than the format holds" tells
/// the reader which operation to look at, and its remedy is about
/// parameters rather than about rebuilding the cache. (The name lives
/// inside the record that was just refused, so the position is all a
/// DECODER can offer - the encoder, which has the table in hand, reports
/// the name.)
fn take_op_record<'a>(
    body: &'a [u8],
    cursor: &mut usize,
    index: usize,
) -> Result<&'a [u8], TableError> {
    let mut probe = *cursor;
    let declared = take_len(body, &mut probe)?;
    if declared > MAX_OP_RECORD_BYTES {
        return Err(TableError::OperationTooLarge {
            index,
            name: None,
            bytes: declared,
            limit: MAX_OP_RECORD_BYTES,
        });
    }
    take_record(body, cursor, MAX_OP_RECORD_BYTES)
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
