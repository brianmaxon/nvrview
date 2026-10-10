# C interface for recordings, ABI 4 (proposal for milestone P)

> **Proposal, not implemented.** This is the contract for review before
> the core and the apps are built in parallel, as [FFI-ABI3.md](FFI-ABI3.md)
> was for M3. Everything in [FFI.md](FFI.md) and [`nvrview.h`](nvrview.h)
> still holds; on implementation this moves into FFI.md and the header, and
> `NV_ABI_VERSION` becomes 4. Requirements come from
> [PLAYBACK.md](PLAYBACK.md), [UX.md](UX.md), and
> [ARCHITECTURE.md](ARCHITECTURE.md); protocol facts from
> [protocol.md](protocol.md) ("Recordings: search and download",
> "Playback", "Cover images", "Replay media headers").

## Goals

1. **Everything PLAYBACK.md promises, through one interface** that the
   macOS app, a Windows app, and later the local API share: search by event
   type, incidents across cameras with the user's edits, background
   resumable downloads, cover thumbnails, synchronized multi-camera
   playback, and ready-made edits.
2. **Recorded video looks like live video to the app.** Playback tiles are
   ordinary tiles: the same `on_action`, the same pipeline, the same
   answering rules, the same generations. The app's renderer does not learn
   a second path.
3. **The core decides, the app draws.** Index, grouping, scheduling,
   pacing, synchronization, and edit decisions are shared Rust; decoding,
   thumbnails as images, composition, and encoding are the platform's.
4. **Protocol neutral.** No command numbers, file IDs, stream names, search
   handles, or replay sequence numbers cross the boundary. A future RTSP or
   ONVIF backend ([ARCHITECTURE.md](ARCHITECTURE.md), "Other protocols")
   could serve the same calls and report what it cannot do.
5. **Safe for the device.** Read-only commands only, over the device's one
   session, bounded per device, never a new login.

## Decisions

