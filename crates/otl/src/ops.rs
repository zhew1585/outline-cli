//! Static IR table compiled from the vendored spec by `build.rs`.

// Defines `pub static OPS: &[engine::ir::OpSpec]`.
include!(concat!(env!("OUT_DIR"), "/ir_table.rs"));

/// Look up an operation by its `resource.method` name.
pub fn find(name: &str) -> Option<&'static engine::OpSpec> {
    OPS.iter().find(|op| op.name == name)
}
