# Windows

The Windows viewer (`NvrView.App`, a WinUI 3 window; see "Platform apps"
in [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)), the C# bindings for
the core's C interface, a console smoke test that drives them against
`tools/mock-nvr`, and the video layer the app draws with, also checked by a
headless demo.

| Path | What |
| --- | --- |
| `NvrView.Interop/` | the bindings: `Native.cs` (raw P/Invoke and structs), `NvCore.cs` (safe wrapper) |
| `NvrView.Smoke/` | the smoke test, and `NvLayoutCheck.cs`, which compares the bindings with the header |
| `layout/nvlayout.c` | prints struct sizes, field offsets, and constants as the C compiler sees `nvrview.h` |
| `NvrView.Video/` | the platform side of the core's decoder actions: Media Foundation decoding into D3D11 NV12 textures, and the D3D11 grid renderer (Windows only) |
| `NvrView.Demo/` | headless demo: `--selftest` checks rendered pixels, `--live` measures tiles against mock-nvr, `--bench` times the decoder (Windows only) |
| `NvrView.Tiles/`, `NvrView.Tiles.Tests/` | the tile features' pure logic (detection tracking, corner indicators and their hold, alerts, stream quality and tier selection, full screen auto-hide, key mapping) and its tests on simulated time; builds and runs on Windows and Linux |
| `NvrView.App/` | the app: a WinUI 3 window with the live tile grid, focus, listening, Settings, and a `--selftest` (Windows only) |

Every project targets .NET 10 (the current LTS). The bindings and the smoke
test run on win-x64 and linux-x64 (and linux-arm64 in Docker on Apple
Silicon); `NvrView.Video` and `NvrView.Demo` target `net10.0-windows`, and
`NvrView.App` `net10.0-windows10.0.19041.0`; they are built only on Windows
(the Linux builds name `NvrView.Smoke` alone, which does not reference
them).

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
| `GridRenderer` | draws every tile's picture into an offscreen BGRA target with `nv_grid_layout` (`Render`), or rectangles the caller lays out into any target (`Draw`), one draw per tile; reads frames back for tests |
| `SwapChainTarget` | the app's flip-model composition swap chain: resize, frame latency waitable, readback (see "The app") |

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

## The app (NvrView.App)

What [docs/UX.md](../../docs/UX.md) asks for, on Windows, for one window
showing every connected channel of every configured device (layouts, the
sidebar, several windows, and window restore come in the next milestone).

### Running it

Build it on the Windows computer (Git Bash, repository root):

```sh
scripts/windows-app.sh build
```

That builds `nvrview.dll` and mock-nvr (release) and the app into
`target/dotnet/bin/NvrView.App/release_win-x64/`, with `nvrview.dll` copied
next to `NvrView.App.exe`. The folder is self-contained (the .NET runtime
and the Windows App SDK are inside it, about 250 MB unpublished); nothing is
installed, and it runs from wherever it is copied.

- **Against mock-nvr** (nothing is stored; the password is the mock's,
  `example-password` unless `NVRVIEW_TEST_PASSWORD` says otherwise):

  ```sh
  target/dotnet/bin/NvrView.App/release_win-x64/NvrView.App.exe --mock-nvr target/release/mock-nvr.exe --channels 9
  ```

  The app starts the mock on a free port with the synthetic H.264 stream and
  AAC tone (`--fixture`, `--audio`, `--fps` choose others) and connects to it.
  `--host 127.0.0.1 --port N --user example-user` connects to a mock that is
  already running instead, with the password from `NVRVIEW_PASSWORD`, which
  is never stored (use it against mock-nvr only).
- **Against a real NVR or camera**: start `NvrView.App.exe` with no options,
  press Ctrl+, (or right-click, Settings, or "Add a device" on the empty
  window) and enter the device's address, port (9000 by default), user, and
  password. The app connects at once and on every later start. The device
  list is in `%LOCALAPPDATA%\NvrView\settings.json` (no secrets in it); the
  password is a generic credential named `nvrview/device/<key>` in Windows
  Credential Manager. Logs (the app's and the core's, both capped) are in
  `%LOCALAPPDATA%\NvrView\logs`; `--data-dir DIR` moves all of it.

Other options: `--gpu auto|hardware|warp`, `--software` (CPU decoding),
`--no-audio`, `--lib PATH` (another `nvrview.dll`).

