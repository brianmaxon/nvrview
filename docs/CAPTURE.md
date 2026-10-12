# Capture: pictures, recordings, and camera control from the tile

> **Design, not implemented.** This is the contract for the tile header
> redesign, the call window as a regular tile, saving pictures, recording the
> live stream and doorbell calls, and pan, tilt, zoom (PTZ) from the tile.
> The user-facing rules are in [UX.md](UX.md) ("Tile header", "Save
> picture", "Recording", "Recording calls", "Pan, tilt, and zoom"); the
> C interface is in [FFI.md](FFI.md) ("Capture", minor 9, and the proposed
> "Camera motion (PTZ)"); audio details in [AUDIO.md](AUDIO.md) ("Recording calls"); the
> difference from the NVR's own recordings in [PLAYBACK.md](PLAYBACK.md)
> ("Local captures and the NVR's recordings"); the PTZ commands in
> [protocol.md](protocol.md) ("What nvrview will send"). Parity status is in
> [PARITY.md](PARITY.md). Built so far: the header model on both apps, and on
> macOS and Windows the header, its menus and keys (PTZ actions say "coming
> soon"), Save picture, and the call window's picture as a single fixed tile;
> in the core, the pure base (`crates/capture`, K2) and the recorders behind
> a Rust API (K4, below), and the C interface with its Swift and C# bindings
> (F5, ABI 3 minor 9, below); live and call recording with the app's AAC
> encoder on macOS (M5, below) and on Windows (W5, below).

## Implementation status (K2)

`crates/capture` is the pure base the recorder will be built on: no
threads, clocks, devices, or file-system access of its own (callers pass
timestamps, free bytes, sizes, and a predicate for names in use). It is not
wired into `viewer-core` or the C interface yet. Each module's documentation
lists its rules; the choices made where this page is silent are:

- **Epoch timeline** (`epoch`): the file clock is microseconds, 0 at the
  first video frame seen; a segment's own 0 is its keyframe. A camera
  timestamp step beyond +5 s or under -1 s starts an epoch (smaller back
  steps are reordering); timestamps wrap at 2^32 microseconds unless told
  otherwise. A new epoch starts at the previous frame's file time plus the
  host-measured gap (at least 1 ms). Camera audio further than 10 s from the
  latest video is dropped and counted (`audio_unplaced`). Our sent audio is
  mapped by the median of host arrival minus file time over the last 64
  frames. Audio chunks within 20 ms of contiguous are snapped, later ones
  leave counted silence, overlap is trimmed.
- **Segments and gaps** (`policy`): a gap is a video step over 2 s; up to
  60 s the file continues and the gap is counted, over that (or a format
  change, or 60 s with no frame) the segment ends and the next keyframe
  opens `(part N)`. No keyframe within 30 s of waiting at the start is
  `no_video`. The duration cap runs from the first segment's keyframe (gaps
  included) and the size cap counts every segment's bytes; both end the
  recording before the frame that would pass them is written.
- **Mixer** (`mixer`): Conversation, Visitor and You are always all
  produced ("Both"); the mixer emits frames 250 ms behind the recording's
  progress, drops (and counts) audio that arrives later or more than 10 s
  ahead, and the limiter is a memoryless soft knee (identity to 0.8,
  ceiling 0.98). One mixer per segment. `EncodeQueue` is the 10 s bound
  for the app's encoder.
- **Caps and disk** (`caps`): settings clamp to 1 minute to 12 hours and
  100 MB to 64 GB (decimal units); finalizing needs the file size plus 2%
  plus 8 MiB free, else the fragmented file is published.
- **Partials** (`partials`): at most 32 entries, valid only as
  `<stem>.partial.mp4` with target `<stem>.mp4`; recovery deletes a partial
  under 4 KiB or whose target already exists, and recovers the rest.
  `recordings::store` (atomic JSON writes) is shared, not copied.
- **Names** (`naming`): built on `recordings::naming` and the still-image
  name; a taken live name is numbered against `<stem>.mp4` and
  `<stem>.partial.mp4`; a call stem follows the transcript's rule.

Not in the crate: the muxer driver, threads, the free-space shim, the
event JSON, and the C interface (K4). The picture-name vectors,
`fixtures/synthetic/capture-names.json`, are generated from these rules by
`tests/name_vectors.rs` (which fails when the file is stale;
`NVRVIEW_UPDATE_VECTORS=1` rewrites it) and read by the apps' tests.

## Implementation status (K4)

`viewer-core` owns the recordings (`src/capture_owner`), behind a Rust API
on `Core` that the C interface (F5) will wrap one to one:
`capture_configure`, `capture_set_encoder` (an `AudioEncoder` trait with
`open`, `encode`, `close`, the `NvAudioEncoder` of FFI.md),
`record_audio_encoded`, `record_start`, `record_stop`, `record_status`,
and `capture_set_free_space` (a `FreeSpace` trait; the default,
`SystemFreeSpace`, asks `statvfs` or `GetDiskFreeSpaceExW`). Errors are a
`CaptureError` (`Limit` carries its `LimitKind`; `limit_answer` writes
`{"limit": ...}`). Events are the `recording` objects of FFI.md. The choices
made where the contract is silent:

- **Threads.** One OS thread per recorder (a task on the runtime in tests
  on the simulated clock). The loop runs on the core's runtime; once it
  ends, the subscription and the talk observer are released and the
  finalize runs without the runtime, so it can finish during shutdown.
  Shutdown, dropping the core, and removing a device stop their
  recordings (`core_shutdown`, `device_removed`) before tiles and
  sessions close, and wait up to 5 s (`FINALIZE_WAIT`); a recorder still
  finalizing then finishes on its own thread, or is recovered at the next
  launch. A session that ends on its own (a refused login) ends its
  recordings with `login_failed`.
- **Audio probe.** Tracks are declared before the first frame, so the
  recorder holds the stream from the first keyframe until the first
  camera audio record (its codec and AAC format decide), or 1.5 s of
  video, 3 s of host time, or 8 MiB (no audio). Later parts reuse the
  decision. Audio records are stamped with the playout's own method
  (`audio_pipeline::timestamp`) and placed with the epoch timeline.
- **Tracks.** Live: the camera's AAC passed through; other codecs decoded
  and sent to the encoder; without an encoder, or when it refuses the
  track, video only (`audio` `unsupported`, `tracks` `["video"]`). Call:
  Conversation and You always encoded; Visitor passed through when the
  camera sends AAC, else encoded from the decoded audio (silent when the
  camera sends none); `audio` reports the Visitor track; a call whose
  encoder refuses a track fails with `encoder_failed`.
- **Encoder bridge.** `track` is the audio track's index in the file
  (live 0; call 0 Conversation, 1 Visitor, 2 You). Each part opens its
  encoded tracks when it starts and closes them with flush when it ends;
  a recording keeps the encoder it started with. The `k`th unit returned
  for a track is placed at the `k`th 1024-sample block's time (the
  priming rule of `mp4-remux`). At most 10 s of frames are with the
  encoder unanswered and 10 s more wait in the queue (then silence); the
  units waiting to be placed are capped at 10 s of three tracks (then
  dropped). `encode_dropped_ms` is the worst track's plus those drops.