| | Decision | Why |
| --- | --- | --- |
| Version | **ABI 4**: every ABI 3 declaration unchanged, plus the functions, types, status codes, and events below | Apps compare `nv_abi_version()` for equality and refuse a mismatch; there is no minor version, so the only honest signal for "this core has recordings" is a new number. New `NvStatus` values also reach every function's return. The macOS app moves to ABI 4 by bumping the constant; nothing it calls today changes. (A minor version was considered; see open question 1.) |
| Objects | One new object, the **playback** (an ID like devices and tiles). Everything else (index, incidents, downloads, thumbnails) is core-wide state addressed by JSON keys | Recordings are shared by every window (one index per device, one download queue), so they are not owned by a handle an app could leak |
| Cameras | `{"device": "<device key>", "channel": 3, "lens": 0}` everywhere in JSON | Incidents, edits, downloads, and groups persist across runs, so they use the stable device key (FFI.md, "Layouts"), never per-run device IDs. `lens` (0 or 1) replaces the Baichuan stream names for a second lens of a multi-lens camera; it defaults to 0 and may be omitted |
| Tiers | `"main"` or `"sub"` in JSON; `NV_TIER_MAIN`/`NV_TIER_SUB` in C | The core maps lens and tier to the device's stream (lens 1 main is `externStream` on Baichuan). `NV_TIER_MOBILE`/`NV_TIER_EXTERN` are refused for playback tiles |
| Time | **Device-local seconds** (`start`, `end`: integer seconds since 1970-01-01 00:00:00 of the device's wall clock, no zone) for the index, incidents, and downloads; **device-local milliseconds** (`*_ms`) for playback and edit lists | Devices report wall-clock time with no zone (protocol.md "Times"); the core never converts it to UTC (it has no time-zone database). Apps format these numbers as if they were UTC to show the device's own wall time. Integers keep timeline arithmetic trivial |
| Kinds | Event `kind` uses the vocabulary of live `detection` events: `motion` (the device's `md`), then the device's own lowercase names (`people`, `vehicle`, `dog_cat`, `face`, `package`, `visitor`, `pir`, `cry`, ...). Unknown kinds pass through | The same badge code serves live and recorded events. `source` says where an event came from: `device` (the camera or NVR), `mac_video` (on-device vision on the computer), `mac_audio` (on-device sound analysis, section 2a). Sound kinds share the namespace: `smoke_alarm`, `co_alarm`, `siren`, `glass_break`, `dog_bark`, `baby_cry`, ... (section 2a) |
| Results | Synchronous queries answer from memory into `out`/`cap`/`len` (the ABI 3 buffer convention); anything that needs the device returns at once and reports through events | No call waits on the network; the app's main thread may query the index |
| Persistence | Under `NvConfig.data_dir/recordings/`; downloads under a directory the app names | Same atomic-write rules as layouts; without `data_dir`, index and edits live in memory for the run and downloads cannot resume |

## 1. Configuration and per-device enablement

```c
/* Sets the recordings configuration (JSON below). Call once after
 * nv_core_new and again to change it; unknown members are ignored. Starts
 * resuming unfinished downloads from an earlier run (they wait for their
 * devices). NV_ERR_INVALID_ARGUMENT for invalid JSON or values. */
NvStatus nv_recordings_configure(NvCore *core, const char *json);

/* Starts (json non-NULL) or stops (json NULL) indexing one device: which
 * channels and lenses, how far back. The device need not be added yet; the
 * index fills while it is connected. NV_ERR_NOT_CONFIGURED before
 * nv_recordings_configure; NV_ERR_UNSUPPORTED if the device's backend has no
 * recordings. */
NvStatus nv_recordings_enable(NvCore *core, const char *device_key, const char *json);
```

```json
{"downloads_dir": "<absolute path the app chose>",
 "index": {"days_back": 14, "max_age_days": 90, "max_bytes": 67108864,
           "refresh_today_s": 60, "pass_interval_s": 60},
 "grouping": {"gap_s": 10, "margin_s": 5},
 "thumbnails": {"max_entries": 4096, "max_bytes": 67108864},
 "downloads": {"chunk_s": 60, "per_device": 1, "max_attempts": 8},
 "playback": {"max_plays_per_device": 4, "buffer_s": 8, "buffer_bytes": 33554432},
 "naming": {"separator": " — ", "extension": "mp4"}}
```

```json
{"channels": [{"channel": 0}, {"channel": 3, "lens": 1}], "days_back": 14,
 "utc_offset_s": 7200}
```

- Every member is optional; the values shown are the defaults (they are
  the existing Rust defaults). `"channels": "all"` indexes every connected
  channel the `channels` event lists, first lens only.
- `utc_offset_s` is the device's offset from UTC, used only to know which
  day is "today" on the device (`recordings::system_clock`); default: the
  computer's current offset, which the app passes. See open question 6.
- `device_info` gains `"recordings": {"search": true, "playback": true,
  "download": true, "thumbnails": true}` so an app can hide what a backend
  cannot do.

## 2. Index and search

The core keeps one index per device key (`recordings::index::DeviceIndex`,
one file per day plus a calendar), filled in the background by
`recordings::indexer` (newest day first; today refreshed every
`refresh_today_s`), shared by every window. Queries read memory only.

```c
/* Days with recordings per camera in [start, end]. */
NvStatus nv_recordings_calendar(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
/* Continuous recording spans per camera (timeline lanes). */
NvStatus nv_recordings_spans(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
/* Events matching the query, by start. */
NvStatus nv_recordings_events(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
/* Asks for a fresh search of some days now (pull to refresh); returns at
 * once, recordings_changed follows. NV_ERR_LIMIT if more than 31 days. */
NvStatus nv_recordings_refresh(NvCore *core, const char *json);
/* Index state of every enabled device: the recordings_index events' data. */
NvStatus nv_recordings_status(NvCore *core, char *out, size_t cap, size_t *len);
```

Query (all members optional except `start` and `end`):

```json
{"start": 1791590400, "end": 1791676799,
 "cameras": [{"device": "d-3f9a0c1b7e2d4a65", "channel": 3}],
 "kinds": ["people", "vehicle"], "limit": 5000}
```

Answers:

```json
{"revision": 41, "days": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
  "days": [1791504000, 1791590400]}]}

{"revision": 41, "spans": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
  "spans": [{"start": 1791590400, "end": 1791594000, "bytes": 1073741824, "causes": ["sched", "md"], "tier": "main"}]}]}

{"revision": 41, "truncated": false, "events": [
  {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
   "kind": "vehicle", "source": "device", "start": 1791618125, "end": 1791618141}]}

{"refresh": {"device": "d-3f9a0c1b7e2d4a65", "days": [1791590400]}}
```

- `revision` grows with every change to any index; a window re-queries
  when `recordings_changed` reports a newer one. Results larger than `cap`
  return `NV_ERR_BUFFER_TOO_SMALL`; because the index may change between
  two calls, the app loops until the call fits (the second call usually
  does).
- `limit` defaults to 5000 and is capped at 100000 items; `truncated` says
  more matched. A result never exceeds 16 MiB of JSON.
- Spans hide the device's file identities: a span is one recording file's
  range, size, and causes (`causes` uses the device's names, including
  `sched` and `manual`).
- Events of a second lens are reported with `lens` 1 (see "Existing code",
  item 2).

## 2a. Sound: loudness and labels

The official client's playback ignores audio, although recordings carry it
(protocol.md, "Playback"). Sound makes some events easy to find that video
misses or cannot see at all: an alarm sounding in another room, glass
breaking, a dog barking at night. This section adds two things to the index:
a **loudness envelope** per camera, drawn as a lane under the timeline, and
**sound labels**, events with `source` `mac_audio` that filter, group, and
search like device events. It is an aid for finding and labeling events,
never an alarm system (docs/ARCHITECTURE.md, "Sound events and audio
alarms").

Who does what, so the core stays free of platform frameworks:

| Part | Where | Why |
| --- | --- | --- |
| Loudness envelope (peak and RMS per second) | core | plain arithmetic on decoded PCM; the same on every platform |
| Alarm-pattern detector (smoke T3, CO T4: a narrow-band tone near 3 kHz in the standard on/off rhythm) | core | deterministic DSP, no model; tested on synthetic audio; works on Windows too |
| General sound classification (sirens, glass, barking, crying, ...) | app (SoundAnalysis on macOS; an ONNX model on Windows later) | platform frameworks; the app reads 16 kHz mono PCM from the existing audio tap or from a recording and hands the labels to the core |
| Storing, indexing, grouping, search | core | one index per device, shared by every window, like device events |

Sources of audio:

1. **Live**: every tile with audio already decodes it (FFI.md, audio
   level). The core keeps the envelope and runs the alarm detector for every
   live tile, audible or not. For cameras not on screen, the app may ask the
   core to keep their **sub-stream** open for sound only
   (`nv_sound_watch`): one session already carries every channel, and a
   sub-stream costs the device about 110 kbit/s (protocol.md, "Sub and main
   of one camera"). Video of a watched-only stream is not decoded.
2. **Recordings**: when a download finishes, or while a camera plays back,
   its audio goes through the same envelope, detector, and (in the app)
   classifier. An optional background pass over the index fetches older
   audio, newest day first, at most one stream per device and only while
   the device is otherwise idle; it is off by default (open question 13).

```c
/* Keeps a camera's sub-stream open for sound analysis while enabled, with
 * no tile and no video decoding. Counts toward the device's stream limit
 * like a tile. `camera` is {"device","channel","lens"}. */
NvStatus nv_sound_watch(NvCore *core, const char *camera, bool enabled);

/* Loudness lanes for the timeline: per camera, one value per bucket in
 * [start, end]. Answers from memory. */
NvStatus nv_recordings_loudness(NvCore *core, const char *query, char *out, size_t cap, size_t *len);

/* Labels the app's classifier produced, for one camera and a time range.
 * The core merges them into the index as events (source "mac_audio"),
 * replacing earlier labels from the same analyzer for that range, so a
 * re-analysis does not duplicate. */
NvStatus nv_sound_labels_put(NvCore *core, const char *json);
```

Loudness query and answer (`bucket_s` 1 to 3600; the core keeps 1-second
buckets and aggregates on the way out):

```json
{"start": 1791590400, "end": 1791676799, "bucket_s": 60,
 "cameras": [{"device": "d-3f9a0c1b7e2d4a65", "channel": 3}]}

{"revision": 41, "lanes": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
  "bucket_s": 60, "start": 1791590400,
  "peak_db": [-48, -47, -12, null, -50], "rms_db": [-60, -59, -30, null, -61]}]}
```

- Values are dBFS rounded to whole decibels, `null` where no audio was
  analyzed (not "silence"). Stored as one byte per value: a camera costs
  about 170 KB per day, bounded by the index's age and size limits.

Labels from the app:

```json
{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
 "analyzer": "soundanalysis-v1", "start": 1791618000, "end": 1791618600,
 "labels": [{"kind": "dog_bark", "start": 1791618125, "end": 1791618131, "confidence": 0.91,
             "label": "dog_bark"}]}
```

- `kind` is from a short, documented set the apps map their classifier's
  labels onto (`smoke_alarm`, `co_alarm`, `siren`, `car_alarm`,
  `glass_break`, `dog_bark`, `baby_cry`, `shout`, `knock`, `doorbell`);
  `label` keeps the classifier's own name, so nothing is lost and the set
  can grow. Unknown kinds pass through, as for device events.
- The core's own alarm detector writes `smoke_alarm` and `co_alarm` with
  `"analyzer": "pattern-v1"` and no confidence (it matched or it did not).
- Labels are capped per camera and day (default 2000; the lowest
  confidence goes first) so a noisy camera cannot grow the index without
  bound. Labels below the app's confidence threshold are not sent.
- Sound events join incidents like other events (they can start or extend
  one), appear in `nv_recordings_events` with `"source": "mac_audio"`, and
  filter by `kinds`.
- Live detections from sound are also sent as live `detection` events with
  `"source": "mac_audio"`, so badges, triggers, and the notification
  verifier treat them like any other detection, under the same rule: a
  missed sound is never shown as "no alarm".

### Live analysis: what is implemented (ABI 3)

The core runs the envelope and the alarm detector on every live tile with
audio (`viewer_core::sound`), on the tile's own thread right after each
record is decoded, on the record's 16 kHz mono form (the samples the tap
carries; `TileAudio::mono_block`). That costs about 10 µs per 64 ms record
(0.016% of one core on Apple M3 Max, `cargo run --release -p viewer-core
--example sound_cost`) and allocates nothing per record (a test with a
counting allocator), so it needs no queue or thread of its own.

- **Alarms** go out as live `detection` events with `source` `mac_audio`
  (docs/FFI.md, "Events"); no ABI change. Several tiles of one channel
  report one episode once; a stalled stream (3 s without audio) or the last
  tile closing ends it.
- **Loudness**: each live tile keeps the last 600 seconds (Rust:
  `CoreConfig::sound.loudness_history_s`, at most 3600) in a ring of one
  byte per value, peak and RMS, about 1.2 KB per tile; seconds are Unix
  seconds, anchored at the wall clock when the stream's first record
  arrived (re-anchored after a discontinuity or a gap). Rust reads it with
  `Core::tile_loudness`.
- **Not done yet**: recordings, `nv_sound_watch`, `nv_sound_labels_put`,
  storing anything in the index, and the C functions below.

Why a pull API for the live envelope and not a `sound_level` event: the
app needs the envelope only while it draws a lane (a tile's level meter
already has `nv_tile_audio_level`), so an event per tile per second would
mostly be thrown away, and with many tiles it would crowd the event queue
that carries state changes. A pull answers from memory when the lane is
drawn, in the same byte form as `nv_recordings_loudness`, and costs nothing
while nobody asks. Adding it to ABI 3 would change the header, so it is
proposed here for ABI 4:

```c
/* A live tile's recent loudness, oldest first, one byte per second and
 * value (0: no audio analyzed; 1..128: -127..0 dBFS, dBFS = byte - 128).
 * Writes at most `cap` values to each of `peak` and `rms` (the newest when
 * more are kept), the Unix second of the first to `*start_s`, and the
 * count to `*len` (0 before any audio). Answers from memory under a brief
 * lock, without allocating; any thread outside callbacks. */
NvStatus nv_tile_loudness(NvCore *core, uint64_t tile, uint8_t *peak, uint8_t *rms,
                          size_t cap, int64_t *start_s, size_t *len);
```

The Rust core already has what it wraps (`Core::tile_loudness`). Once the
index exists, live seconds are also written to it, so
`nv_recordings_loudness` covers live time too and the timeline needs only
that call.

## 3. Incidents and the user's edits

Incidents are derived, not stored: `recordings::incident::group` over the
index's events with the configured grouping and camera groups, then the
user's stored edits replayed in order.

```c
/* Incidents overlapping [start, end], newest first. Query as in section 2
 * (cameras, kinds filter by involvement; "group": name filters by group). */
NvStatus nv_incidents_get(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
/* One incident by key. NV_ERR_UNKNOWN_ID if no incident has it now. */
NvStatus nv_incident_get(NvCore *core, const char *key, char *out, size_t cap, size_t *len);
/* Applies one edit (JSON below), stores it, sends incidents_changed, and
 * returns {"revision":N,"keys":[...]} with the resulting incidents' keys. */
NvStatus nv_incident_edit(NvCore *core, const char *edit, char *out, size_t cap, size_t *len);
/* Camera groups, stored like layouts: {"revision":N,"groups":[...]}. */
NvStatus nv_camera_groups_get(NvCore *core, char *out, size_t cap, size_t *len);
/* Replaces the whole list; regroups; sends camera_groups_changed and
 * incidents_changed. */
NvStatus nv_camera_groups_put(NvCore *core, const char *json);
```

```json
{"revision": 12, "incidents": [
 {"key": "i-1791618125-d-3f9a0c1b7e2d4a65-3-0", "id": "1791618125-9c1e5a2b7d40f316",
  "name": null, "group": "Group 1", "start": 1791618125, "end": 1791618160,
  "margin_s": 5, "kinds": ["vehicle"], "growing": false, "edited": false,
  "cameras": [
   {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0}, "name": "Camera 1",
    "intervals": [[1791618125, 1791618141]], "download": [1791618120, 1791618146], "added": false}],
  "events": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
              "kind": "vehicle", "source": "device", "start": 1791618125, "end": 1791618141}],
  "downloads": ["1791618125-9c1e5a2b7d40f316"]}]}
```

- **Two identifiers.** `id` is the existing content hash
  (`Incident::id`: group, cameras, first start, last end); it changes while
  an incident grows and names its download folder. `key` is new and
  **stable while the incident grows**: its group and its earliest event
  (camera and start). Edits refer to keys, so a rename survives an incident
  that is still getting events.
- `growing` is true while the incident's end is within `gap_s` of the
  device's "now" (more events may join). `edited` is true when a stored
  edit shaped it. `name` is the user's name or null (apps label it from
  time and kinds). Camera `name` is the channel name the device reports,
  or the app's override stored with the camera groups.
- `downloads` lists download IDs made from this incident (section 5).

Edits:

```json
{"op": "merge",         "keys": ["i-...a", "i-...b"]}
{"op": "split",         "key": "i-...a", "at": 1791618140}
{"op": "rename",        "key": "i-...a", "name": "Delivery"}
{"op": "add_camera",    "key": "i-...a", "camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 5}}
{"op": "remove_camera", "key": "i-...a", "camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 5}}
{"op": "revert",        "key": "i-...a"}
```

- Edits map onto the existing functions: `merge`, `split`,
  `Incident::add_camera`, `Incident::remove_camera`, and `Incident::name`.
  `rename` with `null` clears the name; `revert` drops every stored edit
  that shaped the incident.
- Stored in `recordings/incident-edits.json` in order, atomically. After
  every regroup (new events, new groups, new settings) the core replays the
  edits; an edit whose key matches no incident stays stored (its events may
  not be indexed yet) until its time is older than the index's
  `max_age_days`, then it is dropped. At most 10000 edits.
- An edit that cannot apply (a `split` that would leave a part empty, a
  `remove_camera` of the last camera) returns `NV_ERR_INVALID_ARGUMENT` and
  stores nothing; an unknown key returns `NV_ERR_UNKNOWN_ID`.
- A download keeps the incident it was queued with (the scheduler stores
  its own copy); later edits change neither its folder nor its files.

Camera groups:

```json
{"revision": 3, "groups": [{"name": "Group 1", "cameras": [
  {"device": "d-3f9a0c1b7e2d4a65", "channel": 3}, {"device": "d-3f9a0c1b7e2d4a65", "channel": 5, "lens": 1}]}],
 "camera_names": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3}, "name": "Camera 1"}]}
```

- A camera in no group is a group of its own; with no groups at all, every
  camera is one group (existing `group_of`). At most 64 groups of 64
  cameras; names up to 256 bytes; stored in `recordings/groups.json`.
- `camera_names` overrides channel names in incidents and output names
  (optional).

## 4. Cover thumbnails

The core fetches one keyframe per requested range (the device's cover
image) and keeps it in the bounded on-disk cache
(`recordings::thumbs::ThumbnailCache`); the app decodes it, shrinks it, and
may store the small image back in its place.

```c
typedef enum NvThumbKind {
    NV_THUMB_H264_KEYFRAME = 1, /* Annex-B, parameter sets included */
    NV_THUMB_H265_KEYFRAME = 2, /* Annex-B, parameter sets included */
    NV_THUMB_IMAGE = 3,         /* the app's own small image (for example JPEG) */
} NvThumbKind;

typedef struct NvThumbnail {
    NvThumbKind kind;
    uint32_t width;   /* of the picture the bytes decode to */
    uint32_t height;
    int64_t pts_us;   /* the keyframe's stream timestamp */
} NvThumbnail;

/* Copies a cached thumbnail into out. NV_OK: *info and out filled.
 * NV_PENDING: not cached; a fetch is queued (or already was) and a
 * thumbnail event follows. NV_ERR_BUFFER_TOO_SMALL: *len holds the size.
 * Never waits on the device. */
NvStatus nv_thumbnail_get(NvCore *core, const char *key, NvThumbnail *info,
                          uint8_t *out, size_t cap, size_t *len);
/* Replaces the cached entry, usually with NV_THUMB_IMAGE after decoding.
 * Data is copied. NV_ERR_LIMIT if larger than the cache's byte limit. */
NvStatus nv_thumbnail_put(NvCore *core, const char *key, const NvThumbnail *info,
                          const uint8_t *data, size_t len);
/* Drops queued fetches: one key, or every queued fetch when key is NULL
 * (the view scrolled away). A fetch already sent completes and is cached. */
NvStatus nv_thumbnail_cancel(NvCore *core, const char *key);
```

```json
{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
 "tier": "sub", "start": 1791618125, "end": 1791618141}
```

- The key is exactly `recordings::thumbs::ThumbKey` (device, channel, lens
  and tier as the stream, range); an entry matches only the same key.
  Apps ask for an event's range (incident rows) or a slice of a span
  (timeline strip). `tier` defaults to `sub` (smaller keyframes).
- Fetches run through the device's search permit, one at a time per device
  (protocol.md: the official client's one-at-a-time per-hit fetches are why
  its search feels slow, so the queue serves the **newest request first**).
  At most 256 queued per device; beyond that the oldest queued request is
  dropped with a `thumbnail` event `dropped`.
- An empty answer (no recording in the range) is reported as `empty` and is
  not cached, as today.
- `nv_thumbnail_get` may return `NV_ERR_BUFFER_TOO_SMALL` and then
  `NV_PENDING` if the entry was evicted between the calls; the event
  follows as usual.

## 5. Downloads

`recordings::scheduler::Scheduler`, unchanged in behavior: one camera job
per camera, 60 s chunks with retries and resume, one download at a time
per device by default, devices in parallel, and `recordings::finalize::
Mp4Finalizer` joining each camera's chunks into a standard MP4.

```c
/* Queues an incident's download (JSON below) and returns
 * {"download":"<id>","folder":"<absolute path>"}. Queuing the same incident
 * again returns the existing download. NV_ERR_NOT_CONFIGURED without
 * downloads_dir; NV_ERR_UNKNOWN_ID if the key matches no incident. */
NvStatus nv_download_incident(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
/* Pauses or resumes a whole download (camera NULL) or one camera of it. A
 * running chunk is stopped and fetched again on resume. Persisted. */
NvStatus nv_download_pause(NvCore *core, const char *download, const char *camera, bool paused);
/* Queues failed camera jobs again with fresh attempts. */
NvStatus nv_download_retry(NvCore *core, const char *download, const char *camera);
/* Forgets a download with no camera job running (pause first). With
 * delete_partial, removes its .parts folder; finished MP4 files and
 * incident.json are the user's and are never deleted by the core. */
NvStatus nv_download_forget(NvCore *core, const char *download, bool delete_partial);
/* Every known download: {"downloads":[...]} as in the download event. */
NvStatus nv_downloads_get(NvCore *core, char *out, size_t cap, size_t *len);
```

```json
{"incident": "i-1791618125-d-3f9a0c1b7e2d4a65-3-0",
 "cameras": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3}, "tier": "main"}]}
```

- `cameras` is optional (default: every camera of the incident, main
  tier). Names come from camera names (section 3).
- The download ID is the incident's `id` at queue time (the scheduler's
  job key and folder).
- **Resume across restarts**: `nv_recordings_configure` picks up every job
  file under `downloads_dir` (`Scheduler::resume_all`); jobs wait until
  their device is added and connected, and chunks already complete are
  never fetched again. Paused state survives restarts.
- **Output**: `<downloads_dir>/<incident folder>/<camera file>.mp4` plus
  `incident.json` (`recordings::naming`); the MP4 path is reported in each
  camera's `output` once `done`, and only then exists (finalize never
  leaves a partial file under the output name).