Using it: hover a tile for its header (speaker and name; it fades 3 s after
the pointer stops); click the speaker to listen (Ctrl+click to listen to
several); double-click, Space, or Enter focuses the selected tile and back;
arrow keys move the selection by position (in focus they switch the
focused camera); F is full screen, Esc leaves focus and then full screen;
the mouse wheel (or a touchpad pinch) zooms around the pointer, dragging
pans, 0 resets; Ctrl+L listens to the selected tile; Ctrl+D shows
diagnostics (picture size, tier, GPU or CPU decoding, incoming frame rate);
F1 lists the shortcuts; right-click has all of it in a menu.

### The self-test

```sh
scripts/windows-app.sh selftest 10 --credential-test
```

runs `NvrView.App.exe --mock-nvr ... --selftest 10 --log FILE` in the
logged-in user's desktop session, because an SSH session has no desktop:
as a one-time scheduled task that runs only while the user is logged on
(`schtasks /create ... /it`), started at once and deleted afterwards. The
window opens behind every other window without being activated, so it
never takes focus from the person at the computer. It checks, against a
mock with four channels (the last one behaving like a third-party camera
that is refused once, then streams):

1. every channel plays as a tile, and the grid read back from the swap
   chain is not flat inside any tile; the third-party channel reported
   `tile_waiting` first;
2. the render thread allocates nothing over a second of steady playing;
3. a second device with a wrong password ends in `login_failed`, said in
   words; saving a device the way the Settings dialog does connects it and
   opens its tiles, removing it closes them, and the dialog opens and closes;
4. resizing the window on every refresh for about a second keeps the grid
   presenting while the buffers are resized at most every 100 ms, and they
   settle at the panel's exact pixel size;
5. arrow-key selection finds the neighbor to the right;
6. focusing a tile animates it to the whole stage and then switches it to
   the main stream: generation 1 is shown after generation 0's final Reset,
   with `tile_tier` `starting` and `active` (the core's `tier_switch` timing,
   alignment, and hold are printed);
7. the muted tile reads an audio level (the hover meter); listening makes it
   audible, the core starts its playout through the WASAPI stream
   (`audio_stats` `starts` 1, audio buffered), and the meter reads a level;
8. leaving focus returns it to the sub stream (generation 2);
9. digital zoom draws a non-flat zoomed picture;
10. no Decode failed so far; a simulated GPU device loss replaces the
    device, rebuilds every decoder, and every tile plays again;
11. the tile features, on a mock whose channel 1 is a two-lens camera and
    channel 2 refuses the second lens's tiers: detections from the mock show
    as corner indicators in the configured corner (the symbols are rendered
    to a bitmap and checked, and saved as `indicators.png` beside the log); a
    smoke alarm injected as a core event raises an alert on both lens tiles
    of its camera and no other (blinking red border, pinned header with the
    reason, count, times, and state); on a manual clock, full screen (pretended,
    the window never changes) hides the chrome and the pointer after 3 s idle
    while the alert and the symbols stay, and the next interaction brings
    them back; a stop keeps the alert and holds the symbol 2 s; Enter dismisses
    it on every tile of the camera and the next detection raises a fresh one;
    Clear, Balanced, and Auto switch the stream (main, extern, sub) and Auto
    follows zoom (main at 2.5x, still main at 1.7x, sub at 1.5x); a
    second-lens tile opens on mobile, and an injected `tile_error`
    `start_rejected` reopens it on extern for the run (the core ends a real
    refusal only after its 60 s wait);
12. stopping the mock makes every tile say it is reconnecting;
13. no callback threw, no tile reported an error or a failed tier switch;
14. with `--credential-test`, a throwaway credential is written, read back,
    and deleted in Credential Manager.

It writes what it measured and `selftest: PASS` or `selftest: FAIL` to the
log, which the script prints. `scripts/windows-worker.sh` builds the app on
every run and runs this self-test when `NVRVIEW_APP_SELFTEST=1` (passed
through by `scripts/windows-check.sh`), since it needs a logged-in user.

### Windows App SDK

