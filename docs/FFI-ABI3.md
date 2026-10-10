# C interface, ABI 3 (planned for M3)

> **Implemented.** The contract now lives in [FFI.md](FFI.md) and
> [`nvrview.h`](nvrview.h) (`NV_ABI_VERSION` 3); this page is the plan as
> agreed. Where the implementation differs:
>
> 1. `nv_tile_decoded` and `nv_tile_decode_failed` take the action's
>    `generation` (third argument): during a switch two decoders answer,
>    and both streams of a camera can carry the same timestamps.
> 2. The switch reaches the action path too: the old generation gets a
>    final `NV_ACTION_RESET`, ordered with the new generation's actions.
>    A generation that fails also ends with its own Reset.
> 3. `tile_tier` `failed` carries a `reason`; asking for the tier already
>    requested reports `active` again instead of starting anything.
> 4. Detection events also have `alarm` (other device states, such as a
>    doorbell press) and an optional `lens` (second lens of a multi-lens
>    camera); kinds are lowercase.
> 5. `nv_tile_audio_level` is measured before the tile's volume and reads 0
>    for a tile that is not audible or not known (it never fails for an
>    unknown tile: it takes no lock to check).
> 6. Layouts: the core needs a directory, `NvConfig.data_dir` (new, last
>    member); `nv_layouts_get` returns `{"revision":N,"layouts":[...]}` and
>    uses `NV_ERR_BUFFER_TOO_SMALL` (new) to ask for a larger buffer; the
>    core sets `version` itself; `layouts_changed` carries `revision`.
>    Device keys come from `nv_device_key` (new) and `key` in
>    `device_info`, one per device address.

The additions M3 needs, agreed before the core and the app are built in
parallel. Everything in [FFI.md](FFI.md) still holds; on implementation this
moves into FFI.md and `docs/nvrview.h`, and `NV_ABI_VERSION` becomes 3.
Requirements come from [UX.md](UX.md) and [ARCHITECTURE.md](ARCHITECTURE.md).

## 1. Switching a tile's stream tier (focus and full screen)

```c
/* Asks the tile to show `tier` (for example main on focus, sub back in the
 * grid). The tile keeps presenting its current stream until the new one's
 * first keyframe has been decoded, then switches in one step: no gap, no
 * black frame. Returns at once; progress arrives as events. */
NvStatus nv_tile_set_tier(NvCore *core, uint64_t tile, NvStreamTier tier);
```

- The core starts the new tier's stream (shared with any other tile already
  showing it), runs a second pipeline for it, and keeps the old one's
  actions flowing until the new pipeline's first `DECODE` has been answered
  with `nv_tile_decoded`. Then it sends the new pipeline's actions only and
  stops the old stream if no other tile uses it.
- While both run, the app sees actions for both. Every action gains a
  `generation` field (`NvAction.generation`, `uint32_t`, starts at 0 and
  increases with each switch) so the app routes them to two decoders and
  shows the new generation's pictures once the core announces the switch.
- Events: `{"type":"tile_tier","tile":7,"tier":"main","state":"starting"|"active"|"failed","generation":2}`.
- Order the observed official client uses and that we keep: start the new
  tier, then stop the old one.

## 2. Detection events

The core subscribes on every device session (command 192 right after login,
docs/protocol.md "Detection events") and turns the state pushes (command 33)
into start and stop events per channel and type:

```json
{"type":"detection","device":1,"channel":3,"kind":"people","state":"start","source":"device","at_ns":123}
{"type":"detection","device":1,"channel":3,"kind":"people","state":"stop","source":"device","at_ns":456}
{"type":"motion","device":1,"channel":5,"state":"start","at_ns":789}
```

- `kind` uses the device's names (`people`, `vehicle`, `dog_cat`, `face`,
  `package`, ...); apps map them to labels. Unknown kinds are passed through.
- `source` is `device` here; on-device models (Core ML) report through the
  same event later with `source` `"local"`.
- `at_ns` is the core clock at arrival (the pushes carry no usable time).
- Channel capabilities (`ai` bits from 199) are added to the `channels`
  event: `"detects":["people","vehicle","dog_cat"]`.

## 3. Audio level for the meter

`audio_stats` (already sent about once a second) is too slow for a meter.
Add a level to the render path the app already calls:

```c
/* Latest peak and RMS level of a tile's audio, 0..1, updated by
 * nv_audio_render. Lock-free; callable from any thread at display rate. */
NvStatus nv_tile_audio_level(NvCore *core, uint64_t tile, float *peak, float *rms);
```

## 4. Layouts in the core

Layouts are split trees (UX.md), stored by the core so the macOS app, a
Windows app, and a future web viewer share them:

```c
/* JSON in, JSON out; the core validates, stores, and versions them. */
NvStatus nv_layouts_get(NvCore *core, char *out, size_t cap, size_t *len);
NvStatus nv_layout_put(NvCore *core, const char *json);   /* insert or replace by id */
NvStatus nv_layout_delete(NvCore *core, const char *id);
```

```json
{"id":"entrances","name":"Entrances","version":1,
 "root":{"split":"row","ratio":0.62,
   "a":{"camera":{"device":"<device key>","channel":3}},
   "b":{"split":"col","ratio":0.34,"a":{"camera":null},"b":{"camera":{"device":"<device key>","channel":9}}}}}
```

- Device keys are stable identifiers the core assigns per configured device
  (not the per-run device IDs).
- Storage: a small JSON file in the app's data directory, written
  atomically; at most a few hundred layouts.
- Event `{"type":"layouts_changed"}` after any change, so several windows
  stay in step.

## 5. Timing hooks for the budgets

```json
{"type":"timing","what":"first_picture","tile":7,"ns":123456789}
{"type":"timing","what":"tier_switch","tile":7,"ns":2345678}
```

- `first_picture`: from `nv_core_new` to the first `nv_tile_decoded` of the
  first tile. The app adds its own launch-to-`nv_core_new` time.
- `tier_switch`: from `nv_tile_set_tier` to the switch.

## 6. Several devices

Already supported by the core; M3 exercises it with two NVRs at once. No
interface change beyond device keys (section 4).

## Not in ABI 3

Recordings (search, incidents, downloads) get their own interface in
milestone P; motion control and the local API come later.
