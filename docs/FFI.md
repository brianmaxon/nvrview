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

## Retrying a tile

`nv_tile_retry(core, tile)` (minor 3) is the Retry button of a tile that
has shown no picture for a while. The core restarts that tile's stream now
(stop, then a new start with a new message ID, on the same login), and when
its device is waiting before a reconnect it makes the next connection
attempt now instead of waiting out the backoff. It goes through the same
supervisor as every other connection attempt, so the device's login limits
hold: a tile asked again within 2 s does nothing (`NV_OK`), connection
attempts made early are at least 2 s apart, and a device whose login was
refused (`tile_closed` with `login_failed`, state `LoginFailed`) is never
retried: the session is over, so the call does nothing and apps do not show
the button there. Progress arrives as the usual events (`tile_waiting`,
`tile_error`, then pictures).

The core also recovers on its own, without the button: a started preview
that delivers no media is restarted with growing waits (15 s, doubling to
60 s), a start refused with 400 is tried again every 60 s after the first
minute of waiting, and when every preview stays silent the connection is
reestablished once (docs/ARCHITECTURE.md, "Previews that deliver nothing").
`tile_error` `start_rejected` can therefore be followed by a picture; apps
clear their message at the first decoded picture.

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
{"type":"channels","device":1,"channels":[{"channel":5,"connected":true,"tiers":["main","sub"],"detects":["people"],"fisheye":true}]}
{"type":"channels","device":1,"channels":[{"channel":6,"connected":true,"tiers":["main","sub"],"detects":[],"talk":true,"doorbell":true,"ptz":{"pan_tilt":false,"presets":false,"zoom_focus":false}}]}
{"type":"channels","device":1,"channels":[{"channel":7,"connected":true,"tiers":["main","sub"],"detects":[],"talk":false,"doorbell":false,"ptz":{"pan_tilt":true,"presets":true,"zoom_focus":true}}]}
{"type":"detection","device":1,"channel":3,"kind":"people","state":"start","source":"device","at_ns":123}
{"type":"detection","device":1,"channel":3,"lens":1,"kind":"people","state":"stop","source":"device","at_ns":456}
{"type":"detection","device":1,"channel":2,"kind":"smoke_alarm","state":"start","source":"local_audio","at_ns":457}
{"type":"motion","device":1,"channel":5,"state":"start","at_ns":789}
{"type":"alarm","device":1,"channel":2,"kind":"visitor","state":"start","at_ns":790}
{"type":"layouts_changed","revision":4}
{"type":"timing","what":"first_picture","tile":7,"ns":123456789}
{"type":"timing","what":"tier_switch","tile":7,"ns":2345678,"delta_ms":40,"held_ms":0,"capped":false}
{"type":"tile_stats","tile":7,"tier":"sub","generation":0,"received":120,"decoded":118,"gated":0,"dropped":2,"failures":0,"resets":0}
{"type":"tile_closed","tile":7,"reason":"device_removed"}
{"type":"tile_waiting","tile":7,"tier":"sub","generation":0,"state":"waiting","status":400}
{"type":"tile_waiting","tile":7,"tier":"sub","generation":0,"state":"streaming"}
{"type":"tile_error","tile":7,"reason":"start_rejected","status":400}
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
- `tile_waiting` reports a stream the device has not started yet. A
  third-party (ONVIF) camera behind an NVR answers a preview with status 400
  and may send its stream afterwards (docs/protocol.md, "Records as the
  official client reads them"), so the core keeps that preview open and
  restarts it about every 6 s. `waiting` (with the reply `status`) is sent
  once when the wait begins; `streaming` follows when media arrives, and
  pictures follow as usual. If none arrives within 60 s, `tile_error`
  (`reason` `start_rejected`, `status` 400) ends the wait instead. An app
  can show "waiting for the camera" on the tile meanwhile. During a tier
  switch it is reported with the new generation; the switch's own 15 s
  limit still applies (`tile_tier` `failed`, `timeout`).
- `tile_error` with `reason` `start_rejected` means the device refused the
  tile's stream (`status`: the reply status); the tile stays open and shows
  nothing until the device session reconnects or the app reopens the tile
  (once no other tile shows that stream).
- `tile_error` `start_rejected` with `status` 0 means the device accepted the
  start but sent no video: a camera without a second lens answers a lens
  tier (`extern` or `mobile`) with one stream-info
  record of size 0x0 and nothing more (docs/protocol.md, "Second lenses",
  observed). A 0x0 record alone is normal (a real second lens sends one
  too), so the rule is: a 0x0 record and no video within the core's
  info-only limit (`CoreConfig.info_only_limit`, 15 s) fails the start. It is
  sent once per tile, the tile stays open, and a later picture clears it
  like any other `start_rejected`. Apps treat it as a refused start, which
  is what moves a second lens from `mobile` to `extern`.
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
  event with `source` `local_video` (`local_audio` for sound). `at_ns` is the core's clock
  (`nv_clock_now_ns`) at arrival: the pushes carry no usable time. When the
  session drops or the device is removed, everything active gets its
  `stop` first.
- **Alarms heard in a tile's audio.** The core listens to every live tile
  with audio, audible or not, muted or not, for the standard smoke-alarm
  (temporal-three) and CO-alarm (temporal-four) patterns
  (`crates/sound-events`; docs/PLAYBACK.md, "Sound") and reports them as
  `detection` events with `source` `local_audio` (the source of all sound
  labels, docs/FFI-RECORDINGS.md section 2a) and `kind` `smoke_alarm` or
  `co_alarm`; no `lens` (a camera's microphone belongs to no one lens).
  `start` comes once two alarm cycles have matched, about 7.5 s after a
  smoke alarm starts and 10 s after a CO alarm; its `at_ns` is when the
  first matched beep arrived, so it lies in the past. `stop` comes when the
  pattern ends (about 5.5 s after the last smoke beep, 11 s after the last
  CO group), when the tile's audio stops arriving for 3 s, or when the
  last tile showing that channel closes (before its `tile_closed`),
  whichever is first; its `at_ns` is when the last beep arrived, or the
  core's clock for a stall or close. Several tiles of one channel report
  one episode once. Badge it like any detection, but never show the
  absence of this event as "no alarm": a camera microphone is not an alarm
  sensor. On by default; Rust callers turn it off with
  `CoreConfig::sound.alarms`. The C interface has no switch in ABI 3 (it
  always uses the defaults); the recordings minor (ABI 3, minor 5)
  proposes one (docs/FFI-RECORDINGS.md, section 13).
- `detects` in `channels` lists the detection kinds a channel's camera
  advertises (199 `aitype` bits), once known; `[]` for a camera without
  AI. For a multi-lens camera it is every kind of the camera (the
  channel's own `aitype` and all its lenses' together). It may arrive in a
  later `channels` event than the slot list.
- `lenses` is the number of lenses of a multi-lens camera, present only
  when it has two or more. The core knows it in one of two ways:
  - the 199 item of such a camera holds one sub-item per lens (newer NVR
    firmware; docs/protocol.md "Which channels detect what"). `lenses`
    then comes with `detects`, and `lens_detects` lists each lens's
    detection kinds, lens 0 first;
  - otherwise, for a connected channel whose `tiers` do not include
    `extern`, the core runs a one-time probe: a short `extern` preview on
    the device's session (no extra login), stopped again after its first
    picture or about 3 s (`CoreConfig::lens_probe_window`). A picture
    means a second lens. Such a `lenses` has **no `lens_detects`** (what
    each lens detects is unknown), and may come without `detects` (when
    199 could not be read). Probes run one at a time per device, after the
    channel list, names, and capabilities are in, and wait (up to 15 s)
    while a tile is starting on the channel, so on a device's first
    connection `lenses` can appear in a `channels` event seconds after the
    others. Each answer, lens or not, is kept per
    device key and channel in `lenses.json` in the data directory and
    reused on later connections and runs, until the channel's camera
    changes (its `uid` from the channel list, or else its name and model);
    a cached lens is reported once the names are in.

  The `tiers` a channel advertises do not tell a second lens apart
  (single-lens cameras can advertise `extern` too; channels that do are
  never probed).
- `fisheye` is `true` for a fisheye (360°) camera, which sends its raw
  circular image for the app to dewarp (199 `fishEye` 1 or `channelType`
  `fisheye_ipc`; docs/protocol.md "Fisheye cameras"), and absent for every
  other channel. Like `detects` it arrives once the capabilities are read.
  It is an added event field: no header change and no ABI number (apps
  ignore fields they do not know). The core does nothing else for such a
  camera; dewarping is drawing (docs/RENDERING.md, "Fisheye dewarping").
- `talk`, `doorbell` and `ptz` say what a channel's camera can do, read
  from the same capabilities reply (199) and present, like `detects`, once
  it is known (a later `channels` event than the slot list; all three
  together, `false` values included). They are added event fields: no
  header change and no ABI number. They describe the device, not what the
  user allowed: nothing here enables talk-back or motion.
  - `talk` is `true` when the channel's item has `ipcAudioTalk` 1
    (observed: the official client asked for the talk ability of exactly
    those channels). The same test gates `nv_talk_start` (see "Talk-back").
  - `doorbell` is `true` when the item's `channelType` is `Doorbell`
    (observed; compared without regard to case).
  - `ptz` is `{"pan_tilt","presets","zoom_focus"}`, all booleans.
    `pan_tilt` is `ptzType` 3 and `zoom_focus` is `ptzType` 1 or 3 (the
    meanings of the values are inferred from which controls the official
    client offered, not documented by the vendor); `presets` is `ptzPreset`
    bit 0 (reported by the official client). Missing fields read as
    `false`. These only say what the camera has; nvrview sends no motion
    commands for them yet.
  - Only the item's own fields count for the channel (the same rule as
    talk-back). For a multi-lens camera (`lenses`) the capabilities of a
    lens sit in its `subItem`, so `lens_talk` (booleans) and `lens_ptz`
    (objects as `ptz`) list them, lens 0 first, parallel to
    `lens_detects`; a lens that moves while the channel's item says
    nothing is normal (observed: a pan-tilt-zoom lens as the second lens of
    a two-lens camera). `lens_talk` and `lens_ptz` are present only with
    `lenses` from the capabilities, never for a probed second lens.
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

## Talk-back

Sound from the app (the computer's microphone, or system text-to-speech
for a typed reply) through a camera's or doorbell's speaker, for the
doorbell call in [UX.md](UX.md) ("Doorbell calls"). Talk-back is **camera
motion control** (docs/ARCHITECTURE.md, "Device commands"): off for every
camera until the app enables it, sent only when the app asks, rate-limited,
and logged. The protocol is in docs/protocol.md, "Talk-back (motion)".

**Additive to ABI 3.** ABI 3 is not released yet, so these functions,
`NV_ERR_NOT_PERMITTED`, and the `talk` event join it without a version
change; an app built against an earlier ABI 3 header keeps working
(`NV_ABI_VERSION` stays 3).

```c
NvStatus nv_talk_set_permission(NvCore *core, uint64_t device, uint8_t channel, bool allowed);
NvStatus nv_talk_start(NvCore *core, uint64_t tile);
NvStatus nv_talk_push(NvCore *core, uint64_t tile, const float *samples, size_t count,
                      uint32_t sample_rate);
NvStatus nv_talk_stop(NvCore *core, uint64_t tile, bool drain);
NvStatus nv_talk_set_conversation(NvCore *core, uint64_t tile, bool open); /* minor 4 */
```

1. **Permission.** `nv_talk_set_permission(core, device, channel, true)`
   enables talk-back on one channel of a device, for this run of the core.
   The app keeps the user's per-camera setting and passes it after each
   `nv_device_add`; the core stores nothing. Withdrawing it (`false`) stops
   a running talk at once (`stopped`, reason `permission_revoked`).
2. **Start.** `nv_talk_start(core, tile)` asks to talk to the tile's camera
   and returns at once. Without the permission it returns
   `NV_ERR_NOT_PERMITTED` and sends `talk` `refused` (`not_permitted`);
   nothing goes to the device. Otherwise the device session (no extra
   login) checks that the channel supports talk-back (its 199 item has
   `ipcAudioTalk` 1, and its ability offers the observed configuration),
   applies the rate limit (at most 20 starts per channel per minute,
   none during a conversation, item 5), and opens the talk. Progress arrives as `talk` events. Starting a tile whose
   talk is starting or active changes nothing.
3. **Audio.** `nv_talk_push(core, tile, samples, count, sample_rate)`
   queues mono float samples (-1..1) at the app's rate (8 to 192 kHz;
   typically 16 kHz from a speech pipeline or 48 kHz from the microphone).
   The core resamples them to the device's 16 kHz, encodes them as IMA ADPCM,
   and sends one 64 ms record every 64 ms of its own clock (real time, as
   the official client). Audio pushed while the talk is starting is held
   (up to about 1 s) and sent once it is active. Audio pushed faster than
   real time is buffered up to 10 s; beyond that it is dropped (counted as
   `dropped_ms`), never queued without bound. Returns `NV_ERR_UNKNOWN_ID`
   for an unknown tile; samples for a tile without a talk are ignored
   (`NV_OK`). Callable from any app thread; it takes a short lock and may
   allocate, so not from the real-time audio callback.