- **Calls.** A call recording ends when its tile's conversation closes
  (`set_talk_conversation` false) or its tile closes, with `stopped`. One
  call recording per camera at a time (the talk observer is per channel);
  another is refused as the `calls` limit. `file` must be an absolute
  `.mp4` path; when it (or its partial file) exists, the next free
  `(part N)` of its stem is used, so starting again in one call writes
  part 2.
- **Live names.** `name` defaults to `Camera <channel + 1>`; the start
  request may carry `utc_offset_s` (the app's local offset, default 0),
  since the name uses local time. The tier follows the tile's lens: the
  main or sub stream of the first lens, or the `extern` or `mobile`
  stream of the second.
- **Counts.** A recording that is finalizing no longer counts against the
  limits. Free space that cannot be read counts as plenty (logged once).
- **Events.** `waiting_keyframe` at the start, `recording` at the first
  keyframe, `segment` when a later part opens, about once a second while
  nothing changes (as diagnostics, which a full event queue drops first),
  then `finalizing` and `finished` or `failed`. `segment` is at least 1,
  `bytes` counts every part, and two fields are added: `dropped_frames`
  (frames the subscription's queue dropped plus frames out of order) and,
  on the last event, `fragmented` (the fragmented file was published
  because finalizing had no room or failed). `record_status` lists the
  recordings that have not ended; `record_stop` returns at once.
- **Crash safety.** The registry lives at `data_dir/capture/partials.json`
  (in memory only without a data directory: no recovery). Only the first
  `capture_configure` recovers, before it returns; a partial file that
  cannot be finalized is left as it is and no longer listed. A part whose
  parameter sets change in a way the format hash missed (the muxer
  refuses the frame) also starts the next part.

Tests: `crates/viewer-core/tests/capture.rs`, against `tools/mock-nvr` on
the simulated clock (one on threads in real time): a live tile to a
finalized MP4 (tracks, durations, keyframe start), a call with both sides
and You placed within 40 ms of its send time, a 3 s outage as a gap and a
silent minute as part 2, a resolution change as part 2, the disk guard at
start and while recording, the count limits, crash recovery (a cut
partial recovered, a tiny one deleted, an unlisted one untouched), and
shutdown or dropping the core mid-recording.

## Implementation status (F5)

The C interface wraps K4's API one to one as ABI 3 minor 9 (FFI.md,
"Capture", which lists the clarifications): `nv_capture_configure`,
`nv_capture_set_encoder` (an `NvAudioEncoder` of three C functions adapted
to `AudioEncoder`; `open` fills an `NV_ASC_MAX`-byte buffer with the
AudioSpecificConfig), `nv_record_audio_encoded` (allowed inside the
encoder's own functions), `nv_record_start` (`NV_ERR_LIMIT` with the
`{"limit": ...}` answer, `NV_ERR_NOT_CONFIGURED`; an answer that does not
fit stops the recording again), `nv_record_stop`, and `nv_record_status`.
Free space has no C hook; apps pass `utc_offset_s` and call configure
first.

Bindings: Swift `CoreAPI` (`captureConfigure`, `captureSetEncoder` with a
`CaptureEncoder`, `recordAudioEncoded`, `recordStart` with a
`RecordRequest` and a `RecordStartError` naming the limit, `recordStop`,
`recordStatus`, the `recording` event as `RecordingEvent`) in `CCore` and
`FakeCore`; C# `ICapture` and `ICaptureEncoder` on `NvCore` (a limit's
kind in `NvException.Detail`), `RecordingEvent` and `FakeCapture` in
NvrView.Shell. Both bindings keep every encoder they install until the
core is freed, and route `recordAudioEncoded` through a handle that stays
valid until `nv_core_free` returns.

