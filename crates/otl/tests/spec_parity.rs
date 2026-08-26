//! Build-time and run-time compilation must produce the same IR.
//!
//! `build.rs` renders the vendored spec into `static OPS`; `otl spec sync`
//! compiles a fetched spec through `spec-compile` and `spec::to_ir` at run
//! time. They share the parser but not the last mile: the build script
//! renders Rust source with its own copies of the `/api` prefix and the
//! enum variant names, while the runtime converts compiler types to
//! `engine::ir` types.
//!
//! This test closes that gap: compile the very same document the binary
//! was built from and assert the two tables are identical, field for
//! field. Any drift - a renamed variant, a changed prefix, a forgotten
//! facet - fails here instead of silently changing what a synced CLI does.
//!
//! Reading the vendored spec from `CARGO_MANIFEST_DIR` is fine in a test:
//! the startup guard's source scan covers `crates/*/src/**` only, because
//! it is the RUNTIME that must never reach for a spec file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use otl::{ops, spec};

fn vendored_spec() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("spec")
        .join("spec3.json");
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn runtime_compilation_reproduces_the_built_in_table() {
    let compiled = spec_compile::compile_json(&vendored_spec(), &spec::compile_options())
        .expect("the vendored spec compiles at run time");
    let runtime = spec::to_ir(&compiled);

    assert_eq!(
        runtime.len(),
        ops::OPS.len(),
        "operation count differs between build-time and run-time compilation"
    );
    for (index, (fresh, built_in)) in runtime.iter().zip(ops::OPS).enumerate() {
        assert_eq!(
            fresh, built_in,
            "operation #{index} differs between build-time and run-time compilation"
        );
    }
}

/// The columns a SYNCED spec yields must be the columns the built-in spec
/// yields - same fields, same order. `runtime_compilation_reproduces_the_
/// built_in_table` compares whole operations, so this narrows to the part a
/// reader is most likely to doubt after the two tracks merged, and states it
/// in its own terms.
#[test]
fn a_synced_spec_yields_the_same_response_columns() {
    let compiled = spec_compile::compile_json(&vendored_spec(), &spec::compile_options())
        .expect("the vendored spec compiles at run time");
    let runtime = spec::to_ir(&compiled);

    let mut compared = 0;
    for (fresh, built_in) in runtime.iter().zip(ops::OPS) {
        let fresh_fields: Vec<(&str, bool)> = fresh
            .response_fields
            .iter()
            .map(|field| (field.name.as_ref(), field.read_only))
            .collect();
        let built_in_fields: Vec<(&str, bool)> = built_in
            .response_fields
            .iter()
            .map(|field| (field.name.as_ref(), field.read_only))
            .collect();
        assert_eq!(
            fresh_fields, built_in_fields,
            "{}: synced columns differ from built-in ones",
            fresh.name
        );
        if !fresh_fields.is_empty() {
            compared += 1;
        }
    }
    // Not vacuous: the vendored spec really does describe response shapes.
    assert!(
        compared > 10,
        "only {compared} operations had response fields; the comparison is \
         not proving much"
    );
}

#[test]
fn every_built_in_operation_passes_the_safety_rules() {
    // The same rules a cache file is re-checked against, applied to the
    // table the binary ships with: if the vendored spec ever grew a path
    // the loader would reject, that must fail here.
    spec::validate_ops(ops::OPS).expect("the built-in table is safe to use");
    for op in ops::OPS {
        assert!(
            op.path.starts_with("/api/"),
            "{}: unexpected path {}",
            op.name,
            op.path
        );
    }
}

#[test]
fn the_upstream_url_points_at_the_vendored_source() {
    // The vendor record and the sync source must not drift apart: a sync
    // that silently pulled a different document than the one shipped would
    // be very hard to notice.
    let vendor_note = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("spec")
            .join("VENDOR.md"),
    )
    .unwrap();
    assert!(
        vendor_note.contains(spec::UPSTREAM_SPEC_URL),
        "spec/VENDOR.md does not mention {}",
        spec::UPSTREAM_SPEC_URL
    );
}
