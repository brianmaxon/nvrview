//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

mod common;

#[path = "compat.rs"]
mod compat;
#[path = "extra.rs"]
mod extra;
#[path = "fmp4.rs"]
mod fmp4;
#[path = "multitrack.rs"]
mod multitrack;
#[path = "mux.rs"]
mod mux;