WinUI 3 comes from the Windows App SDK, pinned at **1.8.260921001**
(`Microsoft.WindowsAppSDK`, a metapackage that pins each component package
to an exact version). Unpackaged (`WindowsPackageType` `None`) and
self-contained (`WindowsAppSDKSelfContained`): the SDK's files are copied
next to the executable, so there is no installer, no framework package, and
no bootstrapper. It is **not** under an open-source license: the NuGet
package carries the *Microsoft Software License Terms, Microsoft Windows
App SDK*, which the build accepts on restore. In short:

- every file the package places next to the app may be redistributed with
  it, framework-dependent or self-contained;
- whoever distributes it must add significant primary functionality, must
  require distributors and end users to agree to terms that protect the
  code and Microsoft at least as much as the license, must indemnify
  Microsoft for claims about the app, and may not use Microsoft's trademarks
  to suggest endorsement or put the redistributable under a license that
  would require its source to be disclosed;
- the software may send usage data to Microsoft (some of it can be turned
  off, as its documentation describes).

So the repository's MIT OR Apache-2.0 sources stay as they are (the SDK is
restored from NuGet at build time, never committed), but a published binary
build of the Windows app would ship Microsoft's redistributables under
those terms, and its release would need an end-user notice that carries
them. The package's `NOTICE.txt` lists the open-source components inside
it. `TerraFX.Interop.Windows` (MIT) remains the only other package.

### How it works

