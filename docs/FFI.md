# The C interface

The boundary between the shared Rust core and each platform app. The reference header
is [`nvrview.h`](nvrview.h) (cbindgen will generate the real one as
`crates/viewer-ffi/include/nvrview.h`, which must match it); this document is the part of the contract a header cannot say. It refines
"The C interface" in [ARCHITECTURE.md](ARCHITECTURE.md) and carries out
[RENDERING.md](RENDERING.md): the core runs each tile's pipeline and sends the
app **actions**, not raw frames.

## Objects

| Object | Created by | Ends with | Notes |
| --- | --- | --- | --- |
| core | `nv_core_new` | `nv_core_free` | owns the async runtime, every session, every tile, the log |
| device | `nv_device_add` | `nv_device_remove`, or `nv_core_free` | one session (one login) per device, however many tiles |
| tile | `nv_tile_open` | `nv_tile_close`, its device's removal, or `nv_core_free` | one channel and tier, one pipeline, one platform decoder |

IDs are nonzero `uint64_t`, never reused within a core. Two tiles may show the
same channel and tier; each has its own pipeline, and the device carries the
stream once per tile (sharing one stream is a later optimization).

## Threads

- The core owns its threads. A slow `on_action` for one tile never delays
  other tiles or the session's socket: media reaches each tile through a
  bounded queue, and the pipeline's backpressure policy drops to the next
  keyframe when the app falls behind.
- `on_action` is serialized per tile; `on_event` is serialized across the
  core; `on_audio_tap` is serialized across the core on its own thread.
- From inside any callback an app may call only `nv_tile_decoded` and
  `nv_tile_decode_failed`. Everything else (`nv_tile_close`,
  `nv_device_remove`, `nv_core_free`, ...) must be called from outside, or it
  could wait on the thread it runs on.
- `nv_tile_decoded` and `nv_tile_decode_failed` may be called from any thread
  (platform decoders answer on their own threads) and never block.
- **Nothing about an object before its ID exists.** No event names a device
  until `nv_device_add` has returned its ID, and no `on_action` for a tile
  runs until `nv_tile_open` has returned its ID. (The core holds them back.)
- After `nv_tile_close` returns, no `on_action` or `on_audio_tap` for that
  tile is running or will run; the same holds for `on_audio_tap` after
  `nv_tile_audio_tap(..., false)` returns. After `nv_core_free` returns, no
  callback at all.
- `nv_audio_render` is for the app's real-time audio thread: callable from
  any one thread at a time (a second concurrent caller gets silence), it
  never blocks, locks, allocates, or frees (tested with a counting
  allocator through this interface). Stop rendering (on macOS, stop the
  audio engine) before `nv_core_free`.

## Memory

- `NvConfig` and every pointer inside it are read only during
  `nv_core_new`.

- Everything the core passes (action pointers, access units, parameter sets,
  JSON) is borrowed for the duration of the callback. Apps copy what they
  keep; a platform decoder copies the access unit into its own buffer anyway.
- The core never copies video between the session and the callback beyond
  what demuxing needs.
- Strings the app passes are UTF-8, NUL-terminated, and copied before the
  call returns. Passwords are kept in memory only and never logged.

## Actions

Exactly the pipeline's actions (RENDERING.md), in order, per tile:
`CONFIGURE`, `DECODE`, `FLUSH`, `RESET`, `CLEAR`, and `STATUS`. Answering
rules, also from RENDERING.md: every `DECODE` not discarded by a later
`FLUSH`, `RESET`, or `CONFIGURE` gets exactly one `nv_tile_decoded` or
`nv_tile_decode_failed`, dropped pictures included (`NV_FAIL_NO_PICTURE`), and
a failed `CONFIGURE` gets one `nv_tile_decode_failed` with a Configure kind.

**Values an app does not recognize** (a newer core): an unknown action kind
is ignored and never needs an answer (the core adds answerable kinds only
with an ABI change). A `CONFIGURE` with an unknown codec or color matrix is
answered with `NV_FAIL_INVALID_PARAMETER_SETS`; a `DECODE` the app cannot
submit is answered with `NV_FAIL_INVALID_ACCESS_UNIT`.

**Picture size.** `source` rectangles from `nv_grid_layout` are in
visible-picture pixels. VideoToolbox outputs pictures at the visible
(cropped) size, measured on a stream with a conformance window; a platform
decoder that outputs the coded size must add `visible_rect`'s origin.

The core also drives each pipeline's clock (`tick`) and tells it about
reconnects (`discontinuity`); the app does not.

## Events

`on_event` delivers small JSON objects, one per call. Unknown `type` values
and unknown fields must be ignored, so the core can add them without an ABI
change.