4. **Stop.** `nv_talk_stop(core, tile, false)` stops at once;
   `nv_talk_stop(core, tile, true)` stops after the audio already pushed has
   been sent and then 1.5 s of silence (the tail: the device plays about a
   second behind and drops what it has not played at the close; AUDIO.md,
   "Talk-back (sending)"). Use it for a text-to-speech reply pushed in one
   go and when the user lets go of Talk. Pushing again before the close
   continues the same talk (no new start, no `talk` event); stop it again
   the same way. Closing the
   tile, removing its device, freeing the core, the session dropping, a
   device error, 10 s without pushed audio, and 5 minutes of talk also
   stop it. Every stop sends the device's close (command 11) unless the
   connection is already gone.
5. **Conversation** (minor 4). `nv_talk_set_conversation(core, tile, true)`
   tells the core the user has a call open on the tile's camera (a doorbell
   press or Start Conversation in the app); `false` when the call ends.
   While a camera has a tile marked, its talk starts are not limited per
   minute and do not count against the limit later; they still need the
   permission, and every start is still logged (with `conversation=true`).
   The mark also ends when the tile closes, when the camera's permission is
   withdrawn (allowing it again does not restore it: the app marks the call
   again before its next start), and when the core is freed.
   `NV_ERR_UNKNOWN_ID` for an unknown tile. Calls exist only in the apps,
   which is why the app says when one is open.

```json
{"type":"talk","tile":7,"device":1,"channel":3,"state":"starting"}
{"type":"talk","tile":7,"device":1,"channel":3,"state":"active"}
{"type":"talk","tile":7,"device":1,"channel":3,"state":"stopped","reason":"finished","sent_ms":4096,"tail_ms":1536,"dropped_ms":0}
{"type":"talk","tile":7,"device":1,"channel":3,"state":"refused","reason":"not_permitted"}
{"type":"talk","tile":7,"device":1,"channel":3,"state":"refused","reason":"rate_limited","retry_ms":41000}
{"type":"talk","tile":7,"device":1,"channel":3,"state":"refused","reason":"device_refused","status":400}
```

- Each start ends with exactly one `refused` or `stopped`.
- `refused` reasons: `not_permitted`, `rate_limited` (with `retry_ms`),
  `not_supported` (no `ipcAudioTalk`), `unsupported_ability`, `busy`
  (another talk on that channel), `device_refused` (with the reply
  `status`), `session_closed`, `connection_lost`, `timed_out`,
  `query_failed`.
- `stopped` reasons: `stopped` (`nv_talk_stop`), `finished` (a draining
  stop), `tile_closed`, `permission_revoked`, `idle`, `max_duration`,
  `device_error` (with `status`), `session_closed`, `connection_lost`,
  `dropped`. `sent_ms` is the audio sent, `tail_ms` the silent tail sent
  after it (0 unless the talk drained), `dropped_ms` the audio dropped.
- The core logs every start, stop and refusal with its reason (refusals
  at most one line every few seconds).

## Capture