```json
{"type": "download", "download": "1791618125-9c1e5a2b7d40f316", "incident": "i-...",
 "folder": "<downloads_dir>/2026-10-10 07.42 vehicle — Camera 1, Camera 2",
 "state": "running", "paused": false,
 "cameras": [
  {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0}, "name": "Camera 1",
   "state": "running", "paused": false, "chunks_done": 1, "chunks_total": 2,
   "bytes": 20971520, "bytes_estimate": 41943040, "output": null},
  {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 5, "lens": 0}, "name": "Camera 2",
   "state": "failed", "paused": false, "chunks_done": 0, "chunks_total": 2, "bytes": 0,
   "reason": "stalled", "detail": "no data for 10 s (attempt 8 of 8)", "output": null}]}
```

- Download `state`: `queued`, `running`, `paused`, `finalizing`, `done`,
  `failed` (some camera failed and none is still working), derived from the
  cameras. Camera `state`: `waiting_device` (its device is not added or
  not connected), `queued`, `running`, `finalizing`, `done`, `failed`.
- `reason` (with `failed`): `stalled`, `session_closed`, `refused` (the
  device answered with an error), `no_recording` (the range held no
  video), `write_failed` (disk full or not writable), `join_failed` (for
  example a codec change inside the range), `login_failed`. `detail` is
  English for logs and diagnostics, not for parsing.
