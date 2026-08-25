//! Outline's pagination convention.
//!
//! The engine is convention-free: it paginates only what the caller
//! describes. Every Outline-specific name therefore lives here, in the UX
//! layer, and is handed to the engine as a [`PaginationSpec`].
//!
//! Outline's convention (the vendored spec's shared `Pagination` schema):
//! requests take `offset`/`limit`, list responses carry rows in `data` and
//! echo the applied paging under `pagination`.

use std::borrow::Cow;

use engine::{OffsetEcho, OpSpec, PaginationSpec};

/// Request parameter carrying the start offset.
pub const OFFSET_PARAM: &str = "offset";
/// Request parameter carrying the page size.
pub const LIMIT_PARAM: &str = "limit";
/// JSON pointer to the rows of one page.
const ITEMS_POINTER: &str = "/data";
/// JSON pointer to the page size the server applied.
const PAGE_SIZE_POINTER: &str = "/pagination/limit";
/// JSON pointer to the offset the server applied.
///
/// Used in [`OffsetEcho::ValidateIfPresent`] mode, which splits the two
/// cases that deserve different answers:
///
/// - the echo is there but disagrees with the offset we asked for (or is
///   there in an uncomparable form): the server is contradicting itself,
///   and merging its pages would produce duplicated or skipped rows. That
///   is wrong DATA, so the command fails.
/// - the echo is missing entirely: the vendored spec documents
///   `pagination` on paginated responses, but a community spec can be
///   wrong or drift from a given instance. Bricking on such an endpoint
///   would be a worse outcome than paging by our own offset counter, so
///   the rows are returned with a notice that boundaries are unverified.
///
/// `Required` is deliberately not used here for that second reason;
/// `Ignored` is deliberately not used because it would make the
/// contradiction case undetectable.
const OFFSET_POINTER: &str = "/pagination/offset";
/// JSON pointer to the page-local paging echo, dropped after merging.
const PAGE_METADATA_POINTER: &str = "/pagination";
/// Rows requested per page. Outline clamps this to its own maximum and
/// reports the applied value, which the engine then follows.
const PAGE_SIZE: u64 = 100;
/// Safety cap on pages fetched for one command (10_000 rows at
/// [`PAGE_SIZE`]) so a runaway list cannot loop forever.
const MAX_PAGES: u32 = 100;

/// The pagination descriptor for `op`, or `None` if it is not a list
/// operation.
///
/// An operation is list-shaped when the compiled IR says it accepts both
/// paging parameters - the vendored Outline spec composes them into every
/// list request via its shared `Pagination` schema.
pub fn spec_for(op: &OpSpec) -> Option<PaginationSpec> {
    let paged = op.param(OFFSET_PARAM).is_some() && op.param(LIMIT_PARAM).is_some();
    paged.then(outline_spec)
}

/// Outline's pagination convention as an engine descriptor.
fn outline_spec() -> PaginationSpec {
    PaginationSpec {
        offset_param: Cow::Borrowed(OFFSET_PARAM),
        limit_param: Cow::Borrowed(LIMIT_PARAM),
        items_pointer: Cow::Borrowed(ITEMS_POINTER),
        page_size_pointer: Some(Cow::Borrowed(PAGE_SIZE_POINTER)),
        offset_echo: OffsetEcho::ValidateIfPresent {
            pointer: Cow::Borrowed(OFFSET_POINTER),
        },
        stale_metadata_pointer: Some(Cow::Borrowed(PAGE_METADATA_POINTER)),
        page_size: PAGE_SIZE,
        max_pages: MAX_PAGES,
    }
}
