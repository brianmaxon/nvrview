# Capture: pictures, recordings, and camera control from the tile

> **Design, not implemented.** This is the contract for the tile header
> redesign, the call window as a regular tile, saving pictures, recording the
> live stream and doorbell calls, and pan, tilt, zoom (PTZ) from the tile.
> The user-facing rules are in [UX.md](UX.md) ("Tile header", "Save
> picture", "Recording", "Recording calls", "Pan, tilt, and zoom"); the
> proposed C interface is in [FFI.md](FFI.md) ("Capture" and "Camera motion
> (PTZ)"); audio details in [AUDIO.md](AUDIO.md) ("Recording calls"); the
> difference from the NVR's own recordings in [PLAYBACK.md](PLAYBACK.md)
> ("Local captures and the NVR's recordings"); the PTZ commands in
> [protocol.md](protocol.md) ("What nvrview will send"). Parity status is in
> [PARITY.md](PARITY.md).

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

Details settled with the model (`TileHeader.swift`, `TileHeader.cs`; no
view uses it yet):

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
  with vectors in `fixtures/synthetic/capture-names.json`, which the Rust
  `recordings::naming` tests read too.
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
