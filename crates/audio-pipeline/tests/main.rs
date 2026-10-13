//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

mod common;

#[path = "decoder.rs"]
mod decoder;
#[path = "mock_stream.rs"]
mod mock_stream;
#[path = "mono.rs"]
mod mono;
#[path = "render_alloc.rs"]
mod render_alloc;
#[path = "sim.rs"]
mod sim;
#[path = "talk_adpcm.rs"]
mod talk_adpcm;
#[path = "tile.rs"]
mod tile;
