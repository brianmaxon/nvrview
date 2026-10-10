# Windows groundwork

No app yet: the C# bindings for the core's C interface, a console smoke
test that drives them against `tools/mock-nvr`, and the video layer the
WinUI 3 app (see "Platform apps" in
[docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)) will draw with, checked
by a headless demo.

| Path | What |
| --- | --- |
| `NvrView.Interop/` | the bindings: `Native.cs` (raw P/Invoke and structs), `NvCore.cs` (safe wrapper) |
| `NvrView.Smoke/` | the smoke test, and `NvLayoutCheck.cs`, which compares the bindings with the header |
| `layout/nvlayout.c` | prints struct sizes, field offsets, and constants as the C compiler sees `nvrview.h` |
| `NvrView.Video/` | the platform side of the core's decoder actions: Media Foundation decoding into D3D11 NV12 textures, and the D3D11 grid renderer (Windows only) |
| `NvrView.Demo/` | headless demo: `--selftest` checks rendered pixels, `--live` measures tiles against mock-nvr, `--bench` times the decoder (Windows only) |

Every project targets .NET 10 (the current LTS). The bindings and the smoke
test run on win-x64 and linux-x64 (and linux-arm64 in Docker on Apple
Silicon); `NvrView.Video` and `NvrView.Demo` target `net10.0-windows` and are
built only on Windows (the Linux builds name `NvrView.Smoke` alone, which
does not reference them).

## The bindings

Hand-written against the generated header,
`crates/viewer-ffi/include/nvrview.h` (ABI 3, contract in
[docs/FFI.md](../../docs/FFI.md)). The interface is small (21 functions, 11
structs) and cbindgen already makes the Rust source the single definition, so
a second generator (ClangSharp or csbindgen) would add a toolchain to every
build without removing the need for the safe layer, which is most of the
code. Drift is caught by checks instead:

- **Layout.** `nvlayout.c`, compiled with the platform's C compiler (MSVC-ABI
  clang on Windows, gcc on Linux), prints `NV_ABI_VERSION`, `sizeof` of every
  struct, `offsetof` of every field, and every enum constant.
  `NvLayoutCheck` compares them with `Unsafe.SizeOf`, `Marshal.OffsetOf`, and
  each enum member's `[CName]`, in both directions (a field or constant on
  only one side fails).
- **Functions.** Every `nv_*` declaration in the header must have a
  `LibraryImport` in `NvNative`, and the other way round.

How it is built:

- `[assembly: DisableRuntimeMarshalling]`: every struct is blittable and
  crosses as it is in memory. C `bool` in structs is `CBool` (one byte), C
  enums are 32-bit C# enums, `size_t` is `nuint`.
- `LibraryImport` (source-generated) with `CallConvCdecl` on every function;
  the generator handles `bool` arguments and the UTF-8 layout strings.
- Callbacks are `[UnmanagedCallersOnly]` static methods passed as
  `delegate* unmanaged[Cdecl]`; the context pointer is a `GCHandle` to the
  dispatcher. Each catches every exception, counts it
  (`NvCore.CallbackExceptionCount`), and reports it to
  `NvCoreCallbacks.OnCallbackException`; nothing crosses into native code.
- `NvCore` owns the handle through a `SafeHandle` whose release calls
  `nv_core_free` and only then frees the callback context. Calls that may
  not run inside a callback (`Dispose`, `CloseTile`, ...) throw there.
  `Decoded`, `DecodeFailed`, `TileAudioLevel`, and `RenderAudio` work from
  any thread, callbacks included, and return `NV_ERR_CLOSED` once the core is
  freed.
- Strings come back through the `out`/`cap`/`len` loop: on
  `NV_ERR_BUFFER_TOO_SMALL` the call is repeated with `*len + 1` bytes. The
  password's UTF-8 copy is zeroed after `nv_device_add`.
