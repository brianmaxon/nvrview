/*
 * nvrview core C interface. Contract: docs/FFI.md.
 *
 * This is the reference for ABI 3 (M3). cbindgen generates the real header,
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
#define NV_ABI_VERSION 3

/* Bumped for each addition that leaves every existing declaration unchanged
 * (new functions, events, status codes); resets to 0 with a new
 * NV_ABI_VERSION. Minor 1: device discovery. Minor 2: updates. Minor 3:
 * nv_tile_retry. Minor 4: nv_talk_set_conversation. Minor 5: recordings,
 * part 1. Minor 6: recorded playback. Minor 7: "months" in
 * nv_recordings_want. Minor 8: edit lists and the rest of sound. Minor 9:
 * capture (live and call recordings). Minor 10: export (nv_download_range,
 * nv_still_plan, the trim kind). Minor 11: camera motion (PTZ). Minor 12:
 * smart alerts. */
#define NV_ABI_MINOR 12

/* Return codes. Every function that can fail returns one. */
typedef enum NvStatus {
    NV_OK = 0,
    NV_ERR_INVALID_ARGUMENT = -1, /* null pointer, bad UTF-8, out-of-range value */
    NV_ERR_UNKNOWN_ID = -2,       /* device or tile already removed, or never existed */
    NV_ERR_CLOSED = -3,           /* the core is shutting down */
    NV_ERR_LIMIT = -4,            /* too many devices, tiles, layouts, queued requests, or
                                     recordings, or too little disk space for a recording */
    NV_ERR_INTERNAL = -5,
    NV_ERR_BUFFER_TOO_SMALL = -6, /* the output buffer is too small; *len holds the size needed */
    NV_ERR_NOT_PERMITTED = -7,    /* talk-back (camera motion control) is not enabled for that camera */
    NV_ERR_UNSUPPORTED = -8,      /* the device's backend cannot do this (minor 5) */
    NV_ERR_NOT_CONFIGURED = -9,   /* nv_recordings_configure not called, or no downloads_dir (minor 5);
                                     nv_capture_configure not called, or a call recording
                                     without an encoder (minor 9) */
    NV_PENDING = 1,               /* accepted; the answer arrives as an event (nv_thumbnail_get; minor 5) */
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
    /* The tile's stream this action belongs to: 0 when opened, one more for
     * each tier switch (nv_tile_set_tier). Each generation has its own
     * platform decoder; answer with the action's generation. */
    uint32_t generation;
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
    /* The app's data directory (UTF-8, NUL-terminated), where the core keeps
     * layouts and device keys; NULL = in memory only. */
    const char *data_dir;
} NvConfig;

typedef struct NvCore NvCore;

uint32_t nv_abi_version(void);

/* The core's minor version: additions to this ABI version. An app checks
 * nv_abi_version() for equality and nv_abi_minor() >= the minor that
 * introduced a function before calling it. */
uint32_t nv_abi_minor(void);

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

/* Asks the tile to show `tier` (for example main on focus, sub back in the
 * grid). The tile keeps presenting its current stream until the new one
 * has decoded a picture no older than the one shown (between main and sub;
 * at most 300 ms after its first decoded picture) or its first
 * picture (otherwise), then switches in one step: no gap, no black frame,
 * no step back in time. The new stream's actions carry the next generation; the
 * switch is the old generation's final NV_ACTION_RESET (and a tile_tier
 * event). Returns at once; progress arrives as events. Asking for the tier
 * already requested changes nothing. */
NvStatus nv_tile_set_tier(NvCore *core, uint64_t tile, NvStreamTier tier);

/* Retries a tile that has no picture (the app's Retry button): its stream
 * is stopped and started again now, on the same login. When the device is
 * waiting before a reconnect, the next connection attempt is made now. A
 * tile asked again within 2 s does nothing (NV_OK), and so does one whose
 * device refused the login (that state stays terminal; apps do not offer
 * the button for it). Returns at once; progress arrives as the usual
 * events. NV_ERR_UNKNOWN_ID for an unknown tile. Added in minor 3. */
NvStatus nv_tile_retry(NvCore *core, uint64_t tile);

