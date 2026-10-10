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
| tile | `nv_tile_open` | `nv_tile_close`, its device's removal, or `nv_core_free` | one channel; one tier at a time (`nv_tile_set_tier`), each tier a *generation* with its own pipeline and platform decoder |

IDs are nonzero `uint64_t`, never reused within a core. Two tiles may show the
same channel and tier; each has its own pipeline, and the device carries the
stream once (the session shares one preview between every tile showing it).

## Threads

- The core owns its threads. A slow `on_action` for one tile never delays
  other tiles or the session's socket: media reaches each tile through a
  bounded queue, and the pipeline's backpressure policy drops to the next
  keyframe when the app falls behind.
- `on_action` is serialized per tile; `on_event` is serialized across the
  core; `on_audio_tap` is serialized across the core on its own thread.
- From inside any callback an app may call only `nv_tile_decoded`,
  `nv_tile_decode_failed`, and `nv_tile_audio_level`. Everything else (`nv_tile_close`,
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

Exactly the pipeline's actions (RENDERING.md), in order, per tile and
generation (below):
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

## Switching a tile's stream tier

`nv_tile_set_tier(core, tile, tier)` asks a tile to show another tier (main
on focus or full screen, sub back in the grid) without recreating it and
without a gap:

- Every action carries `NvAction.generation`: 0 for the stream the tile was
  opened with, one more for each switch the core starts. Each generation is
  its own pipeline and needs its own platform decoder; the answering rules
  above hold per generation, and `nv_tile_decoded` and
  `nv_tile_decode_failed` take the generation of the action they answer.
  Answers for a generation that has ended are ignored.
- The core starts the new tier's stream first (shared with any tile already
  showing it) and runs the new generation beside the old one, whose actions
  keep flowing. When the new generation is ready (below), the old
  generation gets one final `NV_ACTION_RESET` (destroy that decoder;
  nothing of it follows), and its stream stops unless another tile shows
  it. That Reset, on the action path in order with the new generation's
  actions, is the moment to show the new generation's pictures; the
  `tile_tier` `active` event says the same on the event path. Until then
  the app keeps showing the old generation and does not present the new
  one's pictures (it decodes them all the same).
