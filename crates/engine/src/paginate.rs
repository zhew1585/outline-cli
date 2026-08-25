//! Caller-described auto-pagination.
//!
//! The engine knows the *shape* of a paginated exchange but none of the
//! wire vocabulary: every field and parameter name comes from a
//! [`PaginationSpec`] supplied by the caller. There are deliberately no
//! literal request-parameter or envelope field names in this module, so no
//! particular API vendor's convention is baked into the engine.
//!
//! An operation is paginated exactly when the caller passes a descriptor
//! for it; the engine never sniffs operation or parameter names.
//!
//! The channel never truncates silently: any early stop is reported
//! through [`Truncation`], and a mid-stream response that does not match
//! the descriptor is a hard error rather than a short read.

use std::borrow::Cow;

use serde_json::Value;

use crate::error::EngineError;

/// Fallback page size when a descriptor asks for none.
const MIN_PAGE_SIZE: u64 = 1;

/// How one API expresses offset/limit pagination.
///
/// All field references are [RFC 6901] JSON pointers into the response
/// body (`"/data"`, `"/pagination/limit"`, `"/meta/page/size"`, ...), so
/// any envelope layout can be described without engine changes.
///
/// [RFC 6901]: https://datatracker.ietf.org/doc/html/rfc6901
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaginationSpec {
    /// Request-body parameter carrying the start offset.
    pub offset_param: Cow<'static, str>,
    /// Request-body parameter carrying the page size.
    pub limit_param: Cow<'static, str>,
    /// Pointer to the response array holding one page of rows.
    pub items_pointer: Cow<'static, str>,
    /// Pointer to the page size the server actually applied, if the API
    /// reports one. Without this hint the engine cannot tell a clamped
    /// page from the last page, and keeps fetching until an empty page.
    pub page_size_pointer: Option<Cow<'static, str>>,
    /// How to treat the offset the server reports having applied.
    pub offset_echo: OffsetEcho,
    /// Pointer to page-local metadata that describes only the first page
    /// (offset/limit echoes). It is removed from the merged envelope so
    /// consumers cannot mistake it for the merged result's own paging
    /// state.
    pub stale_metadata_pointer: Option<Cow<'static, str>>,
    /// Page size requested per fetch.
    pub page_size: u64,
    /// Safety cap on automatically fetched pages.
    pub max_pages: u32,
}

/// How much the engine trusts the offset a server reports applying.
///
/// The three states answer two independent questions - does this API
/// report the applied offset at all, and is that report guaranteed? - so
/// they are spelled out at the call site rather than inferred:
///
/// - a report that CONTRADICTS the request (or is there but unusable) is
///   always a hard error, in every mode that looks: continuing would merge
///   rows the engine cannot place;
/// - a report that is ABSENT is a different situation. A spec can be wrong
///   or drift, so an endpoint may simply not send the envelope. Refusing to
///   work at all is a worse outcome than paging by the local counter and
///   saying so, which is what [`Self::ValidateIfPresent`] does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffsetEcho {
    /// Never look at any reported offset; page purely by the local counter.
    Ignored,
    /// Validate the report when the server sends one, and fall back to the
    /// local counter (flagging the page as unconfirmed) when it does not.
    ValidateIfPresent {
        /// Pointer to the offset the server applied.
        pointer: Cow<'static, str>,
    },
    /// The API guarantees the report: a page without a usable one is a
    /// protocol violation and fails the fetch.
    Required {
        /// Pointer to the offset the server applied.
        pointer: Cow<'static, str>,
    },
}

impl OffsetEcho {
    /// The pointer this mode reads, if it reads one.
    fn pointer(&self) -> Option<&str> {
        match self {
            Self::Ignored => None,
            Self::ValidateIfPresent { pointer } | Self::Required { pointer } => Some(pointer),
        }
    }

    /// Whether a missing report is a hard error.
    fn requires_echo(&self) -> bool {
        matches!(self, Self::Required { .. })
    }
}

