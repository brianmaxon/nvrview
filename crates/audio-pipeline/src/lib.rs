//! Sans-I/O per-tile audio for nvrview (docs/AUDIO.md): ADTS parsing and
//! decoding, camera-time stamps for samples that carry none, drift-corrected
//! playout through a lock-free mixer, and 16 kHz mono taps.
//!
//! - [`TileAudio`]: one per tile, on the tile's thread. Feed it each audio
//!   record with [`TileAudio::push`] and tell it about stream restarts with
//!   [`TileAudio::discontinuity`].
//! - [`output()`]: makes the shared [`AudioOutput`] (tiles claim slots on it)
//!   and the [`Renderer`] for the app's real-time callback.
//! - [`tap()`]: a bounded, drop-oldest queue of timestamped 16 kHz blocks.
//! - [`DecodeLevel`]: a tile's level measured as its audio is decoded, for
//!   tiles that are not audible.
//!
//! No I/O, no threads, no platform APIs; the caller supplies every time.

pub mod adpcm;
pub mod adts;
pub mod decode;
pub mod delay;
mod fit;
pub mod level;
pub mod output;
mod playout;
pub mod presentation;
pub mod resample;
pub mod tap;
mod tile;
pub mod timestamp;

pub use decode::{AudioCodec, PcmFormat};
pub use level::DecodeLevel;
pub use output::{
    AudioOutput, LEVEL_PEAK_RELEASE_S, LEVEL_RMS_WINDOW_S, OutputConfig, OutputFormat, Renderer,
    SlotLevel, output,
};
pub use playout::{MAX_CORRECTION, PlayoutStats, RESYNC_US};
pub use presentation::{PresentationEstimator, PresentationMap};
pub use tap::{TAP_RATE, TapBlock, TapReceiver, TapSender, TapStats, tap};
pub use tile::{AudioRecord, AudioStats, TileAudio, TileConfig};
pub use timestamp::{Timeline, TimestampStats};
