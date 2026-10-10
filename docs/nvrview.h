/*
 * nvrview core C interface. Contract: docs/FFI.md.
 *
 * This is the reference for M2. cbindgen generates the real header,
 * crates/viewer-ffi/include/nvrview.h, from the Rust source; it must keep
 * these names, values, layouts, and the comments' meaning.
 */
#ifndef NVRVIEW_H
#define NVRVIEW_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped on any incompatible change; apps compare it with nv_abi_version(). */
#define NV_ABI_VERSION 2

/* Return codes. Every function that can fail returns one. */
typedef enum NvStatus {
    NV_OK = 0,
    NV_ERR_INVALID_ARGUMENT = -1, /* null pointer, bad UTF-8, out-of-range value */
    NV_ERR_UNKNOWN_ID = -2,       /* device or tile already removed, or never existed */
    NV_ERR_CLOSED = -3,           /* the core is shutting down */
    NV_ERR_LIMIT = -4,            /* too many devices or tiles */
    NV_ERR_INTERNAL = -5,
} NvStatus;

typedef enum NvCodec {
    NV_CODEC_H264 = 1,
    NV_CODEC_H265 = 2,
} NvCodec;

typedef enum NvStreamTier {
    NV_TIER_MAIN = 0,
    NV_TIER_SUB = 1,
    NV_TIER_MOBILE = 2,
    NV_TIER_EXTERN = 4,
} NvStreamTier;

typedef enum NvColorMatrix {
    NV_MATRIX_BT601 = 0,
    NV_MATRIX_BT709 = 1,
    NV_MATRIX_BT2020 = 2,
} NvColorMatrix;

typedef enum NvTileStatus {
    NV_TILE_WAITING = 0,
    NV_TILE_PLAYING = 1,
    NV_TILE_RECOVERING = 2,
    NV_TILE_UNDECODABLE = 3,
} NvTileStatus;

/* Why a platform decoder could not answer a Configure or a Decode. */
typedef enum NvDecodeFailure {
    NV_FAIL_INVALID_PARAMETER_SETS = 0, /* answers Configure */
    NV_FAIL_SESSION_CREATE_FAILED = 1,  /* answers Configure */
    NV_FAIL_NOT_CONFIGURED = 2,
    NV_FAIL_INVALID_ACCESS_UNIT = 3,
    NV_FAIL_SUBMIT_FAILED = 4,
    NV_FAIL_DECODE_ERROR = 5,
    NV_FAIL_NO_PICTURE = 6, /* the decoder dropped the picture; still an answer */
} NvDecodeFailure;

typedef enum NvActionKind {
    NV_ACTION_CONFIGURE = 0,
    NV_ACTION_DECODE = 1,
    NV_ACTION_FLUSH = 2,
    NV_ACTION_RESET = 3,
    NV_ACTION_CLEAR = 4,
    NV_ACTION_STATUS = 5,
} NvActionKind;

/* Borrowed bytes: valid only until the callback that received them returns. */
typedef struct NvBytes {
    const uint8_t *data;
    size_t len;
} NvBytes;

typedef struct NvSize {
    uint32_t width;
    uint32_t height;
} NvSize;

/* Pixels, origin at the top left. */
typedef struct NvRect {
    uint32_t x;
    uint32_t y;
    uint32_t width;
    uint32_t height;
} NvRect;

typedef struct NvColor {
    NvColorMatrix matrix;
    bool full_range; /* the decoder must output this range */
    /* Row-major 3x4: (R, G, B) = M * (Y, Cb, Cr, 1), samples as code/255. */
    float ycbcr_to_rgb[12];
} NvColor;

typedef struct NvConfigure {
    NvCodec codec;
    const NvBytes *parameter_sets; /* VPS, SPS, PPS order; NAL units without start codes */
    size_t parameter_set_count;
    NvSize coded_size;
    NvRect visible_rect; /* inside the coded picture */
    NvColor color;
} NvConfigure;

typedef struct NvDecode {
    NvBytes access_unit; /* one picture, Annex-B, parameter sets possibly in band */
    bool keyframe;
    int64_t pts_us;
} NvDecode;

/* One instruction for a tile's platform decoder or view. Read only the member
 * that matches `kind`; Flush, Reset, and Clear carry no data. */
typedef struct NvAction {
    NvActionKind kind;
    NvConfigure configure;
    NvDecode decode;
    NvTileStatus status;
} NvAction;

/* One block of a tile's tapped audio: 16 kHz mono, borrowed for the duration
 * of on_audio_tap. */
typedef struct NvAudioBlock {
    const float *samples; /* samples in -1..1 */
    size_t sample_count;
    uint32_t sample_rate; /* always 16000 */
    uint32_t epoch;       /* numbering and camera times restart with each epoch */
    uint64_t first_sample; /* number of the first sample in this epoch, at 16 kHz */
    int64_t camera_time_us; /* camera time (video timestamp units) of the first sample, if has_camera_time */
    int64_t wall_clock_us;  /* arrival of the record that completed the block
                               (microseconds since the Unix epoch), if has_wall_clock */
    bool has_camera_time;
    bool has_wall_clock;
    bool discontinuity; /* audio is missing or restarted before this block */
} NvAudioBlock;

/*
 * Callbacks. All arrive on threads the core owns and must return quickly.
 * on_action: serialized per tile, never concurrent for the same tile, never
 *   re-entrant. Pointers inside `action` are valid only during the call.
 * on_event: serialized across the whole core. `json` is UTF-8, not
 *   NUL-terminated, valid only during the call.
 * on_audio_tap: may be NULL (no taps). Serialized across the core; `block`
 *   and its samples are valid only during the call.
 * From inside a callback an app may call nv_tile_decoded and
 * nv_tile_decode_failed, and nothing else.
 * No event names a device before nv_device_add returns its ID, and no
 * on_action for a tile runs before nv_tile_open returns its ID.
 */