impl PaginationSpec {
    /// Check the descriptor before any network I/O.
    ///
    /// Every pointer must be strict RFC 6901, and the stale-metadata
    /// pointer must not overlap the items pointer - deleting metadata must
    /// never be able to delete the merged results.
    pub fn validate(&self) -> Result<(), EngineError> {
        let items = validate_pointer("items_pointer", &self.items_pointer)?;
        if let Some(pointer) = self.offset_echo.pointer() {
            validate_pointer("offset_echo pointer", pointer)?;
        }
        for (name, pointer) in [("page_size_pointer", &self.page_size_pointer)] {
            if let Some(pointer) = pointer {
                validate_pointer(name, pointer)?;
            }
        }
        if let Some(stale) = &self.stale_metadata_pointer {
            let stale_tokens = validate_pointer("stale_metadata_pointer", stale)?;
            if overlaps(&stale_tokens, &items) {
                return Err(EngineError::InvalidPaginationSpec {
                    reason: format!(
                        "stale_metadata_pointer `{stale}` overlaps \
                         items_pointer `{}`: removing page metadata would \
                         delete the merged results",
                        self.items_pointer
                    ),
                });
            }
        }
        Ok(())
    }
}

/// The outcome of executing one operation through the request channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    /// The response envelope; for merged results, the descriptor's items
    /// pointer holds every fetched row.
    pub value: Value,
    /// Present exactly when the result may be incomplete - callers MUST
    /// surface this to the user (never truncate silently).
    pub truncation: Option<Truncation>,
    /// True when at least one page arrived without the offset report the
    /// descriptor asked for, so page boundaries rest on the local counter
    /// alone. Callers MUST tell the user (once): the rows are usable, but
    /// unverified.
    pub offset_unconfirmed: bool,
}

impl Fetched {
    /// A complete, non-paginated result.
    pub fn complete(value: Value) -> Self {
        Self {
            value,
            truncation: None,
            offset_unconfirmed: false,
        }
    }
}

/// Report that a result may be incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncation {
    /// Number of rows actually returned.
    pub fetched: u64,
    /// Why fetching stopped early.
    pub cause: TruncationCause,
}

/// Why a result may be incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationCause {
    /// The caller's item cap was reached while more rows existed. This is
    /// definite truncation: the request asked for one row beyond the cap
    /// and got it.
    MaxItems,
    /// [`PaginationSpec::max_pages`] stopped the fetch loop. The data may
    /// have ended exactly at the cap, so this is only *possible*
    /// truncation.
    PageLimit,
    /// The caller pinned the page size itself, so exactly one page was
    /// fetched and it came back full - more rows may exist.
    ManualPage,
    /// The offset counter cannot advance any further (u64 exhausted).
    OffsetSpaceExhausted,
}

impl TruncationCause {
    /// Whether truncation is proven (as opposed to merely possible).
    ///
    /// Only [`Self::MaxItems`] is proven: that path deliberately requests
    /// one row beyond the cap and receives it. Every other cause is a stop
    /// condition that the data may have met exactly - including
    /// [`Self::OffsetSpaceExhausted`], where failing to build the next
    /// offset says nothing about whether the server had more rows.
    pub fn is_definite(self) -> bool {
        matches!(self, Self::MaxItems)
    }
}

/// The offset to start fetching from: the caller's own offset argument
/// when present, otherwise zero.
///
/// A malformed offset is rejected before any request is sent. The
/// offending value is deliberately not echoed (arbitrary user input must
/// not be reflected into diagnostics).
pub(crate) fn start_offset(
    spec: &PaginationSpec,
    args: &[(String, String)],
) -> Result<u64, EngineError> {
    let Some((_, raw)) = args.iter().find(|(key, _)| *key == spec.offset_param) else {
        return Ok(0);
    };
    // A bad offset is an ordinary parameter-validation failure, reported
    // like any other one (and never echoing the offending value).
    raw.trim()
        .parse::<u64>()
        .map_err(|_| EngineError::InvalidParamValue {
            name: spec.offset_param.to_string(),
            reason: "expected a non-negative whole number".to_string(),
        })
}

/// Whether the caller pinned the page size itself (manual paging).
pub(crate) fn has_manual_page_size(spec: &PaginationSpec, args: &[(String, String)]) -> bool {
    args.iter().any(|(key, _)| *key == spec.limit_param)
}