- `bytes_estimate` is the device's own size estimate for the running
  chunk added to the bytes done, when known.
- Events are coalesced: at most 4 per second per download (the newest
  state wins), plus one on every state change. `download_removed` follows
  `nv_download_forget`.

## 6. Recorded playback

A **playback** is a set of cameras on one shared clock, each shown by a
playback tile. Playback tiles are tiles (FFI.md, "Objects"): IDs from the
same space, `on_action` with the same actions and answering rules,
`nv_tile_decoded` and `nv_tile_decode_failed` with generations,
`nv_tile_set_tier`, `nv_tile_audio_set`, `nv_tile_audio_tap`,
`nv_tile_audio_level`, `tile_stats`, and `tile_closed`.

```c
typedef enum NvSeekMode {
    NV_SEEK_KEYFRAME = 0, /* show the keyframe at or before the position (fast; scrubbing) */
    NV_SEEK_EXACT = 1,    /* decode from that keyframe, show the picture at the position */
} NvSeekMode;

typedef enum NvPlaybackState {
    NV_PLAYBACK_BUFFERING = 0,
    NV_PLAYBACK_PLAYING = 1,
    NV_PLAYBACK_PAUSED = 2,
    NV_PLAYBACK_ENDED = 3,
    NV_PLAYBACK_FAILED = 4,
} NvPlaybackState;

/* Opens a playback (JSON below), paused at "position_ms" with an exact
 * seek. Writes one tile ID per camera, in request order, to out_tiles
 * (tile_cap entries; *tile_count receives the number). NV_ERR_LIMIT beyond
 * the playback, camera, or per-device play limits; NV_ERR_UNSUPPORTED if a
 * device cannot play back. */
NvStatus nv_playback_open(NvCore *core, const char *json, uint64_t *out_playback,
                          uint64_t *out_tiles, size_t tile_cap, size_t *tile_count);
/* Adds a camera ({"camera":{...},"tier":"sub","offset_ms":0}); it joins at
 * the current position. */
NvStatus nv_playback_add_camera(NvCore *core, uint64_t playback, const char *json, uint64_t *out_tile);
/* Removes one camera's tile (tile_closed follows, reason "closed"). */
NvStatus nv_playback_remove_camera(NvCore *core, uint64_t playback, uint64_t tile);
NvStatus nv_playback_play(NvCore *core, uint64_t playback);
NvStatus nv_playback_pause(NvCore *core, uint64_t playback);
/* Moves the shared clock. Returns at once; a newer seek replaces one still
 * starting. Clamped to the playback's range. */
NvStatus nv_playback_seek(NvCore *core, uint64_t playback, int64_t position_ms, NvSeekMode mode);
/* Forward rate, 0.25 to 4. Audio plays at 1 only. */
NvStatus nv_playback_set_rate(NvCore *core, uint64_t playback, double rate);
/* While paused: +1 shows the next picture of every camera; -1 is an exact
 * seek one picture back (slower: decodes from the previous keyframe). */
NvStatus nv_playback_step(NvCore *core, uint64_t playback, int32_t frames);
/* Per-camera clock offset: this camera shows position + offset_ms. */
NvStatus nv_playback_set_offset(NvCore *core, uint64_t playback, uint64_t tile, int64_t offset_ms);
/* Current position and state for the scrubber, at display rate. Lock-free;
 * callable from any thread, including callbacks. */
NvStatus nv_playback_position(NvCore *core, uint64_t playback, int64_t *position_ms,
                              NvPlaybackState *state);
/* Stops every play, closes every tile of it (tile_closed for each), then
 * playback_closed. Never call it from a callback. */
NvStatus nv_playback_close(NvCore *core, uint64_t playback);
```