Tests: `crates/viewer-ffi/tests/ffi_capture.rs` (through the C functions,
against the mock on the simulated clock: a live tile to a finalized MP4, a
call through C encoder functions answering from inside them, the limit
answers, a too-small answer buffer, the status codes); the C smoke test
(also under AddressSanitizer) and the C# smoke test record the live tile
and a call with a fake encoder to finalized MP4 files; the Swift tests
cover the fake and, with the real core, the same against the mock; the C#
shell tests cover FakeCapture.

What M5 and W5 need: a platform AAC-LC encoder behind `CaptureEncoder` /
`ICaptureEncoder` (48 kHz mono at the requested bit rate, its
AudioSpecificConfig and priming from `open`, each encoded frame answered
in order with `recordAudioEncoded`, the remaining output on a flushing
`close`), `captureConfigure` at startup and on a settings change, and the
UI that starts and stops recordings and shows the `recording` events.

## Implementation status (M5)

The macOS app records (UX.md "Recording", "Recording calls" have what the
user sees). The pieces:

- **Encoder.** `AACCaptureEncoder` (NVRCore) behind `CaptureEncoder`:
  one `AVAudioConverter` (AudioToolbox's AudioConverter) per open track,
  48 kHz mono float in, AAC-LC out at the nearest supported rate to the
  one asked (96 kbps). `open` writes the two-byte AudioSpecificConfig from
  the format (`0x11 0x88` at 48 kHz mono) and reports the converter's
  `primeInfo.leadingFrames` as the priming (2112, measured: an impulse
  comes back 2114 samples late, within the 48-sample tolerance). Each
  frame is encoded on the recorder's thread that hands it over (well under
  a millisecond), so the units go back with `recordAudioEncoded` in order
  and no queue of its own is needed; the core bounds what waits for it.
  Units are answered after the encoder's lock is released; at most 32
  tracks are open at once. `close` with `flush` drains the converter. The
  encoder holds the core it answers through; the core releases the encoder
  when it is freed.
- **App.** `CaptureController` (NVRView): installs the encoder and sends
  `captureConfigure` when the app starts (after the self-test or the user's
  settings are read, so the first configuration is the right one) and on
  every change of the recordings folder, "Record from", the caps, or
  "Share-safe names". Live starts pass the camera's name and the Mac's
  current UTC offset. `RecordingBoard` (NVRCore, pure) keeps the running
  recordings from the app's own starts and the events; ended ones are
  logged to the event log and announced (`RecordingText`).
- **Calls.** The call's recording and its transcript share a stem
  (`CallTranscriptFile.sharedStem`: the transcript's name, numbered until
  neither `.txt`, `.mp4` nor `.partial.mp4` is taken), reserved when the
  recording starts and reused by the transcript when the call ends. The
  recording ends with the call (the core sees the conversation close).
  "Record calls" starts it as the call's tile opens. Call recordings in
  the Transcripts folder are pruned to the newest 100 and 10 GB after each
  one ends (`TranscriptStore.pruneRecordings`; partial files are never
  touched).
