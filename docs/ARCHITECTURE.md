# Architecture

nvrview is a lightweight, native viewer for Reolink NVRs and cameras that speaks Reolink's
own protocol ("Baichuan", TCP 9000) directly. macOS (Apple Silicon) comes first;
Windows is planned, and the design keeps that possible from the first commit.

## Goals

What it does: live view of every channel on one or more NVRs (a grid of
sub-streams, one channel full screen on its main stream), with listening that
stays in sync with video ([AUDIO.md](AUDIO.md)). How it should do it:

1. **Fix what the official app gets wrong.** No decoder rebuild loops on bad
   streams (wait for the next keyframe), no audio that drifts out of sync over
   hours, no logs that grow without bound, no reconnect storms.
2. **Start fast, stay light, stay responsive.** Native code, hardware video
   decoding, no browser engine; far lower CPU, memory, and energy use than the
   official Electron app, measured against the budgets below, not assumed.
3. **A platform for features.** Transcription, keyword spotting, on-device
   vision, and triggers plug into defined extension points (below) instead of
   being bolted onto the viewer.
4. **Safe for devices and accounts.** Never changes device configuration;
   camera motion control (pointing, focus, a doorbell's speaker) only at the
   user's hand or with the user's per-camera grant ("Device commands"
   below). One login per
   device, logged out on every close; a refused login is never retried
   automatically, so the viewer can never lock an account or crowd out other
   clients.
5. **Private and local.** No cloud account. Video, audio, transcripts, and
   detections stay on the machine; models run on device.
6. **Reliable for days, not minutes.** Runs unattended for weeks: every queue,
   buffer, cache, and log is bounded, and network drops and NVR reboots are
   recovered from without user action. Checked by soak tests against replayed
   real streams.
7. **One core, every platform.** Every decision (protocol, sessions, decoder
   policy, audio timing, layout, extension events) lives in shared Rust; apps
   only decode, draw, play, and present. A fix lands once and behaves the same
   on macOS and Windows.
8. **Testable without hardware.** Everything can be developed against
   `mock-nvr`, replayed recordings, and pixel-checking self-tests; real devices
   are used only to confirm protocol facts.

### Budgets

Measured on every relevant change once M3 exists (Apple silicon, release
build). Targets are set from the first baseline; until then they are
placeholders.

| Measure | Target |
| --- | --- |
| Launch to first live picture (saved credentials, one tile) | TBD at M3 |
| Process memory, 1 tile / 16 tiles | TBD at M3 (includes about 200 MB fixed for the Metal renderer, an accepted cost) |
| CPU per sub-stream tile at steady state | TBD at M3 |
| Energy impact (macOS energy report), 4 and 16 tiles | TBD at M3 |
| Tile to full screen (click to new picture) | TBD at M3 |
| Audio/video offset over 24 h | within ±40 ms |
| Memory growth over a 7-day soak | none beyond startup |

Out of scope for now: recorded-video playback, manual PTZ controls and
two-way audio in the viewer, remote (P2P/relay) access, and device setup.
Camera motion control arrives first for the features under "Device commands". Alerts and triggers come through the
extension points.

## The split

Everything up to "a compressed video frame with a timestamp" is shared Rust.
Decoding, display, and the user interface belong to each platform's app.

```
            shared Rust                                  per platform
 ┌──────────────────────────────────────────────┐   ┌────────────────────────┐
 │ bc-protocol → bc-session → viewer-core → FFI │ → │ decode + render + UI   │
 └──────────────────────────────────────────────┘   └────────────────────────┘
                                                     macOS: Swift, SwiftUI,
                                                       VideoToolbox / Metal
                                                     Windows: C#, WinUI 3,
                                                       Media Foundation / D3D11
```

Decoder and display decisions (gating, recovery, color, layout) are also made
in shared Rust; the platforms only decode and draw. See
[RENDERING.md](RENDERING.md).

## Repository layout

```
crates/
  bc-protocol/   message framing, XOR and AES layers, XML bodies, media
                 deframing; no I/O, tested against synthetic fixtures
  bc-session/    async networking (tokio): connect, login, keepalive,
                 reconnect, one session per device, command allowlist
  discovery/     LAN discovery: the native UDP search and probe and ONVIF
                 WS-Discovery as sans-I/O codecs, plus a small tokio runner
                 that merges the answers into protocol-neutral results
  viewer-log/    bounded, rate-limited file logging (rotation capped by
                 file count and total size)
  viewer-core/   devices, channel subscriptions, stream-tier policy,
                 keyframe gating, backpressure, bounded logging
  video-pipeline/ per-tile decoder decisions (configure, decode, flush,
                 reset), recovery policy, parameter-set parsing, color
                 matrices, grid layout; sans-I/O, shared by every platform
  viewer-ffi/    the C interface (header generated by cbindgen),
                 built as a static library (macOS) and a DLL (Windows)
tools/
  bc-cli/        find devices on the LAN (discover); log in, list
                 channels, write a live channel to a file;
                 capture several tiers on one clock and analyze their
                 timing (docs/STREAM-TIMING.md)
  mock-nvr/      a fake device that serves synthetic sessions or replays
                 recordings (one stream, or several tiers on one shared
                 replay clock), so apps and tests run without a real NVR;
                 over TCP in real time, or in-process on a test's clock
  simtime/       test support: a paused tokio runtime that a test thread
                 steps, for code with a blocking interface
  soak/          accelerated soak runs against a replayed recording
  fixture-gen/   generates fixtures/synthetic/ from made-up credentials
apps/
  macos/         Swift app
  windows/       C# app (later)
fixtures/
  synthetic/     generated, committed; never real captures
docs/
scripts/
```

## Protocol notes

The protocol is undocumented. The details below are confirmed against real
hardware before code depends on them, and the confirmations are recorded in
prose, never as captured bytes (see [SANITIZATION.md](SANITIZATION.md)).

- **Framing.** Each message starts with the magic `f0 de bc 0a`, then
  little-endian command, body length, and message ID. Handshake messages use a
  20-byte header (class `0x1465`); later messages use a 24-byte header with a
  payload offset (classes `0x1464`, `0x0000`).
- **Login.** An empty command 1 returns a nonce, obfuscated with a fixed 8-byte
  XOR table. The session key is the first 16 characters of
  `md5("{nonce}-{password}")`; the login body carries salted MD5 hashes of the
  username and password.
- **Encryption.** After login, control bodies use AES-128-CFB with the fixed IV
  `0123456789abcdef`. Media payloads are length-prefixed and, on the hardware
  tested, unencrypted apart from a short header.
- **Media.** Video and audio arrive as "BCMEDIA" records identified by AVI-style
  four-character codes: an info record, I-frames and P-frames carrying
  H.264/H.265 with 64-bit microsecond timestamps, and audio records (any code
  ending `wb`).
- **Stream types.** `mainStream`, `subStream`, and, on multi-lens cameras,
  `externStream` and `mobileStream`.
- **To confirm before M1:** the live start and stop commands (neolink uses 3
  and 4 with a `<Preview>` body), how several channels share one connection
  (message IDs echoed on media), and the keepalive needed for long sessions.

## Device sessions

NVRs allow only a few concurrent logins, and exhausting them locks out other
clients such as the official app or home-automation integrations. So:

- one TCP session per device, carrying every subscribed channel;
- sessions are closed on every exit path, including errors and app shutdown;
- reconnects back off exponentially and never stack.

### Previews that deliver nothing

A preview can be accepted and still never send a picture (after a
reconnect, behind an NVR, a camera that went quiet). The session never
leaves that state unattended; it works on the same login and never adds
one:

- **Media-silence watchdog.** A started preview with subscribers that has
  delivered no media for `preview_silence_timeout` (15 s: several keyframe
  intervals of a slow sub-stream, short enough to be seen) is stopped and
  started again with a new message ID. Each further silent restart waits
  twice as long, up to `preview_restart_cap` (60 s), and a picture resets it.
  The same covers a stream that stops mid-stream.
- **Refused starts are retried.** A start answered 400 is still waited out
  for `preview_wait_limit` (60 s) with restarts every 6 s. After that the
  subscribers get `StartRejected` (the app keeps saying so), and one more
  start is tried every `preview_slow_retry` (60 s) with a short look each
  time; media clears the refusal. Other refusal statuses, and a wait limit of
  zero, stay final until the connection is reestablished.
- **Escalation.** When every started preview has been restarted
  `preview_escalate_after` (3) times in a row and another is due, the
  connection is failed once, and the supervisor reconnects through its
  normal backoff. At most once per ten minutes, and never while any started
  preview is still getting media (a camera that is simply off cannot make
  the session log in over and over).
- **Diagnostic.** About once a minute, and only when something is wrong, one
  warning lists the tiles (channel and tier) without a picture for over a
  minute and why: not started, waiting on 400, rejected, silent after
  start, or restarting.
- **Manual retry** (`DeviceSession::retry_stream`, `nv_tile_retry`): the
  user's Retry button restarts one stream now, or, when the device is in
  backoff, makes the next connection attempt now. Never faster than
  `MANUAL_RETRY_FLOOR` (2 s) per stream or per early connection attempt, and
  never after the device refused the login (that state stays terminal).

## Device commands

Commands come in three tiers. The session layer's allowlist names the tier of
every command it can send, and the tier decides whether it may be sent.

| Tier | Examples | When it may be sent |
| --- | --- | --- |
| Viewing | login, logout, preview start and stop, connection test, read-only queries (version, channels, streams, PTZ position, presets) | always |
| Camera motion control | PTZ moves and preset recall, lens zoom and focus, a doorbell's speaker (talk-back) | see the rules below: rate-limited, every command logged; the user's own PTZ controls need no enable; talk-back needs the camera's permission; automation needs a per-camera grant |
| Configuration | anything that changes a device's settings, users, network, recording, or firmware; saving or deleting presets | never |

Camera motion control is the one exception to "never change the device",
and these rules hold for every command in the tier:

- **The user's own controls need no enable.** Moving a camera, zooming or
  focusing its lens, and recalling a preset from the app's PTZ controls is
  the user acting by hand, as in the official client; there is no prior
  "enable PTZ" step.
- **Limits sized for runaway software, never felt by hand.** Per camera,
  moves and lens commands at most 20 per second (bursts of 40), preset
  recalls 5 per second, talk-back starts 20 per minute outside a call. A
  person at the controls never reaches them; a loop that sends without
  stopping does. `stop` is never limited.
- **Every command is logged** with its origin (the user's control, a local
  API client, automatic correction) and its reason.
- **Moves always end.** A move is followed by `stop` on every path:
  release, error, timeout, disconnect, tile close, session drop. The core
  also stops a move on its own when the app stops renewing it (a dead-man
  timer) and after a maximum hold.
- **Automation needs a per-camera grant.** Nothing automated (the local
  API, automatic correction such as focus drift or the PTZ watchdog) sends
  a motion command unless the user granted that client or feature for that
  camera, and never while the user is steering the camera.
- **Talk-back keeps its permission.** Sound through a camera's speaker
  needs that camera's "Allow talk-back" option; the first Talk or typed
  reply on a camera without it asks "Allow talk-back for <camera>?"
  (UX.md, "Doorbell calls").
- **Fixed templates.** Every motion body is built by the core from a fixed
  template (never forwarded from a client), and only the forms observed on
  hardware are sent: for PTZ, protocol.md "What nvrview will send". Where
  one command number covers motion and configuration (presets: recall
  versus save and delete), only the motion form is ever built.

Motion control has its own protocol facts (observed from the official app's
PTZ and focus controls, or reported from static analysis) in
`docs/protocol.md` before any code sends it, and a mock-nvr model so it is
tested without hardware.

## Other protocols (later)

Baichuan is the first protocol, not the only one the design allows. Everything
after "a compressed video frame with a timestamp" (video and audio pipelines,
MP4 writing, the renderer, the apps, and most of the C interface) only sees
H.264/H.265, AAC, and timestamps, and would serve any protocol unchanged.

What is tied to Baichuan today: `viewer-core` uses `bc-session`'s sessions and
subscriptions and `bc-protocol`'s stream, channel, and media types directly,
and `recordings` is built on Baichuan's search and download commands.

The plan, when a second protocol starts (not before: an interface extracted
with one implementation behind it takes that implementation's shape):

1. **A device backend interface in `viewer-core`**: connect, list channels,
   subscribe to a stream tier and receive frames, detection and motion
   events, recording search and download, and the motion-control tier. Each
   device reports its capabilities (stream tiers, audio, playback, detection
   classes), as `detects` in the channel info already does. Baichuan becomes
   the first backend with no change in behavior.
2. **RTSP with ONVIF** as the second backend: RTSP for live video from almost
   any IP camera, ONVIF for discovery, events, and PTZ. Check the license of
   any client library before adopting it (MIT or Apache-2.0 compatible).
3. **Vendor HTTP interfaces** later, for what RTSP and ONVIF cover poorly
   (recordings, device AI events).

Every backend follows the same rules as Baichuan: clean-room protocol facts
with provenance in its own protocol notes, its own command allowlist with the
three tiers above (ONVIF can change settings too), one session per device, and
its own mock device for tests. Until then, keep protocol-specific concepts out
of the C interface so apps never depend on which backend serves a device.

## The C interface

A small hand-written C interface, header generated by `cbindgen`. Swift imports
it through a module map; C# uses P/Invoke bindings generated by `csbindgen`.

Two kinds of traffic cross it:

| | Rate | Format |
| --- | --- | --- |
| Control events: connected, login failed, channel list, stream format changed, disconnected (with retry delay) | rare | small JSON strings, so new fields never break the interface |
| Video frames | ~20 fps per channel | a C struct: channel ID, codec, keyframe flag, timestamp (µs), pointer and length of NAL units |

Rules the core guarantees:

- **Threading.** Callbacks arrive on Rust-owned threads, serialized per
  channel, never re-entrant. They must return quickly; apps hand work to their
  own decode queue or UI thread.
- **Memory.** Frame data is borrowed for the duration of the callback only.
  Apps copy it into their own buffer (`CMBlockBuffer`, `IMFMediaBuffer`).
- **Keyframe gating.** After a start, reconnect, or format change, a channel
  receives nothing until a keyframe arrives.
- **Backpressure.** A slow consumer causes drops up to the next keyframe, not a
  growing queue.
- **Frame format.** One callback is one access unit (one picture), delivered
  as Annex-B exactly as the device sends it, so the core never copies or
  rewrites video. Keyframes carry their parameter sets (VPS/SPS/PPS) in band.
  Each platform converts to whatever its decoder needs (the macOS renderer
  rewrites start codes to 4-byte lengths). The keyframe flag must be accurate:
  apps use it for queue policy, though gating trusts the NAL types.
- **Format changes.** A "format changed" event (new codec or parameter sets)
  arrives before the frames that need it; apps flush and gate until the next
  keyframe. Disconnects are signaled so apps can flush.

## Platform apps

The two apps stay at feature parity: a user-visible feature lands on both, or
[PARITY.md](PARITY.md) says why not, and its table (checked against the
self-tests by `scripts/check-parity.py`) is updated in the same change.

**macOS** (minimum macOS 26, Apple Silicon only, Swift 6 strict concurrency):
SwiftUI shell, one `AVSampleBufferDisplayLayer` per visible tile (hardware
decode through VideoToolbox), sub-streams in the grid and the main stream for
the focused channel. The Rust core ships as an XCFramework.

**Windows** (later): C# with WinUI 3, Media Foundation or Direct3D 11 video
decoding into a swap chain. H.265 may require the HEVC Video Extensions; the
fallback is FFmpeg with D3D11VA.
It lives in `apps/windows`: C# bindings for the C interface (blittable
structs checked against the C compiler's layout of the header), a console
smoke test that CI runs on Windows against mock-nvr, the video layer (Media
Foundation decoding into D3D11 textures, a D3D11 grid renderer) with a
headless self-test that checks rendered pixels, and the app: an unpackaged,
self-contained WinUI 3 window whose grid the video layer draws into a
flip-model swap chain, with WASAPI audio and Credential Manager.

Shared across both through Rust: credential storage (the `keyring` crate maps
to Keychain and Windows Credential Manager), logging, settings format.

## Extension points

Features build on events and data the shared core already produces, so they
work the same on every platform and never touch the live-view path's timing.

| Point | What it carries | Where it lives | Planned uses |
| --- | --- | --- | --- |
| Control events | device state, channel lists, tile status and stats (JSON) | core → app (`on_event`) | dashboards, health alerts |
| Audio tap | decoded 16 kHz mono PCM with camera and wall-clock time, per tile, audible or not | core ([AUDIO.md](AUDIO.md)) | transcription, keyword spotting, sound events |
| Frame tap | decoded pictures (on macOS `CVPixelBuffer`s straight from VideoToolbox, no copy), sampled at a configurable rate per tile, with camera time | platform renderer | on-device vision: people, vehicles, packages (Core ML on macOS; ONNX Runtime/DirectML on Windows) |
| Device detections | people, vehicle, animal (and similar) events from cameras and NVRs that run their own AI, pushed live and searchable on the NVR | core, from the session | triggers and search with no extra work on the computer |
| Detections | one stream of timestamped results (labels, boxes, text, confidence) from device detections where a camera has AI, and from on-device models (Core ML) for cameras without it or for what the device does not detect (faces, custom objects) | core | overlays, search, triggers, regardless of where a detection came from |
| Triggers | rules over events and detections ("person on camera 3 after dark", "keyword heard") with actions (notify, focus the tile, mark the timeline, run a shortcut) | core, evaluated once for every platform | alerts without the official app |

Rules for every extension:

- **Bounded and lossy.** Taps feed bounded queues that drop with counters; a
  slow model never delays decoding, drawing, or playback.
- **Off the hot path.** Inference runs on its own threads (or a helper
  process) at its own rate; the frame tap samples, it does not see every
  frame unless asked.
- **Local.** Models are on-device; nothing is uploaded.
- **Timestamped.** Everything carries camera time, so results line up with
  video and audio.
- **Measured.** Each extension states its CPU, memory, and energy cost against
  the budgets above.

### Feature ideas and the points they build on

Not scheduled; recorded so the extension points stay able to carry them.

| Feature | Builds on | Notes |
| --- | --- | --- |
| Timelapse | frame tap (one frame every N seconds per camera), hardware encoder in the app | storage capped by size and age like every cache; needs a small viewer of its own. Could later use the NVR's own recordings through read-only playback commands. |
| MQTT events | control events, detections, triggers (as an action) | publishes to a broker on the local network; implemented once in the core; the bridge to home-automation systems. |
| Private facial recognition | frame tap, on-device face detection and embeddings, detections | off by default and opt-in; enrolled faces stored locally and encrypted; one-step deletion; biometric data is regulated in some jurisdictions even when processed locally. |
| Local API and MCP server ([API.md](API.md)) | control events, detections, transcripts, snapshots, triggers | localhost only, authenticated; exposes viewing data and app actions (such as focusing a tile). Camera motion control only with a separate, per-camera grant to that client, the same limits as the user's own use, and a log of every command; never configuration. Implemented once in the core. |
| Focus drift detection and autofocus | frame tap (a sharpness measure per camera against its own baseline), detections, camera motion control | detection first and on its own: "camera 3 is 40% less sharp than usual", after restarts and over weeks. Correction triggers the camera's own autofocus where the model has it, else a bounded focus search (step, measure, keep the best). Suggest-and-confirm by default, automatic only if enabled for that camera; stops after two failed attempts. |
| PTZ position watchdog | read-only PTZ position queries, frame tap (comparison with a reference snapshot), camera motion control | detects a PTZ camera that drifted or points at the ground (buggy firmware), from its reported position and from the picture. Correction recalls a preset the user saved (such as home), never raw coordinates chosen by us. Same consent and limits as autofocus. |
| Web viewer | the core's local API, live streams, layouts, detections, incidents | the Mac (or the headless service) serves a web app to browsers on the local network: smart TVs, mini-PCs, tablets. Viewers share the server's one login per NVR (devices never see more sessions), and Core ML and the index run on the server. Video is passed through without re-encoding, as fragmented MP4 over a WebSocket to the browser's Media Source Extensions (about half a second of delay); WebRTC later where lower latency matters. Streams a browser cannot decode (H.265 on many TVs) fall back to the sub-stream, or to a hardware H.264 transcode on the server. Local network only, paired and authenticated, TLS; bounded viewers per server. |
| Multi-factor automation | triggers over several signals: facial recognition, expected activity (schedules, past patterns), presence of known devices on the local network | actions fire only when the signals agree (for example an unknown face, outside expected hours, with no household phone at home); every signal and rule stays local; presence detection only observes the local network. |
| Static-scene memory and repeat suppression | detections (boxes with class and camera time), frame tap, the camera's day/night state | cheap filters that run before any model or notification. **Static memory:** per camera and per class, a coarse grid (about 64×36 cells) where a cell rises to 1 when a box of that class covers it and decays toward 0 by wall-clock time otherwise; a box mostly over cells above a threshold is a stationary object (a parked car, a statue, a scarecrow) and is suppressed for alerts but still recorded. Per class, so a parked car never hides a person walking past it; refreshed from periodic baseline frames so the memory follows the scene. **Repeat suppression:** a detection set matching the last alerted one (same classes, same count, boxes overlapping by IoU above about 0.6) does not alert again; a per-camera, per-class cooldown escalates (for example 1 minute, then 5) while activity continues. **Day and night:** thresholds per mode (stricter for vehicles at night, where headlights and IR glare cause false positives), from the camera's reported day/night state, or from a grayscale picture when the state is unknown. All of it is measured on labeled event recordings like the verifier, and its state is per camera, bounded, and local. |
| Notification verifier | triggers, detections, a still from the main stream at the detection time (plus the detector's crop, and a picture about a second later to tell motion from a parked object) | an optional step between the rules and delivery, **off by default** (the default pipeline is a COCO detector such as RT-DETR with the cheap filters, docs/VISION.md; the models measured so far hold about 7–18 GB of memory and take seconds per event): a local multimodal model (through Ollama or similar, in a helper process, never in the core) judges whether an event is worth a notification and writes a one-line caption. Runs once per event, not per frame. It may hold back or annotate a notification, never delete the event: every event stays in the log and timeline; high-severity rules always notify; a timeout or error notifies anyway (fail open). Structured output only (notify, reason, caption). Cheap filters come first (zones and masks, suppression of boxes that never move, cooldowns, one incident across cameras), and the model judges only what is left. Chosen by measurement on labeled event recordings: notifications removed at a fixed rate of missed real events. Two shapes to compare there: one multimodal chat model that decides and writes the caption (simplest; first measurements on a small set favored this), or a hybrid: a decision model that returns a calibrated, repeatable probability (such as "a real person, vehicle, or animal is present") as the gate, with a threshold tuned on the labeled set, and a caption only for events that pass, from a template (camera, time, detected kind) or from a chat model for that one event. The hybrid gives a tunable, deterministic decision and keeps captions off the decision path; it pays off only if the decision model suppresses fewer real events than the chat model at the same false-notification rate, and it costs a second model in memory. Captions describe; they are never evidence: notifications lead with the facts (camera, time, what was detected), since a caption can assert context that is not in the picture. Delivery to a companion app, web push, or a chat service sends text by default; attaching a picture is a separate opt-in, since it leaves the house. |
| Live transcription | audio tap (16 kHz mono per tile), the app's microphone | doorbell calls first (docs/UX.md "Doorbell calls"), keyword search later. Local engines only: Apple SpeechAnalyzer on macOS, Moonshine streaming (MIT, C API) on Windows (until it is measured and integrated, the Windows app uses the desktop recognizer of Windows through System.Speech behind the same interface), chosen by measurement on doorbell-quality speech (word errors, latency to partial text, CPU); Qwen3-ASR (Apache-2.0, the most noise-robust measured but GPU-heavy) is a candidate for an opt-in high-accuracy mode. One engine interface per app: start, feed 16 kHz PCM, partial and final results with times; one session per audio source. |
| Sound events and audio alarms | audio tap (every tile's decoded audio, audible or not), detections, triggers, the recordings index | marks and searches moments by sound, the way video detections do: a smoke alarm's three-beep pattern (T3) or a CO alarm's four beeps (T4), sirens, glass breaking, barking, a baby crying. Alarm patterns first, with a small deterministic detector (a narrow-band tone near 3 kHz in the standard on/off rhythm; no model; tested on synthetic audio; `crates/sound-events`, about 0.01% of one core per camera, see docs/PLAYBACK.md, "Sound"), then a general on-device sound classifier (SoundAnalysis on macOS) for the rest; device detections (such as a camera's own baby-cry detection) are used where a camera has them. Results join the detection stream with camera time, so they label incidents, filter search, and can feed triggers and the notification verifier under the same rules. An aid for finding and labeling events, never a replacement for alarm systems or monitoring: camera microphones are not certified detectors, and a missed sound must never be presented as "no alarm". |
| Doorbell text-to-speech | camera motion control (doorbell speaker), triggers, templates | speaks a message through a doorbell (for example "Leave the package by the door"). Text comes only from user-written templates with a few typed slots (time, camera name); untrusted text (transcripts, detected text, anything a visitor says or shows, other devices' payloads) is never spoken and never instructs an agent. An agent may only pick a template and fill typed slots, with the user's grant; rate-limited and logged. |

**Web viewer: work to do first when it starts.** Both items are done.

1. **Fixed: a resolution change broke browser playback.** Found on
   2026-10-10 with the `crates/mp4/tests/mse` page in Google Chrome on macOS
   (driven by Playwright): `h264-resolution-change` failed with
   `PIPELINE_ERROR_DECODE` (VideoToolbox -12909, bad data) while the
   fixed-resolution samples played. The cause was the test page, not the
   fragmenter: with "pace like a live stream" on, it appended every init
   segment at once and paced only the media segments, so the buffer held the
   last generation's init (480x270 parameter sets) before the first
   320x240 frames arrived, and the decoder failed at about 0.6 s, before the
   first real switch. Appended all at once, in order, the sample played;
   headless Chrome and Playwright's WebKit (Safari's engine, "Media failed
   to decode") failed the same way. The page now releases each init with
   the media segment after it, in stream order, and calls `changeType` only
   when the MIME type changes. A box-level test
   (`generation_switch_is_valid_for_media_source`) confirms what MSE needs
   at a switch: same track ID, timescale and sample entry type, the new
   parameter sets in the new init, increasing sequence numbers, a `tfdt`
   that continues exactly, and a sync sample first. Appending the new init
   without `changeType` also plays in both browsers. The rule for a live
   sender: segments of one stream go to the browser in order, and an init is
   never sent ahead of the media segments before it (see the `mp4-remux`
   crate documentation).
2. **Done: a browser playback check.** `scripts/browser-check.sh` serves
   the MSE samples and plays each, appended at once and paced live, in
   headless Google Chrome (its H.264 and H.265 decoders, VideoToolbox on
   macOS) and Playwright's WebKit, reporting pass or fail per sample; a
   resolution-change sample must also decode more than one picture size.
   Headless Chrome uses the same hardware decoder and reproduced the
   original failure, so no window opens. Optional in `scripts/check.sh`
   (`NVRVIEW_BROWSER_TESTS=1`), since it needs playwright-cli and browsers.
   Real Safari is not covered (Playwright drives WebKit, not Safari itself).

## Mobile (later)

Not planned; recorded so the core and the local API keep it cheap.

- **iOS**: the core builds as one more slice of the XCFramework, and
  VideoKit's VideoToolbox, Metal, and AVAudioEngine code runs on iOS almost
  unchanged; the new work is a touch interface. iOS suspends background apps,
  so live sessions last only while the app is open.
- **Android**: the core builds as a native library reached through JNI
  (bindings could be generated, for example with UniFFI); the platform layer
  is new, as for Windows: MediaCodec decoding, SurfaceView or OpenGL ES/Vulkan
  drawing, AAudio playback, and a Kotlin interface.
- **Companion app (the likely first step)**: a phone app that talks only to
  the local API of a headless nvrview service at home (the core, index,
  triggers, and transcription running on a home server or a Mac). The phone
  then never uses one of a device's scarce logins, background limits matter
  less, and notifications come from the service's own triggers. The local API
  should be designed with such a client in mind: authenticated, and centered
  on incidents and clips.
- **Away from home**: local-network only by design; reach the home network
  through a VPN (WireGuard or similar) rather than a vendor relay.

## Build and CI

- `cargo build` targets `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, and
  `aarch64-pc-windows-msvc`.
- GitHub Actions runs the sanitization check and gitleaks on every push, and
  `cargo test` and `clippy` on Linux and Windows, so the core stays portable
  before a Windows app exists. The macOS app is built locally (or on a
  self-hosted runner) since hosted macOS images can lag the current OS.

### Tests do not wait on real time

Timeouts, retries, backoff, keepalives, frame pacing, probe windows, and
audio timing are tested on a simulated clock, so tests run as fast as the
CPU allows and give the same result on a busy machine:

- The device and the code under test share one clock. `mock_nvr::spawn_in`
  runs the mock on the test's tokio runtime, reached through in-memory
  pipes (`mock_nvr::connect`, and `bc_session::FnConnector` on the client
  side), so its pacing, replay schedules, download rates, and delays use
  that runtime's timers.
- Async tests (`bc-session`, `recordings`, `bc-cli`) use tokio's paused
  clock (`#[tokio::test(start_paused = true)]`): it moves only when every
  task waits for a timer, straight to the next one.
- Code with a blocking interface (`viewer-core`, the C interface, `soak`)
  runs on a `simtime::SimRuntime` through `CoreConfig::test_runtime`: the
  core's tasks, tiles included, run on that runtime, and its host clock
  reads the simulated one. The test holds the clock still while it calls
  and checks, and moves it with `advance` (every timer in between fires in
  order). Work on tokio's blocking threads (`spawn_blocking`, such as chunk
  writes) holds a paused clock still while it runs, so disk work is never
  overtaken by a timeout; keep such tasks short-lived.
- Production builds do not change: without `test_runtime` the core reads
  the host clock (one branch) and keeps its own runtime and tile threads.

Some tests stay in real time on purpose: the core's threading (answers from
other threads, a slow `on_action`, calls from callbacks), the C interface
end to end on the production configuration, the TCP server's behavior with
a full socket buffer, the log writer's thread, the host clock itself, and
the C and C# smoke tests and Linux Docker run. A real-load stress test of
live view during downloads is opt-in (`cargo test -p recordings --test e2e
-- --ignored`).

## Milestones

| | Goal | Proves |
| --- | --- | --- |
| M0 | Confirm live start/stop and keepalive against real hardware | the protocol |
| M1 | `bc-protocol`, `bc-session`, `bc-cli` writing a channel to a playable file; CI green on Linux and Windows | the core, portably |
| M2 | C interface plus one live channel in the macOS app | the boundary |
| M2a | Listening on one tile: AAC decode, timestamping, drift-corrected playback, audio tap | audio that stays in sync |
| M3 | Split-pane layouts, several windows (any camera in any window, the same camera in more than one), listening from any tile, seamless full screen and focus ([UX.md](UX.md)), reconnect, Keychain, an app bundle | a usable macOS app |
| P | Playback by incident ([PLAYBACK.md](PLAYBACK.md); breakdown in [MILESTONE-P.md](MILESTONE-P.md)): search by event type, every angle of an event at once, background resumable downloads, readable output, follow cuts and 2-up/4-up edits | better than the official app's slow, error-prone downloads |
| M4 | Windows app, one channel, then the grid | Windows |

## Lessons from the official app

An analysis of the official desktop app's native SDK found two bugs this design
avoids on purpose:

- When the hardware decoder rejected a frame, the SDK rebuilt the decoder and
  retried every frame, logging six lines each time (about 9,000 lines per
  second).
- Its log rotation renamed files with an ever-increasing index and never
  deleted any, so that loop could fill tens of gigabytes in hours.

Hence keyframe gating, rate-limited error logging, and log rotation capped by
both file count and total size.