/* Decoder results, for the generation of the action they answer. Every
 * Decode not discarded by a later Flush, Reset, or Configure gets exactly
 * one of these; a failed Configure gets one nv_tile_decode_failed.
 * Results for a generation that has ended are ignored. Callable from any
 * thread, including inside on_action; they never block. */
NvStatus nv_tile_decoded(NvCore *core, uint64_t tile, uint32_t generation, int64_t pts_us);
NvStatus nv_tile_decode_failed(NvCore *core, uint64_t tile, uint32_t generation,
                               NvDecodeFailure kind);

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

/* Peak and RMS level of a tile's decoded audio, 0..1, before the tile's
 * volume (peak with a 0.3 s release, RMS over about 0.1 s), audible or not:
 * as nv_audio_render measured it for a tile that plays while the app
 * renders, otherwise as its audio was decoded, falling to 0 within a
 * second when no audio arrives. Both read 0 for a tile without audio or
 * not known. Lock-free and allocation-free; callable from any thread,
 * including callbacks, at display rate. */
NvStatus nv_tile_audio_level(NvCore *core, uint64_t tile, float *peak, float *rms);

/* Talk-back (docs/FFI.md, "Talk-back"): sound through a camera's or
 * doorbell's speaker. Camera motion control: off for every camera until
 * nv_talk_set_permission enables it, rate-limited, logged, and closed on
 * every exit path. Progress arrives as `talk` events. */

/* Enables or disables talk-back on one channel of a device, for this run.
 * Disabling stops a running talk on it. */
NvStatus nv_talk_set_permission(NvCore *core, uint64_t device, uint8_t channel, bool allowed);

/* Starts talk-back to a tile's camera; returns at once. NV_ERR_NOT_PERMITTED
 * (and a `talk` `refused` event) unless the camera was enabled. */
NvStatus nv_talk_start(NvCore *core, uint64_t tile);

/* Queues `count` mono float samples (-1..1) at sample_rate Hz (8000 to
 * 192000) for a tile's talk; resampled to 16 kHz, encoded, and sent in real
 * time. Ignored for a tile without a talk. Not from a real-time thread. */
NvStatus nv_talk_push(NvCore *core, uint64_t tile, const float *samples, size_t count,
                      uint32_t sample_rate);

/* Ends a tile's talk at once, or (drain) after the audio already pushed and
 * 1.5 s of silence (the device drops what it has not played at the close);
 * audio pushed before the close continues the same talk. */
NvStatus nv_talk_stop(NvCore *core, uint64_t tile, bool drain);

/* Marks a tile's camera as in a conversation the user has open in the app
 * (a doorbell call), or no longer. While marked, talk-back starts on it are
 * not limited per minute (otherwise 20 per camera per minute); they still
 * need the permission and are logged. The mark also ends when the tile
 * closes, the permission is withdrawn, or the core is freed. Added in
 * minor 4. */
NvStatus nv_talk_set_conversation(NvCore *core, uint64_t tile, bool open);

/* Camera motion (minor 11; docs/FFI.md, "Camera motion (PTZ)"; event ptz).
 * The app's calls are the user's own manual control and need no enable. */

/* What a tile's camera can do and where its lens is, as JSON: request
 * {"tile":N}; answer {"pan_tilt","zoom":{"min","max","cur"}|null,"focus":...,
 * "presets":[{"id","name"}],"speeds":[1,64]}. Blocks for at most 20 s. */
NvStatus nv_ptz_info(NvCore *core, const char *request, char *out, size_t cap, size_t *len);

/* Starts or renews a move ("left", "right", "up", "down"; speed 1 to 64).
 * Call again at least every 500 ms while the control is held: the camera is
 * stopped 1 s after the last call, after 30 s, and on every exit path. */
NvStatus nv_ptz_move(NvCore *core, uint64_t tile, const char *direction, uint32_t speed);

/* Stops the tile's camera at once; never limited. */
NvStatus nv_ptz_stop(NvCore *core, uint64_t tile);

/* Moves the lens "zoom" or "focus" to an absolute position, clamped to the
 * range nv_ptz_info reported. */