/// One page that has passed every descriptor check: its rows, taken out
/// of the envelope, plus the page size the server said it applied.
pub(crate) struct AcceptedPage {
    /// Rows removed from the response envelope.
    pub items: Vec<Value>,
    /// The server-applied page size, when it reported a usable one.
    pub capacity: Option<u64>,
    /// True when the descriptor asked for an offset report and the server
    /// sent none (only reachable under
    /// [`OffsetEcho::ValidateIfPresent`], which tolerates that).
    pub offset_unconfirmed: bool,
}

/// Validate one response against the descriptor and take its rows out.
///
/// EVERY page the request channel receives goes through here - each page
/// of an auto-paginated fetch and the single page of a manually paged
/// request alike. It is the only way to obtain rows from a response, so no
/// branch can return data that skipped the offset-hint and items-pointer
/// invariants. `page` is 1-based, for messages only.
pub(crate) fn accept_page(
    spec: &PaginationSpec,
    response: &mut Value,
    requested_offset: u64,
    page: u32,
    already_fetched: usize,
) -> Result<AcceptedPage, EngineError> {
    let offset_unconfirmed = check_offset_echo(spec, response, requested_offset)?;
    let capacity = page_capacity(spec, response);
    // A response with no array at the items pointer does not match the
    // descriptor the caller chose. Returning it - verbatim on page one or
    // as partial rows later - would report an unknown result shape as a
    // complete success, so it is always an error.
    let items =
        take_items(response, &spec.items_pointer).ok_or_else(|| EngineError::Pagination {
            reason: format!(
                "page {page} of the result has no array at `{}`; aborting with \
             {already_fetched} rows already fetched rather than reporting a \
             partial result as complete",
                spec.items_pointer
            ),
        })?;
    Ok(AcceptedPage {
        items,
        capacity,
        offset_unconfirmed,
    })
}

/// Put a manually paged response's rows back where they came from.
pub(crate) fn restore_items(spec: &PaginationSpec, value: &mut Value, items: Vec<Value>) {
    if let Some(slot) = value.pointer_mut(&spec.items_pointer) {
        *slot = Value::Array(items);
    }
}

/// Report whether a single manually-paged response may be truncated.
///
/// Fullness is judged against the page size the SERVER says it applied,
/// because a server is free to clamp the requested size (comparing against
/// the request would then miss a full page entirely). When no trustworthy
/// applied size is available the result is reported as possibly truncated:
/// staying silent would be exactly the silent truncation this module
/// exists to prevent.
pub(crate) fn manual_page_truncation(page: &AcceptedPage) -> Option<Truncation> {
    let received = page.items.len() as u64;
    // Some(applied): the server reported the page size it used, so a page
    // that did not fill it is definitely the last one.
    // None: no usable hint. The requested size cannot settle it (the
    // server may have clamped), so warn conservatively.
    let looks_full = page.capacity.is_none_or(|applied| received >= applied);
    looks_full.then_some(Truncation {
        fetched: received,
        cause: TruncationCause::ManualPage,
    })
}

/// Fetch every page via `fetch_page(offset, limit)` and merge the rows.
///
/// Stopping rules, in order: the item cap (`max_items`), an empty page, a
/// page shorter than the server-reported page size, and finally
/// [`PaginationSpec::max_pages`]. Without a server page-size hint a short
/// page proves nothing (the server may have clamped the page), so the
/// fetch continues until an empty page arrives.
pub(crate) fn fetch_all_pages<F>(
    spec: &PaginationSpec,
    mut fetch_page: F,
    start: u64,
    max_items: Option<u64>,
) -> Result<Fetched, EngineError>
where
    F: FnMut(u64, u64) -> Result<Value, EngineError>,
{
    let mut merged: Vec<Value> = Vec::new();
    let mut envelope: Option<Value> = None;
    let mut offset = start;
    let mut unconfirmed = false;

    for page in 0..spec.max_pages {
        let request_limit = page_request_limit(spec, max_items, merged.len() as u64);
        let mut response = fetch_page(offset, request_limit)?;
        let AcceptedPage {
            items,
            capacity,
            offset_unconfirmed,
        } = accept_page(spec, &mut response, offset, page + 1, merged.len())?;
        unconfirmed |= offset_unconfirmed;

        let received = items.len() as u64;
        if envelope.is_none() {
            envelope = Some(response);
        }
        merged.extend(items);

        if let Some(cut) = truncate_to_cap(&mut merged, max_items) {
            return Ok(finish(spec, envelope, merged, Some(cut), unconfirmed));
        }
        if received == 0 || capacity.is_some_and(|capacity| received < capacity) {
            return Ok(finish(spec, envelope, merged, None, unconfirmed));
        }
        let Some(next) = offset.checked_add(received) else {
            let truncation = Truncation {
                fetched: merged.len() as u64,
                cause: TruncationCause::OffsetSpaceExhausted,
            };
            return Ok(finish(
                spec,
                envelope,
                merged,
                Some(truncation),
                unconfirmed,
            ));
        };
        offset = next;
    }

    // Only reachable when every allowed page came back full.
    let truncation = Truncation {
        fetched: merged.len() as u64,
        cause: TruncationCause::PageLimit,
    };
    Ok(finish(
        spec,
        envelope,
        merged,
        Some(truncation),
        unconfirmed,
    ))
}

