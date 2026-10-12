//! The nvrview C interface. Contract: `docs/FFI.md`; header:
//! `include/nvrview.h`, generated from this file by cbindgen
//! (`scripts/gen-header.sh`).
//!
//! Every exported function catches panics and reports them as
//! `NV_ERR_INTERNAL` (or ignores them, for functions without a status), so
//! no unwinding ever crosses into C. Enum values that come *from* C (tier,
//! failure kind) are received as plain integers and validated, so an
//! out-of-range value is an error, never an invalid Rust enum.

// The C ABI needs raw pointers: this crate is the one place in the workspace
// where `unsafe` is allowed. Every unsafe block is small and states why it
// is sound.
#![allow(unsafe_code)]
// Type and variant names are the C names in the contract.
#![allow(non_camel_case_types)]

use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use viewer_core::audio::TapBlock;
use viewer_core::{
    Action, AudioEncoder, Callbacks, CaptureError, Codec, ColorMatrix, Core, CoreConfig, CoreError,
    DecodeFailureKind, EncoderOpened, LogSettings, MAX_CAMERAS, PlaybackState, RecError, SeekMode,
    StreamTier, ThumbKind, Thumbnail, ThumbnailAnswer, TileStatus, clock,
};

/// Bumped on any incompatible change; apps compare it with nv_abi_version().
pub const NV_ABI_VERSION: u32 = 3;

/// Bumped for each addition that leaves every existing declaration
/// unchanged (new functions, events, status codes); resets to 0 with a new
/// NV_ABI_VERSION. Minor 1: device discovery. Minor 2: updates. Minor 3:
/// nv_tile_retry. Minor 4: nv_talk_set_conversation. Minor 5: recordings,
/// part 1 (docs/FFI-RECORDINGS.md, sections 1 to 5). Minor 6: recorded
/// playback (section 6). Minor 7: "months" in nv_recordings_want (a month's
/// calendar alone) and the recordings_month_failed event. Minor 8: edit
/// lists (section 7) and the rest of sound (section 2a). Minor 9: capture
/// (live and call recordings; docs/FFI.md, "Capture"). Minor 10: export
/// (section 7a): nv_download_range, nv_still_plan, the trim kind and in_ms and
/// out_ms of nv_edit_compute, and dest and share_safe on export requests.
pub const NV_ABI_MINOR: u32 = 10;

/// Return codes. Every function that can fail returns one.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvStatus {
    NV_OK = 0,
    /// null pointer, bad UTF-8, out-of-range value
    NV_ERR_INVALID_ARGUMENT = -1,
    /// device or tile already removed, or never existed
    NV_ERR_UNKNOWN_ID = -2,
    /// the core is shutting down
    NV_ERR_CLOSED = -3,
    /// too many devices, tiles, layouts, queued requests, or recordings, or
    /// too little disk space for a recording
    NV_ERR_LIMIT = -4,
    NV_ERR_INTERNAL = -5,
    /// the output buffer is too small; *len holds the size needed
    NV_ERR_BUFFER_TOO_SMALL = -6,
    /// talk-back (camera motion control) is not enabled for that camera
    NV_ERR_NOT_PERMITTED = -7,
    /// the device's backend cannot do this (minor 5)
    NV_ERR_UNSUPPORTED = -8,
    /// nv_recordings_configure not called, or no downloads_dir (minor 5);
    /// nv_capture_configure not called, or a call recording without an
    /// encoder (minor 9)
    NV_ERR_NOT_CONFIGURED = -9,
    /// accepted; the answer arrives as an event (only nv_thumbnail_get
    /// returns it; minor 5)
    NV_PENDING = 1,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvCodec {
    NV_CODEC_H264 = 1,
    NV_CODEC_H265 = 2,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvStreamTier {
    NV_TIER_MAIN = 0,
    NV_TIER_SUB = 1,
    NV_TIER_MOBILE = 2,
    NV_TIER_EXTERN = 4,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvColorMatrix {
    NV_MATRIX_BT601 = 0,
    NV_MATRIX_BT709 = 1,
    NV_MATRIX_BT2020 = 2,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvTileStatus {
    NV_TILE_WAITING = 0,
    NV_TILE_PLAYING = 1,
    NV_TILE_RECOVERING = 2,
    NV_TILE_UNDECODABLE = 3,
}

/// Why a platform decoder could not answer a Configure or a Decode.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvDecodeFailure {
    /// answers Configure
    NV_FAIL_INVALID_PARAMETER_SETS = 0,
    /// answers Configure
    NV_FAIL_SESSION_CREATE_FAILED = 1,
    NV_FAIL_NOT_CONFIGURED = 2,
    NV_FAIL_INVALID_ACCESS_UNIT = 3,
    NV_FAIL_SUBMIT_FAILED = 4,
    NV_FAIL_DECODE_ERROR = 5,
    /// the decoder dropped the picture; still an answer
    NV_FAIL_NO_PICTURE = 6,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvActionKind {
    NV_ACTION_CONFIGURE = 0,
    NV_ACTION_DECODE = 1,
    NV_ACTION_FLUSH = 2,
    NV_ACTION_RESET = 3,
    NV_ACTION_CLEAR = 4,
    NV_ACTION_STATUS = 5,
}

/// What a cover thumbnail's bytes are (minor 5).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvThumbKind {
    /// Annex-B, parameter sets included
    NV_THUMB_H264_KEYFRAME = 1,
    /// Annex-B, parameter sets included
    NV_THUMB_H265_KEYFRAME = 2,
    /// the app's own small image (for example JPEG)
    NV_THUMB_IMAGE = 3,
}

/// How a seek shows its target (minor 6).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvSeekMode {
    /// show the keyframe at or before the position first (fast; scrubbing)
    NV_SEEK_KEYFRAME = 0,
    /// decode from that keyframe and show the picture at the position
    NV_SEEK_EXACT = 1,
}

/// A playback's state (minor 6).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvPlaybackState {
    /// playing, but the clock waits for a camera
    NV_PLAYBACK_BUFFERING = 0,
    NV_PLAYBACK_PLAYING = 1,
    NV_PLAYBACK_PAUSED = 2,
    /// at the end of the range or of the recordings
    NV_PLAYBACK_ENDED = 3,
    /// every camera failed
    NV_PLAYBACK_FAILED = 4,
}

/// An `NvStreamTier` value as received from C (validated before use).
pub type NvStreamTierValue = u32;
/// An `NvThumbKind` value as received from C (validated before use).
pub type NvThumbKindValue = u32;
/// An `NvDecodeFailure` value as received from C (validated before use).
pub type NvDecodeFailureValue = u32;
/// An `NvSeekMode` value as received from C (validated before use).
pub type NvSeekModeValue = u32;

/// Borrowed bytes: valid only until the callback that received them returns.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvBytes {
    pub data: *const u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvSize {
    pub width: u32,
    pub height: u32,
}

/// Pixels, origin at the top left.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvColor {
    pub matrix: NvColorMatrix,
    /// the decoder must output this range
    pub full_range: bool,
    /// Row-major 3x4: (R, G, B) = M * (Y, Cb, Cr, 1), samples as code/255.
    pub ycbcr_to_rgb: [f32; 12],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvConfigure {
    pub codec: NvCodec,
    /// VPS, SPS, PPS order; NAL units without start codes
    pub parameter_sets: *const NvBytes,
    pub parameter_set_count: usize,
    pub coded_size: NvSize,
    /// inside the coded picture
    pub visible_rect: NvRect,
    pub color: NvColor,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvDecode {
    /// one picture, Annex-B, parameter sets possibly in band
    pub access_unit: NvBytes,
    pub keyframe: bool,
    pub pts_us: i64,
}

/// One instruction for a tile's platform decoder or view. Read only the member
/// that matches `kind`; Flush, Reset, and Clear carry no data.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvAction {
    pub kind: NvActionKind,
    pub configure: NvConfigure,
    pub decode: NvDecode,
    pub status: NvTileStatus,
    /// The tile's stream this action belongs to: 0 when opened, one more
    /// for each tier switch (nv_tile_set_tier). Each generation has its own
    /// platform decoder; answer with the action's generation.
    pub generation: u32,
}

/// One block of a tile's tapped audio: 16 kHz mono, borrowed for the
/// duration of on_audio_tap.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvAudioBlock {
    /// samples in -1..1
    pub samples: *const f32,
    pub sample_count: usize,
    /// always 16000
    pub sample_rate: u32,
    /// numbering and camera times restart with each epoch
    pub epoch: u32,
    /// number of the first sample in this epoch, at 16 kHz
    pub first_sample: u64,
    /// camera time (video timestamp units) of the first sample, if has_camera_time
    pub camera_time_us: i64,
    /// arrival of the record that completed the block (microseconds since the Unix epoch), if has_wall_clock
    pub wall_clock_us: i64,
    pub has_camera_time: bool,
    pub has_wall_clock: bool,
    /// audio is missing or restarted before this block
    pub discontinuity: bool,
}

/// Callbacks. All arrive on threads the core owns and must return quickly.
/// on_action: serialized per tile, never concurrent for the same tile, never
///   re-entrant. Pointers inside `action` are valid only during the call.
/// on_event: serialized across the whole core. `json` is UTF-8, not
///   NUL-terminated, valid only during the call.
/// on_audio_tap: may be NULL (no taps). Serialized across the core; `block`
///   and its samples are valid only during the call.
/// From inside a callback an app may call nv_tile_decoded and
/// nv_tile_decode_failed, and nothing else (and from inside an
/// NvAudioEncoder callback, nv_record_audio_encoded).
/// No event names a device before nv_device_add returns its ID, and no
/// on_action for a tile runs before nv_tile_open returns its ID.
pub type NvActionCallback =
    Option<unsafe extern "C" fn(context: *mut c_void, tile: u64, action: *const NvAction)>;
pub type NvEventCallback =
    Option<unsafe extern "C" fn(context: *mut c_void, json: *const c_char, len: usize)>;
pub type NvAudioTapCallback =
    Option<unsafe extern "C" fn(context: *mut c_void, tile: u64, block: *const NvAudioBlock)>;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvCallbacks {
    pub context: *mut c_void,
    pub on_action: NvActionCallback,
    pub on_event: NvEventCallback,
    pub on_audio_tap: NvAudioTapCallback,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvConfig {
    /// Directory for rotated log files (UTF-8, NUL-terminated); NULL = no file log.
    pub log_dir: *const c_char,
    /// 0 = default
    pub log_max_files: u32,
    /// 0 = default
    pub log_max_total_bytes: u64,
    /// The app's data directory (UTF-8, NUL-terminated), where the core keeps
    /// layouts and device keys; NULL = in memory only.
    pub data_dir: *const c_char,
}

/// Grid layout of one tile: its cell, where to draw, and what to sample.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvTileLayout {
    pub cell: NvRect,
    pub dest: NvRect,
    pub source: NvRect,
}

/// A cover thumbnail's description (minor 5): what its bytes are and the
/// picture they decode to.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NvThumbnail {
    pub kind: NvThumbKindValue,
    /// of the picture the bytes decode to (0 when unknown)
    pub width: u32,
    pub height: u32,
    /// the keyframe's stream timestamp (0 for an image that did not keep it)
    pub pts_us: i64,
}

/// The core. Opaque to C.
pub struct NvCore {
    core: Core,
    /// The app passed an on_audio_tap.
    taps: bool,
}

/// Most frames one nv_audio_render call fills.
const MAX_RENDER_FRAMES: u32 = 1 << 20;

/// Most tiles `nv_grid_layout` lays out in one call.
const MAX_LAYOUT_TILES: u32 = 65_536;

fn status(r: Result<(), CoreError>) -> NvStatus {
    match r {
        Ok(()) => NvStatus::NV_OK,
        Err(e) => error_status(e),
    }
}

fn error_status(e: CoreError) -> NvStatus {
    match e {
        CoreError::InvalidArgument => NvStatus::NV_ERR_INVALID_ARGUMENT,
        CoreError::UnknownId => NvStatus::NV_ERR_UNKNOWN_ID,
        CoreError::Closed => NvStatus::NV_ERR_CLOSED,
        CoreError::Limit => NvStatus::NV_ERR_LIMIT,
        CoreError::Internal => NvStatus::NV_ERR_INTERNAL,
        CoreError::NotPermitted => NvStatus::NV_ERR_NOT_PERMITTED,
    }
}