```json
{"start_ms": 1791618120000, "end_ms": 1791618146000, "position_ms": 1791618120000,
 "rate": 1.0, "sync": "wait",
 "cameras": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3}, "tier": "sub", "offset_ms": 0},
             {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 5}, "tier": "sub", "offset_ms": 0}]}
```

Or, for an incident: `{"incident": "<key>", "tier": "sub"}` takes the
incident's cameras and its range with the margins.

**The shared clock.** Position is device-local milliseconds. While
playing, it advances at `rate` on the core's clock (`nv_clock_now_ns`).
Each tile's pictures are released as `Decode` actions when the clock
reaches their recorded time (plus the camera's offset), less that tile's
measured decode latency (the time from `Decode` to `nv_tile_decoded`,
tracked per tile), so a renderer that shows the newest decoded picture, as
both platforms do (RENDERING.md), shows each picture on time. The app does
no pacing. A picture's recorded time comes from the stream: the device's
wall-clock seconds at each keyframe plus the stream timestamps after it
(see "Existing code", item 5, and open question 4).

**Generations for seeks.** Every seek starts a new generation of every
tile of the playback, exactly as a tier switch does (FFI.md, "Switching a
tile's stream tier"): the old generation keeps its last picture on screen
while the new one decodes; when the new generation's picture **at the
target** has been answered with `nv_tile_decoded`, the old generation gets
its final `NV_ACTION_RESET`, which is the moment to show the new one. For
`NV_SEEK_EXACT`, the pictures from the keyframe up to the target are sent
as fast as the decoder answers, before the switch, so they are never
shown. A newer seek replaces one still starting (the superseded generation
ends with its final Reset). Tier switches on a playback tile restart that
tile's play at the current position in the new tier, also as a new
generation.

- **Generation 0 of a playback tile carries no `Decode`**: it is an empty
  placeholder that ends with a final Reset when the opening seek's
  generation (1) shows. So one rule holds for every playback generation:
  show it once the previous generation's final Reset has arrived.
- **Keyframe gating** is the pipeline's, per generation: nothing decodes
  before a random access point, and a dropped picture gates to the next.
- **Device traffic per seek** is bounded: per camera at most one start
  in flight; seeks that arrive meanwhile are coalesced to the newest
  target, so dragging the scrubber sends a handful of starts, not one per
  mouse event.

**Synchronization (`sync`).** `wait` (default): while any camera is
buffering at the clock's position, the clock waits for it, up to 2 s; then
it goes on and the late camera catches up at its next keyframe
(`playback_camera` `late`). `free`: the clock never waits; late cameras
drop to their next keyframe. A camera with no recording at the position
gets `NV_ACTION_CLEAR` and `no_recording`, and starts again by itself where
its recording resumes.

**End of range.** At `end_ms` the clock stops, every play is stopped, and
the playback reports `ended`; the last pictures stay on screen. `play`
after `ended` restarts from `start_ms`.

**Buffering and backpressure.** The device sends a recording faster than
real time at first (protocol.md: 2 to 4.4 s of video in the first 0.25 s).
The session never waits for a tile, so each playback tile buffers at most
`buffer_s` seconds or `buffer_bytes` bytes ahead of the clock. When a tile's
buffer is full the core stops that play and starts it again later from the
end of what it holds; frames are never dropped silently ahead of the clock.
Whether the device paces itself after the burst decides how often this
happens (open question 3).

**Audio.** A playback tile's audio follows its shown generation through the
existing audio path (AUDIO.md) at rate 1 while playing; it is silent while
paused, while stepping, and at any other rate. Taps work as for live tiles,
with camera time.

```json
{"type": "playback_state", "playback": 3, "state": "playing", "position_ms": 1791618127000, "rate": 1.0}
{"type": "playback_camera", "playback": 3, "tile": 11, "state": "buffering"}
{"type": "playback_camera", "playback": 3, "tile": 11, "state": "no_recording"}
{"type": "playback_camera", "playback": 3, "tile": 12, "state": "failed", "reason": "start_rejected"}
{"type": "playback_seek", "playback": 3, "position_ms": 1791618130000, "generation": 4, "state": "active"}
{"type": "playback_closed", "playback": 3, "reason": "closed"}
```

- `playback_state` arrives on every state change and after every seek
  completes; it is not periodic (`nv_playback_position` serves the
  scrubber). `state` as `NvPlaybackState`.
- `playback_camera` `state`: `buffering`, `playing`, `late`,
  `no_recording`, `ended`, `failed` (`reason`: `start_rejected`,
  `stream_ended`, `undecodable`, `session_closed`, `limit`).
- `playback_seek` `state`: `starting`, `active`, `failed`, with the seek's
  generation (the same for every tile of the playback).
- `playback_closed` `reason`: `closed`, `device_removed` (the playback had
  one device and it was removed; with several devices only that device's
  tiles close), `login_failed`, `core_shutdown`.
- `nv_tile_close` on a playback tile returns `NV_ERR_INVALID_ARGUMENT`; use
  `nv_playback_remove_camera`.

## 7. Edit decision lists (follow cut, 2-up, 4-up)

The core computes the decisions as data; the app renders and encodes them
(Metal and VideoToolbox on macOS, Direct3D 11 and Media Foundation on
Windows), so both platforms produce the same edit.

```c
/* Computes an edit list for a downloaded incident (request below) and
 * returns it as JSON. Pure computation over the incident and its finished
 * files; no device traffic. NV_ERR_UNKNOWN_ID if the download is unknown;
 * NV_ERR_INVALID_ARGUMENT if no camera of the request has a finished file. */
NvStatus nv_edit_compute(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
```

```json
{"kind": "follow", "download": "1791618125-9c1e5a2b7d40f316",
 "min_hold_ms": 2000, "pre_roll_ms": 1000, "cut": "keyframe",
 "priority": [{"device": "d-3f9a0c1b7e2d4a65", "channel": 5}], "labels": {"name": true, "time": true}}

{"kind": "grid", "download": "1791618125-9c1e5a2b7d40f316", "layout": "4up",
 "canvas": {"width": 1920, "height": 1080}, "fit": "fit",
 "offsets_ms": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 5}, "offset_ms": -400}]}
```

```json
{"version": 1, "kind": "follow", "download": "1791618125-9c1e5a2b7d40f316",
 "start_ms": 1791618120000, "end_ms": 1791618146000,
 "suggested_file": "<folder>/2026-10-10 07.42.00 follow.mp4",
 "copy_possible": true,
 "sources": [{"source": 0, "camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
              "name": "Camera 1", "file": "<folder>/2026-10-10 07.42.00 Camera 1.mp4",
              "file_start_ms": 1791618119400, "offset_ms": 0,
              "codec": "h264", "width": 2560, "height": 1440}],
 "segments": [{"start_ms": 1791618120000, "end_ms": 1791618131000, "source": 0, "source_in_ms": 600},
              {"start_ms": 1791618131000, "end_ms": 1791618146000, "source": 1, "source_in_ms": 4100}],
 "labels": [{"source": 0, "name": "Camera 1", "time": true}]}
```

For `grid`, `segments` is replaced by cells that run for the whole range:

```json
{"canvas": {"width": 1920, "height": 1080},
 "cells": [{"rect": {"x": 0, "y": 0, "width": 960, "height": 540}, "source": 0, "fit": "fit",
            "present": [[1791618120000, 1791618146000]]}]}
```

- **Follow cut**: from each camera's event intervals in the incident, the
  camera "with the action" at each moment; overlaps go to `priority` order,
  then to the event that started last; no shot shorter than `min_hold_ms`;
  each switch moves `pre_roll_ms` earlier so the subject is seen entering.
  With `cut` `keyframe`, every switch is moved to the next keyframe of the
  incoming source (read from the finished MP4's sync samples), and
  `copy_possible` says the edit can be written without re-encoding when
  all sources share codec and parameter sets; `frame` cuts exactly and
  needs a re-encode.
- **Grid**: `2up` or `4up` (or `auto`: 2 for two cameras, 4 for three or
  four); cells in the order of `cameras` or the incident's; `present` lists
  where the source has video (the cell is black elsewhere). Source
  rectangles inside cells come from the same `nv_grid_layout` the live grid
  uses.
- All times are device-local milliseconds; `source_in_ms` is the offset
  into the source file. `file_start_ms` is the recorded time of the file's
  first picture (new; see "Existing code", item 6). `offset_ms` is the
  user's correction for clocks that disagree, the same as in playback.
- The app writes the output next to the incident's files; the core
  suggests the name. An edit list holds no pixels and is at most a few
  hundred KB.

## 8. Event summary

Every new event, in addition to FFI.md's list. As there, unknown types and
fields must be ignored.

```json
{"type": "recordings_index", "device": "d-3f9a0c1b7e2d4a65", "state": "filling", "days_done": 3, "days_total": 14, "revision": 40}
{"type": "recordings_changed", "device": "d-3f9a0c1b7e2d4a65", "revision": 41, "start": 1791590400, "end": 1791676799}
{"type": "incidents_changed", "revision": 12}
{"type": "camera_groups_changed", "revision": 3}
{"type": "thumbnail", "key": {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0}, "tier": "sub", "start": 1791618125, "end": 1791618141}, "state": "ready"}
{"type": "download", "download": "1791618125-9c1e5a2b7d40f316", "state": "running", "cameras": []}
{"type": "download_removed", "download": "1791618125-9c1e5a2b7d40f316"}
{"type": "playback_state", "playback": 3, "state": "paused", "position_ms": 1791618120000, "rate": 1.0}
{"type": "playback_camera", "playback": 3, "tile": 11, "state": "playing"}
{"type": "playback_seek", "playback": 3, "position_ms": 1791618130000, "generation": 2, "state": "starting"}
{"type": "playback_closed", "playback": 3, "reason": "closed"}
{"type": "sound_watch", "camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 5, "lens": 0}, "state": "watching"}
```

- `sound_watch` `state`: `watching`, `stopped`, `refused` (`reason`: the
  device's stream limit, or the camera has no audio).
- `recordings_index` `state`: `idle` (up to date), `filling`,
  `refreshing`, `waiting_device`, `failed` (`reason`; retried next pass).
  Sent on state changes and at most once a second while filling.
- `recordings_changed` names the range of days that changed;
  `incidents_changed` follows when incidents in it changed.
- `thumbnail` `state`: `ready` (call `nv_thumbnail_get`), `empty`,
  `failed` (`reason`), `dropped`.

## 9. Guarantees

**Threads.**

- Queries, edits, groups, configuration, and download control take short
  locks on in-memory state and never wait on the network or on a device
  permit; they may be called from any thread except inside callbacks
  (FFI.md's rule). Device work happens on core threads and reports by
  event.
- `nv_playback_position` joins `nv_tile_decoded`, `nv_tile_decode_failed`,
  and `nv_tile_audio_level` as callable from inside callbacks: it reads two
  atomics.
- Playback tiles follow every tile rule: `on_action` serialized per tile,
  no action before `nv_playback_open` or `nv_playback_add_camera` has
  returned the tile's ID, none after `nv_playback_remove_camera` or
  `nv_playback_close` returns, `tile_closed` exactly once. No event names a
  playback before `nv_playback_open` returns.
- After `nv_core_free` returns, nothing runs: indexers, thumbnail fetches,
  downloads (stopped; job files keep the progress), and plays (stopped) are
  all ended first.

**Memory.**

- JSON and buffers in follow the ABI 3 rules: UTF-8, NUL-terminated, copied
  before the call returns. Results come back through `out`/`cap`/`len` with
  `NV_ERR_BUFFER_TOO_SMALL`. Thumbnail bytes are copied into the app's
  buffer; the core keeps no pointer into app memory.
- Paths in results are absolute, UTF-8. The core writes only under
  `data_dir` and `downloads_dir`.

**Limits** (all configurable where marked; beyond a limit, `NV_ERR_LIMIT`
or a counted drop, never growth):

| What | Limit |
| --- | --- |
| Open playbacks | 8 |
| Cameras per playback | 16 |
| Concurrent plays per device | 4 (`max_plays_per_device`; see open question 2) |
| Playback buffer per tile | 8 s or 32 MiB (`buffer_s`, `buffer_bytes`) |
| Queued thumbnail fetches per device | 256, newest first, oldest dropped |
| Thumbnail cache | 4096 entries, 64 MiB (`thumbnails`) |
| Index per device | 90 days, 64 MiB of day files (`index`) |
| Query result | `limit` up to 100000 items, 16 MiB of JSON |
| Stored incident edits | 10000 |
| Camera groups | 64 groups of 64 cameras |
| Known downloads | 1000 (forget finished ones) |
| Download events | 4 per second per download, coalesced |
| Event queue | FFI.md's (1024; diagnostics only while short) |

**Device safety.**

- Every new device request is viewing tier and read-only. The allowlist
  gains exactly three commands, all observed in the official client's
  playback (protocol.md, "Playback"): **123** (set the start position),
  **5** (play a recording), **7** (stop it). Searches (142, 14–16,
  272–274), cover images (298), and downloads (143/144) are already on it.
- Never sent: **303** (delete recording files) and everything else on the
  never-send list (300, 343, 765, ...); not allowlisted until observed:
  381/382/383, 150, 8/9, 476, 492–494, and the event-log search
  516–519/759/653. The allowlist test keeps 303 among its "never" cases.
- **One session per device**, shared with live view: playback, searches,
  thumbnails, and downloads ride the device's existing session, routed by
  message ID (and, for plays, by the replay sequence number the core
  chooses; it never crosses the interface). Nothing in this interface
  opens a second login. A device that is not connected makes requests wait
  (`waiting_device`), never connect.
- **Login failures** stop everything for that device (no retries, as for
  live view); downloads become `waiting_device`, plays fail with
  `session_closed`, and nothing resumes until the app adds the device
  again with new credentials.
- **Bounded per device**: one search (index, refresh, thumbnails) and one
  download at a time by default (the session's permits), at most
  `max_plays_per_device` plays, one play start in flight per camera, seeks
  coalesced.
- **Every exit path stops plays**: closing a camera, a playback, a device,
  or the core sends 7 for each active play before the session closes,
  as live previews get their stop today.

## 10. Implementation map

| Function or event | Existing Rust | New |
| --- | --- | --- |
| `nv_recordings_configure`, `nv_recordings_enable` | `IndexConfig`, `IndexerConfig`, `GroupingConfig`, `ThumbCacheConfig`, `SchedulerConfig`, `NamePattern`, `system_clock` | a `recordings` owner in `viewer-core` holding one `DeviceIndex` + `Indexer` per device key, the `Scheduler`, the `ThumbnailCache`; config parsing; `device_info.recordings` |
| `nv_recordings_calendar`, `_spans`, `_events` | `DeviceIndex::recorded_days`, `files`, `events` | JSON shaping, `revision`, `limit`, `md` → `motion`, lens in keys |
| `nv_recordings_refresh`, `_status`, `recordings_index`, `recordings_changed` | `run_pass`, `PassReport`, `Indexer` reports | a forced-day request into the indexer; progress counts |
| `nv_incidents_get`, `nv_incident_get` | `group`, `Incident::{intervals, download_ranges, kinds}` | stable `key`, `growing`, regroup on index change |
| `nv_incident_edit` | `merge`, `split`, `add_camera`, `remove_camera`, `name` | the edit log (store, replay, expiry), `revert` |
| `nv_camera_groups_get`/`_put` | `CameraGroup` | store like layouts, camera name overrides |
| `nv_thumbnail_get`/`_put`/`_cancel` | `ThumbnailCache::{get, put}`, `cover_thumbnail` | a per-device newest-first fetch queue with cancel; thumbnail priority over index searches |
| `nv_download_*`, `download` | `Scheduler::{enqueue, set_paused, retry, forget, resume_all, progress}`, `Mp4Finalizer` | per-camera pause and retry, failure reason codes, coalesced events, `delete_partial`, `waiting_device`, device removal |
| `nv_playback_*`, playback events | tiles, `TilePipeline`, generations (`tile.rs`), audio hub, `Subscription`-style bounded queue, replay framing (`encode_replay_media`, `replay_seq`) | playback commands in `bc-protocol` and `bc-session`, play streams routed by message ID and seq, file-boundary handling, the shared clock and paced release, buffering policy, mock-nvr playback |
| `nv_sound_watch`, `sound_watch` | `Subscription`, audio decoding in `TileAudio` | a tile-less subscription that decodes audio only; stream-limit accounting |
| `nv_recordings_loudness`, `nv_sound_labels_put` | the level meter (`audio-pipeline`), `DeviceIndex` day files | the 1-second envelope store, the T3/T4 pattern detector, label merge and caps, sound kinds in incidents and search |
| `nv_edit_compute` | incident intervals, `nv_grid_layout`, `mp4` crate (reading sync samples) | the follow-cut and grid planners |

## 11. Existing code that conflicts or is missing

1. **Allowlist.** `bc-protocol`'s allowlist test lists 5, 7, and 123 as
   never sent ("not needed yet"). Playback needs them moved to the viewing
   tier, with request builders for 123 (`ReplaySeek`), 5 (`FileInfo` by
   file `Id`), and 7, and `bc-session` support for a play stream (start,
   media routed by message ID with the replay `seq`, stop on drop). The
   framer already reads `seq`.
2. **No lens in the index or incidents.** `IndexedEvent`, `IndexedFile`,
   and `DeviceIndex::put_day` are keyed by channel only, and
   `CameraId` is `(device, channel)`. Indexing both lenses of a channel
   (`IndexedChannel { stream: Extern }` beside `Main`) would overwrite one
   with the other in the same day file. Needs a lens in the keys (index
   format version 2; the index is a cache and can be refetched) and in
   `CameraId` (persisted in job files: version the job file).
3. **Incident IDs are not stable while growing** (`compute_id` includes
   the last end), so edits cannot refer to them; the new `key` is needed.
4. **Scheduler granularity.** Pause and retry exist per incident only;
   `CameraState::Failed` carries free text; there is no `remove_device`
   (a removed device's workers keep its old session). Needs per-camera
   pause, reason codes, and device removal.
5. **No recorded time on frames.** `bc_protocol::media::Demuxer` drops the
   video extra header, which carries the keyframe's wall-clock seconds the
   shared clock needs (protocol.md, "Records as the official client reads
   them": reported; the first keyframe's seconds matching `seekTime` is
   observed). Needs the first 4 bytes kept on video records.
6. **Downloads do not record their first picture's time.** A file starts
   at the keyframe at or before the range start; `incident.json` stores the
   requested range only. The edit list needs `file_start_ms` per file.
7. **Thumbnails share the search permit** with the indexer (a FIFO
   semaphore), so a long indexing pass delays every thumbnail. Needs a
   priority (thumbnails and user refreshes before background passes).
8. **`viewer-core` does not use `recordings` yet**, and mock-nvr has no
   playback (123/5/7). Both are needed before playback can be tested
   without hardware.
9. **`NvStreamTier` carries Baichuan tiers** (`MOBILE`, `EXTERN`) in ABI 3.
   This proposal keeps them for live tiles and uses lens + main/sub for
   everything new; open question 7.

## 12. A Windows app

- **Bindings.** csbindgen generates P/Invoke declarations from the header.
  Strings go in as UTF-8 (`[MarshalAs(UnmanagedType.LPUTF8Str)]` or
  manually encoded `byte*`); JSON comes back through the buffer loop (rent
  a buffer from `ArrayPool<byte>`, call, on `NV_ERR_BUFFER_TOO_SMALL` rent
  `*len + 1` and call again) and is parsed with `System.Text.Json`.
- **Callbacks.** `on_action` and `on_event` are `[UnmanagedCallersOnly]`
  static functions passed as function pointers; `context` is a `GCHandle`
  to the app's core object. Events are posted to the UI thread's
  `DispatcherQueue`; actions go straight to the tile's decoder object.
- **Playback tiles** are live tiles to the renderer: one Media Foundation
  H.264/HEVC decoder (D3D11-aware) per generation, newest picture per tile,
  one Direct3D 11 pass for the grid, generations switched on the old
  generation's final Reset. No pacing code: the core releases pictures on
  time against `nv_clock_now_ns`, which is QueryPerformanceCounter in ns,
  the clock WASAPI reports.
- **Thumbnails**: decode the keyframe with the same Media Foundation
  decoder (or `Windows.Graphics.Imaging` after decoding), scale, encode a
  small JPEG with WIC, and `nv_thumbnail_put` it back.
- **Edits**: render the edit list with the D3D11 grid pass offscreen and
  encode with an `IMFSinkWriter` and the hardware encoder; a `copy_possible`
  follow cut can be remuxed without decoding.
- **Paths**: the app passes its downloads folder (for example under the
  user's Videos known folder) as UTF-8; output names are already safe for
  Windows (`recordings::naming::sanitize`) and capped at 120 characters per
  component, so paths stay well below the long-path limit in ordinary
  folders.

## 13. Header additions (ABI 4)

```c
#define NV_ABI_VERSION 4

/* NvStatus gains: */
NV_PENDING = 1,                /* accepted; the answer arrives as an event (thumbnails) */
NV_ERR_UNSUPPORTED = -7,       /* the device's backend cannot do this */
NV_ERR_NOT_CONFIGURED = -8,    /* nv_recordings_configure not called, or no downloads_dir */

/* New types: NvThumbKind, NvThumbnail, NvSeekMode, NvPlaybackState
 * (sections 4 and 6). New functions: the nv_recordings_*, nv_incident*,
 * nv_camera_groups_*, nv_thumbnail_*, nv_download*, nv_playback_*,
 * nv_sound_*, nv_tile_loudness, and nv_edit_compute declarations above.
 * NvConfig gains, at its end (zero keeps the default):
 *   bool sound_alarms_off;        -- no smoke and CO alarm detection
 *   uint32_t loudness_history_s;  -- live loudness kept per tile; 0: 600 s
 * No existing function changes. */
```

`NV_PENDING` is the first positive status: apps that test `== NV_OK` treat
it as "not yet", and only `nv_thumbnail_get` returns it.

## Open questions for the user

1. **ABI 4 or additive to 3?** This proposal bumps to 4 because apps check
   for an exact match and there is no minor version. Alternative: add
   `nv_abi_minor()` now, so later additive changes (motion control, the
   local API) do not each force a major bump.
2. **Plays per device and live view on the same session.** The official
   client stops a channel's live view before playing it, and session and
   stream limits are not measured (protocol.md, "Open questions"). This
   proposal keeps live view running and caps plays at 4 per device. Should
   playback instead pause live tiles of the same device when a limit is
   hit, or refuse the play (`limit`) as proposed? Needs a measurement on
   hardware first.
3. **Replay pacing.** Whether the device paces a play after its initial
   burst or relies on TCP decides whether the buffer-full stop/restart
   happens rarely or constantly, and whether rates above 1 can be served
   without keyframe-only playback (command 150, not traced). Is 0.25 to 4
   the right first range, with faster scrubbing by seeks?
4. **Synchronization accuracy.** Recorded times anchor on whole seconds at
   keyframes, so two cameras may be up to about a second apart before the
   user nudges `offset_ms`. Is that acceptable for the first version, or
   should the core refine it (for example from where the seconds count
   changes between keyframes, or from audio)? Should offsets the user sets
   be remembered per camera?
5. **How a play ends at a file boundary** is unknown (protocol.md). The
   core will start the next file itself at its start; a short stall at
   each boundary (files run 30 to 60 minutes) may be visible. Acceptable?
6. **Device time zone.** The core needs the device's "today" for indexing;
   this proposal has the app pass `utc_offset_s` (default: the computer's
   offset). Should the core read the device's own time settings instead?
   That needs a read-only query observed and added to protocol.md first.
7. **Clean up `NvStreamTier`?** ABI 4 could replace `NV_TIER_MOBILE` and
   `NV_TIER_EXTERN` with a lens argument on `nv_tile_open` (protocol
   neutrality), at the cost of one change to the macOS app.
8. **Indexing by default.** This proposal indexes nothing until
   `nv_recordings_enable`. Should an app enable every connected channel by
   default (more device searches at first start), or ask the user?
9. **Deleting downloads.** `nv_download_forget` removes only `.parts`;
   finished files are left to the app (move to Trash or Recycle Bin).
   Should the core offer deleting a whole incident folder?
10. **Animal kinds.** Until the device's animal type names are observed,
    `dog_cat` is the only animal kind. Should the follow cut and filters
    treat `dog_cat`, and later on-device `animal` detections, as one
    "animal" filter in the core, or leave grouping of kinds to the apps?
11. **Edits beyond follow cut and grids** (picture-in-picture, a chosen
    camera order per incident, labels with event kinds): in scope for the
    first `nv_edit_compute`, or later?
12. **Watching sound on cameras that are not on screen.** Keeping every
    camera's sub-stream open for sound uses one stream per camera on the
    device. Should the app watch all cameras with audio by default, only
    the ones the user picks, or none until sound features are turned on?
13. **Background analysis of older recordings.** Off by default as
    proposed (it fetches audio from the device), on when the user asks for
    a day or camera, or a slow default pass over the last few days?
14. **The sound kind set.** The proposed ten kinds plus the classifier's
    own label: enough for the first version? Should "cry" from the device
    and `baby_cry` from the computer be one filter?
