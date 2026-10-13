//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

#[path = "mock.rs"]
mod mock;
#[path = "playback_probe.rs"]
mod playback_probe;
#[path = "ptz_probe.rs"]
mod ptz_probe;
#[path = "replay_probe.rs"]
mod replay_probe;
