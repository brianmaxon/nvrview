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
    Action, Callbacks, Codec, ColorMatrix, Core, CoreConfig, CoreError, DecodeFailureKind,
    LogSettings, StreamTier, TileStatus, clock,
};

/// Bumped on any incompatible change; apps compare it with nv_abi_version().
pub const NV_ABI_VERSION: u32 = 3;

/// Bumped for each addition that leaves every existing declaration
/// unchanged (new functions, events, status codes); resets to 0 with a new
/// NV_ABI_VERSION. Minor 1: device discovery. Minor 2: updates.
pub const NV_ABI_MINOR: u32 = 2;

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
    /// too many devices, tiles, layouts, or queued requests
    NV_ERR_LIMIT = -4,
    NV_ERR_INTERNAL = -5,
    /// the output buffer is too small; *len holds the size needed
    NV_ERR_BUFFER_TOO_SMALL = -6,
    /// talk-back (camera motion control) is not enabled for that camera
    NV_ERR_NOT_PERMITTED = -7,
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

/// An `NvStreamTier` value as received from C (validated before use).
pub type NvStreamTierValue = u32;
/// An `NvDecodeFailure` value as received from C (validated before use).
pub type NvDecodeFailureValue = u32;

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
/// nv_tile_decode_failed, and nothing else.
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
        let (Some(on_action), Some(on_event)) = (callbacks.on_action, callbacks.on_event) else {
            return Err(NvStatus::NV_ERR_INVALID_ARGUMENT);
        };
        let cfg = viewer_core::CoreConfig {
            test_runtime: Some(runtime),
            ..viewer_core::CoreConfig::default()
        };
        new_core(cfg, callbacks, on_action, on_event)
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
/// pushed has been sent. The device's close is sent either way.
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
