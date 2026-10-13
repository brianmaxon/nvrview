//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

mod common;

#[path = "fuzz.rs"]
mod fuzz;
#[path = "pipeline.rs"]
mod pipeline;
