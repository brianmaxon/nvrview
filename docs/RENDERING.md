# Rendering

How video gets from the core to the screen, split so that every decision is
made once, in shared Rust, and each platform only carries decisions out with
its own decoder and GPU API.

| | Shared Rust (`crates/video-pipeline`) | Per platform |
| --- | --- | --- |
| Decides | when to configure, decode, flush, or reset a decoder; keyframe gating; backpressure; recovery from decode errors; color conversion matrix; grid layout | nothing |
| Does | no I/O, no threads, no platform APIs | decode, keep the newest picture per tile, draw the grid on the GPU |
| macOS | | VideoToolbox (`VTDecompressionSession`) → `CVPixelBuffer` (NV12, IOSurface) → Metal texture → one Metal pass for the grid |
| Windows | | Media Foundation decoder with a D3D11 device manager → NV12 D3D11 texture → one D3D11 pass for the grid |

Both platforms use the hardware video decoder and draw with the GPU; nothing
is copied through the CPU after the compressed frame.

## The tile pipeline (shared)

One `TilePipeline` per displayed stream. It is sans-I/O: the caller feeds it
inputs and drains actions.

Inputs:

- `push(AccessUnit)`: one picture from the session, Annex-B as the device sent
  it, with codec, keyframe flag, and timestamp (µs).
- `decoded(pts)` and `decode_failed(kind)`: results reported by the platform
  decoder, possibly late and from another thread (the caller serializes them).
  A `Configure` that fails is reported as `decode_failed` too. Pictures still
  in flight when `Flush`, `Reset`, or `Configure` runs are discarded with no
  result, so not every `Decode` gets one; every other `Decode` gets exactly
  one, including pictures the decoder drops (`noPicture`). The failure kinds
  are shared: `invalidParameterSets` and `sessionCreateFailed` answer a
  `Configure`; `notConfigured`, `invalidAccessUnit`, `submitFailed`,
  `decodeError`, and `noPicture` answer the oldest `Decode` in flight.
- `tick(now)`: drives time-based policy (recovery cooldowns, stall detection).
- `discontinuity()`: the stream restarted (reconnect); flush and gate.

Actions, drained in order with `next_action()`:

| Action | Platform does | macOS | Windows |
| --- | --- | --- | --- |
| `Configure { codec, parameter_sets, coded_size, visible_rect, color }` | create a decoder for this format, replacing any existing one | `CMVideoFormatDescription` from the parameter sets, new `VTDecompressionSession` | set input/output media types on the MFT, D3D11-aware output |
| `Decode { access_unit, keyframe, pts }` | submit one picture; must return quickly | rewrite start codes to 4-byte lengths, `VTDecompressionSessionDecodeFrame` (asynchronous) | `ProcessInput` |
| `Flush` | drop everything in flight, keep the decoder | `VTDecompressionSessionFinishDelayedFrames` then discard | `MFT_MESSAGE_COMMAND_FLUSH` |
| `Reset` | destroy the decoder; a `Configure` follows before any `Decode` | invalidate the session | release the MFT |
| `Clear` | show nothing for this tile (black) until a new picture decodes | drop the held picture | drop the held texture |

Status changes (`Waiting`, `Playing`, `Recovering`, `Undecodable`) are reported
alongside, for the user interface.

Policy, all in the pipeline:

- **Gating.** After start, `Flush`, `Reset`, format change, or a dropped frame,
  nothing is decoded until a keyframe (judged by NAL types, not only the flag).
- **Format changes.** New parameter sets or codec in a keyframe produce
  `Reset` + `Configure` before that keyframe's `Decode`.
- **Backpressure.** A few pictures in flight (submitted, not yet answered)
  and a bounded queue of access units behind them; on overflow, drop to the
  next keyframe. Counted, never unbounded.
- **Recovery.** A decode failure leads to `Flush` and gating to the next
  keyframe. Repeated failures with the same format escalate to `Reset` +
  `Configure`, at most a few times per window with growing cooldowns; after
  that the tile is `Undecodable` and waits for a format change. There is never
  a tight rebuild loop, and failures are logged rate-limited.
