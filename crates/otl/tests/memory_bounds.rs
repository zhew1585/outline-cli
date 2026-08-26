//! What `spec sync` and the cache loader may allocate, asserted rather
//! than argued.
//!
//! Every bound this story claims about memory has been about ALLOCATION,
//! and every time it was written as prose arithmetic it was wrong - twice
//! by a factor, once by a whole missing term. So the claims live here now,
//! measured with a heap profiler, and the module docs elsewhere point at
//! this file instead of doing sums.
//!
//! # How to read a failure
//!
//! A failure means the peak moved. That is not automatically a bug: adding
//! a legitimate buffer moves it too. It means the number in the assertion
//! and the reasoning next to it have to be revisited together - which is
//! the point, because the alternative is a comment that quietly stops
//! being true.
//!
//! # Why one test function
//!
//! `dhat` allows one profiler at a time and the harness runs tests in
//! parallel, so all the measurements share a single `#[test]`. Each one
//! runs under its OWN profiler instance, started after its input has been
//! built: `max_bytes` is a running peak, so a measurement only means
//! something if the counter starts at zero and the setup is outside the
//! window.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use engine::ir::{BodyMode, OpSpec};
use otl::spec::cache::{self, CacheMeta};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

// Safe on this side: the `unsafe impl GlobalAlloc` lives inside dhat, the
// same way it lives inside the system allocator. Test-only.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// The download limit, which is what these bounds are relative to.
const DOCUMENT_LIMIT: usize = engine::fetch::MAX_DOCUMENT_BYTES as usize;