/// Runs `f`, turning a panic into `NV_ERR_INTERNAL`.
fn guarded(f: impl FnOnce() -> NvStatus) -> NvStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(NvStatus::NV_ERR_INTERNAL)
}

/// The context pointer the app passed, used from core threads.
#[derive(Clone, Copy)]
struct Context(*mut c_void);

// SAFETY: the core never dereferences the context; it only passes it back to
// the app's callbacks, on core threads, as docs/FFI.md specifies. The app
// supplies a context that is valid to use from those threads.
unsafe impl Send for Context {}
// SAFETY: as above; sharing the pointer value between threads is all the
// core does with it.
unsafe impl Sync for Context {}

struct FfiCallbacks {
    context: Context,
    on_action: unsafe extern "C" fn(*mut c_void, u64, *const NvAction),
    on_event: unsafe extern "C" fn(*mut c_void, *const c_char, usize),
    on_audio_tap: Option<unsafe extern "C" fn(*mut c_void, u64, *const NvAudioBlock)>,
}

fn codec(c: Codec) -> NvCodec {
    match c {
        Codec::H264 => NvCodec::NV_CODEC_H264,
        Codec::H265 => NvCodec::NV_CODEC_H265,
    }
}

fn tile_status(s: TileStatus) -> NvTileStatus {
    match s {
        TileStatus::Waiting => NvTileStatus::NV_TILE_WAITING,
        TileStatus::Playing => NvTileStatus::NV_TILE_PLAYING,
        TileStatus::Recovering => NvTileStatus::NV_TILE_RECOVERING,
        TileStatus::Undecodable => NvTileStatus::NV_TILE_UNDECODABLE,
    }
}

fn bytes(b: &[u8]) -> NvBytes {
    NvBytes {
        data: b.as_ptr(),
        len: b.len(),
    }
}

const NO_BYTES: NvBytes = NvBytes {
    data: std::ptr::null(),
    len: 0,
};

impl Callbacks for FfiCallbacks {
    fn on_action(&self, tile: u64, generation: u32, action: &Action<'_>) {
        let mut a = NvAction {
            generation,
            kind: NvActionKind::NV_ACTION_FLUSH,
            configure: NvConfigure {
                codec: NvCodec::NV_CODEC_H264,
                parameter_sets: std::ptr::null(),
                parameter_set_count: 0,
                coded_size: NvSize::default(),
                visible_rect: NvRect::default(),
                color: NvColor {
                    matrix: NvColorMatrix::NV_MATRIX_BT601,
                    full_range: false,
                    ycbcr_to_rgb: [0.0; 12],
                },
            },
            decode: NvDecode {
                access_unit: NO_BYTES,
                keyframe: false,
                pts_us: 0,
            },
            status: NvTileStatus::NV_TILE_WAITING,
        };
        // Lives until the callback returns, like everything `a` points to.
        let sets: Vec<NvBytes>;
        match *action {
            Action::Configure {
                codec: c,
                parameter_sets,
                coded_size,
                visible_rect,
                color,
            } => {
                sets = parameter_sets.iter().map(|p| bytes(&p.data)).collect();
                a.kind = NvActionKind::NV_ACTION_CONFIGURE;
                a.configure = NvConfigure {
                    codec: codec(c),
                    parameter_sets: sets.as_ptr(),
                    parameter_set_count: sets.len(),
                    coded_size: NvSize {
                        width: coded_size.width,
                        height: coded_size.height,
                    },
                    visible_rect: NvRect {
                        x: visible_rect.x,
                        y: visible_rect.y,
                        width: visible_rect.width,
                        height: visible_rect.height,
                    },
                    color: NvColor {
                        matrix: match color.matrix {
                            ColorMatrix::Bt601 => NvColorMatrix::NV_MATRIX_BT601,
                            ColorMatrix::Bt709 => NvColorMatrix::NV_MATRIX_BT709,
                            ColorMatrix::Bt2020 => NvColorMatrix::NV_MATRIX_BT2020,
                        },
                        full_range: color.full_range,
                        ycbcr_to_rgb: flatten(&color.ycbcr_to_rgb),
                    },
                };
            }
            Action::Decode {
                access_unit,
                keyframe,
                pts,
            } => {
                a.kind = NvActionKind::NV_ACTION_DECODE;
                a.decode = NvDecode {
                    access_unit: bytes(access_unit),
                    keyframe,
                    // Device timestamps fit easily; the cast round-trips
                    // exactly through nv_tile_decoded either way.
                    pts_us: pts as i64,
                };
            }
            Action::Flush => a.kind = NvActionKind::NV_ACTION_FLUSH,
            Action::Reset => a.kind = NvActionKind::NV_ACTION_RESET,
            Action::Clear => a.kind = NvActionKind::NV_ACTION_CLEAR,
            Action::Status(s) => {
                a.kind = NvActionKind::NV_ACTION_STATUS;
                a.status = tile_status(s);
            }
        }
        // SAFETY: `on_action` is the app's non-null function pointer with the
        // contract's signature. `&a` and every pointer inside it (`sets`, the
        // parameter sets, the access unit) stay valid until this call
        // returns, which is all the contract promises.
        unsafe { (self.on_action)(self.context.0, tile, &a) };
    }

    fn on_event(&self, json: &str) {
        // SAFETY: `on_event` is the app's non-null function pointer; `json`
        // is valid UTF-8 for `json.len()` bytes until the call returns.
        unsafe { (self.on_event)(self.context.0, json.as_ptr().cast(), json.len()) };
    }

    fn on_audio_tap(&self, tile: u64, b: &TapBlock) {
        let Some(f) = self.on_audio_tap else { return };
        let block = NvAudioBlock {
            samples: b.samples.as_ptr(),
            sample_count: b.samples.len(),
            sample_rate: viewer_core::audio::TAP_RATE,
            epoch: b.epoch,
            first_sample: b.first_sample,
            camera_time_us: b.camera_time_us.map_or(0, |t| t.round() as i64),
            wall_clock_us: b.wall_clock_us.unwrap_or(0),
            has_camera_time: b.camera_time_us.is_some(),
            has_wall_clock: b.wall_clock_us.is_some(),
            discontinuity: b.discontinuity,
        };
        // SAFETY: `f` is the app's function pointer with the contract's
        // signature; `block` and the samples it points to live until the
        // call returns.
        unsafe { f(self.context.0, tile, &block) };
    }
}

fn flatten(m: &[[f32; 4]; 3]) -> [f32; 12] {
    let mut out = [0.0; 12];
    for (i, v) in m.iter().flatten().enumerate() {
        out[i] = *v;
    }
    out
}

/// Reads a NUL-terminated UTF-8 string from C.
///
/// # Safety
/// `p` is null or points to a NUL-terminated string valid for this call.
unsafe fn string<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null, and the caller promises a NUL-terminated string that
    // outlives this call; the result is copied before the call returns.
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

/// Borrows the core behind a handle.
///
/// # Safety
/// `core` is null or a handle from `nv_core_new` that has not been freed.
unsafe fn core_ref<'a>(core: *const NvCore) -> Option<&'a Core> {
    // SAFETY: per the caller's promise, a non-null `core` points to a live
    // NvCore; NvCore is only ever accessed through shared references (all
    // its state is synchronized), so concurrent calls are fine.
    unsafe { core.as_ref() }.map(|c| &c.core)
}

/// Writes `value` through an out pointer.
///
/// # Safety
/// `out` is null or valid for writing one `T`.
unsafe fn put<T>(out: *mut T, value: T) -> bool {
    if out.is_null() {
        return false;
    }
    // SAFETY: non-null and, per the caller, valid for one write.
    unsafe { out.write(value) };
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn nv_abi_version() -> u32 {
    NV_ABI_VERSION
}

/// The core's minor version: additions to this ABI version. An app checks
/// nv_abi_version() for equality and nv_abi_minor() >= the minor that
/// introduced a function before calling it.
#[unsafe(no_mangle)]
pub extern "C" fn nv_abi_minor() -> u32 {
    NV_ABI_MINOR
}

/// Starts the core. `out` receives the handle. `config` is read only during
/// the call.
///
/// # Safety
/// `config` is null (defaults, no file log) or points to a valid `NvConfig`
/// whose `log_dir` is null or a NUL-terminated string; `out` is valid for
/// one write; `callbacks.context` may be used from core threads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_core_new(
    config: *const NvConfig,
    callbacks: NvCallbacks,
    out: *mut *mut NvCore,
) -> NvStatus {
    guarded(|| {
        if out.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        let (Some(on_action), Some(on_event)) = (callbacks.on_action, callbacks.on_event) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        let mut cfg = CoreConfig::default();
        // SAFETY: the caller promises `config` is null or valid to read.
        if let Some(c) = unsafe { config.as_ref() }
            && !c.data_dir.is_null()
        {
            // SAFETY: non-null and NUL-terminated per the caller.
            let Some(dir) = (unsafe { string(c.data_dir) }) else {
                return NvStatus::NV_ERR_INVALID_ARGUMENT;
            };
            cfg.data_dir = Some(PathBuf::from(dir));
        }
        // SAFETY: as above.
        if let Some(c) = unsafe { config.as_ref() }
            && !c.log_dir.is_null()
        {
            // SAFETY: non-null and NUL-terminated per the caller.
            let Some(dir) = (unsafe { string(c.log_dir) }) else {
                return NvStatus::NV_ERR_INVALID_ARGUMENT;
            };
            cfg.log = Some(LogSettings {
                dir: PathBuf::from(dir),
                max_files: c.log_max_files as usize,
                max_total_bytes: c.log_max_total_bytes,
            });
        }
        match new_core(cfg, callbacks, on_action, on_event) {
            Ok(handle) => {
                // SAFETY: `out` is non-null and valid for one write per the
                // caller.
                unsafe { put(out, handle) };
                NvStatus::NV_OK
            }
            Err(e) => e,
        }
    })
}

type OnAction = unsafe extern "C" fn(*mut c_void, u64, *const NvAction);
type OnEvent = unsafe extern "C" fn(*mut c_void, *const c_char, usize);

/// Starts a core for [`nv_core_new`] (or [`test_support::core_new_on`]).
fn new_core(
    cfg: CoreConfig,
    callbacks: NvCallbacks,
    on_action: OnAction,
    on_event: OnEvent,
) -> Result<*mut NvCore, NvStatus> {
    let cb = Arc::new(FfiCallbacks {
        context: Context(callbacks.context),
        on_action,
        on_event,
        on_audio_tap: callbacks.on_audio_tap,
    });
    let taps = callbacks.on_audio_tap.is_some();
    match Core::new(cfg, cb) {
        Ok(core) => Ok(Box::into_raw(Box::new(NvCore { core, taps }))),
        Err(e) => Err(error_status(e)),
    }
}

/// Test support, for Rust tests only (not part of the C interface): the C
/// interface on a test's runtime and simulated clock
/// ([`viewer_core::TestRuntime`]).
#[doc(hidden)]
pub mod test_support {
    use super::{NvCallbacks, NvCore, NvStatus, new_core};
    pub use viewer_core::TestRuntime;

    /// [`super::nv_core_new`] with default settings on `runtime`. Free the
    /// handle with [`super::nv_core_free`].
    ///
    /// # Errors
    /// As `nv_core_new`.
    pub fn core_new_on(
        runtime: TestRuntime,
        callbacks: NvCallbacks,
    ) -> Result<*mut NvCore, NvStatus> {
        core_new_at(runtime, callbacks, None)
    }

    /// [`core_new_on`] with the simulated clock starting at
    /// `wall_clock_start_us` (microseconds since the Unix epoch), so
    /// recordings tests know which day is "today".
    ///
    /// # Errors
    /// As `nv_core_new`.
    pub fn core_new_at(
        runtime: TestRuntime,
        callbacks: NvCallbacks,
        wall_clock_start_us: Option<i64>,
    ) -> Result<*mut NvCore, NvStatus> {
        let (Some(on_action), Some(on_event)) = (callbacks.on_action, callbacks.on_event) else {
            return Err(NvStatus::NV_ERR_INVALID_ARGUMENT);
        };
        let cfg = viewer_core::CoreConfig {
            test_runtime: Some(runtime),
            test_wall_clock_start_us: wall_clock_start_us,
            ..viewer_core::CoreConfig::default()
        };
        new_core(cfg, callbacks, on_action, on_event)
    }