/// Assemble the merged envelope: rows at the items pointer, page-local
/// metadata dropped (it described the first page only).
fn finish(
    spec: &PaginationSpec,
    envelope: Option<Value>,
    merged: Vec<Value>,
    truncation: Option<Truncation>,
    offset_unconfirmed: bool,
) -> Fetched {
    let mut value = envelope.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if let Some(slot) = value.pointer_mut(&spec.items_pointer) {
        *slot = Value::Array(merged);
    }
    // Validation guarantees this path cannot reach the merged rows.
    if let Some(pointer) = &spec.stale_metadata_pointer {
        if let Ok(path) = validate_pointer("stale_metadata_pointer", pointer) {
            remove_at_path(&mut value, &path);
        }
    }
    Fetched {
        value,
        truncation,
        offset_unconfirmed,
    }
}

/// Check the offset the server reports against the one requested.
///
/// Returns whether the page had to be accepted unconfirmed (no report,
/// under [`OffsetEcho::ValidateIfPresent`]).
///
/// A report that exists but contradicts the request - or is there in a form
/// that cannot be compared - is always fatal: advancing by the received
/// count would then skip or duplicate rows, and wrong data must never be
/// presented as a result. A report that is simply absent is tolerated
/// unless the descriptor declares it [`OffsetEcho::Required`].
fn check_offset_echo(
    spec: &PaginationSpec,
    response: &Value,
    requested: u64,
) -> Result<bool, EngineError> {
    let Some(pointer) = spec.offset_echo.pointer() else {
        return Ok(false);
    };
    let unusable = |detail: &str| EngineError::Pagination {
        reason: format!(
            "server reported {detail} at `{pointer}`, so the requested \
             offset {requested} cannot be confirmed; aborting rather than \
             skipping or duplicating rows"
        ),
    };
    // A JSON null is the wire's way of saying "no value", so it counts as
    // absent rather than as a contradictory report.
    let reported = match response.pointer(pointer) {
        None | Some(Value::Null) => {
            if spec.offset_echo.requires_echo() {
                return Err(unusable("no offset"));
            }
            return Ok(true);
        }
        Some(value) => value
            .as_u64()
            .ok_or_else(|| unusable("an unusable offset"))?,
    };
    if reported == requested {
        return Ok(false);
    }
    Err(EngineError::Pagination {
        reason: format!(
            "server applied offset {reported} for requested offset \
             {requested}; aborting rather than skipping or duplicating rows"
        ),
    })
}

/// Cut `merged` down to `max_items` if it overflowed the cap.
///
/// Each page requests one row beyond the remaining budget, so an overflow
/// here is positive proof that more rows exist on the server.
fn truncate_to_cap(merged: &mut Vec<Value>, max_items: Option<u64>) -> Option<Truncation> {
    let cap = max_items?;
    if (merged.len() as u64) <= cap {
        return None;
    }
    merged.truncate(usize::try_from(cap).unwrap_or(usize::MAX));
    Some(Truncation {
        fetched: cap,
        cause: TruncationCause::MaxItems,
    })
}

