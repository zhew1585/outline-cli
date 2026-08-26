//! Outline CLI (`otl`) library: UX layer over the generic `engine` crate.

#![forbid(unsafe_code)]

pub mod auth;
pub mod browser;
pub mod commands;
pub mod config;
pub mod errors;
pub mod exit;
pub mod export;
pub mod fields;
pub mod ops;
pub mod pager;
pub mod paging;
pub mod render;
pub mod session;
pub mod spec;
pub mod stdio;
pub mod text;