    /// Waits until every event queued so far has been delivered.
    pub fn flush_events(core: &NvCore) {
        core.core.flush_events();
    }

    /// Now on the core's clock (render host times are on it): the
    /// simulated clock of a core from [`core_new_on`].
    pub fn clock_now_ns(core: &NvCore) -> u64 {
        core.core.clock_now_ns()
    }
}

/// Stops every preview, closes every session, joins every core thread. No
/// callback runs after it returns. Never call it from a callback (there it
/// does nothing and leaks the core rather than freeing it under itself).
/// NULL is a no-op.
///
/// # Safety
/// `core` is null or a handle from `nv_core_new` not yet freed; no other
/// call on it may start after this one begins.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_core_free(core: *mut NvCore) {
    if core.is_null() || Core::on_core_thread() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `core` came from Box::into_raw in nv_core_new and the
        // caller hands back ownership exactly once.
        let b = unsafe { Box::from_raw(core) };
        b.core.shutdown();
        drop(b);
    }));
}

/// Adds a device; connecting starts in the background. The password is copied
/// and never logged. `out_device` receives a nonzero ID.
///
/// # Safety
/// `core` is a live handle; `host`, `user`, `password` are NUL-terminated
/// strings; `out_device` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_device_add(
    core: *mut NvCore,
    host: *const c_char,
    port: u16,
    user: *const c_char,
    password: *const c_char,
    out_device: *mut u64,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller's promises on each pointer.
        let (core, host, user, password) =
            unsafe { (core_ref(core), string(host), string(user), string(password)) };
        let (Some(core), Some(host), Some(user), Some(password)) = (core, host, user, password)
        else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        if out_device.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        match core.add_device_deferred(host, port, user, password) {
            Ok(pending) => {
                // SAFETY: non-null and valid for one write per the caller.
                unsafe { put(out_device, pending.id()) };
                // Only now may events name the device.
                pending.publish();
                NvStatus::NV_OK
            }
            Err(e) => error_status(e),
        }
    })
}

/// Closes the device's session (its tiles end first).
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_device_remove(core: *mut NvCore, device: u64) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            Some(c) => status(c.remove_device(device)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Opens a tile: one channel and tier of a device. Actions for it start
/// arriving on on_action. `out_tile` receives a nonzero ID.
///
/// # Safety
/// `core` is a live handle; `out_tile` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_open(
    core: *mut NvCore,
    device: u64,
    channel: u8,
    tier: NvStreamTierValue,
    out_tile: *mut u64,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        let Some(tier) = StreamTier::from_number(tier) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        if out_tile.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        match c.open_tile_deferred(device, channel, tier) {
            Ok(pending) => {
                // SAFETY: non-null and valid for one write per the caller.
                unsafe { put(out_tile, pending.id()) };
                // Only now may actions for the tile run.
                pending.publish();
                NvStatus::NV_OK
            }
            Err(e) => error_status(e),
        }
    })
}

/// Closes a tile. No on_action for it runs after this returns. Never call it
/// from a callback.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_close(core: *mut NvCore, tile: u64) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            Some(c) => status(c.close_tile(tile)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Asks the tile to show `tier` (for example main on focus, sub back in the
/// grid). The tile keeps presenting its current stream until the new one
/// has decoded a picture no older than the one shown (between main and sub;
/// at most 300 ms after its first decoded picture) or its first
/// picture (otherwise), then switches in one step: no gap, no black frame,
/// no step back in time. The new stream's actions carry the next generation; the
/// switch is the old generation's final NV_ACTION_RESET (and a tile_tier
/// event). Returns at once; progress arrives as events. Asking for the tier
/// already requested changes nothing.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_set_tier(
    core: *mut NvCore,
    tile: u64,
    tier: NvStreamTierValue,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        let Some(tier) = StreamTier::from_number(tier) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        status(c.set_tile_tier(tile, tier))
    })
}

/// Retries a tile that has no picture (the app's Retry button): its stream
/// is stopped and started again now, on the same login. When the device is
/// waiting before a reconnect, the next connection attempt is made now. A
/// tile asked again within 2 s does nothing (NV_OK), and so does one whose
/// device refused the login (that state stays terminal; apps do not offer
/// the button for it). Returns at once; progress arrives as the usual
/// events. NV_ERR_UNKNOWN_ID for an unknown tile. Added in minor 3.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_retry(core: *mut NvCore, tile: u64) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            Some(c) => status(c.retry_tile(tile)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Decoder results, for the generation of the action they answer. Every
/// Decode not discarded by a later Flush, Reset, or Configure gets exactly
/// one of these; a failed Configure gets one nv_tile_decode_failed.
/// Results for a generation that has ended are ignored. Callable from any
/// thread, including inside on_action; they never block.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_decoded(
    core: *mut NvCore,
    tile: u64,
    generation: u32,
    pts_us: i64,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            // The inverse of the cast in on_action.
            Some(c) => status(c.decoded(tile, generation, pts_us as u64)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

fn failure(kind: NvDecodeFailureValue) -> Option<DecodeFailureKind> {
    use DecodeFailureKind::*;
    Some(match kind {
        0 => InvalidParameterSets,
        1 => SessionCreateFailed,
        2 => NotConfigured,
        3 => InvalidAccessUnit,
        4 => SubmitFailed,
        5 => DecodeError,
        6 => NoPicture,
        _ => return None,
    })
}

/// See nv_tile_decoded.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_decode_failed(
    core: *mut NvCore,
    tile: u64,
    generation: u32,
    kind: NvDecodeFailureValue,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        match failure(kind) {
            Some(k) => status(c.decode_failed(tile, generation, k)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Now on the core's clock, nanoseconds: the platform host clock (macOS:
/// mach_absolute_time() in ns, which AudioTimeStamp.mHostTime counts;
/// Windows: QueryPerformanceCounter in ns; Linux: CLOCK_MONOTONIC).
/// nv_audio_render's host_time_ns is on this clock.
#[unsafe(no_mangle)]
pub extern "C" fn nv_clock_now_ns() -> u64 {
    clock::now_ns()
}

/// Sets the audio output format the app renders: sample_rate frames per
/// second (8000 to 384000) and 1 or 2 interleaved channels. Call again after
/// the output device or its rate changes; audible tiles move to the new
/// output and resync.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_audio_set_output(
    core: *mut NvCore,
    sample_rate: u32,
    channels: u32,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            Some(c) => status(c.set_audio_output(sample_rate, channels)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Fills `out` (frames x channels floats, interleaved) with the mix of the
/// audible tiles. host_time_ns is when out's first frame will be heard, on
/// the core's clock (nv_clock_now_ns). Real-time safe: never blocks, locks,
/// allocates, or frees. Writes silence and returns NV_ERR_INVALID_ARGUMENT
/// when `channels` differs from nv_audio_set_output's (as during a format
/// change), and NV_ERR_CLOSED before an output is set or while another
/// thread renders. Render from one thread at a time, and stop rendering
/// before nv_core_free.
///
/// # Safety
/// `core` is a live handle; `out` is valid for writing `frames * channels`
/// floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_audio_render(
    core: *mut NvCore,
    out: *mut f32,
    frames: u32,
    channels: u32,
    host_time_ns: u64,
) -> NvStatus {
    guarded(|| {
        if out.is_null() || frames > MAX_RENDER_FRAMES || !(1..=2).contains(&channels) {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        // SAFETY: non-null and, per the caller, valid for this many floats
        // until the call returns.
        let out = unsafe { std::slice::from_raw_parts_mut(out, (frames * channels) as usize) };
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            out.fill(0.0);
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        match c.render_audio(out, channels as usize, host_time_ns) {
            Ok(()) => NvStatus::NV_OK,
            Err(e) => error_status(e),
        }
    })
}

/// Makes a tile audible (it claims one of eight playout slots) or silent
/// (it releases its slot), at `volume` from 0 (muted, keeping its slot and
/// timing) to 1 (more amplifies). Tiles start silent.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_audio_set(
    core: *mut NvCore,
    tile: u64,
    audible: bool,
    volume: f32,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            Some(c) => status(c.set_tile_audio(tile, audible, volume)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Starts or stops a tile's audio tap: on_audio_tap receives its decoded
/// audio as 16 kHz mono blocks (64 ms each), audible or not. A slow
/// consumer loses the oldest blocks (counted in audio_stats). No block for
/// the tile arrives after a call that stops it returns. NV_ERR_INVALID_ARGUMENT
/// if on_audio_tap was NULL.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_audio_tap(
    core: *mut NvCore,
    tile: u64,
    enabled: bool,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(nv) = (unsafe { core.as_ref() }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        if enabled && !nv.taps {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        status(nv.core.set_tile_audio_tap(tile, enabled))
    })
}

/// Peak and RMS level of a tile's decoded audio, 0..1, before the tile's
/// volume (peak with a 0.3 s release, RMS over about 0.1 s), audible or not:
/// as nv_audio_render measured it for a tile that plays while the app
/// renders, otherwise as its audio was decoded, falling to 0 within a
/// second when no audio arrives. Both read 0 for a tile without audio or
/// not known. Lock-free and allocation-free; callable from any thread,
/// including callbacks, at display rate.
///
/// # Safety
/// `core` is a live handle; `peak` and `rms` are valid for one write each.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_audio_level(
    core: *mut NvCore,
    tile: u64,
    peak: *mut f32,
    rms: *mut f32,
) -> NvStatus {
    guarded(|| {
        if peak.is_null() || rms.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        let (p, r) = c.tile_audio_level(tile);
        // SAFETY: both non-null and valid for one write per the caller.
        unsafe {
            put(peak, p);
            put(rms, r);
        }
        NvStatus::NV_OK
    })
}

/// Enables or disables talk-back (camera motion control) on one channel of
/// a device, for this run of the core. Off until enabled; the app passes
/// the user's per-camera setting after each nv_device_add. Disabling stops
/// a running talk on that channel.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_talk_set_permission(
    core: *mut NvCore,
    device: u64,
    channel: u8,
    allowed: bool,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            Some(c) => status(c.set_talk_permission(device, channel, allowed)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Starts talk-back to a tile's camera and returns at once; progress
/// arrives as `talk` events. NV_ERR_NOT_PERMITTED (and a `talk` `refused`
/// event; nothing is sent to the device) unless nv_talk_set_permission
/// enabled the camera.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_talk_start(core: *mut NvCore, tile: u64) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            Some(c) => status(c.talk_start(tile)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Queues `count` mono float samples (-1..1) at `sample_rate` Hz (8000 to
/// 192000) for a tile's talk; the core resamples to 16 kHz, encodes, and
/// sends them in real time. Samples for a tile without a talk are ignored.
/// Short lock, may allocate: not from a real-time audio callback.
///
/// # Safety
/// `core` is a live handle; `samples` is valid for reading `count` floats
/// (or `count` is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_talk_push(
    core: *mut NvCore,
    tile: u64,
    samples: *const f32,
    count: usize,
    sample_rate: u32,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        let pcm: &[f32] = if count == 0 {
            &[]
        } else if samples.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        } else {
            // SAFETY: non-null and valid for `count` floats per the caller.
            unsafe { std::slice::from_raw_parts(samples, count) }
        };
        status(c.talk_push(tile, pcm, sample_rate))
    })
}

