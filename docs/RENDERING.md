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
  result, so not every `Decode` gets one.
- `tick(now)`: drives time-based policy (recovery cooldowns, stall detection).

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
- **Backpressure.** A bounded queue of access units; on overflow, drop to the
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
below, video range.

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

## Where this crosses the C interface (M2)

The core runs the pipeline on its per-channel delivery thread and invokes the
app's callback with actions instead of raw frames. Access-unit bytes in
`Decode` are still borrowed for the duration of the callback. The app reports
decoder results back through thread-safe `decoded` / `decode_failed` calls.
Until M2, the macOS renderer is driven by a test-only stand-in.