| Type | What |
| --- | --- |
| `CoreHost` | the core and the shared GPU device; actions go straight to each tile's `VideoTile` inside the read side of a device gate; events are parsed and handed to the UI thread |
| `Viewer` | the UI thread's state: devices (state, retry time, channels), tiles (status, tier, quality, listening, zoom); opens a tile for every connected channel on the tier the planner picks, and for any lens on request (`OpenCameraTile`); owns the detection tracker, the indicator board, the alert center, and the quality demands; the render thread reads an immutable tile array |
| `StageRenderer` | the render thread: the swap chain, layout, focus animation, zoom, drawing, presenting, device-loss recovery, readback for the self-test |
| `StageView`, `TileChrome` | the XAML above the swap chain: hover header, speaker, level meter, status line, corner indicators, sound badges, alert strip and border, selection outline, diagnostics; input |
| `IndicatorStrip`, `QualityMenu`, `ViewSettings` | self-contained tile-feature parts a window hosts: the symbol strip (tiles, and the sidebar's rows), the Quality submenu, and the saved indicator corner (`view-settings.json`) |
| `NvrView.Tiles` | the pure logic behind them (see the table at the top); `CoreHost.Tiers` is the one `TierPlanner` every window shares |
| `AudioOutput` | WASAPI shared-mode output of the core's mix |
| `WindowsCredentials`, `DeviceStore`, `SettingsDialog` | passwords in Credential Manager, the device list in `settings.json`, the Settings dialog |
| `SelfTest`, `MockNvr` | the self-test and the mock it starts |

- **Swap chain** (`NvrView.Video/SwapChainTarget.cs`). A flip-model
  (`FLIP_SEQUENTIAL`) composition swap chain with two BGRA buffers, given to
  the `SwapChainPanel` through WinUI's `ISwapChainPanelNative`. D3D11 rotates
  a flip-model chain's buffers itself, so one render target view of buffer
  0 serves every frame (D3D11 does not allow writing the other buffers);
  there is no per-buffer view to keep.
- **Pacing.** The chain is created with the frame latency waitable object
  and a maximum latency of one frame; the render thread waits on it, then
  lays out, draws every tile, and presents with sync interval 1. So it draws
  once per refresh, as late as possible, and `Present` does not block. It
  keeps presenting through resizes and animations, and allocates nothing
  per frame (measured by the self-test: 0 bytes over a second; a
  `stackalloc` initializer of floats in the W1 pass turned out to allocate
  an array per frame and was replaced).
- **Resize.** The UI thread hands the panel's size and composition scale to
  the render thread. While the size keeps changing, `ResizeBuffers` runs at
  most every 100 ms, and in between the old buffers are shown scaled to the
  panel with `SetMatrixTransform` (the same transform also undoes the
  display scale, since the buffers are in physical pixels); when the size
  settles, one more resize makes them exact. Measured: 9 resizes for 30
  window sizes in 1.3 s, presentation continuing at about 35 frames per
  second meanwhile (about 50 in steady state on that display).
- **Layout and focus.** `nv_grid_layout` in panel pixels each frame. A
  focused tile's rectangle is interpolated (0.25 s, smoothstep) between its
  grid cell and the whole stage and drawn last, over the others, which stop
  being drawn once it covers them; nothing is recreated. Once the animation
  ends the app asks for the main stream (`nv_tile_set_tier`); `VideoTile`
  keeps showing the sub stream's picture until the old generation's final
  Reset and then shows the new one (docs/FFI.md "Switching a tile's stream
  tier"). Leaving focus asks for the sub stream at once and animates back.
  Each tile's rectangle (in DIPs) is published for the chrome and hit
  testing, so the overlays follow the picture exactly.
- **Digital zoom** changes only the source rectangle: a zoom factor (1 to 8)
  and a center in visible-picture coordinates (0 to 1), so the same region
  stays in view when the tier switch changes the resolution.
- **Device loss.** A `Present` or `ResizeBuffers` that reports
  `DXGI_ERROR_DEVICE_REMOVED` or `_RESET` (or a call that fails while
  `GetDeviceRemovedReason` reports a removal) releases the swap chain and
  renderer and takes the device gate's write side, which holds back every
  action: a new device is created, every `VideoTile` answers its pictures in
  flight with `DecodeError`, gets empty pictures on the new device, and
  rebuilds each generation's decoder from its last Configure at the next
  Decode. A rebuilt decoder that does not start at a keyframe answers that
  Decode `DecodeError`, so the core flushes and waits for one; a rebuild that
  fails answers the same way, and repeated failures make the core reset and
  reconfigure the tile. A new swap chain is attached to the panel on the UI
  thread. The self-test simulates the loss (a real removal cannot be caused
  without driver tools) and checks that every tile plays again.
- **Audio.** WASAPI shared mode, event-driven, 32-bit float stereo at the
  default device's mix rate (`AUTOCONVERTPCM` lets the engine convert), on a
  thread in the "Pro Audio" MMCSS class. Each wake fills every free frame
  with `NvCore.RenderAudio`. The host time of the first frame written is
  `IAudioClock.GetPosition`'s QPC time (100 ns units) times 100 plus the
  frames still queued ahead of it (written minus the device position) at the
  output rate, which is on the core's clock (QPC in ns) with no offset
  estimate. Measured in the self-test's runs: the core's `av_offset_ms`
  within 3 ms once playing, no underruns; the first block is timed about 43 ms ahead (buffer and
  device latency). A device that goes away restarts on the new default
  device a second later.
- **Listening and meters.** The speaker makes a tile audible
  (`nv_tile_audio_set`) and the others silent, unless Ctrl is held. The
  meter is a 3-pixel bar along the top of the tile, its width the peak from
  `nv_tile_audio_level` read once per frame (`CompositionTarget.Rendering`)
  only while it can be seen: green and always shown while the tile is
  audible, white on gray and fading with the header while hovering a muted
  tile.
- **Status in words** on each tile: connecting, reconnecting with the retry
  countdown (`device_state` `backoff`), login failed, waiting for the camera
  (`tile_waiting`), the device refused the stream (`tile_error`), starting,
  recovering, cannot be decoded, and, when a Configure fails because no
  H.265 decoder is installed, that the HEVC Video Extension is needed.
- **Corner indicators** (docs/UX.md). `Viewer.Detections` (a `DetectionTracker`)
  follows `detection`, `motion`, and `alarm` events per camera (device key,
  channel, lens), with sound (`local_audio`) per channel shown on every lens;
  `Viewer.Board` adds the 2 s hold after a stop. `IndicatorStrip` draws one
  small symbol per active kind (Segoe Fluent Icons; a vector paw, which the
  font lacks) in white with a baked-in second copy as the shadow, amber for the
  sound alarms, with a tooltip and automation name ("Person (camera AI)").
  It updates on events and the 250 ms tick only; nothing runs per frame. The
  corner (default top right) is in Settings and the right-click menu and is
  saved in `view-settings.json`. A sidebar row hosts the same control:
  `strip.SetItems(viewer.Board.Items(deviceKey, channel))` (a collapsed device:
  `Items(deviceKey)`), refreshed on `Viewer.IndicatorsChanged`. A sound alarm
  also shows as a slate badge ("Smoke alarm · sound") at the bottom left.
- **Alerts.** A smoke or CO sound alarm raises an alert per (device, channel)
  in `Viewer.Alerts`: every tile of that camera, on any lens, shows a red
  border that blinks about once a second (steady when Windows' "Show
  animations" is off) and a header strip pinned at the top (so full screen
  hiding the chrome does not hide it) with the reason, the first time (with the
  date when not today), the count, the last time, and whether it is sounding
  now, and a Dismiss button; Enter on the selected tile does the same. A stop
  does not clear it; dismissing clears it on every tile and resets the count.
  Raises and dismissals go to the app log.
- **Quality and tiers.** A tile's context menu has Quality > Auto, Clear,
  Fluent, Balanced (Balanced only for a single-lens channel whose tiers list
  `extern`), with a check mark and a note on the main stream's cost;
  `QualityMenu.Create(tile.Quality, viewer.OffersBalanced(tile), q => viewer.SetQuality(tile, q))`
  builds it for any window. `CoreHost.Tiers` (a `TierPlanner`) maps what every
  tile asks (quality, focus, zoom: Auto goes to the main stream at 2x and back
  below 1.6x) to one tier per camera, highest wins, through the core's aligned
  tier switch. A second lens (`OpenCameraTile(device, channel, 1)`) opens on
  `mobile` and uses `extern` focused or zoomed or on Clear; when the core
  reports `tile_error` `start_rejected` for its `mobile` start (or a
  `mobile` switch fails), the tile is reopened on `extern` for the rest of the
  run. A layout leaf stores the choice as its `quality` member:
  `TileQualities.ToMember` and `FromMember` (Auto is no member).
- **Full screen chrome.** In full screen the headers and the pointer hide
  3 s after the last interaction (pointer move, click, key, scroll) and return
  on the next; the video is never resized, and the alert strips and corner
  indicators stay. The timer is a `ChromeIdle` on the viewer's injectable
  clock; the self-test pretends full screen (`FullScreenOverride`) so the window
  never changes.
- **M** (or Ctrl+L) listens to the selected tile; the key mapping
  (`TileKeyMap`) is in `NvrView.Tiles`.
- **Credentials.** `CredWriteW`/`CredReadW`/`CredDeleteW`, generic
  credentials persisted for this user on this computer
  (`CRED_PERSIST_LOCAL_MACHINE`, never roamed), named after the app's own
  key for the device (`DeviceConfig.Key`, `dev-` and 12 hex digits, made
  when the device is first saved), so changing a device's address or user
  keeps its password. The password's UTF-16 copy is cleared after writing.
  Development modes (`--host`, `--mock-nvr`) use an in-memory store and
  never touch Credential Manager or `settings.json`.

### Decoder memory

Private bytes grow by about 20 to 35 MB per GPU-decoded tile (RENDERING.md,
"Windows"), most likely each decoder's own pool of surfaces. Two Media
Foundation settings could have capped it, and `DecoderSettings` (with
`--decoder-samples N` and `--limit-coded-size` on the demo and the app)
tries them: asking for fewer extra output samples
(`MF_SA_MINIMUM_OUTPUT_SAMPLE_COUNT` on the output stream) and telling the
decoder the largest coded size to expect
(`CODECAPI_AVDecVideoMaxCodedWidth`/`Height`). Neither changed memory
measurably (`NvrView.Demo --live`, 15 fps streams, private bytes and working
set after 12 s):

| Tiles | default | max coded size | 1 output sample | both |
| --- | --- | --- | --- | --- |
| 16 x 640x360 H.264 | 413 MB, 232 MB | 451 MB, 222 MB | 445 MB, 216 MB | 445 MB, 217 MB |
| 16 x 640x360 H.265 | 663 MB, 221 MB | 648 MB, 230 MB | 614 MB, 225 MB | 666 MB, 232 MB |
| 4 x 1920x1080 H.264 | 520 MB, 312 MB | 441 MB, 361 MB | 598 MB, 260 MB | 602 MB, 264 MB |

The differences are within run-to-run noise (one run each), so both
settings stay off by default and no cap is applied.

## Running the tests

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

The tile features' tests (`apps/windows/NvrView.Tiles.Tests`, simulated time:
a manual clock, nothing sleeps) run in the same container.

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
dotnet build apps/windows/NvrView.Tiles.Tests -c Release --artifacts-path build/csharp/artifacts
dotnet build/csharp/artifacts/bin/NvrView.Tiles.Tests/release/NvrView.Tiles.Tests.dll
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