typedef void (*NvActionCallback)(void *context, uint64_t tile, const NvAction *action);
typedef void (*NvEventCallback)(void *context, const char *json, size_t len);
typedef void (*NvAudioTapCallback)(void *context, uint64_t tile, const NvAudioBlock *block);

typedef struct NvCallbacks {
    void *context;
    NvActionCallback on_action;
    NvEventCallback on_event;
    NvAudioTapCallback on_audio_tap;
} NvCallbacks;

typedef struct NvConfig {
    /* Directory for rotated log files (UTF-8, NUL-terminated); NULL = no file log. */
    const char *log_dir;
    uint32_t log_max_files;      /* 0 = default */
    uint64_t log_max_total_bytes; /* 0 = default */
} NvConfig;

typedef struct NvCore NvCore;

uint32_t nv_abi_version(void);

/* Starts the core. `out` receives the handle. `config` is read only during
 * the call. */
NvStatus nv_core_new(const NvConfig *config, NvCallbacks callbacks, NvCore **out);

/* Stops every preview, closes every session, joins every core thread. No
 * callback runs after it returns. Never call it from a callback. NULL is a
 * no-op. */
void nv_core_free(NvCore *core);

/* Adds a device; connecting starts in the background. The password is copied
 * and never logged. `out_device` receives a nonzero ID. */
NvStatus nv_device_add(NvCore *core, const char *host, uint16_t port, const char *user,
                       const char *password, uint64_t *out_device);

/* Closes the device's session (its tiles end first). */
NvStatus nv_device_remove(NvCore *core, uint64_t device);

/* Opens a tile: one channel and tier of a device. Actions for it start
 * arriving on on_action. `out_tile` receives a nonzero ID. */
NvStatus nv_tile_open(NvCore *core, uint64_t device, uint8_t channel, NvStreamTier tier,
                      uint64_t *out_tile);

/* Closes a tile. No on_action for it runs after this returns. Never call it
 * from a callback. */
NvStatus nv_tile_close(NvCore *core, uint64_t tile);

/* Decoder results. Every Decode not discarded by a later Flush, Reset, or
 * Configure gets exactly one of these; a failed Configure gets one
 * nv_tile_decode_failed. Callable from any thread, including inside
 * on_action; they never block. */
NvStatus nv_tile_decoded(NvCore *core, uint64_t tile, int64_t pts_us);
NvStatus nv_tile_decode_failed(NvCore *core, uint64_t tile, NvDecodeFailure kind);

/* Audio (docs/AUDIO.md). */

/* Now on the core's clock, nanoseconds: the platform host clock (macOS:
 * mach_absolute_time() in ns, which AudioTimeStamp.mHostTime counts;
 * Windows: QueryPerformanceCounter in ns; Linux: CLOCK_MONOTONIC).
 * nv_audio_render's host_time_ns is on this clock. */
uint64_t nv_clock_now_ns(void);

/* Sets the audio output format the app renders: sample_rate frames per
 * second (8000 to 384000) and 1 or 2 interleaved channels. Call again after
 * the output device or its rate changes; audible tiles move to the new
 * output and resync. */
NvStatus nv_audio_set_output(NvCore *core, uint32_t sample_rate, uint32_t channels);

/* Fills `out` (frames x channels floats, interleaved) with the mix of the
 * audible tiles. host_time_ns is when out's first frame will be heard, on
 * the core's clock (nv_clock_now_ns). Real-time safe: never blocks, locks,
 * allocates, or frees. Writes silence and returns NV_ERR_INVALID_ARGUMENT
 * when `channels` differs from nv_audio_set_output's (as during a format
 * change), and NV_ERR_CLOSED before an output is set or while another
 * thread renders. Render from one thread at a time, and stop rendering
 * before nv_core_free. */
NvStatus nv_audio_render(NvCore *core, float *out, uint32_t frames, uint32_t channels,
                         uint64_t host_time_ns);

/* Makes a tile audible (it claims one of eight playout slots) or silent (it
 * releases its slot), at `volume` from 0 (muted, keeping its slot and
 * timing) to 1 (more amplifies). Tiles start silent. */
NvStatus nv_tile_audio_set(NvCore *core, uint64_t tile, bool audible, float volume);

/* Starts or stops a tile's audio tap: on_audio_tap receives its decoded
 * audio as 16 kHz mono blocks (64 ms each), audible or not. A slow consumer
 * loses the oldest blocks (counted in audio_stats). No block for the tile
 * arrives after a call that stops it returns. NV_ERR_INVALID_ARGUMENT if
 * on_audio_tap was NULL. */
NvStatus nv_tile_audio_tap(NvCore *core, uint64_t tile, bool enabled);

/* Grid layout (pure; no core needed). Fills `out` (tile_count entries) with
 * the destination rectangle in the viewport and the source rectangle in the
 * visible picture. picture_sizes may contain zero sizes for tiles with no
 * picture yet. fill = false letterboxes, true crops. */
typedef struct NvTileLayout {
    NvRect cell;
    NvRect dest;
    NvRect source;
} NvTileLayout;

NvStatus nv_grid_layout(uint32_t tile_count, NvSize viewport, uint32_t gap,
                        const NvSize *picture_sizes, bool fill, NvTileLayout *out);

#ifdef __cplusplus
}
#endif

#endif /* NVRVIEW_H */