- **Stats.** Counters per tile (received, decoded, gated, dropped, failures,
  resets, format changes), exposed instead of per-frame logging.

The pipeline also parses parameter sets (H.264 SPS, H.265 VPS/SPS) for coded
size, cropping, and VUI color description, and turns that into `color`:
matrix (BT.601, BT.709, BT.2020), full or video range, and a ready-to-use
3×4 YCbCr→RGB matrix in `f32`, so both platforms' shaders only multiply.
The matrix is row-major and maps `(Y, Cb, Cr, 1)` to `(R, G, B)`, where Y, Cb
and Cr are 8-bit samples normalized as the GPU reads them (code / 255) and
R, G, B come out in 0…1; range offsets live in the fourth column. The
platform asks its decoder for output in the stream's range (VideoToolbox
otherwise converts to whatever range the output format names), so the
samples match the matrix.
Without a VUI color description: BT.709 for heights of 720 and above, BT.601
below, video range unless the VUI sets its full-range flag.

## Layout (shared)

`grid_layout(tile_count, viewport, gap, picture_sizes, mode)` returns one
destination rectangle per tile in pixels of the viewport plus the source
rectangle to sample in pixels of the picture, both with the origin at the top
left (fit with letterboxing, or fill with cropping). Platforms draw one quad per
tile from these rectangles.

## The platform renderer

- Executes actions synchronously when the core delivers them; only decoding
  itself is asynchronous.
- Keeps exactly one decoded picture per tile: newest wins, older ones are
  released. No other queue.
