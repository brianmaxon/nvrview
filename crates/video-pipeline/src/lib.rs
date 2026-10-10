//! Sans-I/O per-tile decoder decisions for nvrview.
//!
//! See `docs/RENDERING.md` for the contract. This crate decides, once and for
//! every platform, when a tile's hardware decoder is configured, fed, flushed,
//! or reset; how keyframe gating, backpressure, and recovery from decode
//! errors work; which YCbCr to RGB matrix a stream needs; and where each tile
//! goes in the grid. Platforms only carry the decisions out.
//!
//! No I/O, no threads, no clocks, no platform APIs: callers pass time in.
//!
//! - [`nal`]: Annex-B splitting and access unit classification by NAL type.
//! - [`h264`], [`h265`]: parameter set parsing (written from the syntax tables
//!   of ITU-T H.264 and H.265) for picture size, cropping, and VUI color.
//! - [`format`]: parameter set collections and the decoder format.
//! - [`color`]: YCbCr to RGB matrices.

mod bits;
pub mod color;
pub mod format;
pub mod geometry;
pub mod h264;
pub mod h265;
pub mod nal;
mod vui;

pub use bits::ParseError;
pub use color::{ColorDescription, ColorInfo, ColorMatrix, VideoSignal, YcbcrToRgb};
pub use format::{ParameterSet, ParameterSets, SequenceInfo, VideoFormat};
pub use geometry::{Rect, Size};
pub use nal::{
    AccessUnitInfo, Codec, NalUnits, ParameterSetKind, RandomAccess, analyze, nal_units,
};