/// Ends a tile's talk: at once, or with `drain` after the audio already
/// pushed has been sent and then 1.5 s of silence (the device plays about
/// a second behind and drops what it has not played at the close); audio
/// pushed before the close continues the same talk. The device's close is
/// sent either way.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_talk_stop(core: *mut NvCore, tile: u64, drain: bool) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            Some(c) => status(c.talk_stop(tile, drain)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Marks a tile's camera as in a conversation the user has open in the app
/// (a doorbell call, from a ring or Start Conversation), or no longer
/// (`open` false, when the call ends). While marked, talk-back starts on
/// that camera are not limited per minute (outside a conversation: 20 per
/// camera per minute); they still need nv_talk_set_permission and are
/// logged. The mark also ends when the tile closes, when the camera's
/// permission is withdrawn, and when the core is freed. NV_ERR_UNKNOWN_ID
/// for an unknown tile. Added in minor 4.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_talk_set_conversation(
    core: *mut NvCore,
    tile: u64,
    open: bool,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            Some(c) => status(c.set_talk_conversation(tile, open)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Copies `text` and a NUL into `out` when it fits (`cap` > its length);
/// `*len` always receives its length without the NUL.
///
/// # Safety
/// `out` is null or valid for writing `cap` bytes; `len` is valid for one
/// write.
unsafe fn copy_out(text: &str, out: *mut c_char, cap: usize, len: *mut usize) -> NvStatus {
    // SAFETY: per the caller.
    if !unsafe { put(len, text.len()) } {
        return NvStatus::NV_ERR_INVALID_ARGUMENT;
    }
    if out.is_null() || cap <= text.len() {
        return NvStatus::NV_ERR_BUFFER_TOO_SMALL;
    }
    // SAFETY: `out` is valid for `cap` > text.len() bytes per the caller,
    // and does not overlap a string the core owns.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), out.cast::<u8>(), text.len());
        out.add(text.len()).write(0);
    }
    NvStatus::NV_OK
}

/// The device's stable key (docs/FFI.md, "Layouts"): UTF-8 into `out`,
/// NUL-terminated, `*len` bytes without the NUL. The same address (host and
/// port) gets the same key in every run while the data directory is kept.
/// NV_ERR_BUFFER_TOO_SMALL (with `*len` set) when `cap` <= `*len`.
///
/// # Safety
/// `core` is a live handle; `out` is null or valid for writing `cap` bytes;
/// `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_device_key(
    core: *mut NvCore,
    device: u64,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        match c.device_key(device) {
            // SAFETY: per the caller.
            Ok(k) => unsafe { copy_out(&k, out, cap, len) },
            Err(e) => error_status(e),
        }
    })
}

/// Every stored layout as JSON, `{"revision":N,"layouts":[...]}`, into `out`
/// (NUL-terminated; `*len` bytes without the NUL). NV_ERR_BUFFER_TOO_SMALL
/// (with `*len` set) when `cap` <= `*len`: call again with a larger buffer.
///
/// # Safety
/// `core` is a live handle; `out` is null or valid for writing `cap` bytes;
/// `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_layouts_get(
    core: *mut NvCore,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        // SAFETY: per the caller.
        unsafe { copy_out(&c.layouts_json(), out, cap, len) }
    })
}

/// Inserts or replaces (by "id") one layout given as JSON. The core
/// validates it (NV_ERR_INVALID_ARGUMENT if not a valid split tree), sets
/// its "version", stores it atomically, and sends layouts_changed.
/// NV_ERR_LIMIT when the store is full, NV_ERR_INTERNAL when it cannot be
/// written (nothing changes).
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_layout_put(core: *mut NvCore, json: *const c_char) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let (Some(c), Some(json)) = (unsafe { core_ref(core) }, unsafe { string(json) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        match c.put_layout(json) {
            Ok(_) => NvStatus::NV_OK,
            Err(e) => error_status(e),
        }
    })
}

/// Deletes a layout by id and sends layouts_changed. NV_ERR_UNKNOWN_ID if
/// there is none.
///
/// # Safety
/// `core` is a live handle; `id` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_layout_delete(core: *mut NvCore, id: *const c_char) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let (Some(c), Some(id)) = (unsafe { core_ref(core) }, unsafe { string(id) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        status(c.delete_layout(id))
    })
}

/// Starts a search for devices on the local network (minor 1): a few rounds
/// of UDP broadcasts, and the WS-Discovery multicast, on each IPv4 interface
/// that is up (or on the interfaces named in json), then listening until
/// the time is up. Returns at once. Answers arrive as "discovered" events
/// and the end as one "discovery_done" event (docs/FFI.md, "Device
/// discovery"). json may be NULL or empty for the defaults, or:
/// {"seconds":3,"interfaces":["192.0.2.5"],"onvif":true}
/// (seconds 0.5 to 30; interfaces are IPv4 addresses of this computer).
/// NV_ERR_INVALID_ARGUMENT for invalid JSON or values. One search per core:
/// a start while one runs stops it (its discovery_done has "stopped":true)
/// and starts the new one. The core never searches on its own.
///
/// # Safety
/// `core` is a live handle; `json` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_discover_start(core: *mut NvCore, json: *const c_char) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        let request = if json.is_null() {
            None
        } else {
            // SAFETY: per the caller.
            match unsafe { string(json) } {
                Some(s) => Some(s),
                None => return NvStatus::NV_ERR_INVALID_ARGUMENT,
            }
        };
        match viewer_core::discover::parse_options(request) {
            Ok(options) => status(c.discover_start(options).map(|_| ())),
            Err(e) => error_status(e),
        }
    })
}

/// Stops the running search, if any, and closes its sockets; its
/// discovery_done event ("stopped":true) is queued before this returns.
/// NV_OK when nothing is running.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_discover_stop(core: *mut NvCore) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        status(c.discover_stop())
    })
}

/// Starts an update check (minor 2; docs/FFI.md, "Updates";
/// docs/UPDATES.md): fetch the source's manifest and signature, verify it
/// with the trusted keys, and compare versions. Returns at once; the outcome
/// arrives as "update" events ("started", then one of "up_to_date",
/// "available", "failed", "canceled"). json:
/// {"source":"https://... | /folder","keys":["nvrview-ed25519-pub ..."],
///  "current":"0.3.0","platform":"macos-arm64"}
/// (platform optional: the core's own by default). NV_ERR_INVALID_ARGUMENT
/// for invalid JSON, a key that is not an update public key, or a version
/// that is not semantic; a bad source is reported by the event.
/// NV_ERR_LIMIT while another update operation runs (one at a time). The
/// core never checks on its own.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_update_check(core: *mut NvCore, json: *const c_char) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { update_start(core, json, false) }
}

/// Downloads, verifies, and unpacks the version the user accepted
/// (minor 2): the manifest is fetched and verified again and must still
/// offer it; the package's size and SHA-256 must match before anything is
/// unpacked into staging_dir (which must not exist; it is created). Events:
/// "started", "downloading" (received, total), then one of "ready" (path of
/// the unpacked package), "failed", "canceled". json: the check's members
/// plus "version" (the accepted version) and "staging_dir" (an absolute
/// path on the same volume as the installed app). On failure the core
/// removes everything it created. Errors as nv_update_check.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_update_download(core: *mut NvCore, json: *const c_char) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { update_start(core, json, true) }
}

/// Shared by nv_update_check and nv_update_download.
///
/// # Safety
/// As those functions.
unsafe fn update_start(core: *mut NvCore, json: *const c_char, download: bool) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let (Some(c), Some(json)) = (unsafe { core_ref(core) }, unsafe { string(json) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        match viewer_core::update::parse_request(json, download) {
            Ok(request) => status(c.update_start(request).map(|_| ())),
            Err(e) => error_status(e),
        }
    })
}

/// Cancels the running update check or download, if any (minor 2). Its
/// final event ("canceled", or the outcome it reached first) is queued
/// before this returns, within about half a second; nothing more of that
/// operation follows. NV_OK when nothing runs.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_update_cancel(core: *mut NvCore) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        status(c.update_cancel())
    })
}

/// Parses an update public key as a user pastes it or as a key file holds
/// it (pure; no core needed; minor 2). On success `out` receives
/// {"key":"nvrview-ed25519-pub <hex>","id":"<16 hex digits>"}: the
/// canonical text to store and the ID to show. NV_ERR_INVALID_ARGUMENT
/// when the text is not one valid key. NUL-terminated, `*len` bytes
/// without the NUL; NV_ERR_BUFFER_TOO_SMALL (with `*len` set) when `cap`
/// <= `*len`.
///
/// # Safety
/// `text` is a NUL-terminated string; `out` is null or valid for writing
/// `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_update_parse_key(
    text: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(text) = (unsafe { string(text) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        match viewer_core::update::key_info(text) {
            // SAFETY: per the caller.
            Some(info) => unsafe { copy_out(&info, out, cap, len) },
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
    })
}

/// Checks an update source as the user typed it (pure; no core needed;
/// minor 2). `out` receives {"ok":true,"kind":"folder|https|http",
/// "display":"..."} or {"ok":false,"reason":"source_invalid|
/// insecure_source","message":"..."} (NV_OK either way). Out-buffer rules
/// as nv_update_parse_key.
///
/// # Safety
/// `text` is a NUL-terminated string; `out` is null or valid for writing
/// `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_update_parse_source(
    text: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(text) = (unsafe { string(text) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        // SAFETY: per the caller.
        unsafe { copy_out(&viewer_core::update::source_info(text), out, cap, len) }
    })
}

/// Grid layout (pure; no core needed). Fills `out` (tile_count entries) with
/// the destination rectangle in the viewport and the source rectangle in the
/// visible picture. picture_sizes may contain zero sizes for tiles with no
/// picture yet. fill = false letterboxes, true crops.
///
/// # Safety
/// For `tile_count` > 0: `picture_sizes` is null (all sizes unknown) or valid
/// for reading `tile_count` entries, and `out` is valid for writing
/// `tile_count` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_grid_layout(
    tile_count: u32,
    viewport: NvSize,
    gap: u32,
    picture_sizes: *const NvSize,
    fill: bool,
    out: *mut NvTileLayout,
) -> NvStatus {
    use viewer_core::layout::{FitMode, Rect, Size, TileLayout, grid_layout_into};
    guarded(|| {
        if tile_count == 0 {
            return NvStatus::NV_OK;
        }
        if out.is_null() || tile_count > MAX_LAYOUT_TILES {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        let n = tile_count as usize;
        let sizes: Vec<Size> = if picture_sizes.is_null() {
            Vec::new()
        } else {
            // SAFETY: non-null and, per the caller, valid for reading `n`
            // entries for the duration of this call.
            let given = unsafe { std::slice::from_raw_parts(picture_sizes, n) };
            given.iter().map(|s| Size::new(s.width, s.height)).collect()
        };
        let mut layouts = vec![TileLayout::default(); n];
        let mode = if fill { FitMode::Fill } else { FitMode::Fit };
        grid_layout_into(
            0,
            Size::new(viewport.width, viewport.height),
            gap,
            &sizes,
            mode,
            &mut layouts,
        );
        let rect = |r: Rect| NvRect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        };
        // SAFETY: non-null and, per the caller, valid for writing `n`
        // entries; NvTileLayout is plain data.
        let out = unsafe { std::slice::from_raw_parts_mut(out, n) };
        for (o, l) in out.iter_mut().zip(&layouts) {
            *o = NvTileLayout {
                cell: rect(l.cell),
                dest: rect(l.dest),
                source: rect(l.source),
            };
        }
        NvStatus::NV_OK
    })
}

// ---- Recordings, part 1 (minor 5; docs/FFI-RECORDINGS.md, sections 1-5) --

impl From<RecError> for NvStatus {
    fn from(e: RecError) -> NvStatus {
        match e {
            RecError::InvalidArgument => NvStatus::NV_ERR_INVALID_ARGUMENT,
            RecError::UnknownId => NvStatus::NV_ERR_UNKNOWN_ID,
            RecError::Closed => NvStatus::NV_ERR_CLOSED,
            RecError::Limit => NvStatus::NV_ERR_LIMIT,
            RecError::Internal => NvStatus::NV_ERR_INTERNAL,
            RecError::NotConfigured => NvStatus::NV_ERR_NOT_CONFIGURED,
            RecError::Unsupported => NvStatus::NV_ERR_UNSUPPORTED,
        }
    }
}

/// A required string argument.
///
/// # Safety
/// As [`string`].
unsafe fn arg<'a>(p: *const c_char) -> Result<&'a str, NvStatus> {
    // SAFETY: per the caller.
    unsafe { string(p) }.ok_or(NvStatus::NV_ERR_INVALID_ARGUMENT)
}

/// An optional string argument: NULL is `None`, bad UTF-8 an error.
///
/// # Safety
/// As [`string`].
unsafe fn opt_arg<'a>(p: *const c_char) -> Result<Option<&'a str>, NvStatus> {
    if p.is_null() {
        Ok(None)
    } else {
        // SAFETY: per the caller.
        unsafe { arg(p) }.map(Some)
    }
}

