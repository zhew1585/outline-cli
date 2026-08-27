//! What reading a spec and loading a cache may allocate, asserted rather
//! than argued.
//!
//! # What is inside the measurement, and what is not
//!
//! Each case measures the library path the command runs, INCLUDING the
//! input buffer: a document is read from a file inside the profiler
//! window, because a 16 MiB `String` held for the duration of the compile
//! is a real part of the peak, and leaving it outside made an earlier
//! version of this file report about a fifth of the truth.
//!
//! Not included: the process itself - the binary, the allocator's arena,
//! whatever ran before `main`. These numbers are therefore lower than the
//! RSS of the equivalent command by a roughly constant amount, and they do
//! not replace it. For scale, measured with `/usr/bin/time -l` at the time
//! of writing, `spec sync --spec` peaks at 6.9 MB RSS on the vendored
//! spec, 29.2 MB on a 16 MiB document whose bulk is an ignored key, and
//! 46.1 MB on the worst shape the budget accepts (16 MiB of long strings).
//! The gap to the figures below is the process baseline, about 13 MB.
//! Bounded and small; the assertions cover the parts this code controls.
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
/// exactly this closure - which is why a document has to be READ inside it
/// (see [`peak_of_compiling`]).
fn peak_of(work: impl FnOnce()) -> usize {
    let profiler = dhat::Profiler::builder().testing().build();
    work();
    let peak = dhat::HeapStats::get().max_bytes;
    drop(profiler);
    peak
}

