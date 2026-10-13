//! The one integration-test executable of this crate: every other file in
//! this folder is a module of it (see the [[test]] entry in Cargo.toml).

mod common;

#[path = "alerts.rs"]
mod alerts;
#[path = "audio.rs"]
mod audio;
#[path = "audio_clock.rs"]
mod audio_clock;
#[path = "capture.rs"]
mod capture;
#[path = "core.rs"]
mod core;
#[path = "devices.rs"]
mod devices;
#[path = "discovery.rs"]
mod discovery;
#[path = "lens.rs"]
mod lens;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "logging.rs"]
mod logging;
#[path = "playback.rs"]
mod playback;
#[path = "ptz.rs"]
mod ptz;
#[path = "recordings.rs"]
mod recordings;
#[path = "recordings_locks.rs"]
mod recordings_locks;
#[path = "retry.rs"]
mod retry;
#[path = "sound.rs"]
mod sound;
#[path = "sound_alloc.rs"]
mod sound_alloc;
#[path = "sound_downloaded_day.rs"]
mod sound_downloaded_day;
#[path = "sound_recordings.rs"]
mod sound_recordings;
#[path = "talk.rs"]
mod talk;
#[path = "third_party.rs"]
mod third_party;
#[path = "tier.rs"]
mod tier;
#[path = "update.rs"]
mod update;