/// Runs a recordings call without an answer.
///
/// # Safety
/// `core` is null or a live handle.
unsafe fn rec_call(core: *mut NvCore, f: impl FnOnce(&Core) -> Result<(), NvStatus>) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        match f(c) {
            Ok(()) => NvStatus::NV_OK,
            Err(e) => e,
        }
    })
}

/// Runs a recordings call that answers JSON through `out`/`cap`/`len`.
/// `len` is checked before `f` runs, so a call that changes state never
/// does so without being able to report the answer's size.
///
/// # Safety
/// `core` is null or a live handle; `out` is null or valid for writing
/// `cap` bytes; `len` is null or valid for one write.
unsafe fn rec_answer(
    core: *mut NvCore,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
    f: impl FnOnce(&Core) -> Result<String, NvStatus>,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        if len.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        match f(c) {
            // SAFETY: per the caller.
            Ok(text) => unsafe { copy_out(&text, out, cap, len) },
            Err(e) => e,
        }
    })
}

/// Sets the recordings configuration (docs/FFI-RECORDINGS.md, section 1):
/// {"downloads_dir":"<absolute path>","index":{...},"grouping":{...},
///  "thumbnails":{...},"downloads":{...},"playback":{...},"naming":{...}};
/// every member optional, unknown members ignored. Call once after
/// nv_core_new and again to change it. Starts resuming unfinished downloads
/// of an earlier run (they wait for their devices). NV_ERR_INVALID_ARGUMENT
/// for invalid JSON or values. Added in minor 5, as everything down to
/// nv_sound_configure.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_recordings_configure(
    core: *mut NvCore,
    json: *const c_char,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.recordings_configure(arg(json)?)?)) }
}

/// Allows (json non-NULL) or stops (json NULL) indexing one device by its
/// stable key: {"channels":[{"channel":0},{"channel":3,"lens":1}] or "all",
/// "utc_offset_s":7200,"background":{"days_back":14,"pass_interval_s":60}}.
/// Nothing is searched until a view asks (nv_recordings_want,
/// nv_recordings_hold_today, a thumbnail), unless "background" is set. The
/// device need not be added yet. NV_ERR_NOT_CONFIGURED before
/// nv_recordings_configure; NV_ERR_UNSUPPORTED if the device's backend has
/// no recordings.
///
/// # Safety
/// `core` is a live handle; `device_key` is a NUL-terminated string; `json`
/// is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_recordings_enable(
    core: *mut NvCore,
    device_key: *const c_char,
    json: *const c_char,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            Ok(c.recordings_enable(arg(device_key)?, opt_arg(json)?)?)
        })
    }
}

/// Days with recordings per camera in [start, end]:
/// {"revision":N,"truncated":false,"days":[{"camera":{...},"days":[...]}]}.
/// Queries take {"start":S,"end":E,"cameras":[...],"kinds":[...],
/// "limit":5000} (start and end required), answer from memory, and never
/// cause a search. Answers come through out/cap/len: NUL-terminated, *len
/// bytes without the NUL; NV_ERR_BUFFER_TOO_SMALL (with *len set) when out
/// is NULL or cap <= *len. The index may change between two calls, so loop
/// until the answer fits.
///
/// # Safety
/// `core` is a live handle; `query` is a NUL-terminated string; `out` is
/// null or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_recordings_calendar(
    core: *mut NvCore,
    query: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_answer(core, out, cap, len, |c| {
            Ok(c.recordings_calendar(arg(query)?)?)
        })
    }
}

/// Continuous recording spans per camera (timeline lanes):
/// {"revision":N,"truncated":false,"spans":[{"camera":{...},"spans":[{"start",
/// "end","bytes","causes","tier"}]}]}. As nv_recordings_calendar.
///
/// # Safety
/// As nv_recordings_calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_recordings_spans(
    core: *mut NvCore,
    query: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_answer(
            core,
            out,
            cap,
            len,
            |c| Ok(c.recordings_spans(arg(query)?)?),
        )
    }
}

/// Events matching the query, by start:
/// {"revision":N,"truncated":false,"events":[{"camera","kind","source",
/// "start","end"}]}. As nv_recordings_calendar.
///
/// # Safety
/// As nv_recordings_calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_recordings_events(
    core: *mut NvCore,
    query: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_answer(core, out, cap, len, |c| {
            Ok(c.recordings_events(arg(query)?)?)
        })
    }
}

/// Asks for these cameras' days to be in the index:
/// {"cameras":[{"device":"<key>","channel":3}],"days":[S,...],"months":[S,...],
/// "force":false} (days and months: any second within each, device-local;
/// "months", minor 7, asks for only those months' calendars, after the days;
/// either list may be left out, not both). Returns at once;
/// recordings_changed follows for each device as days and months arrive.
/// Days and past months already indexed are searched again only with
/// "force" (the user's refresh). NV_ERR_LIMIT for more than 31 days or 12
/// months or when a device's queue (4096 camera-days) would overflow
/// (nothing is queued); NV_ERR_INVALID_ARGUMENT for a camera
/// nv_recordings_enable does not allow.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_recordings_want(core: *mut NvCore, json: *const c_char) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.recordings_want(arg(json)?)?)) }
}

/// Keeps today fresh for these cameras while the hold lasts: `hold` is any
/// nonzero number the app chooses per window; json {"cameras":[...]} sets
/// or replaces that hold's cameras, NULL releases it. Every hold ends with
/// nv_core_free, and a device no longer enabled leaves every hold.
///
/// # Safety
/// `core` is a live handle; `json` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_recordings_hold_today(
    core: *mut NvCore,
    hold: u64,
    json: *const c_char,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.recordings_hold_today(hold, opt_arg(json)?)?)) }
}

/// Index state of every enabled device: {"revision":N,"devices":[{"device",
/// "state","pending_days","revision"}]}, as the recordings_index events say.
/// Out-buffer rules as nv_recordings_calendar.
///
/// # Safety
/// `core` is a live handle; `out` is null or valid for writing `cap` bytes;
/// `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_recordings_status(
    core: *mut NvCore,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.recordings_status()?)) }
}

/// Incidents overlapping [start, end], newest first:
/// {"revision":N,"truncated":false,"incidents":[...]}. Query as for
/// nv_recordings_calendar, plus "group". Out-buffer rules as there.
///
/// # Safety
/// As nv_recordings_calendar.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_incidents_get(
    core: *mut NvCore,
    query: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.incidents_get(arg(query)?)?)) }
}

/// One incident by its key: {"revision":N,"incident":{...}}.
/// NV_ERR_UNKNOWN_ID if no incident has it now. Out-buffer rules as
/// nv_recordings_calendar.
///
/// # Safety
/// `core` is a live handle; `key` is a NUL-terminated string; `out` is null
/// or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_incident_get(
    core: *mut NvCore,
    key: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.incident_get(arg(key)?)?)) }
}

/// Applies one edit ({"op":"merge"|"split"|"rename"|"add_camera"|
/// "remove_camera"|"revert",...}), stores it, sends incidents_changed, and
/// answers {"revision":N,"keys":[...]}. NV_ERR_UNKNOWN_ID for an unknown
/// key; NV_ERR_INVALID_ARGUMENT for an edit that cannot apply (nothing is
/// stored); NV_ERR_LIMIT beyond 10000 edits or 1000 incidents in a merge.
/// The edit is made even when the answer does not fit: then
/// NV_ERR_BUFFER_TOO_SMALL only says the answer was not written (do not
/// apply the edit again).
///
/// # Safety
/// `core` is a live handle; `edit` is a NUL-terminated string; `out` is
/// null or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_incident_edit(
    core: *mut NvCore,
    edit: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.incident_edit(arg(edit)?)?)) }
}

/// Locks (locked true) or unlocks an incident by key; stores it, sends
/// incidents_changed, and answers {"revision":N,"keys":[...]} plus, when
/// locking, "download", "folder", and "download_status" ("queued",
/// "existing", "when_settled", "not_configured", "limit"). Locking queues
/// the incident's download. NV_ERR_UNKNOWN_ID if no incident has the key;
/// NV_ERR_LIMIT beyond 1000 locks or 31 days. As for nv_incident_edit, the
/// change is made even when the answer does not fit.
///
/// # Safety
/// As nv_incident_get.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_incident_lock(
    core: *mut NvCore,
    key: *const c_char,
    locked: bool,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_answer(core, out, cap, len, |c| {
            Ok(c.incident_lock(arg(key)?, locked)?)
        })
    }
}

/// Camera groups: {"revision":N,"groups":[{"name","cameras"}],
/// "camera_names":[{"camera","name"}]}. Out-buffer rules as
/// nv_recordings_calendar.
///
/// # Safety
/// `core` is a live handle; `out` is null or valid for writing `cap` bytes;
/// `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_camera_groups_get(
    core: *mut NvCore,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.camera_groups_get()?)) }
}

/// Replaces the camera groups and names ({"groups":[...],
/// "camera_names":[...]}); regroups; sends camera_groups_changed and
/// incidents_changed. NV_ERR_LIMIT beyond 64 groups of 64 cameras, 4096
/// names, or 256-byte names (nothing stored).
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_camera_groups_put(core: *mut NvCore, json: *const c_char) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.camera_groups_put(arg(json)?)?)) }
}

fn thumb_kind(k: ThumbKind) -> NvThumbKindValue {
    (match k {
        ThumbKind::H264Keyframe => NvThumbKind::NV_THUMB_H264_KEYFRAME,
        ThumbKind::H265Keyframe => NvThumbKind::NV_THUMB_H265_KEYFRAME,
        ThumbKind::Image => NvThumbKind::NV_THUMB_IMAGE,
    }) as NvThumbKindValue
}

fn thumb_kind_from(k: NvThumbKindValue) -> Option<ThumbKind> {
    Some(match k {
        1 => ThumbKind::H264Keyframe,
        2 => ThumbKind::H265Keyframe,
        3 => ThumbKind::Image,
        _ => return None,
    })
}

/// Copies a cached cover thumbnail into `out`. `key` is the JSON object
/// {"camera":{"device","channel","lens"},"tier":"sub","start":S,"end":E}.
/// NV_OK: *info, out, and *len (the byte count) filled. NV_PENDING: not
/// cached; a fetch is queued (or already was), *len is 0, and a thumbnail
/// event follows. NV_ERR_BUFFER_TOO_SMALL: *info and *len filled (out is
/// NULL or cap < *len; the bytes have no terminator). Never waits on the
/// device.
///
/// # Safety
/// `core` is a live handle; `key` is a NUL-terminated string; `info` and
/// `len` are valid for one write; `out` is null or valid for writing `cap`
/// bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_thumbnail_get(
    core: *mut NvCore,
    key: *const c_char,
    info: *mut NvThumbnail,
    out: *mut u8,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let (Some(c), Some(key)) = (unsafe { core_ref(core) }, unsafe { string(key) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        if info.is_null() || len.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        match c.thumbnail_get(key) {
            Err(e) => e.into(),
            Ok(ThumbnailAnswer::Pending) => {
                // SAFETY: non-null and valid for one write per the caller.
                unsafe { put(len, 0) };
                NvStatus::NV_PENDING
            }
            Ok(ThumbnailAnswer::Ready(t)) => {
                let d = NvThumbnail {
                    kind: thumb_kind(t.kind),
                    width: t.width,
                    height: t.height,
                    pts_us: i64::try_from(t.pts_us).unwrap_or(i64::MAX),
                };
                // SAFETY: both non-null and valid for one write per the
                // caller.
                unsafe {
                    put(info, d);
                    put(len, t.data.len());
                }
                if t.data.is_empty() {
                    return NvStatus::NV_OK;
                }
                if out.is_null() || cap < t.data.len() {
                    return NvStatus::NV_ERR_BUFFER_TOO_SMALL;
                }
                // SAFETY: `out` is valid for `cap` >= data.len() bytes per
                // the caller and does not overlap memory the core owns.
                unsafe { std::ptr::copy_nonoverlapping(t.data.as_ptr(), out, t.data.len()) };
                NvStatus::NV_OK
            }
        }
    })
}