- Draws every tile on each display refresh (`CAMetalDisplayLink` on macOS,
  the swap chain's frame latency waitable on Windows) in a single pass: NV12
  planes as two textures, the tile's matrix as a uniform.
- Can render the same pass into an offscreen texture and read it back. Tests
  and `--selftest` use this to check real pixels without screen-recording
  permission.

## Why our own GPU pass (macOS)

Measured on Apple silicon (macOS 26):

| | Process memory | Notes |
| --- | --- | --- |
| Metal grid (6 tiles) | about 240 MB, flat | about 200 MB of it appears on the first GPU submission of any kind, even an empty render or compute pass; it does not grow with tiles |
| `AVSampleBufferDisplayLayer` per tile | about 21 MB | the system decodes and composites outside the process |
| Decoded `IOSurface`s as `CALayer` contents | about 41 MB with 6 full-HD tiles | composited by the window server; no shader, no readback of the final picture |

We draw with Metal anyway: it applies the shared color matrix (the same
colors on every platform), it can read back exactly what it shows (tests and
`--selftest` check real pixels), and it is where de-warping and overlays will
go. The decoder and the pipeline do not depend on this choice; if memory
matters more at M3, presenting decoded surfaces as layer contents replaces
only the drawing step.

## Windows

`apps/windows/NvrView.Video` (design in
[apps/windows/README.md](../apps/windows/README.md)): the system's H.264 or
H.265 decoder MFT, given the shared D3D11 device's DXGI device manager,
decodes with DXVA into decoder surfaces; each picture is copied on the GPU
into the tile's NV12 texture and drawn by one D3D11 pass with the core's
matrix. Without a GPU (WARP) the same MFTs decode on the CPU and pictures
are uploaded. The headless self-test (`NvrView.Demo --selftest`) checks
real pixels read back from the render target on both paths.

Measured on an AMD Ryzen 5 5560U (6 cores, 12 threads) with integrated
Radeon graphics, Windows 11, release builds, synthetic `testsrc2` streams
(no B-frames, a keyframe every 2 s).

The decoder alone (`NvrView.Demo --bench`, access units submitted back to
back, each picture copied or uploaded into a tile picture): pictures per
second, and CPU time per picture.

| Stream | H.264 GPU | H.264 CPU | H.265 GPU | H.265 CPU |
| --- | --- | --- | --- | --- |
| 640x360 | 720/s, 0.6 ms | 989/s, 2.4 ms | 677/s, 0.7 ms | 593/s, 4.5 ms |
| 1920x1080 | 302/s, 0.8 ms | 118/s, 13.5 ms | 258/s, 0.5 ms | 104/s, 25.1 ms |
| 2560x1440 | 228/s, 1.2 ms | 79/s, 24.5 ms | 176/s, 0.8 ms | 72/s, 37.0 ms |
| 2560x1920 (5 MP) | 179/s, 0.8 ms | 74/s, 29.3 ms | 145/s, 1.4 ms | 59/s, 51.3 ms |
| 3840x2160 | 120/s, 1.4 ms | 48/s, 43.3 ms | 90/s, 1.8 ms | 37/s, 77.0 ms |

So a picture takes 3.3 ms of the GPU's decoder at 1080p and 8 ms at
3840x2160 (H.264), with under 2 ms of CPU; CPU decoding keeps two to three
cores busy and costs 4 (640x360) to 45 (3840x2160 H.265) times the CPU
time.

Live tiles (`NvrView.Demo --live`): N channels of one mock-nvr, every one a
640x360 H.264 sub-stream-like stream at 15 fps, the grid drawn offscreen at
960x720 and 30 fps. CPU is the viewer process (core, decoders, renderer)
as a share of all 12 logical processors; every tile decoded and drew 15
pictures per second (14.7 at worst with 16 tiles on the CPU).

| Tiles | GPU decoding: CPU, working set, private | CPU decoding: CPU, working set, private |
| --- | --- | --- |
| 4 | 0.6%, 103 MB, 123 MB | 1.2%, 98 MB, 64 MB |
| 9 | 1.1%, 143 MB, 236 MB | 3.0%, 134 MB, 110 MB |
| 16 | 1.5%, 215 MB, 377 MB | 5.8%, 180 MB, 169 MB |

Also measured: 16 such tiles in H.265 on the GPU, 2.2% CPU, 204 MB working
set, 588 MB private; 4 tiles of 1920x1080 H.264 at 20 fps on the GPU, 0.6%
CPU, 266 MB working set, 476 MB private. Private bytes grow by about 20 to
35 MB per GPU-decoded 640x360 tile and about 100 MB per 1920x1080 tile,
most likely each decoder MFT's own pool of decoder surfaces, which an
integrated GPU allocates from system memory (not broken down further). The longest single Decode
action in a run (decoder start-up included) was 160 to 220 ms; on average
a Decode action took 0.6 to 3.4 ms on the core's thread for that tile.

The app (`NvrView.App`) draws the same pass into a flip-model composition
swap chain hosted by a WinUI `SwapChainPanel`, paced by the swap chain's
frame latency waitable (one frame of latency), with buffer resizes
coalesced to one per 100 ms and the old buffers scaled in between; design
and measurements (no per-frame allocations, presentation during resizes,
device-loss recovery, decoder memory settings that did not help) are in
[apps/windows/README.md](../apps/windows/README.md), "The app".

## Pacing (shared)

Each app shows the newest decoded picture of a tile at every display
refresh. That shows every picture only if pictures are decoded about as
evenly as their timestamps say; two pictures decoded within one refresh show
as one. The core is what keeps them even:

- **Recorded playback.** The engine releases each picture when the playback
  clock reaches its recorded time (docs/PLAYBACK.md). The recorded time comes
  from the keyframes' UTC seconds plus the device's UTC offset; apps pass the
  computer's offset, and a device on another offset put every picture hours
  before its seek target, so the engine released each arriving group of
  pictures at once (the mock sends two seconds at a time): about one picture
  in eight was drawn. The buffer now takes the device's offset from a fresh
  play's first keyframe when the given one is half a quarter hour or more off
  (FFI-RECORDINGS.md section 1). Measured on the mock (one-camera incident,
  1x, 10 fps, macOS self-test `playback-pacing`): before, 12 of 100 released
  pictures drawn, shown jitter p95 1361 ms; after, 101 of 101, released
  jitter p95 9.6 ms, shown jitter p95 0.5 ms.
- **Live.** Devices deliver live pictures unevenly (protocol.md, "Live
  view"). The tile thread holds a live stream's pictures until their
  timestamp on the arrival clock plus a small delay: the 90th percentile of
  how late recent pictures came relative to the earliest, at most 200 ms, and
  none for an evenly sent stream (`live_pacer.rs`). Measured on macOS with
  nine sub-stream tiles at 15 fps from a mock sending pictures in pairs
  (`--video-burst 2`): the 0.4.0 release read "Device 15.0 fps, Shown 7.5
  fps" on a 60 Hz display; with pacing every delivered picture was drawn
  (135 per second of 135). On nine tiles replaying a recorded 20 fps
  sub-stream with its real arrival times, 71 to 78% of the pictures were
  drawn before (the same in the 0.3.2 and 0.4.0 releases: not a regression)
  and 96 to 98% after, the readout "Device 20.0 fps, Shown 20.2 fps".

The trace (`NVRVIEW_PACING_TRACE=1`, off by default) records per picture when
its Decode reached the app, when it was answered decoded, and when a frame
first drew it, and summarizes intervals against timestamp spacing (jitter
p50, p95, max) every ten seconds. The `live-pacing` and `playback-pacing`
self-test steps run the same measurement on both apps.

**How the verdict is taken.** A display draws pictures on its refresh grid,
and a window behind others on a busy machine misses refreshes now and then,
so a bound on the real display's timing either flakes (a single missed
refresh at 60 Hz is 33.4 ms) or is too loose to mean anything. The
self-tests therefore judge the decode times the trace recorded as an ideal
60 Hz display shows them (`PacingRefresh`: at each refresh the newest decoded
picture is drawn, as both renderers do), and check the real display only as
wiring (at least 80% of the released pictures drawn). The bounds
(`PacingVerdict`, the same constants and test vectors in `PacingStats.cs` and
`PacingStats.swift`): shown pairs' jitter p95 at most 20 ms; at most two
pairs over two and a half refreshes (one late picture, or one stall of any
length, makes two; there is no bound on the single largest jitter); at least
90% of the released pictures shown; at most two decoded pictures never
shown. The
playback step measures 40 shown pairs (about four seconds at the mock's
10 fps) and the live step 30 pictures per tile, each waiting for those
counts, not for a fixed time.

Each part is proven where it is cheapest: the core's release pacing on a
simulated clock (`crates/viewer-core/tests/core.rs`,
`live_pictures_delivered_in_groups_are_decoded_at_their_own_pace`, and
`tests/playback.rs`,
`pictures_are_released_evenly_when_the_given_utc_offset_is_not_the_devices`);
the display stage and the bounds in the shell and NVRCore tests, which also
inject the old bugs: pictures a device sends in pairs decoded on arrival
(without the live pacer) show 30 of 60 pictures with 30 never shown, and a
recording released in one-second bursts (the wrong UTC offset) shows 12 of
60 with a 483 ms jitter, both failing; one picture 50 ms late, or one
110 ms stall, passes, and two late pictures fail. The
apps' steps keep the end to end path: the real core against the mock, the
real decoder, the real renderer.

## Fisheye dewarping

A fisheye camera (docs/protocol.md, "Fisheye cameras"; `fisheye` in the
`channels` event) sends its raw circular image. The apps turn it into
panoramas and virtual views while drawing the tile, in the same grid pass:
no extra decode, no extra stream, no copy. Several tiles of one camera (one
shared tile in the core, one decoded picture) can each show a different
view. Nothing is sent to the camera. The views and the menu are in
[UX.md](UX.md), "Fisheye cameras"; this is the shared math both apps
implement, each in pure code with tests (macOS: `NVRCore/Fisheye.swift`;
Windows: `NvrView.Tiles/Fisheye.cs`) and in one shader (Metal, HLSL).

**Inverse mapping.** For every output pixel the fragment (pixel) shader
computes a direction (which view of the tile the pixel is in, then that
view's projection), turns the direction into a point of the fisheye picture
with the lens model, and samples the decoded NV12 picture there with the
tile's color matrix. Directions outside the lens's field of view draw black.

**Lens model.** Equidistant (`r = f·θ`): the distance from the circle's
center is proportional to the angle θ from the optical axis, and the rim is
θmax, half the field of view (180° assumed, so the rim is the horizon). In
the lens frame (x right and y down in the picture, z out of the lens), a
unit direction `d` lands at

    θ = acos(d.z),  ρ = θ / θmax
    u = cx + ρ · r · d.x / |d.xy|,  v = cy + ρ · r · (W/H) · d.y / |d.xy|

where `u`, `v` are fractions of the picture's width W and height H, the
circle's center is (`cx`, `cy`) in the same fractions and its radius `r` a
fraction of the width. The inverse (tests and circle sampling) is
`θ = ρ·θmax`, `d = (sin θ · x/ρ, sin θ · y/ρ, cos θ)` with
`x = (u − cx)/r`, `y = (v − cy)·(H/W)/r`, `ρ = |(x, y)|`.

**Directions the user thinks in.** A viewer frame with x to the right, y
down (gravity) and z forward: pan ψ turns right about the vertical, tilt τ
looks down from the horizon, and (ψ, τ) is the direction
`(sin ψ cos τ, sin τ, cos ψ cos τ)`. The camera's mount (a per-camera
setting) rotates this frame into the lens frame:

| Mount | Lens frame of (x, y, z) | ψ = 0 | Where τ = 90° is |
| --- | --- | --- | --- |
| Ceiling (default), looking down | (x, −z, y) | toward the top of the picture; ψ grows clockwise | the circle's center |
| Desk, looking up | (x, z, −y) | toward the bottom; ψ grows counterclockwise | outside the view (τ = −90° is the center) |
| Wall, looking ahead | (x, y, z) | the center | the bottom rim |

**Views.** A tile shows one to four views, each in a rectangle of the tile,
each one of two projections:

- *Rectilinear* (a virtual camera; straight lines stay straight): forward
  `f` = (ψ, τ), right `(cos ψ, 0, −sin ψ)`, down `f × right`; the pixel at
  (a, b) ∈ [−1, 1]² of its rectangle looks along
  `f + a·tan(h/2)·right + b·tan(v/2)·down`, with horizontal field of view h
  and vertical `v = 2·atan(tan(h/2) · height/width)` of the rectangle.
- *Cylindrical* (a panorama strip; equirectangular): the pixel at
  (x, y) ∈ [0, 1]² looks along (ψ0 + x·Δψ, τ0 + y·Δτ).

| Mode | Views |
| --- | --- |
| Original | none: the circle as it is, with digital zoom (the ordinary tile) |
| Panorama | ceiling: one strip ψ = pan − 180°…pan + 180°, τ from 0° down by Δτ = clamp(360°·height/width, 30°, 90°) (square pixels when the tile allows); desk: the same strip with τ from −Δτ up to 0°; wall: ψ = −90°…90°, τ centered, Δτ = clamp(180°·height/width, 30°, 180°) |
| Double panorama | ceiling and desk: two 180° strips stacked (pan − 180°…pan on top, pan…pan + 180° below), each with Δτ = clamp(180°·(height/2)/width, 30°, 90°); a wall mount shows Panorama |
| Quad | a 2×2 grid of rectilinear views with h = 90°: ceiling at ψ = pan + 0°, 90°, 180°, 270° (reading order) and τ = 45°; desk the same at τ = −45°; wall at ψ = −67.5°, −22.5°, 22.5°, 67.5° with h = 60° and τ = 0° |
| Virtual PTZ | one rectilinear view (ψ, τ, h): defaults (0°, 45°, 90°) ceiling, (0°, −45°, 90°) desk, (0°, 0°, 90°) wall; h from 20° to 120°; τ within 0°…90° ceiling, −90°…0° desk, −80°…80° wall; ψ wraps (wall: within −90°…90°) |

**Shader constants.** One block per dewarped tile, in 16-byte rows (the
same layout in Metal's `constant` buffer and HLSL's `cbuffer`; tests on both
sides check it), 22 rows, 352 bytes, bound beside the tile's ordinary
constants:

| Row | Contents |
| --- | --- |
| 0 | `cx, cy, r, r·W/H` (circle center and radius, picture fractions) |
| 1 | `θmax` (radians), number of views (1…4), 0, 0 |
| 2 + 5k | view k's rectangle in the tile: left, top, right, bottom in 0…1 |
| 3 + 5k | axis X (xyz, lens frame), w = projection: 0 rectilinear, 1 cylindrical |
| 4 + 5k | axis Y (xyz), w = rectilinear: tan(h/2); cylindrical: ψ0 (radians) |
| 5 + 5k | axis Z (xyz), w = rectilinear: tan(v/2); cylindrical: Δψ |
| 6 + 5k | cylindrical: τ0, Δτ (radians), 0, 0; rectilinear: zeros |

For a rectilinear view the axes are its right, down and forward vectors in
the lens frame; for a cylindrical one they are the viewer frame's x, y and
z axes in the lens frame (the mount's columns), and the shader builds
`cos τ sin ψ · X + sin τ · Y + cos τ cos ψ · Z`. The CPU computes every
row (mount, views, angles) and the shader does only the per-pixel part.

**Finding the circle.** No query reports the circle, so each app measures
it from the first decoded pictures of each camera and keeps it for the run
(in normalized units, so sub and main share it):

1. Border level: the 10th percentile of luma in the four corner squares
   (side 4% of the shorter dimension, at least 4 pixels); threshold = that
   + 20 (8-bit codes). A camera's text overlay in the corners covers too
   little of them to move that percentile.
2. 180 rays from the picture center; on each, step inward one pixel at a
   time from the picture's edge to 10% of the shorter dimension. The first
   run of 4 samples at or above the threshold marks the rim at its
   outermost sample.
3. Fit a circle to the rim points by algebraic least squares, then three
   times drop points whose distance from the circle exceeds
   max(3 × 1.4826 × the median absolute residual, 1 pixel) and fit again.
   (A ray that hits the overlay text first, outside the circle, is dropped
   here.)
4. Accept when at least 90 points remain, the median residual is at most
   1% of the width, the radius is 25%…75% of the shorter dimension, and
   the center is within the middle half of the picture. Otherwise try again
   on a later picture (at most one try per half second); after 8 failed
   tries (a black night picture, a lens cap) use the inscribed circle
   (center (0.5, 0.5), diameter the shorter dimension) and stop.

Measured: on the synthetic fixture (`fixtures/synthetic/fisheye/h264-fisheye.h264`,
circle at (0.49, 0.505), radius 0.46) the fit is within 0.3% of the width;
on a real camera's main stream it agreed within 0.1% across pictures and
with the sub-stream (docs/protocol.md). A manual adjustment of center,
radius and field of view is future work.

**Interaction.** Drags, the wheel and pinch change the tile's view instead
of its digital zoom when the tile is in a dewarped mode (UX.md): a
horizontal drag changes pan in every dewarped mode (and tilt in Virtual
PTZ) by the angle the pointer moved across the view, so the picture follows
the pointer; the wheel and pinch change the PTZ field of view, eased in
logarithmic steps with the same time-based easing as digital zoom (time
constant 0.06 s), so it ends exactly on the target however few refreshes
there are.

**Platforms.** macOS: the Metal grid renderer has a second render pipeline
(`dewarp_fragment`) used for tiles with dewarp constants; the
display-layer renderer of the demo cannot dewarp (it hands pictures to the
system compositor), so dewarping needs the Metal renderer, which the app
always uses. Windows: the D3D11 grid renderer's second pixel shader, same
constants in a second constant buffer.

## Where this crosses the C interface (M2)

The core runs the pipeline on its per-channel delivery thread and invokes the
app's callback with actions instead of raw frames. Access-unit bytes in
`Decode` are still borrowed for the duration of the callback. The app reports
decoder results back through thread-safe `decoded` / `decode_failed` calls.
Until M2, the macOS renderer is driven by a test-only stand-in.