/// Peak heap for reading a document and compiling it, which is what
/// `spec sync --spec` does. The read is inside the window: the document
/// `String` stays live for the whole compile and belongs to the peak.
fn peak_of_compiling(path: &Path, expect_ok: bool) -> usize {
    let path = path.to_path_buf();
    peak_of(move || {
        let raw = fs::read_to_string(&path).unwrap();
        let compiled = spec_compile::compile_json(&raw, &otl::spec::compile_options());
        assert_eq!(
            compiled.is_ok(),
            expect_ok,
            "unexpected outcome: {:?}",
            compiled.err().map(|error| error.to_string())
        );
    })
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
        response_fields: Vec::new().into(),
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
    let inputs = TempDir::new().unwrap();
    let filler = vec!["0"; DOCUMENT_LIMIT / 2 - 100].join(",");
    let ignored_key = inputs.path().join("ignored-key.json");
    let input_size = {
        let bloat = format!(r#"{{"paths":{{"/a.b":{{"post":{{}}}}}},"x":[{filler}]}}"#);
        assert!(bloat.len() > DOCUMENT_LIMIT / 2, "test input too small");
        fs::write(&ignored_key, &bloat).unwrap();
        bloat.len()
    };
    let peak = peak_of_compiling(&ignored_key, true);
    report.push(format!(
        "ignored-key document ({} MiB in): {:.2} MiB peak",
        input_size / (1024 * 1024),
        mib(peak)
    ));
    assert!(
        peak < 40 * 1024 * 1024,
        "reading and parsing a document whose bulk is under an unread key \
         allocated {:.2} MiB; the input buffer is nearly all of it, because \
         an unread key is parsed straight into `IgnoredAny` - this case used \
         to cost 350+ MB",
        mib(peak)
    );

    // 2. The same bulk under a key the compiler DOES read: refused while
    //    parsing, so the tree is never finished.
    let expanding = inputs.path().join("expanding.json");
    fs::write(&expanding, format!(r#"{{"paths":{{"/a.b":[{filler}]}}}}"#)).unwrap();
    drop(filler);
    let peak = peak_of_compiling(&expanding, false);
    report.push(format!("expanding document: {:.2} MiB peak", mib(peak)));
    assert!(
        peak < 64 * 1024 * 1024,
        "a document engineered to expand allocated {:.2} MiB before being \
         refused; the input buffer plus the parse budget is what holds this \
         down",
        mib(peak)
    );

    // 2b. The worst document the budget ACCEPTS: long strings, which cost
    //     about what they cost on the wire, so the charge never trips and
    //     the whole tree is built. This is the largest legitimate-shaped
    //     document that can get through, and therefore the number that
    //     matters more than the refused case above.
    let long_strings = inputs.path().join("long-strings.json");
    {
        let value = "x".repeat(4096);
        let count = DOCUMENT_LIMIT / (value.len() + 8);
        let items = vec![format!("\"{value}\""); count].join(",");
        // A real operation alongside the bulk, so the compile succeeds and
        // the whole materialized tree is live at once.
        fs::write(
            &long_strings,
            format!(r#"{{"paths":{{"/a.b":{{"post":{{}}}},"/bulk":[{items}]}}}}"#),
        )
        .unwrap();
    }
    let long_size = fs::metadata(&long_strings).unwrap().len();
    let peak = peak_of_compiling(&long_strings, true);
    report.push(format!(
        "accepted long-string document ({} MiB in): {:.2} MiB peak",
        long_size / (1024 * 1024),
        mib(peak)
    ));
    assert!(
        peak < 64 * 1024 * 1024,
        "the largest document the budget accepts allocated {:.2} MiB",
        mib(peak)
    );

    // 3. The real vendored spec, for scale: this is what a legitimate sync
    //    costs, and every limit above is a multiple of it.
    let vendored_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("spec")
        .join("spec3.json");
    let vendored_size = fs::metadata(&vendored_path).unwrap().len();
    let peak = peak_of_compiling(&vendored_path, true);
    report.push(format!(
        "vendored spec ({} KiB in): {:.2} MiB peak",
        vendored_size / 1024,
        mib(peak)
    ));
    assert!(
        peak < 16 * 1024 * 1024,
        "reading and compiling the vendored spec allocated {:.2} MiB",
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

    // 5. The worst single record, hand-encoded: EVERY nested container
    //    lies about its length, so each one reserves serde's per-container
    //    cap at the same time. This is the shape the prose
    //    bound kept mis-counting, and the shape the FIRST version of this
    //    test failed to build - its outer container was honest, so it
    //    measured a cheaper case than its own comment claimed.
    //
    //    Mechanism worth keeping written down: bincode's serde bridge
    //    charges its byte budget for what it CONSUMES and never calls
    //    `claim_container_read`, while serde's `Vec` reserves
    //    `min(declared, 1 MiB / size_of::<T>())` on its own. The two are
    //    independent, so the reservation happens once per nesting level
    //    however small the record is.
    //
    //    bincode `standard()`: a varint below 251 is one byte, 251 marks a
    //    u16, 252 a u32. An empty `ParamSpec` is eight zero bytes (empty
    //    name, first enum variant, two false booleans, empty enum list,
    //    empty format, two `None`s).
    let honest_fill: usize = 4000;
    let mut fat = Vec::new();
    fat.extend_from_slice(&[0, 0, 0, 0]); // four empty strings
    fat.push(0); // body_mode variant
                 // The OUTER container claims a billion parameters.
    fat.push(252);
    fat.extend_from_slice(&1_000_000_000u32.to_le_bytes());
    for _ in 0..honest_fill - 1 {
        fat.extend_from_slice(&[0u8; 8]); // an empty parameter, honestly
    }
    // ...and the last parameter's enum_values claims a billion entries.
    fat.extend_from_slice(&[0, 0, 0, 0]); // name, ty, required, nullable
    fat.push(252);
    fat.extend_from_slice(&1_000_000_000u32.to_le_bytes());
    // ...and so does `response_fields`, the THIRD container, which the IR
    // gained after this fixture was written. A container missing from the
    // fixture is a reservation nobody measures, so the count in
    // `bounded.rs` and the lies here have to be kept in step. (The decoder
    // fails inside `params` before it reaches this one, but it is what the
    // record declares, and the next field to arrive belongs here too.)
    fat.push(252);
    fat.extend_from_slice(&1_000_000_000u32.to_le_bytes());
    assert!(
        fat.len() < cache::MAX_OP_RECORD_BYTES,
        "the crafted record must fit the per-record limit to be decoded at \
         all: {} bytes",
        fat.len()
    );

    let one_record = {
        let mut body = Vec::new();
        record(
            &bincode::serde::encode_to_vec(&meta, bincode::config::standard()).unwrap(),
            &mut body,
        );
        body.extend_from_slice(&1u32.to_le_bytes());
        record(&fat, &mut body);
        body
    };
    write_body(&file, &one_record);
    let peak = peak_of(|| {
        // Refused: both containers run out of record long before they have
        // a billion entries.
        assert!(cache::load_at(&file).is_err());
    });
    report.push(format!(
        "one record, all three containers lying ({} B): {:.2} MiB peak",
        fat.len(),
        mib(peak)
    ));
    assert!(
        peak < 4 * 1024 * 1024,
        "the worst single record allocated {:.2} MiB",
        mib(peak)
    );

    // 5c. The other reachable shape: a COMPLETE parameter list (so
    //     decoding gets past it) followed by a lying `response_fields`.
    //     This is what says the reservations are bounded by the DEPTH of
    //     the path being decoded, not by how many containers the type has:
    //     a lying container never completes, so the one after it is never
    //     reached, and the two shapes below bracket the real worst case.
    let mut complete = Vec::new();
    complete.extend_from_slice(&[0, 0, 0, 0]); // four empty strings
    complete.push(0); // body_mode
    complete.push(251); // params: u16 length
    complete.extend_from_slice(&(honest_fill as u16).to_le_bytes());
    for _ in 0..honest_fill {
        complete.extend_from_slice(&[0u8; 8]); // a complete empty parameter
    }
    complete.push(252); // response_fields claims a billion
    complete.extend_from_slice(&1_000_000_000u32.to_le_bytes());

    let mut body = Vec::new();
    record(
        &bincode::serde::encode_to_vec(&meta, bincode::config::standard()).unwrap(),
        &mut body,
    );
    body.extend_from_slice(&1u32.to_le_bytes());
    record(&complete, &mut body);
    write_body(&file, &body);
    let peak = peak_of(|| {
        assert!(cache::load_at(&file).is_err());
    });
    report.push(format!(
        "one record, complete params then a lying field list ({} B): {:.2} MiB peak",
        complete.len(),
        mib(peak)
    ));
    assert!(
        peak < 4 * 1024 * 1024,
        "a complete parameter list followed by a lying field list allocated \
         {:.2} MiB",
        mib(peak)
    );

    // 5b. The COMBINATION, which is the actual worst case for the loader:
    //     the table's own Vec at full size AND that record being decoded
    //     inside it. Neither test covered this, so the largest number
    //     either of them could report was smaller than what the loader can
    //     really hold at once.
    let mut combined = Vec::new();
    record(
        &bincode::serde::encode_to_vec(&meta, bincode::config::standard()).unwrap(),
        &mut combined,
    );
    combined.extend_from_slice(&(cache::MAX_CACHED_OPS as u32).to_le_bytes());
    record(&fat, &mut combined);
    // Enough minimal records after it that the declared count is not a
    // lie the framing can reject.
    for _ in 1..cache::MAX_CACHED_OPS {
        record(&minimal, &mut combined);
    }
    write_body(&file, &combined);
    let combined_size = fs::metadata(&file).unwrap().len();
    let peak = peak_of(|| {
        assert!(cache::load_at(&file).is_err());
    });
    report.push(format!(
        "worst record inside a full table ({combined_size} B file): {:.2} MiB peak",
        mib(peak)
    ));
    assert!(
        peak < 6 * 1024 * 1024,
        "the table Vec and the worst record together allocated {:.2} MiB; \
         this is the loader's real worst case, and the number to watch",
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
