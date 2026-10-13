//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

mod common;

#[path = "archive.rs"]
mod archive;
#[path = "mock_nvr.rs"]
mod mock_nvr;
#[path = "replay.rs"]
mod replay;