NvStatus nv_ptz_lens(NvCore *core, uint64_t tile, const char *what, int32_t position);

/* Recalls a preset from nv_ptz_info's list (recall only). */
NvStatus nv_ptz_preset(NvCore *core, uint64_t tile, uint32_t id);

/* Grants or withdraws motion control on one channel for an automation (a
 * local API client by name, or "auto_correction") for this run of the core;
 * the app passes the user's grants after each nv_device_add. */
NvStatus nv_motion_grant(NvCore *core, uint64_t device, uint8_t channel, const char *client,
                         bool granted);

/* Layouts and device keys (docs/FFI.md, "Layouts"). Strings come back
 * UTF-8 and NUL-terminated in `out`; *len receives the length without the
 * NUL. When `cap` <= *len (or out is NULL) the call returns
 * NV_ERR_BUFFER_TOO_SMALL with *len set: call again with a larger buffer. */

/* The device's stable key: the same address (host and port) gets the same
 * key in every run while the data directory is kept. */
NvStatus nv_device_key(NvCore *core, uint64_t device, char *out, size_t cap, size_t *len);

/* Every stored layout: {"revision":N,"layouts":[...]}. */
NvStatus nv_layouts_get(NvCore *core, char *out, size_t cap, size_t *len);

/* Inserts or replaces (by "id") one layout given as JSON. The core
 * validates it (NV_ERR_INVALID_ARGUMENT if not a valid split tree), sets
 * its "version", stores it atomically, and sends layouts_changed.
 * NV_ERR_LIMIT when the store is full, NV_ERR_INTERNAL when it cannot be
 * written (nothing changes). */
NvStatus nv_layout_put(NvCore *core, const char *json);

/* Deletes a layout by id and sends layouts_changed. NV_ERR_UNKNOWN_ID if
 * there is none. */
NvStatus nv_layout_delete(NvCore *core, const char *id);

/* Device discovery (minor 1; docs/FFI.md, "Device discovery").
 *
 * Starts a search for devices on the local network: a few rounds of UDP
 * broadcasts, and the WS-Discovery multicast, on each IPv4 interface that is
 * up (or on the interfaces named in json), then listening until the time is
 * up. Returns at once. Answers arrive as "discovered" events and the end as
 * one "discovery_done" event. json may be NULL or empty for the defaults, or:
 * {"seconds":3,"interfaces":["192.0.2.5"],"onvif":true}
 * (seconds 0.5 to 30; interfaces are IPv4 addresses of this computer).
 * NV_ERR_INVALID_ARGUMENT for invalid JSON or values. One search per core:
 * a start while one runs stops it (its discovery_done has "stopped":true)
 * and starts the new one. The core never searches on its own. */
NvStatus nv_discover_start(NvCore *core, const char *json);

/* Stops the running search, if any, and closes its sockets; its
 * discovery_done event ("stopped":true) is queued before this returns.
 * NV_OK when nothing is running. */
NvStatus nv_discover_stop(NvCore *core);

/* Updates (minor 2; docs/FFI.md, "Updates"; docs/UPDATES.md).
 *
 * Starts an update check: fetch the source's manifest and signature, verify
 * it with the trusted keys, and compare versions. Returns at once; the
 * outcome arrives as "update" events ("started", then one of "up_to_date",
 * "available", "failed", "canceled"). json:
 * {"source":"https://... | /folder","keys":["nvrview-ed25519-pub ..."],
 *  "current":"0.3.0","platform":"macos-arm64"}
 * (platform optional: the core's own by default). NV_ERR_INVALID_ARGUMENT
 * for invalid JSON, a key that is not an update public key, or a version
 * that is not semantic; a bad source is reported by the event.
 * NV_ERR_LIMIT while another update operation runs (one at a time). The
 * core never checks on its own. */
NvStatus nv_update_check(NvCore *core, const char *json);

