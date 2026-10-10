# Windows groundwork

No app yet: the C# bindings for the core's C interface and a console smoke
test that drives them against `tools/mock-nvr`. The WinUI 3 app (see
"Platform apps" in [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)) will
build on `NvrView.Interop`.

| Path | What |
| --- | --- |
| `NvrView.Interop/` | the bindings: `Native.cs` (raw P/Invoke and structs), `NvCore.cs` (safe wrapper) |
| `NvrView.Smoke/` | the smoke test, and `NvLayoutCheck.cs`, which compares the bindings with the header |
| `layout/nvlayout.c` | prints struct sizes, field offsets, and constants as the C compiler sees `nvrview.h` |

Both projects target .NET 10 (the current LTS); the smoke test runs on
win-x64 and linux-x64 (and linux-arm64 in Docker on Apple Silicon).

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

CI runs the same steps on `windows-latest` and `ubuntu-latest`
(`.github/workflows/ci.yml`). To point the smoke test at an already running
mock instead, pass `--host 127.0.0.1 --port N` (or set `NVRVIEW_MOCK_PORT`);
the password comes from `NVRVIEW_TEST_PASSWORD` (default
`example-password`).
