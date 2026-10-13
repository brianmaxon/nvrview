//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

#[path = "ffi.rs"]
mod ffi;
#[path = "ffi_alerts.rs"]
mod ffi_alerts;
#[path = "ffi_audio.rs"]
mod ffi_audio;
#[path = "ffi_capture.rs"]
mod ffi_capture;
#[path = "ffi_edit_sound.rs"]
mod ffi_edit_sound;
#[path = "ffi_export.rs"]
mod ffi_export;
#[path = "ffi_playback.rs"]
mod ffi_playback;
#[path = "ffi_recordings.rs"]
mod ffi_recordings;