/* Downloads, verifies, and unpacks the version the user accepted: the
 * manifest is fetched and verified again and must still offer it; the
 * package's size and SHA-256 must match before anything is unpacked into
 * staging_dir (which must not exist; it is created). Events: "started",
 * "downloading" (received, total), then one of "ready" (path of the
 * unpacked package), "failed", "canceled". json: the check's members plus
 * "version" (the accepted version) and "staging_dir" (an absolute path on
 * the same volume as the installed app). On failure the core removes
 * everything it created. Errors as nv_update_check. */
NvStatus nv_update_download(NvCore *core, const char *json);

/* Cancels the running update check or download, if any. Its final event
 * ("canceled", or the outcome it reached first) is queued before this
 * returns, within about half a second; nothing more of that operation
 * follows. NV_OK when nothing runs. */
NvStatus nv_update_cancel(NvCore *core);

/* Parses an update public key as a user pastes it or as a key file holds it
 * (pure; no core needed). On success `out` receives
 * {"key":"nvrview-ed25519-pub <hex>","id":"<16 hex digits>"}: the canonical
 * text to store and the ID to show. NV_ERR_INVALID_ARGUMENT when the text is
 * not one valid key. Out-buffer rules as nv_layouts_get. */
NvStatus nv_update_parse_key(const char *text, char *out, size_t cap, size_t *len);

/* Checks an update source as the user typed it (pure; no core needed).
 * `out` receives {"ok":true,"kind":"folder|https|http","display":"..."} or
 * {"ok":false,"reason":"source_invalid|insecure_source","message":"..."}
 * (NV_OK either way). Out-buffer rules as nv_layouts_get. */
NvStatus nv_update_parse_source(const char *text, char *out, size_t cap, size_t *len);

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

/* Recordings, part 1 (minor 5; docs/FFI-RECORDINGS.md, sections 1 to 5;
 * events in its section 8). JSON in is UTF-8, NUL-terminated, and copied
 * before the call returns; JSON out follows the out-buffer rules of
 * nv_layouts_get. None of these waits on the network: device work runs on
 * core threads and reports by event. Any thread, never inside a callback.
 * Cameras are {"device":"<device key>","channel":3,"lens":0} (lens
 * optional); times are device-local seconds. */

/* Sets the recordings configuration (JSON, every member optional, unknown
 * members ignored): downloads_dir, index, grouping, thumbnails, downloads,
 * playback, naming. Call once after nv_core_new and again to change it.
 * Starts resuming unfinished downloads of an earlier run. */
NvStatus nv_recordings_configure(NvCore *core, const char *json);

/* Allows (json non-NULL) or stops (json NULL) indexing one device by its
 * stable key: {"channels":[...]|"all","utc_offset_s":0,"background":{...}}.
 * Nothing is searched until a view asks, unless "background" is set.
 * NV_ERR_NOT_CONFIGURED before nv_recordings_configure; NV_ERR_UNSUPPORTED
 * if the device's backend has no recordings. */
NvStatus nv_recordings_enable(NvCore *core, const char *device_key, const char *json);

/* Queries ({"start","end","cameras","kinds","limit"}), answered from
 * memory; they never cause a search. The index may change between two
 * calls, so loop until the answer fits. */
NvStatus nv_recordings_calendar(NvCore *core, const char *query, char *out, size_t cap,
                                size_t *len);
NvStatus nv_recordings_spans(NvCore *core, const char *query, char *out, size_t cap,
                             size_t *len);
NvStatus nv_recordings_events(NvCore *core, const char *query, char *out, size_t cap,
                              size_t *len);

/* Asks for cameras' days to be in the index ({"cameras","days","months",
 * "force"}; "months", minor 7: only those months' calendars); returns at
 * once, recordings_changed follows. NV_ERR_LIMIT beyond 31 days, 12 months,
 * or a full queue (nothing queued); NV_ERR_INVALID_ARGUMENT for a camera
 * nv_recordings_enable does not allow. */
NvStatus nv_recordings_want(NvCore *core, const char *json);

/* Keeps today fresh for these cameras while hold (any nonzero number the
 * app chooses per window) lasts: json {"cameras":[...]} sets or replaces
 * its cameras, NULL releases it. */
NvStatus nv_recordings_hold_today(NvCore *core, uint64_t hold, const char *json);