/// Replaces the cached entry of `key` (usually with NV_THUMB_IMAGE after
/// decoding). The bytes are copied. NV_ERR_LIMIT if larger than the cache's
/// byte limit.
///
/// # Safety
/// `core` is a live handle; `key` is a NUL-terminated string; `info` is
/// valid for reading one NvThumbnail; `data` is valid for reading `len`
/// bytes (or `len` is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_thumbnail_put(
    core: *mut NvCore,
    key: *const c_char,
    info: *const NvThumbnail,
    data: *const u8,
    len: usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            let key = arg(key)?;
            // SAFETY: null or valid for one read per the caller; every
            // field is a plain integer, so any bit pattern is a value.
            let info = info.as_ref().ok_or(NvStatus::NV_ERR_INVALID_ARGUMENT)?;
            let kind = thumb_kind_from(info.kind).ok_or(NvStatus::NV_ERR_INVALID_ARGUMENT)?;
            let pts_us =
                u64::try_from(info.pts_us).map_err(|_| NvStatus::NV_ERR_INVALID_ARGUMENT)?;
            let bytes: &[u8] = if len == 0 {
                &[]
            } else if data.is_null() {
                return Err(NvStatus::NV_ERR_INVALID_ARGUMENT);
            } else {
                // SAFETY: non-null and valid for `len` bytes per the caller;
                // copied below before the call returns.
                std::slice::from_raw_parts(data, len)
            };
            let thumb = Thumbnail {
                kind,
                width: info.width,
                height: info.height,
                pts_us,
                data: bytes.to_vec(),
            };
            Ok(c.thumbnail_put(key, &thumb)?)
        })
    }
}

/// Drops queued thumbnail fetches: of one key, or every queued fetch when
/// key is NULL (the view scrolled away). A fetch already sent completes and
/// is cached.
///
/// # Safety
/// `core` is a live handle; `key` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_thumbnail_cancel(core: *mut NvCore, key: *const c_char) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.thumbnail_cancel(opt_arg(key)?)?)) }
}

/// Queues an incident's download ({"incident":"<key>","cameras":[{"camera":
/// {...},"tier":"main"}],"dest":"<absolute directory>","share_safe":false},
/// all but incident optional) and answers {"download":"<id>",
/// "folder":"<absolute path>","existing":bool}. An unfinished download of
/// the same incident key, or a finished one of the unchanged incident, is
/// returned instead. NV_ERR_NOT_CONFIGURED without downloads_dir;
/// NV_ERR_UNKNOWN_ID if the key matches no incident; NV_ERR_LIMIT if 1000
/// downloads are known. As for nv_incident_edit, the download is queued
/// even when the answer does not fit.
///
/// # Safety
/// `core` is a live handle; `request` is a NUL-terminated string; `out` is
/// null or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_download_incident(
    core: *mut NvCore,
    request: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_answer(core, out, cap, len, |c| {
            Ok(c.download_incident(arg(request)?)?)
        })
    }
}

/// Queues the download of a time range of chosen cameras ({"start":..,
/// "end":..,"cameras":[{"camera":{...},"tier":"main"}],"name":null,
/// "dest":"<absolute directory>","share_safe":false}; seconds, device-local)
/// through the same scheduler as nv_download_incident, and answers
/// {"download":"<id>","folder":"<absolute path>","existing":bool}. The same
/// cameras, tiers, and range return the existing download.
/// NV_ERR_NOT_CONFIGURED without downloads_dir (or without a data directory
/// when dest is elsewhere); NV_ERR_LIMIT beyond 16 cameras, 24 hours, or 1000
/// known downloads; NV_ERR_INVALID_ARGUMENT for an empty range or a camera
/// that nv_recordings_enable does not allow. As for nv_incident_edit, the
/// download is queued even when the answer does not fit. Added in minor 10.
///
/// # Safety
/// `core` is a live handle; `request` is a NUL-terminated string; `out` is
/// null or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_download_range(
    core: *mut NvCore,
    request: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_answer(
            core,
            out,
            cap,
            len,
            |c| Ok(c.download_range(arg(request)?)?),
        )
    }
}

/// Plans still images and answers their sources and suggested files as JSON
/// (section 7a). {"playback":<id>,"tiles":[..],"format":"png","dest":..,
/// "share_safe":false} names the picture each playback tile shows (the
/// newest one the app answered with nv_tile_decoded); {"download":"<id>",
/// "camera":{...},"at_ms":..,"exact":true,"format":"jpeg"} names a position
/// in a finished file. Answers from memory. NV_ERR_UNKNOWN_ID for an unknown
/// playback, tile, or download; NV_ERR_INVALID_ARGUMENT for invalid JSON or
/// values, a time outside the file, or a tile that has shown nothing.
/// Out-buffer rules as nv_recordings_calendar. Added in minor 10.
///
/// # Safety
/// `core` is a live handle; `request` is a NUL-terminated string; `out` is
/// null or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_still_plan(
    core: *mut NvCore,
    request: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.still_plan(arg(request)?)?)) }
}

/// Pauses or resumes a whole download (camera NULL) or one camera of it
/// (a JSON camera object). A running chunk is stopped and fetched again on
/// resume. Persisted. NV_ERR_UNKNOWN_ID for an unknown download or a camera
/// not in it.
///
/// # Safety
/// `core` is a live handle; `download` is a NUL-terminated string; `camera`
/// is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_download_pause(
    core: *mut NvCore,
    download: *const c_char,
    camera: *const c_char,
    paused: bool,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            Ok(c.download_pause(arg(download)?, opt_arg(camera)?, paused)?)
        })
    }
}

/// Queues failed camera jobs again with fresh attempts: every failed camera
/// (camera NULL) or one (a JSON camera object).
///
/// # Safety
/// As nv_download_pause.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_download_retry(
    core: *mut NvCore,
    download: *const c_char,
    camera: *const c_char,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            Ok(c.download_retry(arg(download)?, opt_arg(camera)?)?)
        })
    }
}

/// Forgets a download with no camera job running (pause it first;
/// NV_ERR_INVALID_ARGUMENT otherwise). With delete_partial, removes its
/// .parts folder; finished MP4 files and incident.json are never deleted.
/// download_removed follows.
///
/// # Safety
/// `core` is a live handle; `download` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_download_forget(
    core: *mut NvCore,
    download: *const c_char,
    delete_partial: bool,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            Ok(c.download_forget(arg(download)?, delete_partial)?)
        })
    }
}

/// For moving a download to the Trash: stops its camera jobs, forgets it,
/// and answers {"folder":"<absolute path>"}. The core deletes nothing; the
/// app moves the folder to the Trash or Recycle Bin after the
/// download_removed event ("trash":true) for it, which follows once nothing
/// writes there. NV_ERR_UNKNOWN_ID for an unknown download. As for
/// nv_incident_edit, the download is trashed even when the answer does not
/// fit (the event carries the folder too).
///
/// # Safety
/// `core` is a live handle; `download` is a NUL-terminated string; `out` is
/// null or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_download_trash(
    core: *mut NvCore,
    download: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_answer(core, out, cap, len, |c| {
            Ok(c.download_trash(arg(download)?)?)
        })
    }
}

/// Every known download: {"downloads":[...]}, each as in the download
/// event. No paging (at most 1000 downloads): out-buffer rules as
/// nv_recordings_calendar.
///
/// # Safety
/// `core` is a live handle; `out` is null or valid for writing `cap` bytes;
/// `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_downloads_get(
    core: *mut NvCore,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.downloads_get()?)) }
}

/// Live sound settings, JSON (unknown members ignored; members left out keep
/// their values): {"alarms":true,"loudness_history_s":600}. Applies to tiles
/// opened afterwards. NV_ERR_INVALID_ARGUMENT for invalid JSON or values.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_sound_configure(core: *mut NvCore, json: *const c_char) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.sound_configure(arg(json)?)?)) }
}

// ---- Edit lists and sound (minor 8; docs/FFI-RECORDINGS.md, sections 2a
// and 7) -----------------------------------------------------------------

/// Computes an edit list for a finished download, as JSON (section 7):
/// {"kind":"follow","download":"<id>","min_hold_ms":2000,"pre_roll_ms":1000,
///  "cut":"keyframe","priority":[{...}],"labels":{"name":true,"time":true}} or
/// {"kind":"grid","download":"<id>","layout":"4up","canvas":{...},"fit":"fit",
///  "cameras":[...],"offsets_ms":[...]}; every member but kind and download
/// optional. Pure computation over the incident and its finished files; no
/// device traffic. NV_ERR_UNKNOWN_ID for an unknown download;
/// NV_ERR_INVALID_ARGUMENT for invalid JSON or values or when no camera has
/// a finished file. Minor 10 added the "trim" kind
/// ({"kind":"trim","download":"<id>","cameras":[...],"in_ms":..,"out_ms":..,
/// "cut":"keyframe"|"exact"}), in_ms and out_ms on follow and grid, and dest
/// and share_safe on every kind (section 7a); a range download takes trim
/// only. Out-buffer rules as nv_recordings_calendar. Added in minor 8.
///
/// # Safety
/// `core` is a live handle; `request` is a NUL-terminated string; `out` is
/// null or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_edit_compute(
    core: *mut NvCore,
    request: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.edit_compute(arg(request)?)?)) }
}

/// Keeps a camera's sub-stream open for sound analysis while `enabled`, with
/// no tile and no video decoding: `camera` is {"device","channel","lens"}.
/// Standing by while a tile shows the channel. `sound_watch` events report
/// watching, stopped, or refused (reason no_audio, device_closed,
/// sound_off). At most 64 cameras; the core does not store the list. The
/// device need not be added yet. NV_ERR_NOT_CONFIGURED before
/// nv_recordings_configure. Added in minor 8.
///
/// # Safety
/// `core` is a live handle; `camera` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_sound_watch(
    core: *mut NvCore,
    camera: *const c_char,
    enabled: bool,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.sound_watch(arg(camera)?, enabled)?)) }
}

/// Loudness lanes for the timeline: {"start":S,"end":E,"bucket_s":60,
/// "cameras":[{"device","channel"}]} (bucket_s 1 to 3600, default 60;
/// cameras left out means every camera with loudness) answers {"revision":N,
/// "lanes":[{"camera","bucket_s","start","peak_db":[...],"rms_db":[...]}]},
/// whole dBFS or null where no audio was analyzed. Answers from memory;
/// NV_ERR_LIMIT beyond 100000 buckets per lane. Out-buffer rules as
/// nv_recordings_calendar. Added in minor 8.
///
/// # Safety
/// `core` is a live handle; `query` is a NUL-terminated string; `out` is
/// null or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_recordings_loudness(
    core: *mut NvCore,
    query: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_answer(core, out, cap, len, |c| {
            Ok(c.recordings_loudness(arg(query)?)?)
        })
    }
}

/// Labels the app's classifier produced for one camera and a time range:
/// {"camera":{...},"analyzer":"soundanalysis-v1","start":S,"end":E,
///  "labels":[{"kind","start","end","confidence","label"}]}. The core merges
/// them into the index as events of source local_audio, replacing earlier
/// labels of the same analyzer in the range. The analyzer pattern-v1 is the
/// core's. NV_ERR_INVALID_ARGUMENT for invalid values or a device that is
/// not enabled for recordings; NV_ERR_LIMIT beyond 20000 labels or 31 days.
/// Added in minor 8.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_sound_labels_put(core: *mut NvCore, json: *const c_char) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            c.sound_labels_put(arg(json)?)?;
            Ok(())
        })
    }
}

/// Asks for the sound of one camera's recorded day to be analyzed:
/// {"camera":{...},"day":S} (any second of the day, device-local). Answers
/// {"request":N,"state":"queued"}, or the existing request for the same
/// camera-day; sound_analyze events follow (running, done with seconds and
/// labels, failed with a reason, canceled). At most 32 requests, one running
/// per device. Asking again for a queued camera-day returns its request, so
/// a too-small buffer can be retried. Out-buffer rules as
/// nv_recordings_calendar. Added in minor 8.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string; `out` is null
/// or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_sound_analyze_day(
    core: *mut NvCore,
    json: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_answer(
            core,
            out,
            cap,
            len,
            |c| Ok(c.sound_analyze_day(arg(json)?)?),
        )
    }
}