/// The page size to request next.
///
/// With an item cap in play this asks for `remaining + 1`: receiving that
/// extra row is definitive evidence of truncation without a probe request.
fn page_request_limit(spec: &PaginationSpec, max_items: Option<u64>, fetched: u64) -> u64 {
    let page_size = spec.page_size.max(MIN_PAGE_SIZE);
    match max_items {
        Some(cap) => page_size
            .min(cap.saturating_sub(fetched).saturating_add(1))
            .max(MIN_PAGE_SIZE),
        None => page_size,
    }
}

/// The server-applied page size, when the API reports a usable one. A page
/// shorter than this proves the collection is exhausted; without it, only
/// an empty page does.
fn page_capacity(spec: &PaginationSpec, response: &Value) -> Option<u64> {
    spec.page_size_pointer
        .as_ref()
        .and_then(|pointer| response.pointer(pointer))
        .and_then(Value::as_u64)
        .filter(|&size| size > 0)
}

/// Move the array of rows at `pointer` out of `value`, leaving it empty.
fn take_items(value: &mut Value, pointer: &str) -> Option<Vec<Value>> {
    match value.pointer_mut(pointer)? {
        Value::Array(items) => Some(std::mem::take(items)),
        _ => None,
    }
}

/// Validate a JSON pointer as strict RFC 6901 and return its decoded
/// reference tokens.
///
/// The empty pointer denotes the whole document; any other pointer must
/// start with `/`. Within a token, `~` may only be followed by `0` or `1`.
/// serde_json is lenient about stray escapes, which would silently change
/// which node a descriptor designates.
fn validate_pointer(name: &str, pointer: &str) -> Result<Vec<String>, EngineError> {
    let invalid = |reason: &str| EngineError::InvalidPaginationSpec {
        reason: format!("invalid {name} `{pointer}`: {reason}"),
    };
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let Some(body) = pointer.strip_prefix('/') else {
        return Err(invalid(
            "a JSON pointer must be empty or start with `/` (RFC 6901)",
        ));
    };
    body.split('/')
        .map(|token| {
            decode_pointer_token(token).ok_or_else(|| {
                invalid("`~` must be followed by `0` or `1` in a JSON pointer (RFC 6901)")
            })
        })
        .collect()
}

/// Decode the RFC 6901 escapes `~1` (`/`) and `~0` (`~`), rejecting any
/// other escape sequence (including a trailing `~`).
fn decode_pointer_token(token: &str) -> Option<String> {
    let mut decoded = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c != '~' {
            decoded.push(c);
            continue;
        }
        match chars.next() {
            Some('0') => decoded.push('~'),
            Some('1') => decoded.push('/'),
            _ => return None,
        }
    }
    Some(decoded)
}

/// Whether two token paths designate overlapping nodes: equal, or one an
/// ancestor of the other.
fn overlaps(left: &[String], right: &[String]) -> bool {
    let shared = left.len().min(right.len());
    left[..shared] == right[..shared]
}

/// Remove whatever a validated token path designates. A path to the
/// document root is ignored (there is nothing to remove it from).
fn remove_at_path(value: &mut Value, path: &[String]) {
    let Some((token, parents)) = path.split_last() else {
        return;
    };
    let mut node = value;
    for parent in parents {
        node = match node {
            Value::Object(map) => match map.get_mut(parent.as_str()) {
                Some(child) => child,
                None => return,
            },
            Value::Array(items) => match array_index(parent).and_then(|i| items.get_mut(i)) {
                Some(child) => child,
                None => return,
            },
            _ => return,
        };
    }
    match node {
        Value::Object(map) => {
            map.remove(token.as_str());
        }
        Value::Array(items) => {
            if let Some(index) = array_index(token).filter(|&i| i < items.len()) {
                items.remove(index);
            }
        }
        _ => {}
    }
}

/// Parse an RFC 6901 array index: digits only, and no leading zeros
/// (`0` itself excepted). `-` (past the end) designates no element.
fn array_index(token: &str) -> Option<usize> {
    let valid = token == "0"
        || (!token.starts_with('0')
            && !token.is_empty()
            && token.bytes().all(|b| b.is_ascii_digit()));
    valid.then(|| token.parse().ok()).flatten()
}