/// Peak heap allocated by `work`, in bytes.
///
/// The profiler is created here, so the count starts at zero and covers
/// exactly this closure. Anything the caller allocated beforehand (the
/// test input, for one) is outside the window.
fn peak_of(work: impl FnOnce()) -> usize {
    let profiler = dhat::Profiler::builder().testing().build();
    work();
    let peak = dhat::HeapStats::get().max_bytes;
    drop(profiler);
    peak
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn op(name: &str) -> OpSpec {
    OpSpec {
        name: name.to_string().into(),
        path: format!("/api/{name}").into(),
        summary: String::new().into(),
        content_type: String::new().into(),
        body_mode: BodyMode::KeyValue,
        params: Vec::new().into(),
    }
}

/// A cache file with a valid header around an arbitrary body.
fn write_body(file: &Path, body: &[u8]) {
    let mut raw = Vec::new();
    raw.extend_from_slice(b"OTL-IRC\x00");
    raw.extend_from_slice(&2u32.to_le_bytes());
    raw.extend_from_slice(&Sha256::digest(body));
    raw.extend_from_slice(body);
    fs::write(file, raw).unwrap();
}

fn record(bytes: &[u8], body: &mut Vec<u8>) {
    body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    body.extend_from_slice(bytes);
}

#[test]
fn parsing_and_loading_stay_within_their_bounds() {
    let mut report = Vec::new();

    // 1. The measured 367 MB case: a 16 MiB document whose bulk sits under
    //    a key the compiler never reads. It must cost almost nothing now,
    //    because an unread key is parsed straight into `IgnoredAny`.
    let filler = vec!["0"; DOCUMENT_LIMIT / 2 - 100].join(",");
    let bloat = format!(r#"{{"paths":{{"/a.b":{{"post":{{}}}}}},"x":[{filler}]}}"#);
    assert!(bloat.len() > DOCUMENT_LIMIT / 2, "test input too small");
    let peak = peak_of(|| {
        let compiled = spec_compile::compile_json(&bloat, &otl::spec::compile_options());
        assert!(compiled.is_ok(), "{:?}", compiled.err());
    });
    report.push(format!(
        "ignored-key document ({} MiB in): {:.2} MiB peak",
        bloat.len() / (1024 * 1024),
        mib(peak)
    ));
    assert!(
        peak < 4 * 1024 * 1024,
        "parsing a document whose bulk is under an unread key allocated \
         {:.2} MiB; an unread key is parsed straight into `IgnoredAny`, so \
         this used to be 350+ MB and should now be nothing",
        mib(peak)
    );

    // 2. The same bulk under a key the compiler DOES read: refused while
    //    parsing, so the tree is never finished.
    let bloat = format!(r#"{{"paths":{{"/a.b":[{filler}]}}}}"#);
    let peak = peak_of(|| {
        let error = spec_compile::compile_json(&bloat, &otl::spec::compile_options())
            .expect_err("must be refused");
        assert!(
            error.to_string().contains("expands to more than"),
            "{error}"
        );
    });
    report.push(format!("expanding document: {:.2} MiB peak", mib(peak)));
    assert!(
        peak < 24 * 1024 * 1024,
        "a document engineered to expand allocated {:.2} MiB before being \
         refused; the parse budget is what holds this down",
        mib(peak)
    );

    // 3. The real vendored spec, for scale: this is what a legitimate sync
    //    costs, and every limit above is a multiple of it.
    let vendored = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("spec")
            .join("spec3.json"),
    )
    .unwrap();
    let peak = peak_of(|| {
        spec_compile::compile_json(&vendored, &otl::spec::compile_options()).expect("compiles");
    });
    report.push(format!(
        "vendored spec ({} KiB in): {:.2} MiB peak",
        vendored.len() / 1024,
        mib(peak)
    ));
    assert!(
        peak < 8 * 1024 * 1024,
        "compiling the vendored spec allocated {:.2} MiB",
        mib(peak)
    );

    // 4. A cache declaring the maximum number of minimal operations: 82 KB
    //    of file, and the table's own Vec is the largest thing in it. That
    //    Vec is now charged against the footprint budget before it is
    //    reserved.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("ir-cache.bin");
    let mut body = Vec::new();
    let meta = CacheMeta::new("a".repeat(64), "https://spec.example".to_string());
    record(
        &bincode::serde::encode_to_vec(&meta, bincode::config::standard()).unwrap(),
        &mut body,
    );
    body.extend_from_slice(&(cache::MAX_CACHED_OPS as u32).to_le_bytes());
    let minimal = bincode::serde::encode_to_vec(op("a.b"), bincode::config::standard()).unwrap();
    for _ in 0..cache::MAX_CACHED_OPS {
        record(&minimal, &mut body);
    }
    write_body(&file, &body);
    let file_size = fs::metadata(&file).unwrap().len();
    let peak = peak_of(|| {
        // Rejected for duplicate names; what matters is what it allocated
        // on the way there.
        assert!(cache::load_at(&file).is_err());
    });
    report.push(format!(
        "cache declaring {} minimal operations ({file_size} B file): {:.2} MiB peak",
        cache::MAX_CACHED_OPS,
        mib(peak)
    ));
    assert!(
        peak < 4 * 1024 * 1024,
        "loading a {file_size}-byte cache allocated {:.2} MiB; the table's \
         own Vec is the largest part and is charged against the budget",
        mib(peak)
    );

    // 5. The worst single record, hand-encoded: BOTH nested containers lie
    //    about their length. This is the shape an independent review
    //    measured at 2.09 MiB, and the one the previous prose bound missed
    //    a whole term of - the outer `params` container can reserve
    //    serde's per-container cap at the same time as an inner
    //    `enum_values` container does.
    //
    //    bincode `standard()`: a varint length below 251 is one byte, and
    //    251 marks a u16, 252 a u32. An empty `ParamSpec` is eight zero
    //    bytes (empty name, first enum variant, two false booleans, empty
    //    enum list, empty format, two `None`s).
    let honest_params: u16 = 4000;
    let mut fat = Vec::new();
    fat.extend_from_slice(&[0, 0, 0, 0]); // four empty strings
    fat.push(0); // body_mode variant
    fat.push(251); // params: u16 length follows
    fat.extend_from_slice(&honest_params.to_le_bytes());
    for _ in 0..honest_params - 1 {
        fat.extend_from_slice(&[0u8; 8]); // an empty parameter
    }
    // The last parameter's enum_values claims a billion entries.
    fat.extend_from_slice(&[0, 0, 0, 0]); // name, ty, required, nullable
    fat.push(252); // enum_values: u32 length follows
    fat.extend_from_slice(&1_000_000_000u32.to_le_bytes());

    let mut body = Vec::new();
    record(
        &bincode::serde::encode_to_vec(&meta, bincode::config::standard()).unwrap(),
        &mut body,
    );
    body.extend_from_slice(&1u32.to_le_bytes());
    record(&fat, &mut body);
    assert!(
        fat.len() < cache::MAX_OP_RECORD_BYTES,
        "the crafted record must be within the per-record limit to be \
         decoded at all: {} bytes",
        fat.len()
    );
    write_body(&file, &body);
    let peak = peak_of(|| {
        // Refused: the inner container runs out of record long before it
        // has a billion entries.
        assert!(cache::load_at(&file).is_err());
    });
    report.push(format!(
        "cache with two lying containers in one {}-byte record: {:.2} MiB peak",
        fat.len(),
        mib(peak)
    ));
    assert!(
        peak < 3 * 1024 * 1024,
        "the worst single record allocated {:.2} MiB; serde's per-container \
         reservation cap (1 MiB) applies once per nesting level, so this is \
         where the number to watch lives",
        mib(peak)
    );

    // 6. A legitimate cache, for scale.
    let ops: Vec<OpSpec> = (0..200).map(|index| op(&format!("t{index}.op"))).collect();
    cache::store_at(&file, &ops, &meta).expect("stores");
    let peak = peak_of(|| {
        cache::load_at(&file).expect("loads").expect("is present");
    });
    report.push(format!(
        "legitimate 200-operation cache: {:.2} MiB peak",
        mib(peak)
    ));
    assert!(
        peak < 1024 * 1024,
        "a normal cache load: {:.2} MiB",
        mib(peak)
    );

    // Printed with --nocapture; the numbers are the documentation.
    for line in report {
        println!("{line}");
    }
}