- **Ready, without stepping back in time.** Between main and sub, which
  run on one timestamp clock (docs/protocol.md, "Sub and main of one
  camera"), the new
  generation is ready at its first picture answered with `nv_tile_decoded`
  whose `pts_us` is at or after the `pts_us` of the old generation's
  picture answered last (the picture on screen, near enough). Its earlier
  pictures are decoded but never shown. If it has not caught up 300 ms after
  its first decoded picture, it is ready anyway (the old picture is held at
  most that long; no delay beyond it). Main usually arrives a little ahead
  of sub, so the switch is normally immediate. Switches involving the
  extern or mobile tier (whose clocks are not measured yet), switches whose
  two timestamps are more than 5 s apart (evidently not one clock), and
  switches before anything of the old generation was decoded are ready at
  the new generation's first decoded picture.
- Asking for the tier already requested changes nothing (with no switch in
  progress it reports `active` again). A newer request replaces one still
  starting. A generation that never shows (its stream refused or ended, it
  became undecodable, or nothing decoded within 15 s) ends with a final
  `NV_ACTION_RESET` of that generation and `tile_tier` `failed`; the old
  generation keeps playing.
- Audio follows the shown generation (it restarts with the new stream).

```json
{"type":"tile_tier","tile":7,"tier":"main","state":"starting","generation":1}
{"type":"tile_tier","tile":7,"tier":"main","state":"active","generation":1}
{"type":"tile_tier","tile":7,"tier":"sub","state":"failed","generation":2,"reason":"timeout"}
```

`reason` (with `failed` only): `superseded`, `timeout`, `start_rejected`,
`stream_ended`, `undecodable`, `session_closed`.

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
{"type":"channels","device":1,"channels":[{"channel":3,"connected":true,"tiers":["main","sub"],"detects":["people","vehicle","dog_cat"]}]}
{"type":"channels","device":1,"channels":[{"channel":4,"connected":true,"tiers":["main","sub"],"detects":["people","vehicle","dog_cat"],"lenses":2,"lens_detects":[["people","vehicle"],["dog_cat"]]}]}
{"type":"detection","device":1,"channel":3,"kind":"people","state":"start","source":"device","at_ns":123}
{"type":"detection","device":1,"channel":3,"lens":1,"kind":"people","state":"stop","source":"device","at_ns":456}
{"type":"motion","device":1,"channel":5,"state":"start","at_ns":789}
{"type":"alarm","device":1,"channel":2,"kind":"visitor","state":"start","at_ns":790}
{"type":"layouts_changed","revision":4}
{"type":"timing","what":"first_picture","tile":7,"ns":123456789}
{"type":"timing","what":"tier_switch","tile":7,"ns":2345678,"delta_ms":40,"held_ms":0,"capped":false}
{"type":"tile_stats","tile":7,"tier":"sub","generation":0,"received":120,"decoded":118,"gated":0,"dropped":2,"failures":0,"resets":0}
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
  diagnostics overlays; apps may ignore it. It describes the shown
  generation.
- `device_info` also carries the device's `key` (see "Layouts").
- **Detections.** Every device session subscribes to the device's
  detection pushes right after login (command 192; docs/protocol.md
  "Detection events") and the core turns each state snapshot into `start`
  and `stop` events per channel: `detection` for the device's AI types
  (`kind` in the device's names, lowercase: `people`, `vehicle`,
  `dog_cat`, `face`, `package`, `cry`, ...; unknown kinds are passed
  through), `motion` for motion, and `alarm` for other states the device
  reports (`kind` as the device names it, such as `visitor` for a doorbell
  press). `lens` is present (1) for the second lens of a multi-lens camera.
  `source` is `device`; on-device models will report through the same
  event with `source` `local`. `at_ns` is the core's clock
  (`nv_clock_now_ns`) at arrival: the pushes carry no usable time. When the
  session drops or the device is removed, everything active gets its
  `stop` first.
- `detects` in `channels` lists the detection kinds a channel's camera
  advertises (199 `aitype` bits), once known; `[]` for a camera without
  AI. For a multi-lens camera it is every kind of the camera (the
  channel's own `aitype` and all its lenses' together). It may arrive in a
  later `channels` event than the slot list.
- `lenses` (with `detects`) is the number of lenses of a multi-lens camera,
  present only when it has two or more: the 199 item of such a camera
  holds one sub-item per lens (newer NVR firmware; docs/protocol.md "Which
  channels detect what"). `lens_detects` then lists each lens's detection
  kinds, lens 0 first. The `tiers` a channel advertises do not tell a
  second lens apart (single-lens cameras can advertise `extern` too).
- `timing`: `first_picture` is the time from `nv_core_new` to the first
  `nv_tile_decoded` of any tile (sent once; the app adds its own
  launch-to-`nv_core_new` time); `tier_switch` is the time from
  `nv_tile_set_tier` to the switch, with `delta_ms` (the shown new
  picture's timestamp minus the old generation's latest decoded picture's,
  in ms: negative means the switch stepped back in time; absent when
  nothing of the old generation was decoded), `held_ms` (how long the
  switch waited for alignment after the new generation's first decoded
  picture) and `capped` (true when that wait hit the 300 ms cap). See
  "Switching a tile's stream tier".
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

## Audio level

`nv_tile_audio_level(core, tile, &peak, &rms)` reads the level of a
tile's decoded audio, before the tile's volume, for a meter at display
rate, from any thread (callbacks included), lock-free and allocation-free:
peak with a 0.3 s release, RMS over about 0.1 s, both 0..1. It reads the
same whether the tile is audible or not (a muted or silent tile still shows
that sound arrives):

- An audible tile whose audio is playing, while the app renders, reads what
  `nv_audio_render` measured on the samples it mixes (what is heard).
- Any other tile (silent, not yet playing, or the app has not rendered for
  0.5 s) reads a level measured as its records are decoded (every record is
  decoded, audible or not). Gaps of up to 0.2 s between records (arrival
  jitter) hold the level; a longer stall lets it decay, and it reads 0
  once no audio has arrived for 1 s, never a frozen value.
- A tile without audio, or an unknown tile, reads 0.

## Layouts

Layouts are split trees ([UX.md](UX.md)) the core stores, so every app on
the machine (and later the web viewer) shares them.

```json
{"id":"entrances","name":"Entrances","version":1,
 "root":{"split":"row","ratio":0.62,
   "a":{"camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3}},
   "b":{"split":"col","ratio":0.34,"a":{"camera":null},"b":{"camera":{"device":"d-3f9a0c1b7e2d4a65","channel":9}}}}}
```

- A node is a leaf `{"camera": null}` (an empty tile) or
  `{"camera": {"device": key, "channel": 0..255, "lens": 0..1}}` (`lens` is
  optional: 0, the default, is the camera's first lens, 1 the second lens of
  a multi-lens camera, shown through the `extern`/`mobile` tiers), or a split
  `{"split": "row"|"col", "ratio": 0.02..0.98, "a": node, "b": node}`
  (`row`: `a` left of `b`; `col`: `a` above `b`). At most 16 splits deep
  and 64 tiles; `id` (non-empty), `name` and device keys up to 256 bytes;
  a layout up to 64 KiB of JSON; up to 500 layouts. Members the core does
  not know (anywhere in the layout) are kept as given, so apps may store
  their own settings, such as a tile's tier or fill mode.
- `nv_layout_put` inserts or replaces by `id`; the core sets `version` (1,
  then one more on each replacement; a `version` the app passes is
  ignored). `nv_layout_delete` removes one. `nv_layouts_get` returns
  `{"revision": N, "layouts": [...]}` in the order layouts were first
  stored; `revision` grows with every change. Each change sends
  `{"type":"layouts_changed","revision":N}`, so several windows stay in
  step.
- **Device keys** name devices in layouts. The core assigns one per device
  address (host, case-insensitive, and port) the first time it is added,
  and keeps it across runs: `nv_device_key`, and `key` in `device_info`.
  The per-run device IDs are not stable and never go into a layout. A
  device whose address changes gets a new key (rekeying by device identity
  is future work).
- Storage: `layouts.json` and `devices.json` in `NvConfig.data_dir`, each
  rewritten whole and atomically (temporary file, flush, rename). An
  unreadable file is renamed to `*.unreadable` and the store starts empty.
  With no `data_dir` both live in memory for the run.
- Strings come back through `out`/`cap`/`len`: `*len` always receives the
  length without the NUL; when `out` is NULL or `cap` <= `*len` the call
  returns `NV_ERR_BUFFER_TOO_SMALL` and the app calls again with a buffer of
  `*len + 1` bytes.

## Logging

The core logs to rotated files in `log_dir` (capped by file count and total
size; defaults from `viewer-log`), rate-limited, with credentials and device
addresses redacted. Apps keep their own logs; nothing is printed to stdout.

## Versioning

`NV_ABI_VERSION` changes on any incompatible change to the header. ABI 2
added audio: `on_audio_tap` at the end of `NvCallbacks`, `NvAudioBlock`,
`nv_clock_now_ns`, `nv_audio_set_output`, `nv_audio_render`,
`nv_tile_audio_set`, and `nv_tile_audio_tap`. ABI 3 (M3, planned in
[FFI-ABI3.md](FFI-ABI3.md)) added tier switching (`nv_tile_set_tier`,
`NvAction.generation`, and a `generation` argument to `nv_tile_decoded` and
`nv_tile_decode_failed`), `nv_tile_audio_level`, the layouts store
(`nv_layouts_get`, `nv_layout_put`, `nv_layout_delete`, `nv_device_key`,
`NvConfig.data_dir`), `NV_ERR_BUFFER_TOO_SMALL`, and the detection,
`tile_tier`, `layouts_changed`, and `timing` events. Apps check
`nv_abi_version()` at startup and refuse to run on a mismatch.

## Builds

- macOS: a static library per architecture, packaged as an XCFramework with
  the header and a module map (`import NVRViewCore` in Swift).
- Windows: a DLL plus import library; C# bindings come later (M4).
- Linux: built and tested in CI so the core stays portable.