/// Cancels an analysis request that is queued or running (a running one
/// stops at its next chunk and stores nothing). NV_ERR_UNKNOWN_ID for an
/// unknown or finished request. Added in minor 8.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_sound_analyze_cancel(core: *mut NvCore, request: u64) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.sound_analyze_cancel(request)?)) }
}

/// A live tile's recent loudness, oldest first, one byte per second and
/// value (0: no audio analyzed; 1..128: -127..0 dBFS, dBFS = byte - 128).
/// Writes at most `cap` values to each of `peak` and `rms` (the newest when
/// more are kept), the Unix second of the first to `*start_s`, and the count
/// to `*len` (0 before any audio). NV_ERR_UNKNOWN_ID for an unknown tile;
/// `peak` and `rms` may be NULL with `cap` 0 to learn only the count. Any
/// thread outside callbacks. Added in minor 8.
///
/// # Safety
/// `core` is a live handle; `peak` and `rms` are null or valid for writing
/// `cap` bytes each; `start_s` and `len` are valid for one write each.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_loudness(
    core: *mut NvCore,
    tile: u64,
    peak: *mut u8,
    rms: *mut u8,
    cap: usize,
    start_s: *mut i64,
    len: *mut usize,
) -> NvStatus {
    guarded(|| {
        if start_s.is_null() || len.is_null() || ((peak.is_null() || rms.is_null()) && cap > 0) {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        let l = match c.tile_loudness(tile) {
            Ok(l) => l,
            Err(e) => return error_status(e),
        };
        let n = l.peak.len().min(l.rms.len());
        let skip = n.saturating_sub(cap);
        let count = n - skip;
        // SAFETY: `peak` and `rms` are valid for `cap` >= `count` bytes each
        // when `count` > 0 (checked above); `start_s` and `len` are valid
        // for one write each.
        unsafe {
            for i in 0..count {
                peak.add(i).write(l.peak[skip + i].to_byte());
                rms.add(i).write(l.rms[skip + i].to_byte());
            }
            start_s.write(
                i64::try_from(l.start_s)
                    .unwrap_or(i64::MAX)
                    .saturating_add(skip as i64),
            );
            len.write(count);
        }
        NvStatus::NV_OK
    })
}

// ---- Recorded playback (minor 6; docs/FFI-RECORDINGS.md, section 6) ------

fn seek_mode(m: NvSeekModeValue) -> Option<SeekMode> {
    match m {
        0 => Some(SeekMode::Keyframe),
        1 => Some(SeekMode::Exact),
        _ => None,
    }
}

fn playback_state(s: PlaybackState) -> NvPlaybackState {
    match s {
        PlaybackState::Buffering => NvPlaybackState::NV_PLAYBACK_BUFFERING,
        PlaybackState::Playing => NvPlaybackState::NV_PLAYBACK_PLAYING,
        PlaybackState::Paused => NvPlaybackState::NV_PLAYBACK_PAUSED,
        PlaybackState::Ended => NvPlaybackState::NV_PLAYBACK_ENDED,
        PlaybackState::Failed => NvPlaybackState::NV_PLAYBACK_FAILED,
    }
}

/// The playback and tile IDs of the core's open answer
/// (`{"playback": N, "tiles": [...]}`).
fn open_answer(answer: &str) -> Option<(u64, Vec<u64>)> {
    let v: serde_json::Value = serde_json::from_str(answer).ok()?;
    let id = v["playback"].as_u64()?;
    let tiles = v["tiles"]
        .as_array()?
        .iter()
        .map(serde_json::Value::as_u64)
        .collect::<Option<Vec<u64>>>()?;
    Some((id, tiles))
}

/// How many cameras an open request names (an incident's, as it is now);
/// `None` when the request is invalid (opening reports why).
fn camera_count(c: &Core, json: &str) -> Option<usize> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if let Some(key) = v.get("incident").and_then(serde_json::Value::as_str) {
        let inc: serde_json::Value = serde_json::from_str(&c.incident_get(key).ok()?).ok()?;
        return inc["incident"]["cameras"].as_array().map(Vec::len);
    }
    v.get("cameras")?.as_array().map(Vec::len)
}

/// Opens a playback (docs/FFI-RECORDINGS.md, section 6), paused at
/// "position_ms" with an exact seek:
/// {"start_ms":S,"end_ms":E,"position_ms":P,"rate":1.0,"sync":"wait"|"free",
///  "cameras":[{"camera":{"device":"<key>","channel":3,"lens":0},
///  "tier":"sub"|"main","offset_ms":0}]}, or {"incident":"<key>","tier":"sub"}
/// for an incident's cameras and range with its margins. Times are
/// device-local milliseconds. Writes the playback's ID to *out_playback and
/// one tile ID per camera, in request order, to out_tiles (tile_cap
/// entries; *tile_count receives the number, at most 4). Playback tiles are
/// tiles like live ones (on_action, nv_tile_decoded, nv_tile_set_tier,
/// audio, tile_stats, tile_closed); nothing about them is delivered before
/// this call returns. When tile_cap is too small nothing is opened:
/// NV_ERR_BUFFER_TOO_SMALL with *tile_count set. (Only if an incident
/// gained a camera while it was being opened is the playback opened and
/// closed again; tile_closed and playback_closed for IDs never handed out
/// may then follow, and are ignored like any unknown ID.) NV_ERR_LIMIT beyond 8 playbacks, 4 cameras, a 24-hour range, or
/// the core's tile limit; NV_ERR_INVALID_ARGUMENT for invalid JSON, a rate
/// other than 0.25, 0.5, 1, 2, 4, 8, or 16, a camera twice, or a camera of
/// a device not enabled with nv_recordings_enable; NV_ERR_UNKNOWN_ID for an
/// unknown incident; NV_ERR_NOT_CONFIGURED before nv_recordings_configure.
/// Any thread, never inside a callback. Added in minor 6, as everything
/// down to nv_playback_close.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string;
/// `out_playback` and `tile_count` are valid for one write; `out_tiles` is
/// null or valid for writing `tile_cap` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_open(
    core: *mut NvCore,
    json: *const c_char,
    out_playback: *mut u64,
    out_tiles: *mut u64,
    tile_cap: usize,
    tile_count: *mut usize,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let (Some(c), Some(json)) = (unsafe { core_ref(core) }, unsafe { string(json) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        if out_playback.is_null() || tile_count.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        let fits = |n: usize| !out_tiles.is_null() && tile_cap >= n;
        // Known before opening in almost every case, so a small out_tiles
        // opens nothing.
        if let Some(n) = camera_count(c, json).filter(|n| *n <= MAX_CAMERAS && !fits(*n)) {
            // SAFETY: non-null and valid for one write per the caller.
            unsafe { put(tile_count, n) };
            return NvStatus::NV_ERR_BUFFER_TOO_SMALL;
        }
        let (answer, pending) = match c.playback_open_deferred(json) {
            Ok(a) => a,
            Err(e) => return e.into(),
        };
        let Some((id, tiles)) = open_answer(&answer) else {
            return NvStatus::NV_ERR_INTERNAL;
        };
        // SAFETY: non-null and valid for one write per the caller.
        unsafe { put(tile_count, tiles.len()) };
        if !fits(tiles.len()) {
            // An incident that gained cameras since it was counted: close
            // it again (the engine needs its gate open to stop).
            drop(pending);
            let _ = c.playback_close(id);
            return NvStatus::NV_ERR_BUFFER_TOO_SMALL;
        }
        // SAFETY: `out_tiles` is valid for `tile_cap` >= tiles.len()
        // entries and `out_playback` for one write, per the caller; neither
        // overlaps memory the core owns.
        unsafe {
            std::ptr::copy_nonoverlapping(tiles.as_ptr(), out_tiles, tiles.len());
            put(out_playback, id);
        }
        // Only now, with the IDs in the app's hands, do the tiles and the
        // playback's events start.
        pending.publish();
        NvStatus::NV_OK
    })
}

/// Adds a camera ({"camera":{...},"tier":"sub","offset_ms":0}); it joins at
/// the current position (its device's plays start again together). Writes
/// its tile ID to *out_tile. NV_ERR_LIMIT beyond 4 cameras or the tile
/// limit; NV_ERR_INVALID_ARGUMENT for a camera already in the playback.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string; `out_tile`
/// is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_add_camera(
    core: *mut NvCore,
    playback: u64,
    json: *const c_char,
    out_tile: *mut u64,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            let json = arg(json)?;
            if out_tile.is_null() {
                return Err(NvStatus::NV_ERR_INVALID_ARGUMENT);
            }
            let (tile, pending) = c.playback_add_camera_deferred(playback, json)?;
            // SAFETY: non-null and valid for one write per the caller.
            put(out_tile, tile);
            // The tile's actions start once the app has its ID.
            pending.publish();
            Ok(())
        })
    }
}

/// Removes one camera: its play stops and its tile closes (tile_closed,
/// reason "closed"). Waits for the playback's engine (bounded). Never call
/// it from a callback.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_remove_camera(
    core: *mut NvCore,
    playback: u64,
    tile: u64,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.playback_remove_camera(playback, tile)?)) }
}

/// Starts the shared clock (from start_ms after the end). Returns at once;
/// playback_state follows.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_play(core: *mut NvCore, playback: u64) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.playback_play(playback)?)) }
}

/// Holds the shared clock; the pictures stay on screen.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_pause(core: *mut NvCore, playback: u64) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.playback_pause(playback)?)) }
}

/// Moves the shared clock to position_ms (device-local; clamped to the
/// playback's range). Returns at once; every tile starts a new generation
/// (playback_seek events), and a newer seek replaces one still starting.
/// NV_ERR_INVALID_ARGUMENT for a mode that is not an NvSeekMode.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_seek(
    core: *mut NvCore,
    playback: u64,
    position_ms: i64,
    mode: NvSeekModeValue,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            let mode = seek_mode(mode).ok_or(NvStatus::NV_ERR_INVALID_ARGUMENT)?;
            Ok(c.playback_seek(playback, position_ms, mode)?)
        })
    }
}

/// Forward rate: 0.25, 0.5, 1, 2, 4, 8, or 16 (others
/// NV_ERR_INVALID_ARGUMENT). Audio plays at 1 only. Every tile starts a new
/// generation; playback_state reports the new rate.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_set_rate(
    core: *mut NvCore,
    playback: u64,
    rate: f64,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.playback_set_rate(playback, rate)?)) }
}

/// Per-camera clock offset: this camera shows position + offset_ms (a nudge
/// of a few seconds; at most 24 hours either way). Its device's plays start
/// again. NV_ERR_UNKNOWN_ID for a tile not in the playback.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_set_offset(
    core: *mut NvCore,
    playback: u64,
    tile: u64,
    offset_ms: i64,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            Ok(c.playback_set_offset(playback, tile, offset_ms)?)
        })
    }
}

/// The position (device-local ms) and state for the scrubber, as of the
/// engine's last step (every 10 ms). One brief lock; callable from any
/// thread, including inside callbacks.
///
/// # Safety
/// `core` is a live handle; `position_ms` and `state` are valid for one
/// write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_position(
    core: *mut NvCore,
    playback: u64,
    position_ms: *mut i64,
    state: *mut NvPlaybackState,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        if position_ms.is_null() || state.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        match c.playback_position(playback) {
            Ok((p, s)) => {
                // SAFETY: both non-null and valid for one write per the
                // caller.
                unsafe {
                    put(position_ms, p);
                    put(state, playback_state(s));
                }
                NvStatus::NV_OK
            }
            Err(e) => e.into(),
        }
    })
}

/// The playback's details: {"playback","state","position_ms","rate","sync",
/// "start_ms","end_ms","cameras":[{"tile","camera","tier","offset_ms",
/// "state","reason"?,"generation"}]}. Out-buffer rules as
/// nv_recordings_calendar; the answer may change between two calls. Never
/// call it from a callback.
///
/// # Safety
/// `core` is a live handle; `out` is null or valid for writing `cap` bytes;
/// `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_status(
    core: *mut NvCore,
    playback: u64,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.playback_status(playback)?)) }
}

/// Stops every play, closes every tile of the playback (tile_closed for
/// each), then playback_closed ("closed"). Waits for the playback's engine
/// (bounded). Never call it from a callback.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_playback_close(core: *mut NvCore, playback: u64) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.playback_close(playback)?)) }
}

