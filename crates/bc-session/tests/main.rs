//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

mod common;

#[path = "archive.rs"]
mod archive;
#[path = "lens.rs"]
mod lens;
#[path = "mock.rs"]
mod mock;
#[path = "play.rs"]
mod play;
#[path = "ptz.rs"]
mod ptz;
#[path = "session.rs"]
mod session;
#[path = "talk.rs"]
mod talk;
#[path = "third_party.rs"]
mod third_party;
