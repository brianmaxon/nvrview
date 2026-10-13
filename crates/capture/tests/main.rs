//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

#[path = "call_flow.rs"]
mod call_flow;
#[path = "name_vectors.rs"]
mod name_vectors;
#[path = "props.rs"]
mod props;
