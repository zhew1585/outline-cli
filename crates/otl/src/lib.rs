//! Outline CLI (`otl`) library: UX layer over the generic `engine` crate.

#![forbid(unsafe_code)]

pub mod commands;
pub mod config;
pub mod errors;
pub mod exit;
pub mod ops;
pub mod paging;
pub mod render;
pub mod spec;
pub mod stdio;