> **Minor 9.** The contract for [CAPTURE.md](CAPTURE.md) (live recordings
> and call recordings; pictures are rendered and written by the apps and
> need nothing here). The core side is `viewer_core::capture_owner` (K4);
> the C functions wrap it one to one (F5). "Clarifications from the
> implementation (F5)", after the events, lists what wrapping it settled.
> The apps' AAC encoders and their use of these calls come with M5 and W5.

```c
#define NV_ASC_MAX 64

typedef bool (*NvEncoderOpen)(void *context, uint64_t recording, uint32_t track,
                              uint32_t sample_rate, uint32_t channels, uint32_t bit_rate,
                              uint8_t *asc, size_t asc_cap, size_t *asc_len,
                              uint32_t *priming_frames);
typedef void (*NvEncoderEncode)(void *context, uint64_t recording, uint32_t track,
                                const float *pcm, size_t frames);
typedef void (*NvEncoderClose)(void *context, uint64_t recording, uint32_t track, bool flush);

typedef struct NvAudioEncoder {
    void *context;
    /* Opens an AAC-LC encoder for one track of a recording. Writes the
     * AudioSpecificConfig into asc (asc_len bytes, 1 to asc_cap, which is
     * NV_ASC_MAX) and the encoder's priming in frames. Returns false if it
     * cannot. */
    NvEncoderOpen open;
    /* One frame of 1024 mono float samples, on the recorder's thread. */
    NvEncoderEncode encode;
    /* Ends the track; with flush, the encoder's remaining output first. */
    NvEncoderClose close;
} NvAudioEncoder;

NvStatus nv_capture_configure(NvCore *core, const char *json);

NvStatus nv_capture_set_encoder(NvCore *core, const NvAudioEncoder *encoder);
NvStatus nv_record_audio_encoded(NvCore *core, uint64_t recording, uint32_t track,
                                 const uint8_t *data, size_t len);

NvStatus nv_record_start(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
NvStatus nv_record_stop(NvCore *core, uint64_t recording);
NvStatus nv_record_status(NvCore *core, char *out, size_t cap, size_t *len);
```

1. **Configure.** `nv_capture_configure(core, json)` sets the limits and
   the live recordings' folder, and recovers partial files left by a crash
   (CAPTURE.md, "Crash safety"; event `recovered` for each):

   ```json
   {"live_dir": "<absolute directory>", "max_duration_s": 3600, "max_bytes": 4000000000,
    "min_free_bytes": 2000000000, "stop_free_bytes": 1000000000,
    "max_live": 4, "max_calls": 2, "quality": "main", "share_safe": false}
   ```

   `quality` is the "Record from" setting (`main` or `sub`); `share_safe`
   the "Share-safe names" setting. Every field but `live_dir` is optional
   with the defaults shown. Calling it again changes the settings for
   recordings started afterwards.
2. **Encoder.** `nv_capture_set_encoder(core, encoder)` installs the app's
   AAC encoder (NULL removes it; the struct is copied). The core calls
   `open` once per track that needs encoding, `encode` with 1024 frames at a
   time on the recorder's own thread, and `close` at the end. The app
   answers each encoded frame with `nv_record_audio_encoded(core,
   recording, track, data, len)`: one raw AAC access unit, in order, from
   any thread, until `close` returns. The core keeps at most 10 s of PCM
   per track waiting for the encoder; beyond that it encodes silence and
   counts `encode_dropped_ms`. Without an encoder, a live recording whose
   camera audio is not AAC is video only (`audio` `unsupported`), and a
   call recording cannot start.
3. **Start.** `nv_record_start(core, request, out, cap, len)`:

   ```json
   {"kind": "live", "tile": 7, "quality": "sub", "name": "Camera 1", "share_safe": false}
   {"kind": "call", "tile": 7, "file": "<absolute path>.mp4"}
   ```

   A live recording records the tile's camera (device, channel, lens) on
   its own subscription; `quality` overrides the configured tier, `name` is
   the camera name for the file, `share_safe` overrides the setting, and
   `utc_offset_s` (optional, default 0) is the computer's local offset, for
   the local time in the file name. A call
   recording writes to the path the app reserved beside the transcript.
   The answer: `{"recording": 12, "file": "<absolute path>",
   "state": "waiting_keyframe"}`. Errors: `NV_ERR_LIMIT` (the count limits,
   at most 2 main-stream recorders per device, or under `min_free_bytes`
   free; the answer then holds `{"limit": "live"|"calls"|"main_streams"|"disk_low"}`),
   `NV_ERR_NOT_CONFIGURED` (the status the recordings minor adds; no
   `live_dir`, or a call without an encoder),
   `NV_ERR_UNKNOWN_ID` (no such tile), `NV_ERR_INVALID_ARGUMENT`.
4. **Stop and status.** `nv_record_stop(core, recording)` stops and
   finalizes (state `finalizing`, then `finished`). `nv_record_status`
   answers `{"recordings": [...]}`, each entry shaped like the event below.
   A recording outlives its tile; it ends on stop, a cap, low disk, the
   device's removal, a refused login, or `nv_core_free` (which stops and
   finalizes what it can within 5 s; the rest is recovered at the next
   `nv_capture_configure`).

```json
{"type":"recording","recording":12,"kind":"live","device":1,"channel":3,"lens":0,"tier":"main",
 "state":"recording","file":"<absolute path>","segment":1,"bytes":10485760,"duration_ms":83000,
 "gaps":0,"audio":"passthrough","tracks":["video","audio"],"encode_dropped_ms":0}
{"type":"recording","recording":12,"kind":"live","device":1,"channel":3,"lens":0,"tier":"main",
 "state":"finished","reason":"max_duration","file":"<absolute path>","segment":1,
 "bytes":1048576000,"duration_ms":3600000,"gaps":1,"audio":"passthrough","tracks":["video","audio"],
 "encode_dropped_ms":0}
```

- `state`: `waiting_keyframe`, `recording`, `segment` (a new part started;
  `file` and `segment` name it), `finalizing`, `finished`, `failed`,
  `recovered`. Sent at once on a change and about once a second while
  recording.
- `dropped_frames`: video frames lost on the way (a slow recorder's
  queue, or out of order); `fragmented` (on `finished` and `failed`): the
  fragmented file was published as the `.mp4` because finalizing had no
  room or failed.
- `audio`: `passthrough`, `encoded`, `none` (the camera sends no audio),
  `unsupported` (not AAC and no encoder). A call recording's `tracks` are
  `video`, `Conversation`, `Visitor`, `You`.
- `reason` (with `finished` or `failed`): `stopped`, `max_duration`,
  `max_size`, `disk_low`, `device_removed`, `login_failed`,
  `core_shutdown`, `write_failed`, `encoder_failed`, `no_video`,
  `format_changed`, `long_gap`.

### Clarifications from the implementation (F5)

- **Configure first.** Until `nv_capture_configure` succeeds,
  `nv_record_start` answers `NV_ERR_NOT_CONFIGURED`. `live_dir` must be
  absolute; the core creates it.
- **Free space** is read by the core itself (`statvfs`, or
  `GetDiskFreeSpaceExW` on Windows); there is no C hook for it.
- **`utc_offset_s`** in a live start request is how an app gets its local
  time into the file name; the core has no time zone of its own. Apps pass
  the computer's current offset.
- **Status mapping.** `CaptureError` maps one to one: `InvalidArgument`
  (bad JSON or values, a relative `live_dir` or call `file`, or a call
  from inside a callback), `UnknownId`, `Closed`, `Limit`,
  `NotConfigured`, `Internal`.
- **The limit answer.** On `NV_ERR_LIMIT`, `nv_record_start` writes
  `{"limit": ...}` to `out` when it fits and sets `*len` either way (a
  limit is reported even with a zero `cap`). For every other error `*len`
  is 0.
- **An answer that does not fit.** The recording has started by the time
  its answer is known, so when the answer does not fit the core stops it
  again and returns `NV_ERR_BUFFER_TOO_SMALL` with `*len` set; its events,
  for an ID never handed out, may follow (ignore them like any unknown ID),
  and starting again with a larger buffer is right. Pass a buffer of a few
  KiB (the bindings use 4096) so this does not happen.
