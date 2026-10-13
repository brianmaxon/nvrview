//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

#[path = "e2e.rs"]
mod e2e;
#[path = "finalize.rs"]
mod finalize;
#[path = "lane.rs"]
mod lane;
#[path = "range_download.rs"]
mod range_download;
#[path = "scheduler_control.rs"]
mod scheduler_control;
#[path = "thumbs.rs"]
mod thumbs;