/* {"revision":N,"devices":[{"device","state","pending_days","revision"}]}. */
NvStatus nv_recordings_status(NvCore *core, char *out, size_t cap, size_t *len);

/* Incidents overlapping the query's range, newest first; one by key
 * (NV_ERR_UNKNOWN_ID if none has it now). */
NvStatus nv_incidents_get(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
NvStatus nv_incident_get(NvCore *core, const char *key, char *out, size_t cap, size_t *len);

/* Applies, stores, and answers one edit ({"revision":N,"keys":[...]});
 * locks or unlocks an incident (locking queues its download). These, and
 * nv_download_incident and nv_download_trash, make their change even when
 * the answer does not fit: NV_ERR_BUFFER_TOO_SMALL then only says the
 * answer was not written, so do not repeat the call to read it. */
NvStatus nv_incident_edit(NvCore *core, const char *edit, char *out, size_t cap, size_t *len);
NvStatus nv_incident_lock(NvCore *core, const char *key, bool locked, char *out, size_t cap,
                          size_t *len);

/* Camera groups and name overrides; put replaces the whole list
 * (NV_ERR_LIMIT beyond 64 groups of 64 cameras, 4096 names, 256-byte
 * names). */
NvStatus nv_camera_groups_get(NvCore *core, char *out, size_t cap, size_t *len);
NvStatus nv_camera_groups_put(NvCore *core, const char *json);

typedef enum NvThumbKind {
    NV_THUMB_H264_KEYFRAME = 1, /* Annex-B, parameter sets included */
    NV_THUMB_H265_KEYFRAME = 2, /* Annex-B, parameter sets included */
    NV_THUMB_IMAGE = 3,         /* the app's own small image (for example JPEG) */
} NvThumbKind;

typedef struct NvThumbnail {
    NvThumbKind kind;
    uint32_t width; /* of the picture the bytes decode to (0 when unknown) */
    uint32_t height;
    int64_t pts_us; /* the keyframe's stream timestamp */
} NvThumbnail;

/* Thumbnail keys are {"camera":{...},"tier":"sub","start":S,"end":E}.
 * get: NV_OK fills *info, out, and *len; NV_PENDING (a fetch is queued, a
 * thumbnail event follows; *len is 0); NV_ERR_BUFFER_TOO_SMALL fills *info
 * and *len (out NULL or cap < *len; bytes have no terminator). put copies
 * the bytes (NV_ERR_LIMIT beyond the cache's byte limit). cancel drops
 * queued fetches of one key, or all when key is NULL. */
NvStatus nv_thumbnail_get(NvCore *core, const char *key, NvThumbnail *info, uint8_t *out,
                          size_t cap, size_t *len);
NvStatus nv_thumbnail_put(NvCore *core, const char *key, const NvThumbnail *info,
                          const uint8_t *data, size_t len);
NvStatus nv_thumbnail_cancel(NvCore *core, const char *key);

/* Downloads: queue an incident's ({"incident","cameras"} ->
 * {"download","folder","existing"}; NV_ERR_NOT_CONFIGURED without
 * downloads_dir), pause or resume, retry failed cameras (camera NULL: all,
 * else a JSON camera object), forget one with no job running, hand one to
 * the Trash ({"folder"}; the app moves it after download_removed), list
 * every known one ({"downloads":[...]}). */
NvStatus nv_download_incident(NvCore *core, const char *request, char *out, size_t cap,
                              size_t *len);
NvStatus nv_download_pause(NvCore *core, const char *download, const char *camera, bool paused);
NvStatus nv_download_retry(NvCore *core, const char *download, const char *camera);
NvStatus nv_download_forget(NvCore *core, const char *download, bool delete_partial);
NvStatus nv_download_trash(NvCore *core, const char *download, char *out, size_t cap,
                           size_t *len);
NvStatus nv_downloads_get(NvCore *core, char *out, size_t cap, size_t *len);

/* Live sound settings: {"alarms":true,"loudness_history_s":600} (members
 * left out keep their values); applies to tiles opened afterwards. */
NvStatus nv_sound_configure(NvCore *core, const char *json);

/* Edit lists and the rest of sound (minor 8; docs/FFI-RECORDINGS.md,
 * sections 7 and 2a; events sound_watch, sound_analyze). */

/* An edit list for a finished download, as JSON: {"kind":"follow" | "grid",
 * "download":"<id>", ...}. Pure computation; no device traffic.
 * Minor 10 added the "trim" kind, in_ms and out_ms, dest, and share_safe. */
NvStatus nv_edit_compute(NvCore *core, const char *request, char *out, size_t cap, size_t *len);

/* Export (minor 10; docs/FFI-RECORDINGS.md, section 7a). Queues the download
 * of a time range of chosen cameras ({"start","end","cameras":[{"camera",
 * "tier"}],"name","dest","share_safe"} -> {"download","folder","existing"};
 * made even when the answer does not fit, as nv_download_incident). */
NvStatus nv_download_range(NvCore *core, const char *request, char *out, size_t cap,
                           size_t *len);

/* Plans still images from a playback's tiles ({"playback","tiles","format",
 * "dest","share_safe"}) or from a position in a finished file
 * ({"download","camera","at_ms","exact","format"}). */
NvStatus nv_still_plan(NvCore *core, const char *request, char *out, size_t cap, size_t *len);

/* Keeps a camera's sub-stream open for sound only while enabled. */
NvStatus nv_sound_watch(NvCore *core, const char *camera, bool enabled);

/* Loudness lanes: {"start","end","bucket_s","cameras"} answers {"revision",
 * "lanes":[{"camera","bucket_s","start","peak_db","rms_db"}]}. */
NvStatus nv_recordings_loudness(NvCore *core, const char *query, char *out, size_t cap,
                                size_t *len);

/* Labels from the app's classifier, merged into the index as events. */
NvStatus nv_sound_labels_put(NvCore *core, const char *json);

/* Analyze one camera's recorded day: {"camera","day"} answers {"request":N,
 * "state":"queued"}; cancel it by request. */
NvStatus nv_sound_analyze_day(NvCore *core, const char *json, char *out, size_t cap, size_t *len);
NvStatus nv_sound_analyze_cancel(NvCore *core, uint64_t request);

/* A live tile's recent loudness, oldest first, one byte per second and value
 * (0: no audio; 1..128: dBFS = byte - 128); the newest `cap` when more are
 * kept; *start_s is the Unix second of the first value. */
NvStatus nv_tile_loudness(NvCore *core, uint64_t tile, uint8_t *peak, uint8_t *rms, size_t cap,
                          int64_t *start_s, size_t *len);

/* Recorded playback (minor 6; docs/FFI-RECORDINGS.md, section 6; events
 * playback_state, playback_camera, playback_seek, playback_closed). A
 * playback is up to 4 cameras on one shared clock, each shown by a playback
 * tile: a tile like a live one (on_action, nv_tile_decoded, generations,
 * nv_tile_set_tier, audio, tile_stats, tile_closed) that nv_tile_close
 * refuses. Times are device-local milliseconds. Any thread, never inside a
 * callback, except nv_playback_position. */

typedef enum NvSeekMode {
    NV_SEEK_KEYFRAME = 0, /* show the keyframe at or before the position first (fast; scrubbing) */
    NV_SEEK_EXACT = 1,    /* decode from that keyframe and show the picture at the position */
} NvSeekMode;

typedef enum NvPlaybackState {
    NV_PLAYBACK_BUFFERING = 0, /* playing, but the clock waits for a camera */
    NV_PLAYBACK_PLAYING = 1,
    NV_PLAYBACK_PAUSED = 2,
    NV_PLAYBACK_ENDED = 3,     /* at the end of the range or of the recordings */
    NV_PLAYBACK_FAILED = 4,    /* every camera failed */
} NvPlaybackState;

/* Opens a playback, paused at "position_ms" with an exact seek:
 * {"start_ms","end_ms","position_ms","rate","sync":"wait"|"free",
 *  "cameras":[{"camera":{...},"tier":"sub"|"main","offset_ms":0}]} or
 * {"incident":"<key>","tier":"sub"}. Writes one tile ID per camera, in
 * request order, to out_tiles (tile_cap entries; *tile_count receives the
 * number). A tile_cap too small opens nothing: NV_ERR_BUFFER_TOO_SMALL with
 * *tile_count set. NV_ERR_LIMIT beyond 8 playbacks, 4 cameras, 24 hours, or
 * the tile limit; NV_ERR_INVALID_ARGUMENT for a rate not listed below, a
 * camera twice, or a device not enabled for recordings. */
NvStatus nv_playback_open(NvCore *core, const char *json, uint64_t *out_playback,
                          uint64_t *out_tiles, size_t tile_cap, size_t *tile_count);
/* Adds a camera ({"camera":{...},"tier":"sub","offset_ms":0}) at the
 * current position; removes one (tile_closed "closed" follows). */
NvStatus nv_playback_add_camera(NvCore *core, uint64_t playback, const char *json,
                                uint64_t *out_tile);
NvStatus nv_playback_remove_camera(NvCore *core, uint64_t playback, uint64_t tile);
NvStatus nv_playback_play(NvCore *core, uint64_t playback);
NvStatus nv_playback_pause(NvCore *core, uint64_t playback);
/* Moves the shared clock (clamped to the range); returns at once, and a
 * newer seek replaces one still starting. */
NvStatus nv_playback_seek(NvCore *core, uint64_t playback, int64_t position_ms, NvSeekMode mode);
/* 0.25, 0.5, 1, 2, 4, 8, or 16. Audio plays at 1 only. */
NvStatus nv_playback_set_rate(NvCore *core, uint64_t playback, double rate);
/* This camera shows position + offset_ms. */
NvStatus nv_playback_set_offset(NvCore *core, uint64_t playback, uint64_t tile,
                                int64_t offset_ms);
/* Position and state for the scrubber, as of the engine's last 10 ms step.
 * Callable from any thread, including callbacks. */
NvStatus nv_playback_position(NvCore *core, uint64_t playback, int64_t *position_ms,
                              NvPlaybackState *state);
/* {"playback","state","position_ms","rate","sync","start_ms","end_ms",
 *  "cameras":[{"tile","camera","tier","offset_ms","state","reason"?,
 *  "generation"}]}, out-buffer rules as nv_layouts_get. */
NvStatus nv_playback_status(NvCore *core, uint64_t playback, char *out, size_t cap,
                            size_t *len);
/* Stops every play, closes every tile (tile_closed each), then
 * playback_closed. */
NvStatus nv_playback_close(NvCore *core, uint64_t playback);

/* Capture (minor 9; docs/FFI.md, "Capture"; event recording): live
 * recordings of a tile's camera and call recordings, written by the core.
 * The app supplies the AAC encoder. */

/* Most bytes of an AudioSpecificConfig an encoder's open may write. */
#define NV_ASC_MAX 64

/* The app's AAC encoder, copied by nv_capture_set_encoder. track is the audio
 * track's index in the file (live: 0; call: 0 Conversation, 1 Visitor,
 * 2 You). Called on the recorders' own threads (never at once for one
 * recording); return quickly. Answer each encoded frame with
 * nv_record_audio_encoded, from inside encode or close or any thread, until
 * close returns. The functions and context stay in use until nv_core_free
 * returns. */
typedef bool (*NvEncoderOpen)(void *context, uint64_t recording, uint32_t track,
                              uint32_t sample_rate, uint32_t channels, uint32_t bit_rate,
                              uint8_t *asc, size_t asc_cap, size_t *asc_len,
                              uint32_t *priming_frames);
typedef void (*NvEncoderEncode)(void *context, uint64_t recording, uint32_t track,
                                const float *pcm, size_t frames);
typedef void (*NvEncoderClose)(void *context, uint64_t recording, uint32_t track, bool flush);
typedef struct NvAudioEncoder {
    void *context;
    /* Opens AAC-LC for one track: writes the AudioSpecificConfig (1 to
     * asc_cap bytes) and the priming in frames; false if it cannot. */
    NvEncoderOpen open;
    /* 1024 mono float samples, valid during the call. */
    NvEncoderEncode encode;
    /* Ends the track; with flush, the remaining output first. */
    NvEncoderClose close;
} NvAudioEncoder;

/* {"live_dir":"<absolute directory>","max_duration_s","max_bytes",
 *  "min_free_bytes","stop_free_bytes","max_live","max_calls","quality",
 *  "share_safe"}; all but live_dir optional. Call before nv_record_start.
 * The first call recovers partial files a crash left (recovered events). */
NvStatus nv_capture_configure(NvCore *core, const char *json);
/* Installs (struct copied) or removes (NULL) the encoder; running
 * recordings keep theirs. NV_ERR_INVALID_ARGUMENT for a NULL function. */
NvStatus nv_capture_set_encoder(NvCore *core, const NvAudioEncoder *encoder);
/* One raw AAC access unit (1 to 8192 bytes), in order, from any thread,
 * including inside the encoder's callbacks. */
NvStatus nv_record_audio_encoded(NvCore *core, uint64_t recording, uint32_t track,
                                 const uint8_t *data, size_t len);
/* {"kind":"live","tile","quality"?,"name"?,"share_safe"?,"utc_offset_s"?} or
 * {"kind":"call","tile","file"} answers {"recording","file","state"}.
 * NV_ERR_LIMIT answers {"limit":"live"|"calls"|"main_streams"|"disk_low"}
 * (written when it fits; *len set either way). When the answer does not
 * fit, the recording is stopped again: pass a buffer of a few KiB. */
NvStatus nv_record_start(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
/* Stops and finalizes; returns at once (finalizing, then finished). */
NvStatus nv_record_stop(NvCore *core, uint64_t recording);
/* {"recordings":[...]}: the latest recording event of each running one. */
NvStatus nv_record_status(NvCore *core, char *out, size_t cap, size_t *len);

/* Smart alerts (minor 12; docs/FFI.md, "Smart alerts"; events
 * verify_request, verify_request_done, verify_step, alert,
 * alerts_configured, trigger_rejected). The core wakes cameras and holds
 * pictures; the app decodes them, runs its detector, and answers with
 * boxes. */

/* The settings document (docs/NOTIFICATIONS.md "Settings", plus "enabled",
 * "utc_offset_min", "acquisition", "inputs"). Cameras by this run's device
 * IDs. Issues arrive in the alerts_configured event. */
NvStatus nv_alerts_configure(NvCore *core, const char *json);
/* The event log, newest first: {"entries":[...],"more":bool,"status":{...}}.
 * query: filters as JSON, or NULL. */
NvStatus nv_alerts_get(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
/* A trigger from the app; writes its ID (trigger_id may be NULL).
 * NV_ERR_NOT_CONFIGURED while smart alerts are off. */
NvStatus nv_alerts_trigger_put(NvCore *core, const char *json, uint64_t *trigger_id);
/* A verify_request's frames: Annex-B access units from a keyframe
 * (parameter sets included) to the picture; info gets the codec as an
 * NvThumbKind keyframe kind, the size, and the picture's timestamp.
 * NV_ERR_UNKNOWN_ID once answered or expired. */
NvStatus nv_alert_frames_get(NvCore *core, const char *request, NvThumbnail *info,
                             uint8_t *out, size_t cap, size_t *len);
/* {"detections":[{"class","score","box":{"x","y","w","h"}}]}, normalized to
 * the whole picture. */
NvStatus nv_alert_detections_put(NvCore *core, const char *request, const char *json);
/* The app's picture of an alert (at most 1 MiB; copied). */
NvStatus nv_alert_snapshot_put(NvCore *core, uint64_t alert_id, const uint8_t *data, size_t len);
NvStatus nv_alert_snapshot_get(NvCore *core, uint64_t alert_id, uint8_t *out, size_t cap,
                               size_t *len);
/* {"alert_id","vote":"up"|"down","reason"?}. */
NvStatus nv_alert_feedback(NvCore *core, const char *json);
/* {"device","channel","lens"} muted for seconds (at most 24 h; 0 unmutes). */
NvStatus nv_alert_mute(NvCore *core, const char *camera, uint32_t seconds);

#ifdef __cplusplus
}
#endif

#endif /* NVRVIEW_H */