- The library is `nvrview.dll` (Windows) or `libnvrview.so` (Linux), found by
  the normal search or loaded from a path with
  `NativeLibrary.SetDllImportResolver` (the smoke test's `--lib`).

## The smoke test

One run, against a spawned mock-nvr with a synthetic H.264 stream and AAC
tone:

1. Layout and function checks (`--layout`, `--header`).
2. Starts a core with log and data directories in a temporary folder, adds
   the device, waits for `connected`, opens channel 0 on the sub stream.
3. Plays it with a fake decoder: generation 0 answered inside `on_action`,
   later generations from a decoder thread through a bounded queue. Checks
   Configure contents, that the first Decode after each Configure is a
   keyframe, no Decode before Configure, Annex-B access units, and that every
   Decode is answered exactly once.
4. Reads the audio level while the tile is silent (it must show the tone),
   then makes it audible and renders from a separate thread on a contiguous
   timeline, with a tap attached.
5. Switches to main (generation 1) and back to sub (generation 2); each switch
   must bring `tile_tier` `starting` then `active`, one Configure, and exactly
   one final Reset of the old generation, after which nothing of it arrives.
6. Device key (through a 4-byte first buffer, so the loop runs) and its match
   with `device_info`; layout put, get, replace (version and revision grow,
   unknown members kept, `layouts_changed` arrives), invalid layout refused,
   delete, delete again `NV_ERR_UNKNOWN_ID`; `layouts.json` and
   `devices.json` on disk; `nv_grid_layout`.
7. Close: one `tile_closed` (reason `closed`), no action after
   `nv_tile_close` returns, second close and tier 3 refused, device removed
   (`closed` event), rendering stopped, `nv_core_free` completes and no
   callback runs after it.
8. A second core on the same data directory gets the same device key.
9. One event handler throws on purpose; the run passes only if the
   dispatcher caught and reported exactly that one exception.

It prints one summary line and `csharp-smoke: ok`, exit code 0; any problem
is printed and the exit code is 1. A 120 s watchdog ends a stuck run.

## The video layer (NvrView.Video)

The Windows half of [docs/RENDERING.md](../../docs/RENDERING.md): the core
decides when to configure, decode, flush, or reset and which color matrix
applies; this layer only carries the actions out and draws.

| Type | What |
| --- | --- |
| `GpuDevice` | the one D3D11 device decoders and the renderer share: the default hardware adapter with video support, or WARP (`GpuChoice`); multithread protected; a DXGI device manager for Media Foundation |
| `VideoTile` | one core tile: takes `NvCoreCallbacks.OnAction` for it, one `MfDecoder` per generation (tier switches), answers every Decode through `NvCore`, exposes the shown generation's picture |
| `MfDecoder` | one Media Foundation decoder MFT: Configure, Decode, Flush, Reset |
| `TilePicture` | the one picture a tile keeps: an NV12 texture (hardware decoding) or R8 + R8G8 textures (CPU decoding), its visible rectangle, and its color matrix |
| `GridRenderer` | draws every tile's picture into an offscreen BGRA target with `nv_grid_layout`, one draw per tile; reads frames back for tests |

### Bindings: TerraFX.Interop.Windows

D3D11, DXGI, Media Foundation, and the HLSL compiler come from
[TerraFX.Interop.Windows](https://github.com/terrafx/terrafx.interop.windows)
(MIT, pinned at 10.0.26100.6, one package with no dependencies). It is a
raw, blittable translation of the Windows SDK headers: COM interfaces are
structs called through function pointers, so a call allocates nothing and
reference counts are explicit. Vortice.Windows (also MIT, 3.8.x) was the
other candidate; it wraps every COM pointer in a managed object, so the
per-frame path (`GetBufferByIndex`, `QueryInterface` for the DXGI buffer,
`GetResource`) would allocate a wrapper per call, against the rule of no
per-frame allocations. The cost of TerraFX is `unsafe` code and a large
assembly (28 MB; trimming at publish should cut most of it, not measured
yet).

### Decoding

- **Which decoder.** `MFTEnumEx` for a synchronous decoder MFT from H.264 or
  H.265 to NV12, best merit first: the in-box *Microsoft H264 Video Decoder
  MFT*, and for H.265 the *HEVCVideoExtension* decoder (the HEVC Video
  Extension from the Microsoft Store; without it H.265 tiles fail to
  configure and the core marks them undecodable).
- **GPU or CPU.** The MFT gets the shared device's DXGI device manager when
  it says it is D3D11-aware and hardware decoding is allowed; it then
  decodes with DXVA into D3D11 decoder surfaces. If it refuses the input
  type that way (a profile or size the GPU cannot decode), the same MFT
  decodes on the CPU instead. With WARP (no GPU) there is no device manager
  and every tile decodes on the CPU. `MfDecoder.Kind` reports what happened,
  judged from the first picture: `Hardware` when the output texture is a
  decoder surface (`D3D11_BIND_DECODER`), `Software` for memory output.
- **Configure.** Input type from the codec and the core's coded size (the
  H.265 decoder refuses its output type without a frame size), output type
  NV12, `MF_LOW_LATENCY` so a picture comes out for every access unit. The
  parameter sets (NAL units without start codes) are joined as Annex-B and
  put in front of a keyframe that does not carry its own.
- **Decode.** The access unit is copied as delivered (Annex-B, which both
  decoders take) into one of four reused input samples (a slot the decoder
  still holds is replaced, never overwritten), `ProcessInput`, then
  `ProcessOutput` until the decoder needs more input. It runs on the core's
  thread for that tile, so a picture is normally answered before the action
  returns; a decoder that holds pictures back is answered when they come
  out, matched by timestamp, and past three held pictures (the core keeps at
  most four in flight) the oldest is answered `NoPicture`.
- **Pictures.** A hardware picture is copied on the GPU
  (`CopySubresourceRegion`) from the decoder's surface into the tile's NV12
  texture, so the decoder's surface pool never waits for the renderer; a CPU
  picture is uploaded (`UpdateSubresource`) into R8 and R8G8 textures. The
  decoder outputs the samples as coded, in the stream's range, so the
  core's matrix applies directly; textures may be the coded size, and the
  renderer samples the visible rectangle inside them.
- **Flush** sends `MFT_MESSAGE_COMMAND_FLUSH` and forgets what was in
  flight without answering it; **Reset** releases the MFT; **Clear** marks
  the picture empty (the tile draws black).
- **Generations.** Each tier-switch generation has its own decoder and
  picture. The shown generation stays on screen until its final Reset
  arrives while a newer generation exists; then the newer one is shown and
  the old picture released (docs/FFI.md, "Switching a tile's stream tier").

### Rendering

One pass into the target: clear to black, then for each tile with a
picture, one four-vertex triangle strip whose corners come from the vertex
ID and a small constant buffer (destination in clip space, source in
texture coordinates, the 3x4 matrix), sampling luma and chroma with a
linear clamp sampler; the same shader as the macOS Metal pass. Layout is
`nv_grid_layout` with each picture's visible size.

### Threads and locking

The device is multithread protected, and `GpuDevice.Lock()`
(`ID3D11Multithread`) is the one lock for the immediate context and every
`TilePicture`: decoders take it to copy or upload a picture, the renderer
holds it for a frame. Media Foundation takes the same lock inside DXVA
calls. Actions for a tile run on the core's thread for that tile, one at a
time; nothing in the action path or the render loop allocates in steady
state (the demo's `--live` reports managed allocations: about 20 bytes per
decoded frame, all of it the demo's own JSON event parsing).

## The headless demo (NvrView.Demo)

```text
NvrView.Demo --mock-nvr EXE [options] --selftest SECONDS FILE...
NvrView.Demo --mock-nvr EXE [options] --live SECONDS [--channels N] [--fixture FILE] [--fps N] [--tier sub|main]
NvrView.Demo [options] --bench FILE [--frames N] [--coded WxH]
options: --lib PATH  --gpu auto|hardware|warp  --software  --size WxH  --expect hardware|software
```

- `--selftest` starts one mock-nvr per file (each serving it as channel 0),
  opens one sub-stream tile per file through the real core, switches every
  tile to the main stream at a quarter of the time, and renders the grid
  offscreen at 30 fps. It reads back the frames at the midpoint, 0.12 s
  later, and at the end, and checks each tile like the macOS demo's
  self-test: it decodes and keeps decoding with no failures, its picture
  fills its rectangle with the picture's aspect ratio and black just
  outside, is not flat, and changed; the known-color fixtures come out
  within 8 codes of the colors they were made from; the tier switch
  completed. `--expect hardware` also fails any tile that did not decode on
  the GPU. One line per tile, then `selftest: PASS` (exit 0) or
  `selftest: FAIL` (exit 1).
- `--live` plays N channels of one mock-nvr and reports, after a warm-up,
  decoded and drawn frames per second per tile, time per Decode action,
  process CPU, memory, and managed allocations.
- `--bench` runs the decoder alone on a file as fast as it goes.

`scripts/csharp-video-selftest.sh` builds the demo and runs the self-test
on every synthetic fixture, first with `--gpu hardware --expect hardware`,
then with `--gpu warp --expect software`; `--ci` runs only the WARP pass on
the H.264 fixtures (hosted runners have no GPU and no HEVC Video
Extension); CI runs that on `windows-latest`, not yet required to pass
until a run confirms Media Foundation's H.264 decoder on the runner image.
`scripts/windows-worker.sh` runs the full script after the C# smoke test.
No window is ever shown, so it works over SSH.

Measured numbers (decoder throughput, CPU and memory for 4, 9, and 16
tiles) are in [docs/RENDERING.md](../../docs/RENDERING.md), "Windows".

## Running it

### On macOS or Linux, in Docker (no .NET needed)

```sh
scripts/csharp-check.sh
```

Builds `libnvrview.so`, mock-nvr, and the layout helper in the Rust image
(sharing the `nvrview-linux-*` volumes of `scripts/linux-check.sh`), then
builds and runs the smoke test in `mcr.microsoft.com/dotnet/sdk` (sources
mounted read-only; output in the `nvrview-csharp-*` volumes). About 10 s when
warm. `scripts/check.sh` runs it when Docker is up (`NVRVIEW_CSHARP_TESTS=0`
skips it); `scripts/clean-docker.sh` removes the volumes.

### On Windows

With the Rust toolchain, the .NET 10 SDK, and Visual Studio's C++ tools (or
LLVM clang), from the repository root in Git Bash:

```sh
cargo build --release -p viewer-ffi -p mock-nvr
mkdir -p build/csharp
clang -std=c11 -I crates/viewer-ffi/include apps/windows/layout/nvlayout.c -o build/csharp/nvlayout.exe
build/csharp/nvlayout.exe > build/csharp/layout.txt
dotnet build apps/windows/NvrView.Smoke -c Release --artifacts-path build/csharp/artifacts
dotnet build/csharp/artifacts/bin/NvrView.Smoke/release/NvrView.Smoke.dll \
  --mock-nvr target/release/mock-nvr.exe --fixtures fixtures/synthetic \
  --lib target/release/nvrview.dll --layout build/csharp/layout.txt \
  --header crates/viewer-ffi/include/nvrview.h
```

Then the video self-test (GPU, then WARP):

```sh
scripts/csharp-video-selftest.sh
```

CI runs the smoke test on `windows-latest` and `ubuntu-latest`
(`.github/workflows/ci.yml`). To point the smoke test at an already running
mock instead, pass `--host 127.0.0.1 --port N` (or set `NVRVIEW_MOCK_PORT`);
the password comes from `NVRVIEW_TEST_PASSWORD` (default
`example-password`).