- **Not built:** the per-doorbell Default, Always, Never for "Record
  calls"; the self-test steps `capture-record-cap`,
  `capture-record-reconnect`, `capture-record-survives-tile-close` and
  `capture-call-record-auto` (the core's tests cover caps, gaps and
  lifetime).

Tests: `AACCaptureEncoderTests` (a 1 kHz tone through the encoder, its
units decoded by the system's AAC decoder as ADTS: the onset at the
reported priming, the level and the pitch; the impulse priming; the track
bound), `CaptureRecordingsTests` (the settings JSON with its clamps, the
pills, every limit's and end's message, the board, the pill never
overflowing the header, the shared stem). Self-test on the fake core:
`capture-encoder-priming`, `capture-record-tile` (R, then the header's
Stop), `capture-record-indicator` (the pill stays when the header fades
and at 100 pt, the toolbar item, the Recordings submenu),
`capture-record-disk-low`, `capture-record-main-limit` (the sub-stream
offered and taken), `capture-call-record-button` (⇧⌘R in a call window:
beside the transcripts under its stem, the call bar pill, the four tracks
through the AAC encoder). With `scripts/macos-real-selftest.sh` (the mock
now sends AAC audio): `capture-record-real-core` (a live tile for about
3.5 s: one video and one AAC track, the duration within 0.5 s of the
events', a keyframe first, the AAC decoding) and
`capture-call-record-real-core` (a conversation's recording ended by the
call: video and three audio tracks, only Conversation enabled, each
decoding).

## Implementation status (W5)

The Windows app records the same way (UX.md "Recording", "Recording calls").
What differs from M5:

- **Encoder.** `AacCaptureEncoder` (NvrView.Video) behind `ICaptureEncoder`:
  one AAC encoder MFT (`CLSID_AACMFTEncoder`, synchronous) per open track,
  16-bit mono PCM at the track's rate in, raw AAC-LC units out at the
  nearest of the bit rates the MFT offers to the one asked (96 kbps mono is
  12000 bytes per second). The AudioSpecificConfig is the one in the output
  type's user data (after the 12 bytes of the HEAACWAVEINFO extension), else
  built from the format. The priming is measured once per process
  (`AacRoundTrip.Priming`): an impulse at sample 4096 goes through the
  encoder and the system's AAC decoder (`CLSID_CMSAACDecMFT`) and the
  position it comes back at, less 4096, is reported, so it is the delay a
  player sees (the macOS encoder reports its converter's prime info); 1024
  if it cannot be measured. Each frame is encoded on the recorder's thread
  that hands it over and its units are answered at once, in order, through
  `ICapture.RecordAudioEncoded`; a fresh input sample is made per frame
  (the MFT may keep its input); a flushing `Close` ends the stream and
  drains the MFT. At most 32 tracks are open.
- **App.** `CaptureController` (NvrView.App) installs the encoder and sends
  `CaptureConfigure` when the app starts (a test run or a given data
  directory records into its own `Recordings` folder) and on every change of
  the recordings folder, "Record from", the limits or "Share-safe names"
  (settings kept in `view-settings.json`); live starts pass the camera's name
  and the computer's UTC offset. `RecordingBoard`, `CaptureSettings` and
  `RecordingText` (NvrView.Shell, with the macOS test vectors) are the pure
  side. The window header lists the recordings; messages are toasts over
  the stage with an optional button.
- **Calls.** As on macOS: the recording and the transcript share a stem
  (`CallRecordingFiles.SharedStem`, reserved when the recording starts),
  "Record calls" starts the recording once the doorbell's tile is open, the
  call ending ends it, and old call recordings in the Transcripts folder are
  pruned to the newest 100 and 10 GB after each one ends
  (`TranscriptStore.PruneRecordings`).
- **Not built:** as on macOS, the per-doorbell Default, Always, Never and the
  steps `capture-record-cap`, `capture-record-reconnect`,
  `capture-record-survives-tile-close` and `capture-call-record-auto`.

Tests: the pure pieces in `NvrView.Shell.Tests` (`CaptureRecordingsTests`:
the settings JSON and its clamps, the pills, every limit's and end's
message, the board, the shared stem and the pruning, `ToneCheck`, and
`Mp4Inspector` on a built MP4; `Mp4Inspector` also reads the files the core
writes in the C# smoke test), the pill's corner in `NvrView.Tiles.Tests`.
Self-test on the real core against the mock (`SelfTest.Capture.cs`, the macOS
IDs): `capture-encoder-priming` (the priming reported and measured, a 1 kHz
tone through the encoder and the system's decoder: pitch and level),
`capture-record-tile`, `capture-record-indicator`, `capture-record-disk-low`,
`capture-record-main-limit`, `capture-record-real-core` (the file read
back), `capture-call-record-button` (Ctrl+Shift+R in a call window) and
`capture-call-record-real-core` (the call's four tracks, only Conversation
enabled, ended with the call).

## Principles

1. **One tile, one header, everywhere.** The grid's tiles, the call window
   and (later) playback tiles share one header model, one context menu built
   from it, and one set of keys. Both apps compute the header from the same
   pure model and the same test vectors.
2. **The core writes the files.** Muxing, timing, segmenting, crash safety,
   limits, and disk checks are shared Rust. The apps only render pictures
   (they own the decoded frames and the shaders) and encode PCM to AAC with
   the platform's encoder. Neither app writes a recording itself, so the
   files are the same on both.
3. **Bounded and crash-safe.** Every queue and buffer has a cap; a recording
   is always a playable file on disk, even after a crash or a full disk.
4. **Nothing changes on the device.** A recording is one more subscriber to a
   stream the session already carries (or a second tier on the same
   session), never a new login and never a device setting. PTZ uses the
   camera motion control tier (ARCHITECTURE.md, "Device commands").
5. **Local captures are not the NVR's recordings.** They are what this
   computer received, gaps included (PLAYBACK.md).

## The tile header

### The call window is a regular tile

Today each app draws the picture of a call window with code of its own. It
becomes the same stage view the viewer windows use, in a **single fixed tile**
mode: one leaf, no layout editing, split, swap, focus, Show Camera, Remove or
Clear; the window holds its own lease; the selection is always that tile. So
digital zoom, the zoom map, fisheye views, Quality, Save picture and Record
work there exactly as in the grid. The call window, top to bottom:

1. the **call bar**: camera name, call kind (a press or a conversation),
   elapsed time, the recording pill ("● Recording 0:42") while the call is
   recorded, the pin, Pop In, and close;
2. **the tile**, with its hover header (Save picture, Record, Speaker or
   Unmute, PTZ, Quality, "…") and the Unmute cover drawn as a veil inside
   the tile;
3. the captions;
4. the Talk row (hold to talk, Keep talking, the echo status);
5. the reply field and Send.

In a call window **Record** is the call recording ("Recording calls"
below), not a live recording. Insets and the Windows call cards keep their
own drawing for now, but their headers get the same buttons from the shared
model.

### The header model

`TileHeaderLayout` is a pure function in NVRCore (Swift) and NvrView.Tiles
(C#), with shared vectors in `fixtures/synthetic/tile-header.json`:

- **Input:** the tile's width (points or DIPs), the items available for that
  tile (from the camera's capabilities and the tile's state), and item
  widths (an icon button 26, the title at least 48, the recording pill 64).
- **Output:** the visible items in order, the overflow list, and whether the
  title is truncated.
- **Rule:** items are placed greedily by priority (UX.md, "Tile header").
  Once anything is hidden, room for "…" is reserved first. Indicators (the
  recording pill, an alert's strip and Dismiss) never overflow. A width
  change of less than 8 pt from the one that last changed the result keeps
  that result (hysteresis), so the header does not flicker while a divider
  is dragged. Below 120 pt only the pill, the title and "…" show.
- **Veils:** under a no-picture veil ("Connecting", "Reconnecting") the
  header shows only the title and "…"; Retry stays in the veil.
- **Right-click menu** = the full overflow list from the same model, so
  everything in the header is also in the menu, in the same order.

Details settled with the model (`TileHeader.swift`, `TileHeader.cs`; the
macOS tile chrome lays its header out with it, UX.md "Tile header"):

- The padding (8) is the header's inset at each end; items sit side by side.
  The title gets what the buttons leave, at least 48; `titleTruncated`
  compares that with the title's natural width, which the app measures.
- Display order: the pill, the title, Dismiss, the buttons by priority, then
  "…". Items have stable identifiers (`save-picture`, `pan-tilt`, ...), the
  English labels of the menus, and key hints for each platform.
- The "…" menu is the right-click menu without what the header shows, so it
  also holds the long tail and, below 120 pt, Dismiss. Start Conversation is
  in the menus for every camera and in the header only for a doorbell or a
  camera with talk-back. In the call window's tile Record and Save Picture
  carry ⇧⌘R / Ctrl+Shift+R and ⇧⌘P / Ctrl+Shift+P, and Record Main Stream
  and Record Sub-stream are not offered.
- Hysteresis keeps the visible items (and the anchor width) for the same
  tile state; the title's truncation still follows the width. A change of
  the tile's state (zoom, recording, a veil) lays it out again at once.

Corner indicators are unchanged.

## Save picture

What the tile shows, saved as an image file.

- **What is saved.** By default the **full frame** of the stream the tile
  shows, at its decoded size (the menu item names the pixel size), whatever
  the digital zoom. "Save Zoomed Area" (offered while zoomed) crops the
  decoded picture to the visible region, at source resolution, never
  upscaled. A fisheye tile saves what it shows (dewarped at the source's
  resolution), and "Save Original Fisheye Picture" saves the circle as
  received. The tier is the one shown; saving never switches streams.
- **Content.** No metadata beyond what the format requires. An optional
  burned-in label (time and camera name) is off by default.
- **Rendering.** One offscreen render path per app reusing the renderer's
  shaders and color matrix, so the file matches the screen. macOS: Metal
  into a texture, then `CGImage` and ImageIO on a background queue. Windows:
  D3D11 render to texture under `GpuDevice.Lock`, a staging copy, then WIC.
- **Feedback.** A short shutter flash on the tile and a toast "Picture
  saved" with Show in Finder (macOS) or Show in Explorer (Windows). Errors
  name the file name only, never a full path.
- **Where.** `~/Pictures/NVRView` on macOS; `Pictures\NvrView` on Windows
  (the known folder `FOLDERID_Pictures`). Settings: "Save pictures to",
  Format (PNG, or JPEG at quality 0.9), and "Share-safe names" (shared with
  live recordings).
- **Names.** `YYYY-MM-DD HH.MM.SS.mmm <camera>.png` or `.jpg`, the
  still-image rule of FFI-RECORDINGS.md section 7a: local time, camera name
  sanitized, at most 120 characters, `Camera A` when share-safe, ` (2)` and
  so on when taken. The rule is implemented in NVRCore and NvrView.Tiles
  with vectors in `fixtures/synthetic/capture-names.json`, generated by the
  Rust rules (`capture` crate, `tests/name_vectors.rs`) and read by the
  Swift (`PictureNamingTests`) and C# tests.
- **Later:** playback tiles save their picture through `nv_still_plan`
  (FFI-RECORDINGS.md, section 7a (c)).

## Live recording

Recording a camera's live stream to an MP4 on this computer.

### Encoding

The core muxes everything. Apps only encode PCM to AAC-LC, 48 kHz mono at
96 kbps, when a track needs encoding:

- macOS: `AVAudioConverter`; the encoder delay (priming) comes from
  `kAudioConverterPrimeInfo` (about 2112 frames).
- Windows: the Media Foundation AAC encoder MFT; its priming is measured by
  a self-test that encodes an impulse. The encoder's documented constraints
  (44.1 or 48 kHz; 96, 128, 160 or 192 kbps) are confirmed by that test.

The priming goes into the MP4 as an edit list, so players start at the
first real sample. Rejected: Opus (a C build to ship, and poor MP4 playback
in the systems' own players), LPCM (compatibility), and apps writing whole
files (two implementations to keep equal).

### Tracks

- **Live recording:** the video, remuxed as received (no re-encode), and the
  camera's AAC audio passed through, placed on the video's time line by the
  audio pipeline's timeline. Audio in other codecs is decoded and encoded
  through the app's encoder; without an encoder the file is video only and
  says why (`audio` `unsupported`).
- **Call recording:** see "Recording calls" below.
- Handler names are fixed generic words ("VideoHandler", "SoundHandler", and
  the call track names), allowed by `export::mp4_metadata`.

### Timing

- A recording starts at the **next keyframe** (state `waiting_keyframe`);
  audio before it is dropped, and file time 0 is that keyframe. There is no
  pre-roll in this version (a picture cache for ring-triggered call
  recordings may come later).
- Each recorder keeps **epochs** mapping camera timestamps onto one
  continuous file clock. A discontinuity, a reconnect, or a timestamp jump
  starts a new epoch, offset by the gap measured on the host clock.

### Stream

The recorder has its **own subscription at a fixed tier**, so switching the
tile's quality, focusing, or zooming never touches a recording. Which tier:

- Settings: **"Record from"**, Main stream (default) or Sub-stream.
- Per recording: "Record Main Stream" and "Record Sub-stream" in the Record
  menu and the overflow; R and the Record button use the setting.
- Call recordings use the setting.

### Reconnects and segments

- After a reconnect with the same parameter sets and a gap of at most 60 s,
  the file continues: the last picture holds through the gap, and the gap
  is counted (`gaps`).
- Changed parameter sets (a new resolution or codec) or a gap over 60 s end
  the segment and start `<name> (part 2).mp4` (state `segment`); the
  recording continues under the same ID.

### Crash safety

- While recording, the core writes `<stem>.partial.mp4` in the destination:
  a fragmented MP4 with 2 s fragments, each flushed and synced.
- On stop it finalizes to a temporary file, renames it to `<stem>.mp4`, and
  deletes the partial file (the same crash-safe join as downloads).
- A registry, `data_dir/capture/partials.json`, lists the partial files in
  progress. After a crash, `nv_capture_configure` finalizes them (event
  `recovered`). Only files listed there are ever touched.

### Bounds

- **Memory:** the session's bounded queue; per recorder at most one 2 s
  fragment (also capped in bytes) and 1 MiB of audio; at most 10 s of PCM
  per encoded track (overflow becomes silence and is counted as
  `encode_dropped_ms`); the finalize index (about 1.3 MB per hour).
- **Disk:** free space is read through a shim (`statvfs` on Unix,
  `GetDiskFreeSpaceExW` on Windows), injectable for tests. A recording does
  not start with under 2 GB free (`NV_ERR_LIMIT`, `disk_low`). Free space is
  checked every 5 s and at each fragment; the recording stops when free
  space falls under 1 GB plus the current file's size. If even finalizing
  cannot fit, the fragmented file is published as the `.mp4` (it plays),
  logged, and the event says so.
- **Length:** a live recording stops at 60 minutes or 4 GB, whichever comes
  first (no roll-over). Settings allow 1 minute to 12 hours and 100 MB to
  64 GB. A call recording has a safety cap of 2 hours or 2 GB.
- **Count:** per core at most 4 live recordings and 2 call recordings, and
  at most 2 main-stream recorders per device. One OS thread per recorder.

### Lifetime

A recording survives its tile closing. It ends on stop, a cap, low disk,
the device's removal, a refused login, or the core being freed. At quit the
app stops every recording and waits up to 5 s; anything left is recovered at
the next launch.

### Files

- Live recordings go to `~/Movies/NVRView` on macOS and `Videos\NvrView` on
  Windows (setting: "Save recordings to").
- Name: `YYYY-MM-DD HH.MM.SS <camera>.mp4` (local start time), sanitized as
  for pictures, `Camera A` with share-safe names, ` (2)` when taken, and
  ` (part 2)` for later segments.

## Recording calls

A doorbell call (a press or Start Conversation) can be recorded with both
sides of the conversation.

### Opting in

- Settings > Alerts > Doorbell calls: **"Record calls"**, off by default,
  and per doorbell Default, Always, or Never.
- Turning it on for the first time shows a consent note: "Recording a
  conversation may need the other person's consent where you live. The
  visitor is not told that the call is recorded." There is no automatic
  announcement, since that would be automatic talk-back.
- With recording on for a camera, it starts when the call opens. The call's
  Record button starts and stops it at any time either way; starting again
  in the same call writes `(part 2)`.

### Tracks

| Track | Content | Flags |
| --- | --- | --- |
| video | the doorbell's video, remuxed | enabled |
| "Conversation" | the visitor and the user mixed | enabled, alternate group 1, first audio track |
| "Visitor" | the doorbell's audio (AAC passed through, else encoded) | disabled, alternate group 1 |
| "You" | what was sent through the doorbell's speaker | disabled, alternate group 1 |

Players play Conversation; the other two are there to listen to one side.

### The user's side

"You" is exactly what the doorbell received: the 16 kHz PCM that the talk
sender encodes to ADPCM, record by record including the silent tail, each
stamped with the instant it was sent on the core's clock. A bounded
observer on the talk sender, keyed by device and channel, drops its oldest
records rather than slow talk-back. The recorder maps the host send time to
camera time with its own presentation map (the median of host arrival time
minus camera timestamp), so the user's words land in the file where the
doorbell played them, within ±40 ms (tested on a simulated clock).

### The mix

Visitor and You at unity gain, a soft limiter, resampled to 48 kHz,
1024-sample frames, silence when nobody talks. Echo is left as recorded:
the visitor track may contain the user's words coming back from the
doorbell's speaker (AUDIO.md, "Recording calls").

### Files

- Beside the call transcripts: `~/Library/Application
  Support/NVRView/Transcripts` on macOS, `%LOCALAPPDATA%\NvrView\Transcripts`
  on Windows, with the **same stem** as the call's transcript (UX.md,
  "Transcript files"). The app reserves the stem when the recording starts,
  passes the full path, and the transcript writer reuses it.
- Call recordings use the camera's name (share-safe names apply to pictures
  and live recordings only).
- The folder keeps the newest 100 recordings and at most 10 GB of `.mp4`
  files, pruned like the transcripts (which keep their own `.txt` limits).
  "Open Call Recordings Folder" sits next to the setting.

## Indicators

- **Tile:** a "● REC 1:23" pill, always visible (not hover chrome), on every
  tile of the recorded camera in every window, full screen included, in the
  corner opposite the corner indicators.
- **Call bar:** "● Recording 0:42".
- **Window:** on Windows, the window's header shows "● N recording" (a list
  with Stop for each and Stop All; until that header exists, at the top of
  the sidebar). On macOS, a toolbar item while at least one recording runs,
  and the Camera menu's Recordings submenu. No menu-bar status item.

## Pan, tilt, and zoom

> **Implementation status (Z1).** The core's side is built and tested
> against the mock NVR's PTZ model (`tools/mock-nvr`, `ptz`):
> ARCHITECTURE.md, "Device commands" has what it does, FFI.md "Camera
> motion (PTZ)" the Rust API and events. Not built: the C functions and
> the apps' controls and self-test steps (Z2).

Moving a PTZ camera, its lens zoom and focus, and recalling its presets from
the tile. It is camera motion control (ARCHITECTURE.md, "Device commands"):
the user's own controls need no prior enable; everything is rate-limited and
logged; automation needs a grant.

### Capabilities

From each channel's `Support` item (command 199; protocol.md,
"Capabilities"), passed to the apps in the `channels` event:

- `ptzType` 3: pan and tilt (and lens zoom and focus when 294 reports
  ranges);
- `ptzType` 1: lens zoom and focus only (inferred from the client capture);
- `ptzPreset` bit 0: presets;
- `ipcAudioTalk` 1: talk-back (Start Conversation shows in the header);
- `channelType` "Doorbell": a doorbell (observed).

### What the core sends

Only the observed forms, built from fixed templates (protocol.md, "What
nvrview will send"): 18 with `left`, `right`, `up`, `down` and `stop`; 19
with `toPos` only; 295 with `zoomPos` or `focusPos`, clamped to the range
294 reported; and the reads 294, 190 and 340. Diagonals and the 18 zoom and
focus steps are added only after they are observed on hardware.

### Safety

- **Dead-man move.** A move is renewed by the app at least every 500 ms
  while the control is held; the core sends `stop` 1 s after the last
  renewal. Renewals are not sent to the camera and not counted.
- **Maximum hold** 30 s, then `stop`.
- **Stop on every path:** release, error, timeout, disconnect, tile close,
  session drop.
- **Limits per camera,** sized against runaway software and never felt by
  hand: moves and lens commands 20 per second with bursts of 40; preset
  recalls 5 per second.
- **Logged:** every command with its origin (the app's manual control, the
  local API client, or automatic correction).
- **Automation** (the local API, automatic correction) sends motion only for
  a camera the user granted it (`nv_motion_grant`). Talk-back keeps its own
  per-camera permission and prompt.

## Tests

Self-test steps, with the same parity IDs on both apps (PARITY.md, "The
self-test comparison"):

| Area | IDs |
| --- | --- |
| Header | `header-priority-narrow`, `header-overflow-menu`, `call-window-tile-zoom`, `call-window-tile-menu` |
| Pictures | `capture-picture-tile`, `capture-picture-zoomed`, `capture-picture-fisheye`, `capture-picture-call`, `capture-picture-text-field` (P does nothing while a text field is edited) |
| Live recording | `capture-record-tile` (opened with AVAsset or the Media Foundation source reader: one video and one AAC track, duration within 0.5 s), `capture-record-indicator`, `capture-record-cap`, `capture-record-disk-low` (a huge `min_free_bytes`), `capture-record-reconnect` (a mock disconnect: same file, `gaps` 1), `capture-record-survives-tile-close` |
| Calls | `capture-call-record-button` (the transcript's stem as `.mp4` and `.txt`, four tracks, only Conversation enabled), `capture-call-record-auto`, `capture-encoder-priming` |
| PTZ | `ptz-pad-hold` (the mock logs 18 with a direction, then `stop`), `ptz-deadman`, `ptz-preset-recall` (19 with `toPos` only), `ptz-lens-clamp` |

Core tests: a multi-track fragment and finalize round trip; a partial file
cut at random points is recovered; epochs across timestamp resets; the mixer
and limiter; the placement of "You" within ±40 ms on the simulated clock;
the disk guard through the injected shim; a parameter-set change starts a
segment; allowlist tests for the fixed 18, 19, 295, 294, 190 and 340
templates; a PTZ model in mock-nvr.