- **The encoder** is checked when installed: a `NULL` function pointer is
  `NV_ERR_INVALID_ARGUMENT`. Its functions and context stay in use until
  `nv_core_free` returns (a recording keeps the encoder it started with,
  and recordings finalize during `nv_core_free`), so the bindings keep
  every encoder they install until then. `open` writes into a buffer of
  `NV_ASC_MAX` bytes; an `asc_len` of 0 or above `asc_cap` refuses the
  track. `encode` always gets 1024 frames.
- **`nv_record_audio_encoded`** is allowed inside the encoder's own
  functions (no inside-callback refusal) and from any other thread; it
  takes a unit of 1 to 8192 bytes. A unit for a recording that has ended
  but is not yet forgotten is accepted and ignored; `NV_ERR_UNKNOWN_ID`
  is an unknown recording.
- **Stopping** a recording that is already ending does nothing (`NV_OK`);
  `NV_ERR_UNKNOWN_ID` is an unknown one. `nv_record_status` may be called
  from any thread outside callbacks.

The `channels` event gains, without an ABI change, from each channel's
`Support` item: `"talk": true` (`ipcAudioTalk` 1), `"doorbell": true`
(`channelType` "Doorbell"), and `"ptz": {"pan_tilt": true, "presets": true,
"zoom_focus": true}` (CAPTURE.md, "Capabilities").

## Camera motion (PTZ)

> **Proposed, not yet implemented.** Pan, tilt, lens zoom and focus, and
> preset recall for [CAPTURE.md](CAPTURE.md) "Pan, tilt, and zoom"; the
> commands are in protocol.md, "What nvrview will send". Its own minor,
> the next free one when it lands ("Versioning").
>
> **Implementation status (Z1).** The Rust side exists on `Core`:
> `ptz_info`, `ptz_move`, `ptz_stop`, `ptz_lens`, `ptz_preset` (each
> taking a `MotionOrigin`; the C functions will pass `Manual`) and
> `set_motion_grant`, with the `ptz` events below. The C functions are
> not exported yet: minors are numbered in the order they land and other
> additions in flight are taking the next ones, so they come with the
> apps' PTZ controls (Z2) on the next free minor then. As built, beyond the shapes
> below: a renewal sends nothing and reports nothing; a lens move's `sent`
> event carries the clamped `position`; `refused` (the core's rules:
> `not_granted`, `user_steering`, `rate_limited` with `retry_after_ms`,
> `max_hold`, `not_supported`, `unknown_preset`) is told apart from
> `failed` (`device_refused` with `status`, `connection_lost`,
> `timed_out`, `session_closed`, `query_failed`); and the session's own
> stops come on the tile that started the move as `{"type":"ptz",
> "tile":7,"action":"stop","state":"sent","reason":"deadman"}`, with
> `reason` one of `released` (an `nv_ptz_stop`), `deadman`, `max_hold`,
> `replaced`, `device_refused`, `grant_withdrawn`, `session_closed`,
> `reconnected`. A speed outside 1–64, or an automation name that is empty,
> longer than 64 bytes or holds control characters, is
> `NV_ERR_INVALID_ARGUMENT`. `nv_ptz_info` blocks for at most 20 s.

```c
NvStatus nv_ptz_info(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
NvStatus nv_ptz_move(NvCore *core, uint64_t tile, const char *direction, uint32_t speed);
NvStatus nv_ptz_stop(NvCore *core, uint64_t tile);
NvStatus nv_ptz_lens(NvCore *core, uint64_t tile, const char *what, int32_t position);
NvStatus nv_ptz_preset(NvCore *core, uint64_t tile, uint32_t id);
NvStatus nv_motion_grant(NvCore *core, uint64_t device, uint8_t channel,
                         const char *client, bool granted);
```

- `nv_ptz_info(core, "{\"tile\":7}", ...)` reads 199, 294 and 190 (viewing
  tier) for the tile's camera: `{"pan_tilt": true, "zoom": {"min": 0,
  "max": 28, "cur": 11} | null, "focus": {...} | null, "presets": [{"id": 0,
  "name": "Preset 1"}], "speeds": [1, 64]}`.
- `nv_ptz_move(core, tile, "left"|"right"|"up"|"down", speed)` (speed 1 to
  64) starts a move, and renews it: the app calls it again at least every
  500 ms while the control is held. The core sends the move once and `stop`
  1 s after the last renewal (dead-man), or after 30 s of holding;
  renewals are not sent and not counted. `nv_ptz_stop` stops at once.
- `nv_ptz_lens(core, tile, "zoom"|"focus", position)` sends 295, clamped to
  the range 294 reported.
- `nv_ptz_preset(core, tile, id)` recalls a preset (19 with `toPos` only).
- The app's own calls are the user's manual control and need no enable.
  `nv_motion_grant(core, device, channel, client, granted)` grants
  automation (a local API client, or `"auto_correction"`) motion control on
  one camera for this run of the core; the app passes the user's grants
  after each `nv_device_add`. Talk-back keeps `nv_talk_set_permission`.
- Limits per camera: moves and lens commands 20 per second, bursts of 40;
  preset recalls 5 per second. `stop` is sent on every path (release,
  error, timeout, disconnect, tile close, session drop) and is never
  limited. Every command is logged with its origin.

```json
{"type":"ptz","tile":7,"action":"move","state":"sent"}
{"type":"ptz","tile":7,"action":"preset","state":"refused","reason":"rate_limited"}
{"type":"ptz","tile":7,"action":"lens","state":"failed","reason":"device_refused"}
```

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
- Storage: `layouts.json`, `devices.json`, and `lenses.json` (second-lens
  probe answers, see `channels`) in `NvConfig.data_dir`, each rewritten
  whole and atomically (temporary file, flush, rename). An unreadable file
  is renamed to `*.unreadable` and the store starts empty. With no
  `data_dir` they live in memory for the run.
- Strings come back through `out`/`cap`/`len`: `*len` always receives the
  length without the NUL; when `out` is NULL or `cap` <= `*len` the call
  returns `NV_ERR_BUFFER_TOO_SMALL` and the app calls again with a buffer of
  `*len + 1` bytes.

## Device discovery

Minor 1 of ABI 3: an addition that leaves every existing declaration
unchanged (see "Versioning"). The core side is the `discovery` crate
(native search and probe, docs/protocol.md "LAN discovery"; ONVIF
WS-Discovery) wrapped in `viewer-core`'s `discover` module, and
`bc-cli discover` runs the same crate from a terminal.

```c
/* Starts a search for devices on the local network: a few rounds of UDP
 * broadcasts (and the WS-Discovery multicast) on each IPv4 interface that is
 * up, or on the interfaces named in json, then listening until the time is
 * up. Answers arrive as `discovered` events, then one `discovery_done`.
 * json (may be NULL or empty for defaults):
 *   {"seconds":3, "interfaces":["192.0.2.5"], "onvif":true}
 * Returns at once; NV_ERR_INVALID_ARGUMENT for invalid JSON or values.
 * One search per core: a start while a search runs stops it (its
 * `discovery_done` has "stopped":true) and starts the new one. */
NvStatus nv_discover_start(NvCore *core, const char *json);

/* Stops the running search, if any, and closes its sockets; its
 * `discovery_done` ("stopped":true) is queued before this returns. NV_OK
 * when nothing is running. */
NvStatus nv_discover_stop(NvCore *core);

