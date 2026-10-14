//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

mod common;

#[path = "cooldown.rs"]
mod cooldown;
#[path = "engine.rs"]
mod engine;
#[path = "props.rs"]
mod props;
#[path = "scenario.rs"]
mod scenario;
#[path = "test_trigger.rs"]
mod test_trigger;