// ---- Capture (minor 9) ----------------------------------------------------

/// Most bytes of an AudioSpecificConfig an encoder's `open` may write.
pub const NV_ASC_MAX: usize = 64;

/// The app's AAC encoder (docs/FFI.md, "Capture"), copied by
/// nv_capture_set_encoder. `track` is the audio track's index in the file
/// (a live recording: 0; a call: 0 Conversation, 1 Visitor, 2 You). All
/// three functions are called on the recorders' own threads (never at the
/// same time for one recording) and must return quickly. Each encoded frame
/// is answered with nv_record_audio_encoded, from inside `encode` or `close`
/// or from any other thread, until `close` returns. The functions and
/// `context` stay in use until nv_core_free returns.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvAudioEncoder {
    pub context: *mut c_void,
    /// Opens an AAC-LC encoder for one track of a recording. Writes the
    /// AudioSpecificConfig into asc (*asc_len bytes, 1 to asc_cap, which is
    /// NV_ASC_MAX) and the encoder's priming in frames. Returns false if it
    /// cannot.
    pub open: NvEncoderOpen,
    /// One frame of 1024 mono float samples (`frames` is 1024), valid only
    /// during the call.
    pub encode: NvEncoderEncode,
    /// Ends the track; with flush, the encoder's remaining output first.
    pub close: NvEncoderClose,
}

pub type NvEncoderOpen = Option<
    unsafe extern "C" fn(
        context: *mut c_void,
        recording: u64,
        track: u32,
        sample_rate: u32,
        channels: u32,
        bit_rate: u32,
        asc: *mut u8,
        asc_cap: usize,
        asc_len: *mut usize,
        priming_frames: *mut u32,
    ) -> bool,
>;
pub type NvEncoderEncode = Option<
    unsafe extern "C" fn(
        context: *mut c_void,
        recording: u64,
        track: u32,
        pcm: *const f32,
        frames: usize,
    ),
>;
pub type NvEncoderClose =
    Option<unsafe extern "C" fn(context: *mut c_void, recording: u64, track: u32, flush: bool)>;

/// An [`NvAudioEncoder`] with every function present, as the core's
/// [`AudioEncoder`].
struct FfiEncoder {
    context: Context,
    open: unsafe extern "C" fn(
        *mut c_void,
        u64,
        u32,
        u32,
        u32,
        u32,
        *mut u8,
        usize,
        *mut usize,
        *mut u32,
    ) -> bool,
    encode: unsafe extern "C" fn(*mut c_void, u64, u32, *const f32, usize),
    close: unsafe extern "C" fn(*mut c_void, u64, u32, bool),
}

impl AudioEncoder for FfiEncoder {
    fn open(
        &self,
        recording: u64,
        track: u32,
        sample_rate: u32,
        channels: u32,
        bit_rate: u32,
    ) -> Option<EncoderOpened> {
        let mut asc = [0u8; NV_ASC_MAX];
        let mut asc_len = 0usize;
        let mut priming = 0u32;
        // SAFETY: the app's function, with its own context, a buffer of
        // NV_ASC_MAX bytes, and two out pointers valid for the call, as
        // NvAudioEncoder documents.
        let ok = unsafe {
            (self.open)(
                self.context.0,
                recording,
                track,
                sample_rate,
                channels,
                bit_rate,
                asc.as_mut_ptr(),
                asc.len(),
                &mut asc_len,
                &mut priming,
            )
        };
        if !ok || asc_len == 0 || asc_len > asc.len() {
            return None;
        }
        Some(EncoderOpened {
            audio_specific_config: asc[..asc_len].to_vec(),
            priming_frames: priming,
        })
    }

    fn encode(&self, recording: u64, track: u32, pcm: &[f32]) {
        // SAFETY: the app's function with its own context; `pcm` is valid
        // for its length during the call.
        unsafe {
            (self.encode)(self.context.0, recording, track, pcm.as_ptr(), pcm.len());
        }
    }

    fn close(&self, recording: u64, track: u32, flush: bool) {
        // SAFETY: the app's function with its own context.
        unsafe { (self.close)(self.context.0, recording, track, flush) }
    }
}

impl From<CaptureError> for NvStatus {
    fn from(e: CaptureError) -> NvStatus {
        match e {
            CaptureError::InvalidArgument => NvStatus::NV_ERR_INVALID_ARGUMENT,
            CaptureError::UnknownId => NvStatus::NV_ERR_UNKNOWN_ID,
            CaptureError::Closed => NvStatus::NV_ERR_CLOSED,
            CaptureError::Limit(_) => NvStatus::NV_ERR_LIMIT,
            CaptureError::NotConfigured => NvStatus::NV_ERR_NOT_CONFIGURED,
            CaptureError::Internal => NvStatus::NV_ERR_INTERNAL,
        }
    }
}

/// Sets the capture settings (docs/FFI.md, "Capture"): {"live_dir":
/// "<absolute directory>","max_duration_s":3600,"max_bytes":4000000000,
/// "min_free_bytes":2000000000,"stop_free_bytes":1000000000,"max_live":4,
/// "max_calls":2,"quality":"main"|"sub","share_safe":false}; all but
/// live_dir optional. Applies to recordings started afterwards. The first
/// call also finalizes (event `recording` `recovered`) or deletes the
/// partial files a crash left, before it returns. Call it before
/// nv_record_start, which answers NV_ERR_NOT_CONFIGURED until then.
/// NV_ERR_INVALID_ARGUMENT for invalid JSON or values or a relative
/// live_dir. Any thread, never inside a callback. Added in minor 9, as
/// everything down to nv_record_status.
///
/// # Safety
/// `core` is a live handle; `json` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_capture_configure(core: *mut NvCore, json: *const c_char) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.capture_configure(arg(json)?)?)) }
}

/// Installs the app's AAC encoder (the struct is copied), or removes it
/// (NULL). Recordings already running keep the encoder they started with.
/// Without one, a live recording whose camera audio is not AAC is video only
/// (`audio` `unsupported`) and a call recording cannot start.
/// NV_ERR_INVALID_ARGUMENT when a function pointer is NULL. Any thread,
/// never inside a callback.
///
/// # Safety
/// `core` is a live handle; `encoder` is null or valid for reading one
/// NvAudioEncoder, whose functions and context stay valid until
/// nv_core_free returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_capture_set_encoder(
    core: *mut NvCore,
    encoder: *const NvAudioEncoder,
) -> NvStatus {
    // SAFETY: per the caller.
    let e = unsafe { encoder.as_ref() }.copied();
    // SAFETY: per the caller.
    unsafe {
        rec_call(core, |c| {
            let enc: Option<Arc<dyn AudioEncoder>> = match e {
                None => None,
                Some(NvAudioEncoder {
                    context,
                    open: Some(open),
                    encode: Some(encode),
                    close: Some(close),
                }) => Some(Arc::new(FfiEncoder {
                    context: Context(context),
                    open,
                    encode,
                    close,
                })),
                Some(_) => return Err(NvStatus::NV_ERR_INVALID_ARGUMENT),
            };
            Ok(c.capture_set_encoder(enc)?)
        })
    }
}

/// One encoded AAC access unit (raw, no ADTS header; 1 to 8192 bytes) for
/// `track` of `recording`, answering the encoder's `encode` (and the flush
/// of `close`) in order. Any thread, including inside the encoder's own
/// NV_ERR_UNKNOWN_ID for an unknown recording.
/// NV_ERR_UNKNOWN_ID for a recording that has ended and been forgotten.
///
/// # Safety
/// `core` is a live handle; `data` is valid for reading `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_record_audio_encoded(
    core: *mut NvCore,
    recording: u64,
    track: u32,
    data: *const u8,
    len: usize,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        if data.is_null() || len == 0 {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        // SAFETY: non-null and valid for `len` bytes per the caller.
        let unit = unsafe { std::slice::from_raw_parts(data, len) };
        match c.record_audio_encoded(recording, track, unit) {
            Ok(()) => NvStatus::NV_OK,
            Err(e) => e.into(),
        }
    })
}

/// Starts a live or call recording of a tile's camera:
/// {"kind":"live","tile":7,"quality":"sub","name":"Camera 1",
///  "share_safe":false,"utc_offset_s":0} or {"kind":"call","tile":7,
///  "file":"<absolute path>.mp4"}. Answers {"recording":12,"file":
/// "<absolute path>","state":"waiting_keyframe"}; `recording` events follow.
/// NV_ERR_LIMIT for a count limit, the main-stream limit, or low disk, with
/// {"limit":"live"|"calls"|"main_streams"|"disk_low"} as the answer
/// (written when it fits; *len set either way); NV_ERR_NOT_CONFIGURED
/// before nv_capture_configure, or for a call without an encoder;
/// NV_ERR_UNKNOWN_ID for an unknown tile; NV_ERR_INVALID_ARGUMENT. When the
/// answer does not fit, the recording is stopped again (its events, for an
/// ID never handed out, may follow) and NV_ERR_BUFFER_TOO_SMALL returned
/// with *len set: pass a buffer of a few KiB. Any thread, never inside a
/// callback.
///
/// # Safety
/// `core` is a live handle; `request` is a NUL-terminated string; `out` is
/// null or valid for writing `cap` bytes; `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_record_start(
    core: *mut NvCore,
    request: *const c_char,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let (Some(c), Some(request)) = (unsafe { core_ref(core) }, unsafe { string(request) })
        else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        if len.is_null() {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        }
        match c.record_start(request) {
            Ok(answer) => {
                // SAFETY: per the caller.
                let s = unsafe { copy_out(&answer, out, cap, len) };
                if s != NvStatus::NV_OK {
                    // The app cannot learn the ID: take the recording back.
                    if let Some(id) = serde_json::from_str::<serde_json::Value>(&answer)
                        .ok()
                        .and_then(|v| v["recording"].as_u64())
                    {
                        let _ = c.record_stop(id);
                    }
                }
                s
            }
            Err(e) => {
                if let Some(a) = e.limit_answer() {
                    // SAFETY: per the caller; the limit is reported whether
                    // or not its answer fits.
                    let _ = unsafe { copy_out(&a, out, cap, len) };
                } else {
                    // SAFETY: non-null and valid for one write per the caller.
                    unsafe { put(len, 0) };
                }
                e.into()
            }
        }
    })
}

/// Stops a recording and finalizes it; returns at once (events
/// `finalizing`, then `finished`). NV_ERR_UNKNOWN_ID for an unknown
/// recording (stopping one that is ending does nothing). Any thread, never
/// inside a callback.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_record_stop(core: *mut NvCore, recording: u64) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_call(core, |c| Ok(c.record_stop(recording)?)) }
}

/// {"recordings":[...]}: the latest `recording` event of each recording
/// that has not ended.
///
/// # Safety
/// `core` is a live handle; `out` is null or valid for writing `cap` bytes;
/// `len` is valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_record_status(
    core: *mut NvCore,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> NvStatus {
    // SAFETY: per the caller.
    unsafe { rec_answer(core, out, cap, len, |c| Ok(c.record_status())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_match_the_pipeline() {
        for k in 0..7u32 {
            assert_eq!(failure(k).unwrap() as u32, k);
        }
        assert!(failure(7).is_none());
        for (s, n) in [
            (TileStatus::Waiting, NvTileStatus::NV_TILE_WAITING),
            (TileStatus::Playing, NvTileStatus::NV_TILE_PLAYING),
            (TileStatus::Recovering, NvTileStatus::NV_TILE_RECOVERING),
            (TileStatus::Undecodable, NvTileStatus::NV_TILE_UNDECODABLE),
        ] {
            assert_eq!(s as i32, n as i32);
            assert_eq!(tile_status(s), n);
        }
        for t in [
            NvStreamTier::NV_TIER_MAIN,
            NvStreamTier::NV_TIER_SUB,
            NvStreamTier::NV_TIER_MOBILE,
            NvStreamTier::NV_TIER_EXTERN,
        ] {
            assert_eq!(
                StreamTier::from_number(t as u32).unwrap().number(),
                t as u32
            );
        }
        assert_eq!(std::mem::size_of::<NvStatus>(), 4);
        assert_eq!(
            std::mem::size_of::<NvBytes>(),
            2 * std::mem::size_of::<usize>()
        );
    }
}
