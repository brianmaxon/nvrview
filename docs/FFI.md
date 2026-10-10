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
| tile | `nv_tile_open` | `nv_tile_close`, its device's removal, or `nv_core_free` | one channel and tier, one pipeline, one platform decoder |

IDs are nonzero `uint64_t`, never reused within a core. Two tiles may show the
same channel and tier; each has its own pipeline, and the device carries the
stream once per tile (sharing one stream is a later optimization).

## Threads

- The core owns its threads. A slow `on_action` for one tile never delays
  other tiles or the session's socket: media reaches each tile through a
  bounded queue, and the pipeline's backpressure policy drops to the next
  keyframe when the app falls behind.
- `on_action` is serialized per tile; `on_event` is serialized across the core.
- From inside either callback an app may call only `nv_tile_decoded` and
  `nv_tile_decode_failed`. Everything else (`nv_tile_close`,
  `nv_device_remove`, `nv_core_free`, ...) must be called from outside, or it
  could wait on the thread it runs on.
- `nv_tile_decoded` and `nv_tile_decode_failed` may be called from any thread
  (platform decoders answer on their own threads) and never block.
- **Nothing about an object before its ID exists.** No event names a device
  until `nv_device_add` has returned its ID, and no `on_action` for a tile
  runs until `nv_tile_open` has returned its ID. (The core holds them back.)
- After `nv_tile_close` returns, no `on_action` for that tile is running or
  will run. After `nv_core_free` returns, no callback at all.

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

Exactly the pipeline's actions (RENDERING.md), in order, per tile:
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
{"type":"tile_stats","tile":7,"received":120,"decoded":118,"gated":0,"dropped":2,"failures":0,"resets":0}
{"type":"tile_closed","tile":7,"reason":"device_removed"}
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
- `tile_stats` arrives about once a second per tile while it is open, for
  diagnostics overlays; apps may ignore it.

## Logging

The core logs to rotated files in `log_dir` (capped by file count and total
size; defaults from `viewer-log`), rate-limited, with credentials and device
addresses redacted. Apps keep their own logs; nothing is printed to stdout.

## Versioning

`NV_ABI_VERSION` changes on any incompatible change to the header. Apps check
`nv_abi_version()` at startup and refuse to run on a mismatch.

## Builds

- macOS: a static library per architecture, packaged as an XCFramework with
  the header and a module map (`import NVRViewCore` in Swift).
- Windows: a DLL plus import library; C# bindings come later (M4).
- Linux: built and tested in CI so the core stays portable.