/* The minor version (see "Versioning"); NV_ABI_MINOR in the header. */
uint32_t nv_abi_minor(void);
```

- **Request.** Every member is optional and unknown members are ignored.
  `seconds` is 0.5 to 30 (default 3), `interfaces` lists IPv4 addresses of
  this computer (at most 32; default: every IPv4 interface that is up,
  loopback and point-to-point excluded), `onvif` turns the WS-Discovery
  probe off (default true). Anything of the wrong type or range is
  `NV_ERR_INVALID_ARGUMENT` and starts nothing.
- **Calls.** Like the other calls, both are refused from inside a callback
  (`NV_ERR_INVALID_ARGUMENT`) and after `nv_core_free` began
  (`NV_ERR_CLOSED`). `nv_core_free` ends a running search the same way
  `nv_discover_stop` does, so its `discovery_done` is delivered first.

Events (same rules as every event: ignore unknown types and fields):

```json
{"type":"discovery_started","search":1}
{"type":"discovered","search":1,"key":"d1","kind":"nvr","name":"NVR 1","address":"192.0.2.10","port":9000,"channels":16,"services":[{"kind":"native","port":9000}]}
{"type":"discovered","search":1,"key":"d2","kind":"camera","name":"Camera 1","model":"EXAMPLE-CAM","address":"192.0.2.20","services":[{"kind":"onvif","port":8000,"url":"http://192.0.2.20:8000/onvif/device_service"}]}
{"type":"discovered","search":1,"key":"d3","kind":"unknown","address":"192.0.2.30","battery_percent":80,"services":[{"kind":"native"}]}
{"type":"discovery_done","search":1,"count":3,"stopped":false,"errors":["en1: native search on 192.0.2.5:3000: address in use"]}
```

- **Protocol-neutral fields.** `kind` is `nvr`, `camera` or `unknown`;
  `name`, `model`, `channels` and `battery_percent` appear when an answer
  carried them (the native search carries a name and a channel count, not a
  model; ONVIF carries a model). `address` is the IPv4 address that
  answered. `port` is the TCP port of the native protocol when the answer
  said (the search does; the battery-camera probe does not): the app fills
  it into its add form, with 9000 as the usual default when it is absent.
  `services` lists how to connect, each with `kind` (`native` for the
  port-9000 protocol, `onvif`, more later), `port` and `url` when known.
  Absent fields are unknown. The app picks a backend from `services` and
  passes `address` and `port` to `nv_device_add`; nothing in the event is
  specific to one wire protocol beyond the service names.
- **Already added.** The core does not say whether a found device is one the
  app already has. The app compares `address` (and `port`) with the
  devices it added; a `"known"` field matched by the core against device
  keys remains future work.
- **Started.** `discovery_started` follows every successful
  `nv_discover_start`, before any answer, and names the search ID. An app
  that starts searches in quick succession counts its starts and treats the
  last one's events as current (the stopped search's `discovery_done` and
  the new search's events can interleave with the calls).
- **Updates.** A device that answers again with more (another backend, a
  battery reading) is sent again with the same `key` and the merged fields;
  the app replaces its row. `key` is opaque and lives only for one search
  (`search` is the ID `nv_discover_start` started; events of a search that
  was stopped or restarted never follow its `discovery_done`).
- **IDs stay in the core.** Device UIDs and ONVIF endpoint IDs are used to
  merge answers (a device that answers at two addresses is one row) but never
  appear in events, logs, or the UI. MAC addresses are also left out of
  events; addresses are logged at debug level only (see "Logging"). The
  `errors` of `discovery_done` are the runner's per-interface messages
  (interface names and addresses, such as a port already in use): show them
  at most in a details view, never log them above debug level.
- **Only on request.** The core never searches on its own: the app calls
  `nv_discover_start` when the user opens its "find devices" screen and
  `nv_discover_stop` when it closes. Nothing leaves the local network:
  broadcasts go to each interface's subnet broadcast address and the
  WS-Discovery multicast goes out with a TTL of 1.
- **Bounded.** One search per core; at most 1024 devices per search;
  `seconds` from 0.5 to 30 (default 3); answers that are malformed or answer
  someone else's probe are counted and dropped.
- **Port 3000.** The native search's answers come to UDP port 3000. The core
  binds it on each interface's own address, which works next to the official
  desktop client holding it on the wildcard address (observed on macOS);
  when it cannot, `discovery_done` lists the error and the other backends
  still run.
- **macOS local network privacy.** A newly built command-line binary's
  broadcasts and the answers to them were silently dropped on macOS 26
  (sends reported success, nothing arrived), while an already approved
  interpreter making the same calls worked. For the app bundle:
  `NSLocalNetworkUsageDescription` (already in its Info.plist; the text
  mentions the search) is what the system's Local Network prompt shows, and
  the user's approval is needed the first time it searches. Neither
  `NSBonjourServices` (it declares Bonjour service types the app browses; the
  search uses none) nor the `com.apple.developer.networking.multicast`
  entitlement (a restricted entitlement Apple documents for iOS and iPadOS;
  Apple's developer forums state that neither Bonjour advertising nor direct
  local connections need it, and nothing found says macOS requires it for
  UDP broadcast) is added, which also keeps the app signable with an Apple
  Development identity. While access is denied or waiting, the ONVIF
  multicast sends fail with "No route to host" (reported in the `errors` of
  `discovery_done`) and the broadcasts go nowhere; the sends can also delay
  the search's end by several seconds. An app that gets `count` 0 with such
  errors points at System Settings, Privacy & Security, Local Network.

## Updates

Minor 2 of ABI 3. The core side is the `updates` crate wrapped in
`viewer-core`'s `update` module; [UPDATES.md](UPDATES.md) is the design
(sources, manifest, signatures, staging, and what the apps do to apply).
The core checks, downloads, verifies, and unpacks; it never applies an
update and never checks on its own.

```c
NvStatus nv_update_check(NvCore *core, const char *json);
NvStatus nv_update_download(NvCore *core, const char *json);
NvStatus nv_update_cancel(NvCore *core);
NvStatus nv_update_parse_key(const char *text, char *out, size_t cap, size_t *len);
NvStatus nv_update_parse_source(const char *text, char *out, size_t cap, size_t *len);
```

- **Request.** `{"source":"https://updates.example.com/nvrview/",
  "keys":["nvrview-ed25519-pub <hex>"],"current":"0.3.0",
  "platform":"macos-arm64"}`; a download adds `"version":"0.4.0"` (the
  version the user accepted) and `"staging_dir":"/abs/path"` (must not
  exist; on the same volume as the installed app). `platform` is optional
  (the core's own: `macos-arm64`, `win-x64`, ...). `"require_published":
  true` (optional, default false) refuses a manifest without a `published`
  time (`unpublished`). Unknown members are ignored. Invalid JSON, a key that does not parse, more than 16 keys, or a
  version that is not semantic is `NV_ERR_INVALID_ARGUMENT`; an unacceptable
  source is reported by the operation's `failed` event, so the app shows one
  kind of message for every failure.
- **One at a time.** A start while a check or download runs is
  `NV_ERR_LIMIT`. `nv_update_cancel` waits up to about half a second for the
  operation to end by itself; either way its final event is queued before
  the call returns and nothing more of it follows. A worker still blocked
  in a network read (a server that stopped answering) ends at its next
  chunk or timeout, removing what it created; its outcome is dropped.
  `nv_core_free` cancels the same way.
- **Calls.** Like the other calls, refused from inside a callback and after
  `nv_core_free` began. The two parse functions need no core and follow the
  out-buffer rules of "Layouts".

Events (ignore unknown types and fields, as always):

```json
{"type":"update","op":1,"action":"check","state":"started"}
{"type":"update","op":1,"action":"check","state":"available","current":"0.3.0","version":"0.4.0","notes":"Fixes.","size":31457280,"key_id":"0011223344556677","published":"2026-10-11T12:00:00Z"}
{"type":"update","op":2,"action":"check","state":"up_to_date","current":"0.4.0","latest":"0.4.0","published":"2026-10-11T12:00:00Z"}
{"type":"update","op":3,"action":"check","state":"up_to_date","current":"0.3.0","latest":"0.4.0","no_package":true,"warning":"unpublished"}
{"type":"update","op":4,"action":"download","state":"downloading","received":1048576,"total":31457280}
{"type":"update","op":4,"action":"download","state":"ready","version":"0.4.0","path":"/Applications/.NVRView-update/staging-0.4.0/content","published":"2026-10-11T12:00:00Z"}
{"type":"update","op":5,"action":"check","state":"failed","reason":"bad_signature","message":"the manifest's signature does not verify with any trusted key"}
{"type":"update","op":6,"action":"download","state":"canceled"}
```

- Every operation sends `started` first, then at most one final event:
  `available` or `up_to_date` (check), `ready` (download), `failed`, or
  `canceled`. `downloading` comes at most once per percent, first with
  `received` 0.
- `reason` (with `failed`): `source_invalid`, `insecure_source`,
  `no_trusted_keys`, `unreachable`, `not_found`, `too_large`,
  `redirect_refused`, `bad_signature`, `bad_manifest`, `unsupported_format`,
  `version_changed`, `not_newer`, `bad_package`, `unsafe_archive`,
  `staging`, `unsupported_cpu`, `expired`, `stale_manifest`,
  `not_yet_valid`, `unpublished` (UPDATES.md says when each happens).
  `message` is a sentence for people; it can name the source's host or
  folder, so apps show it but log only the reason.
- `available` only for a version strictly newer than `current` with a
  package for the platform; `no_package` marks a newer version without one.
- `available`, `up_to_date`, and `ready` carry the verified manifest's
  `published` time (`YYYY-MM-DDTHH:MM:SSZ`), or `"warning":"unpublished"`
  when it has none (UPDATES.md, "Dates and replays"). The core keeps the
  newest accepted `published` per source in `update-state.json` in
  `data_dir` (in memory without one) and refuses older manifests from that
  source.
- `ready`'s `path` is the unpacked package (`<staging_dir>/content`): on
  macOS it holds `NVRView.app`, on Windows the app folder's files.
- The core logs one line per outcome with its reason; sources and messages
  only at debug level.

## Recordings (minor 5)

Part 1 of the recordings interface (slice F1 of
[MILESTONE-P.md](MILESTONE-P.md)): configuration, the index and its
on-demand requests, incidents with the user's edits and locks, camera
groups, cover thumbnails, downloads with the Trash, and
`nv_sound_configure`. [FFI-RECORDINGS.md](FFI-RECORDINGS.md) is the
contract (sections 1 to 5, 8, 9, and 13, with its decisions and
clarifications); the core side is `viewer_core::recordings_owner` (C11,
C13, C15). Recorded playback (section 6, F2) is minor 6 ("Recorded
playback (minor 6)"); edit lists and the rest of
sound (sections 2a and 7, F3) are minor 8 ("Edit lists and sound (minor
8)"); export (section 7a, F4) comes later; an app checks the minor before
each group.

```c
NvStatus nv_recordings_configure(NvCore *core, const char *json);
NvStatus nv_recordings_enable(NvCore *core, const char *device_key, const char *json);
NvStatus nv_recordings_calendar(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
NvStatus nv_recordings_spans(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
NvStatus nv_recordings_events(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
NvStatus nv_recordings_want(NvCore *core, const char *json);
NvStatus nv_recordings_hold_today(NvCore *core, uint64_t hold, const char *json);
NvStatus nv_recordings_status(NvCore *core, char *out, size_t cap, size_t *len);
NvStatus nv_incidents_get(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
NvStatus nv_incident_get(NvCore *core, const char *key, char *out, size_t cap, size_t *len);
NvStatus nv_incident_edit(NvCore *core, const char *edit, char *out, size_t cap, size_t *len);
NvStatus nv_incident_lock(NvCore *core, const char *key, bool locked, char *out, size_t cap,
                          size_t *len);
NvStatus nv_camera_groups_get(NvCore *core, char *out, size_t cap, size_t *len);
NvStatus nv_camera_groups_put(NvCore *core, const char *json);
NvStatus nv_thumbnail_get(NvCore *core, const char *key, NvThumbnail *info, uint8_t *out,
                          size_t cap, size_t *len);
NvStatus nv_thumbnail_put(NvCore *core, const char *key, const NvThumbnail *info,
                          const uint8_t *data, size_t len);
NvStatus nv_thumbnail_cancel(NvCore *core, const char *key);
NvStatus nv_download_incident(NvCore *core, const char *request, char *out, size_t cap,
                              size_t *len);
NvStatus nv_download_pause(NvCore *core, const char *download, const char *camera, bool paused);
NvStatus nv_download_retry(NvCore *core, const char *download, const char *camera);
NvStatus nv_download_forget(NvCore *core, const char *download, bool delete_partial);
NvStatus nv_download_trash(NvCore *core, const char *download, char *out, size_t cap,
                           size_t *len);
NvStatus nv_downloads_get(NvCore *core, char *out, size_t cap, size_t *len);
NvStatus nv_sound_configure(NvCore *core, const char *json);
```

- **Status codes.** Three are new, and only these functions return them:
  `NV_ERR_UNSUPPORTED` (-8, the device's backend cannot do this),
  `NV_ERR_NOT_CONFIGURED` (-9, `nv_recordings_configure` was not called,
  or a download needs `downloads_dir`), and `NV_PENDING` (1, the first
  positive status: accepted, the answer arrives as an event; only
  `nv_thumbnail_get` returns it). The core's `RecError` maps one to one:
  `InvalidArgument` -1, `UnknownId` -2, `Closed` -3, `Limit` -4,
  `Internal` -5, `Unsupported` -8, `NotConfigured` -9. A NULL `core`, a
  required string that is NULL or not UTF-8, and a NULL `len` or `info`
  are `NV_ERR_INVALID_ARGUMENT`.
- **Threads.** Every one of these answers from memory under short locks and
  never waits on the network or a device permit; searches, thumbnail
  fetches, and downloads run on core threads and report by event. Call
  them from any thread but never from inside a callback (there they return
  `NV_ERR_INVALID_ARGUMENT`). After `nv_core_free` returns, no search,
  fetch, or download runs (job files keep the progress).
- **JSON in and out.** Arguments are UTF-8, NUL-terminated, and copied
  before the call returns; cameras are `{"device": "<device key>",
  "channel": 3, "lens": 0}` (lens optional), times device-local seconds.
  Answers follow the out-buffer rules of "Layouts". Queries may change
  between two calls (the index fills meanwhile), so bindings loop until the
  answer fits.
- **Calls that change state answer once.** `nv_incident_edit`,
  `nv_incident_lock`, `nv_download_incident`, `nv_download_trash`, and (minor
  8) `nv_sound_analyze_day` make
  their change even when the answer does not fit: `NV_ERR_BUFFER_TOO_SMALL`
  (with `*len` set) then only says the answer was not written, and calling
  again would apply the change again (or report `NV_ERR_UNKNOWN_ID` for a
  download already trashed). Bindings pass a large buffer (64 KiB) for
  these; `len` is checked before anything changes.
- **Thumbnails.** `NvThumbnail.kind` is an `NvThumbKind`, received as an
  integer and validated (`NV_ERR_INVALID_ARGUMENT` for other values), and
  `pts_us` must not be negative. The bytes are binary, without a
  terminator: `nv_thumbnail_get` returns `NV_ERR_BUFFER_TOO_SMALL` when
  `out` is NULL or `cap` < `*len`, with `*info` and `*len` filled, and
  `NV_PENDING` with `*len` 0. A fetch for a device that is not enabled and
  added answers `NV_PENDING` and a `thumbnail` event `failed`.
- **`device_info`** gains `"recordings": {"search": true, "playback":
  true, "download": true, "thumbnails": true}`, what the device's backend
  can do (all four for Baichuan devices).

- **A month's calendar alone (minor 7).** `nv_recordings_want` also takes
  `"months"` (any second within each month, device-local; at most 12): only
  those months' calendars (142, one request per month for every camera
  asked), searched after any wanted days, so a calendar paged to another
  month gets its recording days without searching a day. A past month's
  calendar fetched after the month ended is not asked again unless
  `force`; the current month is asked again after `refresh_today_s`.
  `"days"` and `"months"` may each be left out, not both. A month that
  could not be searched is reported once by `recordings_month_failed`
  (not retried); one that was searched is reported by `recordings_changed`
  over the whole month.
- **Camera names** in answers (`name` of an incident's cameras, a
  download's cameras and folder) are the user's override, else the name
  the device reports for the channel (with " · Lens N" for a multi-lens
  camera), else "Camera N" until the device has reported its names (after
  each login; the names are not stored).

Events (FFI-RECORDINGS.md, section 8; ignore unknown types and fields):

```json
{"type":"recordings_index","device":"d-3f9a0c1b7e2d4a65","state":"busy","pending_days":11,"revision":40}
{"type":"recordings_month_failed","device":"d-3f9a0c1b7e2d4a65","cameras":[{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0}],"month":1788220800,"reason":"session closed during the search"}
{"type":"recordings_day_failed","device":"d-3f9a0c1b7e2d4a65","camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"day":1791504000,"reason":"session closed during the search"}
{"type":"recordings_changed","device":"d-3f9a0c1b7e2d4a65","revision":41,"start":1791590400,"end":1791676799}
{"type":"incidents_changed","revision":12}
{"type":"camera_groups_changed","revision":3}
{"type":"thumbnail","key":{"camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"tier":"sub","start":1791618125,"end":1791618141},"state":"ready"}
{"type":"download","download":"1791618125-9c1e5a2b7d40f316","state":"running","cameras":[]}
{"type":"download_removed","download":"1791618125-9c1e5a2b7d40f316","trash":true,"folder":"/abs/downloads/2026-10-10 07.42 vehicle — Camera 1"}
```

The C smoke test (`scripts/test-c-ffi.sh`, also under AddressSanitizer)
goes through configure, enable, want, the queries, an edit, a thumbnail from
pending to ready and back as an image, a download to the end, a lock that
returns it, and the Trash, against mock-nvr with two days of recordings.

## Edit lists and sound (minor 8)

Slice F3 of [MILESTONE-P.md](MILESTONE-P.md): edit decision lists
([FFI-RECORDINGS.md](FFI-RECORDINGS.md), section 7) and the rest of sound
(section 2a, after `nv_sound_configure`). The core side is
`viewer_core::recordings_owner` (`edit_compute` over `recordings::edl`;
`sound_watch`, `recordings_loudness`, `sound_labels_put`,
`sound_analyze_day`, `sound_analyze_cancel`; C8 and C12). The rules of both
sections and their C8 and C12 clarifications apply; "Clarifications from the
implementation (F3)" there lists what wrapping them settled.

```c
NvStatus nv_edit_compute(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
NvStatus nv_sound_watch(NvCore *core, const char *camera, bool enabled);
NvStatus nv_recordings_loudness(NvCore *core, const char *query, char *out, size_t cap,
                                size_t *len);
NvStatus nv_sound_labels_put(NvCore *core, const char *json);
NvStatus nv_sound_analyze_day(NvCore *core, const char *json, char *out, size_t cap, size_t *len);
NvStatus nv_sound_analyze_cancel(NvCore *core, uint64_t request);
NvStatus nv_tile_loudness(NvCore *core, uint64_t tile, uint8_t *peak, uint8_t *rms, size_t cap,
                          int64_t *start_s, size_t *len);
```

- **Status codes, threads, JSON in and out** are those of "Recordings (minor
  5)" (`RecError` maps one to one; out-buffer rules; never inside a
  callback). `nv_tile_loudness` is the one that is not an answer in JSON:
  it takes two brief locks and may be called from any thread outside
  callbacks.
- **`nv_edit_compute`** takes `{"kind": "follow" | "grid", "download":
  "<id>", ...}` and answers the plan of section 7 (`version` 1). The
  download must have finished; cameras whose job is not done or whose file
  cannot be read are left out, and `NV_ERR_INVALID_ARGUMENT` says no camera
  has a finished file or the plan has nothing to show (no events in the
  files' footage). `NV_ERR_UNKNOWN_ID` is an unknown download or one whose
  incident no longer exists. (Minor 8 answered `NV_ERR_UNSUPPORTED` to
  `in_ms`, `out_ms`, and the `trim` kind; minor 10 accepts them, see
  "Export (minor 10)".) The call reads each
  finished file's `moov` box (bounded) on the caller's thread, so it takes
  a few milliseconds per file, not a network round trip.
- **Sound** calls are those of section 2a: `nv_sound_watch` (at most 64
  cameras; `sound_watch` events), `nv_recordings_loudness` (lanes in
  dBFS, `null` where no audio was analyzed), `nv_sound_labels_put` (answers
  nothing; the reserved analyzer `pattern-v1` is refused),
  `nv_sound_analyze_day` and `nv_sound_analyze_cancel` (`sound_analyze`
  events; the day's audio is read from the camera's finished downloads: a
  request fails with `no_audio` when none covers the day, `no_source` when
  no downloads folder is configured), and `nv_tile_loudness` (a live tile's newest seconds, one
  byte per value: 0 no audio, 1 to 128 is -127 to 0 dBFS).

Events (FFI-RECORDINGS.md, sections 2a and 8; ignore unknown types and fields):

```json
{"type":"sound_watch","camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"state":"watching"}
{"type":"sound_analyze","request":4,"camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"day":1791590400,"state":"done","seconds":86000,"labels":3}
```

`state` of `sound_watch` is `watching`, `stopped`, or `refused` (`reason`
`no_audio`, `device_closed`, `sound_off`); of `sound_analyze` `queued`,
`running`, `done`, `failed` (`reason` `no_source`, `no_audio`,
`source_unavailable`, `not_enabled`, `index`, `internal`), or `canceled`.

## Export (minor 10)

Slice F4 of [MILESTONE-P.md](MILESTONE-P.md): export
([FFI-RECORDINGS.md](FFI-RECORDINGS.md), section 7a). The core plans (ranges,
edit lists, trims, names); the app decodes, encodes, and writes with platform
APIs. The core side is `viewer_core::recordings_owner` (`download_range`,
`still_plan`, `edit_compute` over `recordings::export` and
`Scheduler::enqueue_range`; C14 and C15). The rules of section 7a and the
"Clarifications from the implementation (F4)" there apply.

```c
NvStatus nv_download_range(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
NvStatus nv_still_plan(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
```

- **Status codes, threads, JSON in and out** are those of "Recordings (minor
  5)". `nv_download_range` changes state like `nv_download_incident`: the
  download is queued even when the answer does not fit the buffer, and asking
  again returns the same download as `existing`, so a binding makes the call
  once with room for the answer. `nv_still_plan` answers from memory (it
  reads a finished file's `moov` box for a file still) and may be repeated.
- **`nv_download_range`** takes `{"start", "end", "cameras": [{"camera",
  "tier"}], "name", "dest", "share_safe"}` (seconds, device-local) and
  answers `{"download", "folder", "existing"}`. `NV_ERR_NOT_CONFIGURED`
  without `downloads_dir` (or without a data directory when `dest` is not
  the downloads directory), `NV_ERR_LIMIT` beyond 16 cameras, 24 hours, or
  1000 known downloads, `NV_ERR_INVALID_ARGUMENT` for an empty range, a
  `dest` that is not an absolute directory, or a camera
  `nv_recordings_enable` does not allow. The `download` event of a range
  download has `incident` null and `range` `{"start","end"}`.
- **`nv_edit_compute`** gains the `trim` kind (`in_ms`, `out_ms`, `cut`
  `keyframe` or `exact`, optional `cameras`), which works on incident and
  range downloads; `in_ms` and `out_ms` on `follow` and `grid` (a range
  download has no incident, so those two answer `NV_ERR_INVALID_ARGUMENT`
  for one); and `dest` and `share_safe` on every kind. Its declaration is
  unchanged.
- **`nv_still_plan`** takes `{"playback", "tiles", "format", "dest",
  "share_safe"}` (the picture each playback tile shows now: the newest one
  the app answered with `nv_tile_decoded` for the generation the tile shows)
  or `{"download", "camera", "at_ms", "exact", "format"}` (a position in a
  finished file). `NV_ERR_UNKNOWN_ID` for an unknown playback, tile, or
  download.
- **`dest` and `share_safe`** are accepted by `nv_download_incident` too.

## Recorded playback (minor 6)

Part 2 of the recordings interface (slice F2 of
[MILESTONE-P.md](MILESTONE-P.md)): playbacks of up to four cameras on one
shared clock. [FFI-RECORDINGS.md](FFI-RECORDINGS.md), section 6, is the
contract, with its C10 and F2 clarifications; the core side is
`viewer_core::playback` (C10).

```c
typedef enum NvSeekMode { NV_SEEK_KEYFRAME = 0, NV_SEEK_EXACT = 1 } NvSeekMode;
typedef enum NvPlaybackState {
    NV_PLAYBACK_BUFFERING = 0, NV_PLAYBACK_PLAYING = 1, NV_PLAYBACK_PAUSED = 2,
    NV_PLAYBACK_ENDED = 3, NV_PLAYBACK_FAILED = 4,
} NvPlaybackState;

NvStatus nv_playback_open(NvCore *core, const char *json, uint64_t *out_playback,
                          uint64_t *out_tiles, size_t tile_cap, size_t *tile_count);
NvStatus nv_playback_add_camera(NvCore *core, uint64_t playback, const char *json,
                                uint64_t *out_tile);
NvStatus nv_playback_remove_camera(NvCore *core, uint64_t playback, uint64_t tile);
NvStatus nv_playback_play(NvCore *core, uint64_t playback);
NvStatus nv_playback_pause(NvCore *core, uint64_t playback);
NvStatus nv_playback_seek(NvCore *core, uint64_t playback, int64_t position_ms, NvSeekMode mode);
NvStatus nv_playback_set_rate(NvCore *core, uint64_t playback, double rate);
NvStatus nv_playback_set_offset(NvCore *core, uint64_t playback, uint64_t tile,
                                int64_t offset_ms);
NvStatus nv_playback_position(NvCore *core, uint64_t playback, int64_t *position_ms,
                              NvPlaybackState *state);
NvStatus nv_playback_status(NvCore *core, uint64_t playback, char *out, size_t cap,
                            size_t *len);
NvStatus nv_playback_close(NvCore *core, uint64_t playback);
```

- **Playback tiles are tiles.** Their IDs come from the same space as live
  tiles, and their media arrives through `on_action` with the same actions,
  generations, and answering rules (`nv_tile_decoded`,
  `nv_tile_decode_failed`), so an app's tile view shows recorded video as
  it shows live video. `nv_tile_set_tier`, the audio calls, `tile_stats`,
  and `tile_closed` work on them; `nv_tile_close` returns
  `NV_ERR_INVALID_ARGUMENT` (use `nv_playback_remove_camera` or
  `nv_playback_close`). Generation 0 of each tile is an empty placeholder
  ended by a final Reset; every seek or rate change is a new generation.
- **Opening.** The request is section 6's JSON (a camera list with a range,
  or `{"incident": "<key>"}`); the cameras' devices must be enabled with
  `nv_recordings_enable` and their days in the index (`nv_recordings_want`),
  or the camera shows `no_recording`. Nothing about the playback or its
  tiles (no action, no event) is delivered before `nv_playback_open`
  returns its IDs. When `tile_cap` is smaller than the number of cameras,
  nothing is opened: `NV_ERR_BUFFER_TOO_SMALL` with `*tile_count` set (4 is
  always enough).
- **Status codes** are the recordings' (`RecError` as in "Recordings"):
  `NV_ERR_NOT_CONFIGURED` before `nv_recordings_configure`;
  `NV_ERR_INVALID_ARGUMENT` for invalid JSON, a rate other than 0.25, 0.5,
  1, 2, 4, 8, or 16, a seek mode that is not an `NvSeekMode`, a camera
  twice (also when adding), a camera of a device not enabled, an offset
  beyond 24 hours, or a NULL out pointer; `NV_ERR_UNKNOWN_ID` for an
  unknown playback, tile, or incident; `NV_ERR_LIMIT` beyond 8 playbacks, 4
  cameras per playback, a 24-hour range, the core's tile limit, or a full
  command queue (64 waiting requests); `NV_ERR_CLOSED` while the core shuts
  down.
- **Threads.** Every call returns at once, except `nv_playback_remove_camera`
  and `nv_playback_close`, which wait (at most about 10 s) for the
  playback's engine to stop the plays. Any thread, never inside a callback
  (there they return `NV_ERR_INVALID_ARGUMENT`), except
  `nv_playback_position`, which takes one brief lock and is meant for the
  scrubber, from any thread including callbacks.
- **Not in minor 6:** `nv_playback_step` (section 6) is not implemented
  yet and not exported; it takes a later minor.

Events (FFI-RECORDINGS.md, sections 6 and 8):

```json
{"type":"playback_state","playback":3,"state":"playing","position_ms":1791618127000,"rate":1.0}
{"type":"playback_camera","playback":3,"tile":11,"state":"buffering"}
{"type":"playback_seek","playback":3,"position_ms":1791618130000,"generation":4,"state":"active"}
{"type":"playback_closed","playback":3,"reason":"closed"}
```

The C smoke test opens a two-camera playback of the mock's recordings,
plays it, seeks, changes the rate to 4x, and closes it, also under
AddressSanitizer.

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
`tile_tier`, `layouts_changed`, and `timing` events, and talk-back
(`nv_talk_set_permission`, `nv_talk_start`, `nv_talk_push`,
`nv_talk_stop`, `NV_ERR_NOT_PERMITTED`, and the `talk` event; added
before ABI 3 was released, see "Talk-back"). Apps check
`nv_abi_version()` at startup and refuse to run on a mismatch.

`nv_abi_minor()` (`NV_ABI_MINOR`) counts additions that leave every existing
declaration unchanged, as [FFI-RECORDINGS.md](FFI-RECORDINGS.md) "Decisions"
(item 1) introduced it: the major number changes only for incompatible
changes. It is 0 for everything above and resets to 0 with a new major
number. An app needs the core's minor to be at least the minor that
introduced a function before calling it (an older core would not export the
symbol at all, so a bound app checks first):

| minor | added |
| --- | --- |
| 1 | `nv_abi_minor`, device discovery (`nv_discover_start`, `nv_discover_stop`, the `discovered` and `discovery_done` events) |
| 2 | updates (`nv_update_check`, `nv_update_download`, `nv_update_cancel`, `nv_update_parse_key`, `nv_update_parse_source`, the `update` event) |
| 3 | `nv_tile_retry` (the Retry button of a tile without a picture) |
| 4 | `nv_talk_set_conversation` (no per-minute talk-back limit while the user has a call open; see "Talk-back") |
| 5 | recordings, part 1 (see "Recordings"): `nv_recordings_*`, `nv_incident*`, `nv_camera_groups_*`, `nv_thumbnail_*`, `nv_download*`, `nv_sound_configure`, `NvThumbKind`, `NvThumbnail`, `NV_ERR_UNSUPPORTED`, `NV_ERR_NOT_CONFIGURED`, `NV_PENDING`, and the recordings events; `device_info.recordings` |
| 6 | recorded playback (see "Recorded playback"): `nv_playback_open`, `nv_playback_add_camera`, `nv_playback_remove_camera`, `nv_playback_play`, `nv_playback_pause`, `nv_playback_seek`, `nv_playback_set_rate`, `nv_playback_set_offset`, `nv_playback_position`, `nv_playback_status`, `nv_playback_close`, `NvSeekMode`, `NvPlaybackState`, and the `playback_state`, `playback_camera`, `playback_seek`, and `playback_closed` events |
| 7 | a month's calendar alone: `months` in `nv_recordings_want` and the `recordings_month_failed` event (see "Recordings"); no declaration changed |
| 8 | edit lists and sound (see "Edit lists and sound (minor 8)"): `nv_edit_compute`, `nv_sound_watch`, `nv_recordings_loudness`, `nv_sound_labels_put`, `nv_sound_analyze_day`, `nv_sound_analyze_cancel`, `nv_tile_loudness`, and the `sound_watch` and `sound_analyze` events |
| 9 | capture (see "Capture"): `nv_capture_configure`, `nv_capture_set_encoder`, `nv_record_audio_encoded`, `nv_record_start`, `nv_record_stop`, `nv_record_status`, `NvAudioEncoder` (and its `NvEncoderOpen`, `NvEncoderEncode`, `NvEncoderClose`), `NV_ASC_MAX`, and the `recording` event |
| 10 | export (see "Export (minor 10)"): `nv_download_range`, `nv_still_plan`, the `trim` kind and `in_ms`/`out_ms` of `nv_edit_compute`, `dest` and `share_safe` on export requests, and `range` in the `download` event |

Later minors are taken in the order the additions land; this table is the
source of truth. Stepping a playback (`nv_playback_step`) takes the next
free minor. Camera motion ("Camera motion (PTZ)") is proposed and takes
the next free minor when it lands.

## Builds

- macOS: a static library per architecture, packaged as an XCFramework with
  the header and a module map (`import NVRViewCore` in Swift).
- Windows: a DLL plus import library; C# bindings in `apps/windows`
  (`NvrView.Interop`), checked by a smoke test in CI.
- Linux: built and tested in CI so the core stays portable.
