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

## Where this crosses the C interface (M2)

The core runs the pipeline on its per-channel delivery thread and invokes the
app's callback with actions instead of raw frames. Access-unit bytes in
`Decode` are still borrowed for the duration of the callback. The app reports
decoder results back through thread-safe `decoded` / `decode_failed` calls.
Until M2, the macOS renderer is driven by a test-only stand-in.