```json
{"type":"device_state","device":1,"state":"connecting"}
{"type":"device_state","device":1,"state":"connected"}
{"type":"device_state","device":1,"state":"backoff","attempt":3,"retry_ms":8000}
{"type":"device_state","device":1,"state":"login_failed","status":401,"remain_times":2}
{"type":"device_state","device":1,"state":"closed"}
{"type":"device_info","device":1,"name":"NVR","model":"EXAMPLE-MODEL"}
{"type":"channels","device":1,"channels":[{"channel":0,"connected":true,"tiers":["main","sub"],"name":"Channel 1","model":"EXAMPLE-CAM"}]}
{"type":"tile_stats","tile":7,"received":120,"decoded":118,"gated":0,"dropped":2,"failures":0,"resets":0}
{"type":"tile_closed","tile":7,"reason":"device_removed"}
{"type":"audio_stats","tile":7,"records":386,"bytes":26722,"decoded":386,"concealed":0,"rejected":0,"talkback_ignored":0,"epoch":0,"audible":true,"volume":1,"tapped":true,"no_free_slot":false,"playout_delay_ms":223.76,"outliers":0,"timestamp_resets":0,"codec":"aac","sample_rate":16000,"channels":1,"rate_estimate":15999.93,"playout_delay_target_ms":100,"av_offset_ms":0.092,"buffered_ms":199.104,"correction_ppm":1.819,"resyncs":0,"starts":1,"underruns":0,"underrun_frames":0,"overflow_frames":0,"blocks_not_played":0,"tap_blocks":386,"tap_dropped":0}
```

- `login_failed` is terminal for that device: the core does not retry (devices
  lock accounts out). The app asks for new credentials and adds the device
  again.
- `channels` arrives after each login and whenever the device pushes a new
  list; names come from per-channel queries and may arrive in a later
  `channels` event than the slot list.
- `tile_closed` is sent once for every tile that ends, whatever ended it,
  after that tile's last `on_action` has returned; no action for it follows.
  For `nv_tile_close` it may arrive after the call returns (`reason`:
  `closed`, `device_removed`, `login_failed`, `core_shutdown`).
- `tile_stats` arrives about once a second per tile while it is open, for
  diagnostics overlays; apps may ignore it.
- `audio_stats` arrives about once a second for each tile whose stream has
  carried audio. Fields that are not known yet are left out: `codec`,
  `sample_rate`, `channels` and `rate_estimate` until a record decodes,
  `playout_delay_target_ms` until about 10 s of audio has been seen, the
  playout fields (`av_offset_ms` through `blocks_not_played`) while the
  tile is not audible, and `tap_*` while it has no tap. `av_offset_ms` is
  the filtered error against the target (audio heard at its picture's time
  plus `playout_delay_ms`), not the delay itself.

## Audio

Listening, as designed in [AUDIO.md](AUDIO.md). The core decodes,
timestamps, and drift-corrects every tile's audio; the app only plays what
`nv_audio_render` gives it.

1. At startup and after every output device or sample-rate change, call
   `nv_audio_set_output(core, rate, channels)` with the format the app's
   real-time callback renders (1 or 2 interleaved channels; on macOS the
   app renders interleaved stereo at the output device's rate).
2. In the real-time callback, call `nv_audio_render(core, out, frames,
   channels, host_time_ns)`, where `host_time_ns` is when `out[0]` will be
   heard on the core's clock. During a format change the callback may
   briefly render with the old channel count: it then gets silence and
   `NV_ERR_INVALID_ARGUMENT`, never an overrun.
3. `nv_tile_audio_set(core, tile, audible, volume)`: tiles start silent.
   Audible tiles claim one of eight playout slots; silent tiles release
   theirs; volume 0 mutes while keeping the slot and its timing. Policy
   (such as only the focused tile audible) is the app's.
4. `nv_tile_audio_tap(core, tile, enabled)` sends a tile's decoded audio,
   16 kHz mono in 64 ms blocks with camera time, arrival wall-clock time,
   epoch, and a discontinuity flag, to `on_audio_tap` (which must not be
   NULL to enable a tap). Each tile's tap queue holds 64 blocks (about 4 s)
   and drops the oldest for a slow consumer, counted as `tap_dropped`.

**The clock.** The core's clock *is* the platform host clock in
nanoseconds, so the app passes the time its audio API reports without any
offset estimate: on macOS `AudioTimeStamp.mHostTime` (mach ticks) converted
to nanoseconds (`AudioConvertHostTimeToNanos`, or the mach timebase), plus
the output's presentation latency; on Windows WASAPI's QPC position (in
100 ns units) times 100; on Linux the monotonic clock. `nv_clock_now_ns()`
returns it. A private clock with an offset the app estimates was the
alternative; two reads cannot be paired exactly, and the offset would break
if the two clocks counted sleep differently.

**Talk-back records** (stream index 1, `01wb`, the form the official client
sends its own talk-back in) are never played: they are counted as
`talkback_ignored` in `audio_stats`.

## Logging

The core logs to rotated files in `log_dir` (capped by file count and total
size; defaults from `viewer-log`), rate-limited, with credentials and device
addresses redacted. Apps keep their own logs; nothing is printed to stdout.

## Versioning

`NV_ABI_VERSION` changes on any incompatible change to the header. ABI 2
added audio: `on_audio_tap` at the end of `NvCallbacks`, `NvAudioBlock`,
`nv_clock_now_ns`, `nv_audio_set_output`, `nv_audio_render`,
`nv_tile_audio_set`, and `nv_tile_audio_tap`. Apps check
`nv_abi_version()` at startup and refuse to run on a mismatch.

## Builds

- macOS: a static library per architecture, packaged as an XCFramework with
  the header and a module map (`import NVRViewCore` in Swift).
- Windows: a DLL plus import library; C# bindings come later (M4).
- Linux: built and tested in CI so the core stays portable.
