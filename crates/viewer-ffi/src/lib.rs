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
use viewer_core::{
    Action, Callbacks, Codec, ColorMatrix, Core, CoreConfig, CoreError, DecodeFailureKind,
    LogSettings, StreamTier, TileStatus,
};

/// Bumped on any incompatible change; apps compare it with nv_abi_version().
pub const NV_ABI_VERSION: u32 = 1;

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
    /// too many devices or tiles
    NV_ERR_LIMIT = -4,
    NV_ERR_INTERNAL = -5,
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
}

/// Callbacks. Both arrive on threads the core owns and must return quickly.
/// on_action: serialized per tile, never concurrent for the same tile, never
///   re-entrant. Pointers inside `action` are valid only during the call.
/// on_event: serialized across the whole core. `json` is UTF-8, not
///   NUL-terminated, valid only during the call.
/// From inside a callback an app may call nv_tile_decoded and
/// nv_tile_decode_failed, and nothing else.
/// No event names a device before nv_device_add returns its ID, and no
/// on_action for a tile runs before nv_tile_open returns its ID.
pub type NvActionCallback =
    Option<unsafe extern "C" fn(context: *mut c_void, tile: u64, action: *const NvAction)>;
pub type NvEventCallback =
    Option<unsafe extern "C" fn(context: *mut c_void, json: *const c_char, len: usize)>;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NvCallbacks {
    pub context: *mut c_void,
    pub on_action: NvActionCallback,
    pub on_event: NvEventCallback,
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
}

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
    fn on_action(&self, tile: u64, action: &Action<'_>) {
        let mut a = NvAction {
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
        let cb = Arc::new(FfiCallbacks {
            context: Context(callbacks.context),
            on_action,
            on_event,
        });
        match Core::new(cfg, cb) {
            Ok(core) => {
                let handle = Box::into_raw(Box::new(NvCore { core }));
                // SAFETY: `out` is non-null and valid for one write per the
                // caller.
                unsafe { put(out, handle) };
                NvStatus::NV_OK
            }
            Err(e) => error_status(e),
        }
    })
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

/// Decoder results. Every Decode not discarded by a later Flush, Reset, or
/// Configure gets exactly one of these; a failed Configure gets one
/// nv_tile_decode_failed. Callable from any thread, including inside
/// on_action; they never block.
///
/// # Safety
/// `core` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nv_tile_decoded(core: *mut NvCore, tile: u64, pts_us: i64) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        match unsafe { core_ref(core) } {
            // The inverse of the cast in on_action.
            Some(c) => status(c.decoded(tile, pts_us as u64)),
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
    kind: NvDecodeFailureValue,
) -> NvStatus {
    guarded(|| {
        // SAFETY: per the caller.
        let Some(c) = (unsafe { core_ref(core) }) else {
            return NvStatus::NV_ERR_INVALID_ARGUMENT;
        };
        match failure(kind) {
            Some(k) => status(c.decode_failed(tile, k)),
            None => NvStatus::NV_ERR_INVALID_ARGUMENT,
        }
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
