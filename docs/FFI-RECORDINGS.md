# C interface for recordings, ABI 3 minor 4 (proposal for milestone P)

> **Proposal, not implemented.** This is the contract for review before
> the core and the apps are built in parallel, as [FFI-ABI3.md](FFI-ABI3.md)
> was for M3. Everything in [FFI.md](FFI.md) and [`nvrview.h`](nvrview.h)
> still holds; on implementation this moves into FFI.md and the header as
> **ABI 3, minor 4**: `NV_ABI_VERSION` stays 3 and `NV_ABI_MINOR` becomes 4
> (minors 1 to 3 are discovery, updates, and `nv_tile_retry`; FFI.md,
> "Versioning"). Requirements come from
> [PLAYBACK.md](PLAYBACK.md), [UX.md](UX.md), and
> [ARCHITECTURE.md](ARCHITECTURE.md); protocol facts from
> [protocol.md](protocol.md) ("Recordings: search and download",
> "Playback", "Cover images", "Replay media headers").

## Goals

1. **Everything PLAYBACK.md promises, through one interface** that the
   macOS app, a Windows app, and later the local API share: search by event
   type over an index that fills on demand, incidents across cameras with
   the user's edits and locks, resumable downloads that run beside live
   view, cover thumbnails, synchronized multi-camera playback, ready-made
   edits, and exports (trims, time ranges, still images, share-safe
   output).
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
| Version | **ABI 3, minor 4**: every ABI 3 declaration unchanged, plus the functions, types, status codes, and events below | Decision 1: the major number changes only for incompatible changes, and recordings are additions, so they take the next minor (`NV_ABI_MINOR` 4; minors 1 to 3 are taken). An app checks `nv_abi_minor() >= 4` before calling any function here. New `NvStatus` values can reach an older app only through these new functions, and no existing struct changes size (section 13) |
| Objects | One new object, the **playback** (an ID like devices and tiles). Everything else (index, incidents, downloads, thumbnails) is core-wide state addressed by JSON keys | Recordings are shared by every window (one index per device, one download queue), so they are not owned by a handle an app could leak |
| Cameras | `{"device": "<device key>", "channel": 3, "lens": 0}` everywhere in JSON | Incidents, edits, downloads, and groups persist across runs, so they use the stable device key (FFI.md, "Layouts"), never per-run device IDs. `lens` (0 or 1) replaces the Baichuan stream names for a second lens of a multi-lens camera; it defaults to 0 and may be omitted |
| Tiers | `"main"` or `"sub"` in JSON; `NV_TIER_MAIN`/`NV_TIER_SUB` in C | The core maps lens and tier to the device's stream (lens 1 main is `externStream` on Baichuan). `NV_TIER_MOBILE`/`NV_TIER_EXTERN` are refused for playback tiles |
| Time | **Device-local seconds** (`start`, `end`: integer seconds since 1970-01-01 00:00:00 of the device's wall clock, no zone) for the index, incidents, and downloads; **device-local milliseconds** (`*_ms`) for playback and edit lists | Devices report wall-clock time with no zone (protocol.md "Times"); the core never converts it to UTC (it has no time-zone database). Apps format these numbers as if they were UTC to show the device's own wall time. Integers keep timeline arithmetic trivial |
| Kinds | Event `kind` uses the vocabulary of live `detection` events: `motion` (the device's `md`), then the device's own lowercase names (`people`, `vehicle`, `dog_cat`, `face`, `package`, `visitor`, `pir`, `cry`, ...). Unknown kinds pass through | The same badge code serves live and recorded events. `source` says where an event came from: `device` (the camera or NVR), `local_video` (on-device vision on the computer), `local_audio` (on-device sound analysis, section 2a). Sound kinds share the namespace: `smoke_alarm`, `co_alarm`, `siren`, `glass_break`, `dog_bark`, `baby_cry`, ... (section 2a) |
| Results | Synchronous queries answer from memory into `out`/`cap`/`len` (the ABI 3 buffer convention); anything that needs the device returns at once and reports through events | No call waits on the network; the app's main thread may query the index |
| Persistence | Under `NvConfig.data_dir/recordings/`; downloads under a directory the app names | Same atomic-write rules as layouts; without `data_dir`, index and edits live in memory for the run and downloads cannot resume |

## 1. Configuration and per-device enablement

```c
/* Sets the recordings configuration (JSON below). Call once after
 * nv_core_new and again to change it; unknown members are ignored. Starts
 * resuming unfinished downloads from an earlier run (they wait for their
 * devices). NV_ERR_INVALID_ARGUMENT for invalid JSON or values. */
NvStatus nv_recordings_configure(NvCore *core, const char *json);

/* Allows (json non-NULL) or stops (json NULL) indexing one device: which
 * channels and lenses may be searched. Nothing is searched until a view
 * asks (nv_recordings_want, nv_recordings_hold_today, a thumbnail), unless
 * the optional "background" block is set. The device need not be added
 * yet. NV_ERR_NOT_CONFIGURED before nv_recordings_configure;
 * NV_ERR_UNSUPPORTED if the device's backend has no recordings. */
NvStatus nv_recordings_enable(NvCore *core, const char *device_key, const char *json);
```

```json
{"downloads_dir": "<absolute path the app chose>",
 "index": {"max_age_days": 90, "max_bytes": 67108864, "refresh_today_s": 60},
 "grouping": {"gap_s": 10, "margin_s": 5},
 "thumbnails": {"max_entries": 4096, "max_bytes": 67108864},
 "downloads": {"chunk_s": 60, "per_device": 1, "max_attempts": 8},
 "playback": {"max_plays_per_device": 2, "buffer_s": 8, "buffer_bytes": 33554432},
 "naming": {"separator": " — ", "extension": "mp4"}}
```

```json
{"channels": [{"channel": 0}, {"channel": 3, "lens": 1}], "utc_offset_s": 7200,
 "background": {"days_back": 14, "pass_interval_s": 60}}
```

- Every member is optional; the values shown are the defaults (they are
  the existing Rust defaults), except `background`, which is **absent by
  default** (Decision 8: no background indexing). `"channels": "all"`
  allows every connected channel the `channels` event lists, first lens
  only.
- `refresh_today_s` is how old today's entry may get while a view holds
  today before it is searched again (`LaneConfig::refresh_today`).
- `background`, when present, fills the last `days_back` days for this
  device, newest first, again every `pass_interval_s` seconds, through the
  device's search lane, after thumbnails (so its days are reported like
  requested ones). It asks only while the session is connected and the
  lane has nothing waiting, so a refused login stops it. It is for people
  who want the index filled ahead of time; apps leave it off unless the
  user turns it on.
- `utc_offset_s` is the device's offset from UTC, used only to know which
  day is "today" on the device; the app passes the computer's current
  offset (Decision 6). The core has no time-zone database: absent means 0.
- `device_info` gains `"recordings": {"search": true, "playback": true,
  "download": true, "thumbnails": true}` so an app can hide what a backend
  cannot do.

## 2. Index and search

The core keeps one index per device key (`recordings::index::DeviceIndex`,
one file per day plus a calendar), shared by every window. It is a cache of
what views asked for (Decision 8), filled **on demand** by the device's
search lane (`recordings::lane`): the days a view asks for with
`nv_recordings_want` (newest day first: each month's calendar, then each
camera-day's files and events), and today again every `refresh_today_s`
while some view holds it with `nv_recordings_hold_today`. Nothing is
searched otherwise, unless the device's `background` block is set. Queries
read memory only and never cause a search: a view asks for what it shows,
then queries.

```c
/* Days with recordings per camera in [start, end]. */
NvStatus nv_recordings_calendar(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
/* Continuous recording spans per camera (timeline lanes). */
NvStatus nv_recordings_spans(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
/* Events matching the query, by start. */
NvStatus nv_recordings_events(NvCore *core, const char *query, char *out, size_t cap, size_t *len);
/* Asks for these cameras' days to be in the index (JSON below); returns at
 * once, recordings_changed follows for each device as days arrive. Days
 * already indexed are not searched again unless "force" (the user's
 * refresh, which also asks the months' calendars again). NV_ERR_LIMIT for
 * more than 31 days or when the device's queue (4096 camera-days) would
 * overflow, and nothing is queued; NV_ERR_INVALID_ARGUMENT if a camera is
 * not allowed by nv_recordings_enable. */
NvStatus nv_recordings_want(NvCore *core, const char *json);
/* Keeps today fresh for these cameras while the hold lasts: hold is any
 * nonzero number the app chooses per window; json {"cameras":[...]} sets
 * or replaces that hold's cameras, NULL releases it. The app releases a
 * window's hold when the window closes or stops showing today; every hold
 * ends with nv_core_free, and a removed device's cameras leave every hold. */
NvStatus nv_recordings_hold_today(NvCore *core, uint64_t hold, const char *json);
/* Index state of every enabled device: the recordings_index events' data. */
NvStatus nv_recordings_status(NvCore *core, char *out, size_t cap, size_t *len);
```

Want (`days` are any second within each day, device-local; `force`
optional, default false):

```json
{"cameras": [{"device": "d-3f9a0c1b7e2d4a65", "channel": 3}],
 "days": [1791590400, 1791504000], "force": false}
```

There is no separate refresh call: pull to refresh is `nv_recordings_want`
with `"force": true`.

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
**sound labels**, events with `source` `local_audio` that filter, group, and
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
   classifier. Older recordings are analyzed only on request, for a day or
   camera the user picks, one stream per device; there is no background
   pass (Decision 13).

```c
/* Keeps a camera's sub-stream open for sound analysis while enabled, with
 * no tile and no video decoding. Counts toward the device's stream limit
 * like a tile. `camera` is {"device","channel","lens"}. */
NvStatus nv_sound_watch(NvCore *core, const char *camera, bool enabled);

/* Loudness lanes for the timeline: per camera, one value per bucket in
 * [start, end]. Answers from memory. */
NvStatus nv_recordings_loudness(NvCore *core, const char *query, char *out, size_t cap, size_t *len);

/* Labels the app's classifier produced, for one camera and a time range.
 * The core merges them into the index as events (source "local_audio"),
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
  one), appear in `nv_recordings_events` with `"source": "local_audio"`, and
  filter by `kinds`.
- Live detections from sound are also sent as live `detection` events with
  `"source": "local_audio"`, so badges, triggers, and the notification
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

- **Alarms** go out as live `detection` events with `source` `local_audio`
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
while nobody asks. It is proposed here as an addition in minor 4:

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
/* Locks (locked true) or unlocks an incident by key (see "Locked
 * incidents" below); stores it, sends incidents_changed, and returns
 * {"revision":N,"keys":[...],"download":"<id>"|null,"folder":"<path>"|null,
 *  "download_status":"queued"|"existing"|"when_settled"|"not_configured"|"limit"}
 * (download members only when locking). NV_ERR_UNKNOWN_ID if no incident
 * has the key; NV_ERR_LIMIT when locking beyond 1000 locks. */
NvStatus nv_incident_lock(NvCore *core, const char *key, bool locked, char *out, size_t cap,
                          size_t *len);
/* Camera groups, stored like layouts: {"revision":N,"groups":[...]}. */
NvStatus nv_camera_groups_get(NvCore *core, char *out, size_t cap, size_t *len);
/* Replaces the whole list; regroups; sends camera_groups_changed and
 * incidents_changed. */
NvStatus nv_camera_groups_put(NvCore *core, const char *json);
```

```json
{"revision": 12, "incidents": [
 {"key": "i-1791618125-d-3f9a0c1b7e2d4a65-3-0-5d2e8a41", "id": "1791618125-9c1e5a2b7d40f316",
  "name": null, "group": "Group 1", "start": 1791618125, "end": 1791618160,
  "margin_s": 5, "kinds": ["vehicle"], "growing": false, "edited": false, "locked": false,
  "cameras": [
   {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0}, "name": "Camera 1",
    "intervals": [[1791618125, 1791618141]], "download": [1791618120, 1791618146], "added": false}],
  "events": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
              "kind": "vehicle", "source": "device", "start": 1791618125, "end": 1791618141}],
  "downloads": ["1791618125-9c1e5a2b7d40f316"]}]}
```

- **Two identifiers.** `id` is the existing content hash
  (`Incident::id`: group, cameras, first start, last end); it changes while
  an incident grows. `key` is **stable while the incident grows**: it is
  made from the incident's earliest event (start, then the lowest camera)
  and, for an incident of a named camera group, a hash of the group's name
  (`Incident::key`). Edits refer to keys, so a rename survives an incident
  that is still getting events.
- **Keys are opaque.** Apps store and pass them back but never parse them
  or build them; the format may change. Because a key includes the group,
  **renaming or deleting a camera group, or moving a camera between
  groups, changes the keys of the incidents in it**: stored edits that
  referred to the old keys no longer match and expire as unmatched edits
  do (below), except those of locked incidents, which follow the lock.
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
  `max_age_days`, then it is dropped. Edits of a **locked** incident never
  expire. At most 10000 edits; a `merge` takes at most 1000 incidents
  (`NV_ERR_LIMIT` beyond either).
- An edit that cannot apply (a `split` that would leave a part empty, a
  `remove_camera` of the last camera) returns `NV_ERR_INVALID_ARGUMENT` and
  stores nothing; an unknown key returns `NV_ERR_UNKNOWN_ID`.
- A download keeps the incident it was queued with (the scheduler stores
  its own copy); later edits change neither its folder nor its files.

### Locked incidents

The NVR overwrites its oldest recordings, and the core's index, edits, and
thumbnails expire after `max_age_days`. A **locked** incident is one the
user wants kept: the core keeps everything it holds about it, and its
video is downloaded, because only a download keeps the video once the NVR
has overwritten it.

- **What a lock keeps.** The incident's stored edits (never expired), its
  index days (the day files of every camera and day its range covers, kept
  past `max_age_days` and exempt from size eviction: they still count
  toward `max_bytes`, but every unlocked day is evicted first), and its
  cover thumbnails (exempt from the cache's least-recently-used eviction,
  at most 16 entries per lock; if locked entries alone exceed the cache's
  limits, those of the oldest lock are evicted first, and the app can make
  covers from the downloaded files). Expiry and eviction never remove
  anything a lock covers.
- **Locking queues the download** of the whole incident (every camera,
  main tier), as `nv_download_incident` would, in the default downloads
  folder. If a download of the incident's key already exists (any state),
  it is returned instead (`download_status` `existing`). A **growing**
  incident's download is queued when it stops growing, so it holds the
  whole incident (`when_settled`; a `download` event follows then). Without
  `downloads_dir` (`not_configured`) or at the download cap (`limit`), the
  lock is still stored and the app tells the user the video is not being
  kept. A locked incident that later grows through edits or regrouping
  does not queue another download by itself; the app can offer one
  (`nv_download_incident` makes a new folder, section 5).
- **Unlocking** makes the incident ordinary again: expiry and eviction
  resume from that moment. It never deletes, pauses, or forgets a download
  and never touches downloaded files.
- **Storage.** `recordings/incident-locks.json`, written atomically like
  the edits, survives restarts. Each **lock** holds the keys of the
  incidents it covers now and its extent: per camera, the time range (with
  margins) those incidents cover. Extents only grow, except by unlocking
  part of a lock (below).
- **How locks follow edits and regrouping.** After every regroup and edit
  replay, an incident is locked when its key is one of a lock's keys or
  when it overlaps a lock's extent (the same camera, overlapping time);
  each lock's keys are then set to the incidents it now covers, and its
  extent grows to include their full ranges. So:
  - **merge**: the merged incident is locked if any part was; locks that
    now cover one incident become one lock (freeing a slot);
  - **split** (and `remove_camera`, and a regroup that cuts an incident):
    every part stays locked, under the same lock;
  - **group rename or delete** (keys change): the incident still overlaps
    its lock's extent, so it stays locked, and the core rewrites the
    stored edits that referred to its old key to the new key, so a locked
    incident keeps its edits;
  - **unlock** of one part removes that incident's ranges from its lock's
    extent and its key from the lock; other parts stay locked; a lock with
    nothing left is dropped. `keys` in the answer lists the incidents whose
    lock state changed.
  A lock whose incidents are not in the index at the moment (its days not
  searched yet in this run) stays stored and keeps its extent's days.
- **Cap: 1000 locks.** At the cap, locking returns `NV_ERR_LIMIT` and
  changes nothing; locks are never dropped by the core to make room.
  Because the parts of a split share their lock, edits and regrouping
  never raise the count; only `nv_incident_lock` does. The extent of one
  lock covers at most 31 days; a lock or merge that would exceed that
  returns `NV_ERR_LIMIT`.

Camera groups:

```json
{"revision": 3, "groups": [{"name": "Group 1", "cameras": [
  {"device": "d-3f9a0c1b7e2d4a65", "channel": 3}, {"device": "d-3f9a0c1b7e2d4a65", "channel": 5, "lens": 1}]}],
 "camera_names": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3}, "name": "Camera 1"}]}
```

- A camera in no group is a group of its own; with no groups at all, every
  camera is one group (existing `group_of`). A camera listed in several
  groups belongs to the first. At most 64 groups of 64 cameras; names up to
  256 bytes; stored in `recordings/groups.json`.
- `camera_names` overrides channel names in incidents and output names
  (optional; at most 4096 entries, names up to 256 bytes).
- Beyond any of these caps `nv_camera_groups_put` returns `NV_ERR_LIMIT`
  and stores nothing.

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

The `key` argument of all three functions is this JSON object (a
NUL-terminated string):

```json
{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
 "tier": "sub", "start": 1791618125, "end": 1791618141}
```

- The core maps it onto `recordings::thumbs::ThumbKey` (device, channel,
  stream, range): lens and tier become the device's stream
  (`DeviceCamera::stream`), so `ThumbKey` has no lens or tier of its own.
  An entry matches only the same camera, tier, and range.
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

`recordings::scheduler::Scheduler`: one camera job per camera, 60 s chunks
with retries and resume, one download at a time per device by default,
devices in parallel, per-camera pause and retry, failure reason codes, and
device removal (jobs wait without spending attempts), with
`recordings::finalize::Mp4Finalizer` joining each camera's chunks into a
standard MP4.

```c
/* Queues an incident's download (JSON below) and returns
 * {"download":"<id>","folder":"<absolute path>","existing":bool}. While a
 * download of the same incident key is unfinished, queuing again returns
 * it (see "Download IDs"). NV_ERR_NOT_CONFIGURED without downloads_dir;
 * NV_ERR_UNKNOWN_ID if the key matches no incident; NV_ERR_LIMIT if 1000
 * downloads are already known. */
NvStatus nv_download_incident(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
/* Pauses or resumes a whole download (camera NULL) or one camera of it;
 * `camera` is a JSON camera object, {"device","channel","lens"} as
 * everywhere (lens optional). A running chunk is stopped and fetched again
 * on resume. Persisted. NV_ERR_UNKNOWN_ID for an unknown download or a
 * camera not in it. */
NvStatus nv_download_pause(NvCore *core, const char *download, const char *camera, bool paused);
/* Queues failed camera jobs again with fresh attempts: every failed
 * camera (camera NULL) or one, given as a JSON camera object. */
NvStatus nv_download_retry(NvCore *core, const char *download, const char *camera);
/* Forgets a download with no camera job running (pause first). With
 * delete_partial, removes its .parts folder; finished MP4 files and
 * incident.json are the user's and are never deleted by the core. */
NvStatus nv_download_forget(NvCore *core, const char *download, bool delete_partial);
/* For moving a download to the Trash (Decision 9): stops its camera jobs,
 * forgets it, and returns {"folder":"<absolute path>"}. The core deletes
 * nothing; the app moves the folder to the Trash or Recycle Bin after the
 * download_removed event for it, which follows once nothing writes there.
 * NV_ERR_UNKNOWN_ID for an unknown download. */
NvStatus nv_download_trash(NvCore *core, const char *download, char *out, size_t cap,
                           size_t *len);
/* Every known download: {"downloads":[...]} as in the download event. No
 * paging: a result larger than cap is NV_ERR_BUFFER_TOO_SMALL and the app
 * loops with a larger buffer (at most 1000 downloads). */
NvStatus nv_downloads_get(NvCore *core, char *out, size_t cap, size_t *len);
```

```json
{"incident": "i-1791618125-d-3f9a0c1b7e2d4a65-3-0",
 "cameras": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3}, "tier": "main"}]}
```

- `cameras` is optional (default: every camera of the incident, main
  tier). Names come from camera names (section 3). `dest` and `share_safe`
  are optional (section 7a); a range download is also in section 7a.
- **Download IDs** are opaque strings. An incident's download is
  identified by the incident's `id` at queue time; queuing decides as
  follows, so one ID never names two downloads:
  1. An **unfinished** download (any state but `done`, including `failed`
     and `paused`) of the same incident **`key`** exists: it is returned,
     even if the incident has grown since (its range stays the one it was
     queued with; use retry for failed cameras).
  2. Otherwise a **finished** download with the same `id` exists (the
     incident has not changed): it is returned.
  3. Otherwise a new download is queued. A finished download stays as it
     is, so queuing an incident that grew after its download finished makes
     a new folder; when the folder name is taken, ` (2)`, ` (3)`, ... is
     appended.
  `existing` in the answer says whether 1 or 2 applied.
- **Trash.** `nv_download_trash` is the only way the interface removes a
  download's files, and it does not remove them: it hands the whole folder
  (finished files, `incident.json`, and any `.parts`) to the app, which
  moves it with the system (`NSFileManager.trashItem` on macOS, the shell's
  recycle operation on Windows). If moving fails, the folder simply stays
  as the user's files. Trashing the download of a locked incident does not
  unlock it, and the lock does not queue it again.
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
  Both are derived by the recordings owner, not stored: the scheduler's
  camera states are `Queued`, `Running`, `ChunksComplete` (shown as
  `finalizing`), `Done`, `Failed`, and `waiting_device` is its separate
  `CameraProgress::waiting_device` flag, shown in place of `queued` or
  `running` while set.
- `reason` (with `failed`): `stalled`, `session_closed`, `refused` (the
  device answered with an error), `no_recording` (the range held no
  video), `write_failed` (disk full or not writable), `join_failed` (for
  example a codec change inside the range), `login_failed`, `other`
  (anything else, and failures recorded by older versions). `detail` is
  English for logs and diagnostics, not for parsing.
- `bytes_estimate` is the device's own size estimate for the running
  chunk added to the bytes done, when known.
- Events are coalesced: at most 4 per second per download (the newest
  state wins), plus one on every state change. `download_removed` follows
  `nv_download_forget` and `nv_download_trash` (with `"trash": true` and
  the `folder`, sent once no job writes into it).

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
(see "Existing code", item 5, and Decision 4).

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
happens; observed: after the burst it sends at roughly real time
(protocol.md, "Playback"; Decision 3).

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
- The app writes the output next to the incident's files, or in the
  request's `dest`; the core suggests the name. An edit list holds no
  pixels and is at most a few hundred KB.
- Every kind takes optional `in_ms` and `out_ms` (device-local ms) to trim
  the result to that part of the range, and `dest` and `share_safe`
  (section 7a).

## 7a. Export

The user takes recordings out of the app in four ways, all planned by the
core and written by the app, as the edits are: the core decides ranges,
edit lists, and names; the app decodes, encodes, and writes video and
images with platform APIs (AVFoundation, VideoToolbox, and ImageIO on
macOS; Media Foundation and WIC on Windows). Nothing here sends a new kind
of request to a device: video comes from finished downloads, and a range
that is not downloaded yet is downloaded first (b).

### (a) Trim

A trim is one more `nv_edit_compute` kind: one output per camera, cut to
in and out points inside a downloaded incident or range download, the
points coming from the incident view or the scrubber.

```json
{"kind": "trim", "download": "1791618125-9c1e5a2b7d40f316",
 "cameras": [{"device": "d-3f9a0c1b7e2d4a65", "channel": 3}],
 "in_ms": 1791618128250, "out_ms": 1791618139500, "cut": "keyframe",
 "dest": "<absolute directory>", "share_safe": false}
```

```json
{"version": 1, "kind": "trim", "download": "1791618125-9c1e5a2b7d40f316",
 "start_ms": 1791618127000, "end_ms": 1791618139500,
 "outputs": [{"source": 0, "camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
              "name": "Camera 1", "file": "<folder>/2026-10-10 07.42.05 Camera 1.mp4",
              "source_in_ms": 7600, "source_out_ms": 20100, "copy_possible": true,
              "suggested_file": "<dest>/2026-10-10 07.42.07 Camera 1 trim.mp4"}]}
```

- `cut` `keyframe` (fast): the in point moves back to the keyframe at or
  before `in_ms` (read from the file's sync samples, as for the follow
  cut), so the moment asked for is included and the app can copy the
  samples without re-encoding (`copy_possible`). `exact`: the output starts
  at the picture at `in_ms`, which needs a re-encode of at least the first
  group of pictures; the app may re-encode the whole output. The out point
  is exact in both (a copy can end on any picture).
- `cameras` is optional (default: every camera of the download with a
  finished file). `start_ms`/`end_ms` are the output's range after the
  keyframe move. `NV_ERR_INVALID_ARGUMENT` when `in_ms >= out_ms` or the
  points lie outside every chosen camera's file.
- To trim while scrubbing a recording that is not downloaded, the app
  first downloads just that range (b), then computes the trim on it.

### (b) Range download

Any time range of chosen cameras, not tied to a detected incident, through
the same scheduler, with the same chunks, retries, resume, pause, retry,
trash, events, and limits as an incident's download.

```c
/* Queues the download of a time range (JSON below) and returns
 * {"download":"<id>","folder":"<absolute path>","existing":bool}.
 * Queuing the same cameras, tiers, and range again returns the existing
 * download. NV_ERR_NOT_CONFIGURED without downloads_dir (or without
 * data_dir when dest is outside it); NV_ERR_LIMIT beyond 16 cameras, 24
 * hours, or 1000 known downloads; NV_ERR_INVALID_ARGUMENT for an empty
 * range or a camera not allowed by nv_recordings_enable. */
NvStatus nv_download_range(NvCore *core, const char *request, char *out, size_t cap,
                           size_t *len);
```

```json
{"start": 1791618100, "end": 1791618400,
 "cameras": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3}, "tier": "main"},
             {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 5}, "tier": "main"}],
 "name": null, "dest": "<absolute directory>", "share_safe": false}
```

- Seconds, device-local, as for incidents; the device widens each chunk to
  the keyframe at or before its start, as for any download.
- **ID**: opaque, `r-<start>-<hash>` of the cameras, tiers, and range, so
  the same request names the same download. No dedupe across overlapping
  but different ranges.
- **Folder**: `<date> <HH.MM>–<HH.MM> range — <camera names>` with the same
  separator, sanitizing, and 120-character cap as incident folders
  (`recordings::naming`), or `<date> <HH.MM>–<HH.MM> <name>` when the user
  named it; ` (2)` and so on when taken. Each camera's file is named as in
  an incident folder. `incident.json` is written with `"kind": "range"`,
  the range, the cameras, and the indexed events inside the range (none if
  the days are not indexed).
- In the `download` event, `incident` is null and `range` holds `start`
  and `end`.

### (c) Still images

The current picture, or one picture per camera, as PNG or JPEG. The core
says which picture and what to call it; the app encodes the image (ImageIO
on macOS, WIC on Windows) and writes it, with no metadata of its own
beyond the image format's required fields.

```c
/* Plans still images (JSON below) and returns their sources and suggested
 * files. Answers from memory; any thread outside callbacks.
 * NV_ERR_UNKNOWN_ID for an unknown playback, tile, or download. */
NvStatus nv_still_plan(NvCore *core, const char *request, char *out, size_t cap, size_t *len);
```

```json
{"playback": 3, "tiles": [11, 12], "format": "png", "dest": "<absolute directory>", "share_safe": false}

{"download": "1791618125-9c1e5a2b7d40f316", "camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3},
 "at_ms": 1791618131250, "exact": true, "format": "jpeg"}
```

```json
{"stills": [{"tile": 11, "camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
             "name": "Camera 1", "generation": 4, "position_ms": 1791618131240,
             "label": {"name": "Camera 1", "time": "2026-10-10 07.42.11.240"},
             "suggested_file": "<dest>/2026-10-10 07.42.11.240 Camera 1.png"}]}

{"stills": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0}, "name": "Camera 1",
             "file": "<folder>/2026-10-10 07.42.05 Camera 1.mp4", "source_in_ms": 12250,
             "position_ms": 1791618131250,
             "suggested_file": "<folder>/2026-10-10 07.42.11.250 Camera 1.jpg"}]}
```

- **From a playback** (`tiles` optional, default every tile; "one per
  camera"): for each tile, the picture it shows now, which is the newest
  picture of its shown generation that the app answered with
  `nv_tile_decoded`; `position_ms` is that picture's recorded time and
  `generation` its generation. The app saves the picture it holds for that
  tile and generation (it has it on screen). Pause first for a set taken
  at one moment; while playing, each tile's picture is its own latest.
- **From a file**: `source_in_ms` is the position in the MP4 of the
  picture at `at_ms` (`exact` true) or of the keyframe at or before it
  (`exact` false, decodes only one picture); the app decodes it from the
  file.
- `format` `png` (default) or `jpeg`; the extension follows it. `label` is
  for an optional caption the app burns in (absent with `share_safe` for
  the name).

### (d) Destination and share-safe output

- **Destination.** Every export request (trims and other edit lists,
  stills, range and incident downloads) takes an optional `dest`, an
  absolute directory the app chose (for example with a save panel).
  Defaults: edit lists and stills from a file go next to their download's
  files; stills from a playback and downloads go to `downloads_dir`. A
  download outside `downloads_dir` is recorded in
  `data_dir/recordings/downloads.json` so it resumes after a restart
  (without `data_dir`, `NV_ERR_NOT_CONFIGURED`). On macOS a sandboxed app
  keeps access with a security-scoped bookmark for as long as the download
  is known.
- **Share-safe** (`"share_safe": true` on any export request) leaves out
  everything that identifies the installation:
  - **Names**: folder and file names use `Camera A`, `Camera B`, ... in the
    request's camera order instead of camera or device names, and leave
    out the user's incident name (time and kinds stay).
  - **Overlays**: `labels` and `label` carry no camera names (time only,
    if asked); the app burns in nothing else.
  - **Device details and `incident.json`**: no `incident.json`, no device
    keys, UIDs, channel numbers, or models in anything written.
  - **MP4 metadata the core writes**: the core's MP4 writer (finalized
    downloads) normally records the creation time and its own handler
    names, and nothing else; for a share-safe download it writes zero
    times and generic handler names. The
    app writes no metadata of its own (no location, device, or encoder
    tags it can omit) into share-safe video and images.
  - A share-safe download's `.parts` folder still holds the job state
    (with device keys) while the download is known; it is removed with
    `nv_download_forget(..., true)` or goes with the folder to the Trash.
    Share the MP4 files, not the folder, until then.

## 8. Event summary

Every new event, in addition to FFI.md's list. As there, unknown types and
fields must be ignored.

```json
{"type": "recordings_index", "device": "d-3f9a0c1b7e2d4a65", "state": "busy", "pending_days": 11, "revision": 40}
{"type": "recordings_day_failed", "device": "d-3f9a0c1b7e2d4a65", "camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0}, "day": 1791504000, "reason": "session closed during the search"}
{"type": "recordings_changed", "device": "d-3f9a0c1b7e2d4a65", "revision": 41, "start": 1791590400, "end": 1791676799}
{"type": "incidents_changed", "revision": 12}
{"type": "camera_groups_changed", "revision": 3}
{"type": "thumbnail", "key": {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0}, "tier": "sub", "start": 1791618125, "end": 1791618141}, "state": "ready"}
{"type": "download", "download": "1791618125-9c1e5a2b7d40f316", "state": "running", "cameras": []}
{"type": "download_removed", "download": "1791618125-9c1e5a2b7d40f316", "trash": false}
{"type": "playback_state", "playback": 3, "state": "paused", "position_ms": 1791618120000, "rate": 1.0}
{"type": "playback_camera", "playback": 3, "tile": 11, "state": "playing"}
{"type": "playback_seek", "playback": 3, "position_ms": 1791618130000, "generation": 2, "state": "starting"}
{"type": "playback_closed", "playback": 3, "reason": "closed"}
{"type": "sound_watch", "camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 5, "lens": 0}, "state": "watching"}
```

- `sound_watch` `state`: `watching`, `stopped`, `refused` (`reason`: the
  device's stream limit, or the camera has no audio).
- `recordings_index` `state` is the device's search lane's
  (`LaneState`): `idle` (nothing waiting), `busy` (working through
  requests), `waiting_device` (not connected; requests stay queued),
  `ended` (login refused or session closed: every waiting request failed,
  and nothing is searched until the app adds the device again).
  `pending_days` is the number of camera-days still queued. Sent on state
  changes and at most once a second while busy.
- `recordings_day_failed`: one camera-day (or its month's calendar) could
  not be searched. It is **not retried automatically**; the view asks
  again (`nv_recordings_want`) if it still shows that day. `reason` is
  English, for logs, not for parsing.
- `recordings_changed` names one device and the range of days that
  changed (`start` of the first day, `end` of the last). The lane reports
  one camera-day at a time; the owner coalesces those into one event per
  device (at most one a second while filling), so a range may cover days
  that did not change. `incidents_changed` follows when incidents in it
  changed.
- `thumbnail` `state`: `ready` (call `nv_thumbnail_get`), `empty`,
  `failed` (`reason`), `dropped`.

## 9. Guarantees

**Threads.**

- Queries, edits, locks, groups, configuration, index requests and
  holds, download control (including trash and range downloads), and
  export plans take short locks on in-memory state and never wait on the
  network or on a device permit; they may be called from any thread except inside callbacks
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
- After `nv_core_free` returns, nothing runs: search lanes (and any
  background pass), thumbnail fetches,
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
| Concurrent plays per device | 2 (`max_plays_per_device`; Decision 2; beside live view a third play was observed to degrade silently, docs/protocol.md) |
| Playback buffer per tile | 8 s or 32 MiB (`buffer_s`, `buffer_bytes`) |
| Queued thumbnail fetches per device | 256, newest first, oldest dropped |
| Thumbnail cache | 4096 entries, 64 MiB (`thumbnails`) |
| Index per device | 90 days, 64 MiB of day files (`index`) |
| Query result | `limit` up to 100000 items, 16 MiB of JSON |
| Stored incident edits | 10000; at most 1000 incidents per `merge` |
| Camera groups | 64 groups of 64 cameras; names up to 256 bytes |
| Camera name overrides | 4096 |
| Days per index request | 31 (`NV_ERR_LIMIT` beyond) |
| Camera-days waiting to be searched, per device | 4096 (a request that would exceed it is refused whole with `NV_ERR_LIMIT`) |
| Known downloads | 1000 (`NV_ERR_LIMIT`; forget finished ones) |
| Locked incidents | 1000 locks (`NV_ERR_LIMIT`; never dropped by the core); 31 days per lock; 16 pinned thumbnails per lock |
| Range download | 16 cameras, 24 hours |
| Still images per plan | 16 (one per playback camera) |
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
| `nv_recordings_configure`, `nv_recordings_enable` | `IndexConfig`, `IndexerConfig`, `LaneConfig`, `GroupingConfig`, `ThumbCacheConfig`, `SchedulerConfig`, `NamePattern`, `system_clock` | the `recordings` owner in `viewer-core` (C11) holding one `DeviceIndex` and `DeviceLane` per device key, the `Scheduler`, the `ThumbnailCache`, the `EditLog`, the `GroupStore`; config parsing; `device_info.recordings` |
| `nv_recordings_calendar`, `_spans`, `_events` | `DeviceIndex::recorded_days`, `files`, `events` (lens in keys) | JSON shaping, `revision`, `limit`, `md` → `motion` |
| `nv_recordings_want`, `_hold_today`, `_status`, `recordings_index`, `recordings_day_failed`, `recordings_changed` | `LaneHandle::{request_days, hold_today, pending}` (`TodayHold` per app hold number), `LaneEvent`, `LaneState`; `request_days` again for the optional `background` block | owner-side aggregation into per-device events, the hold table |
| `nv_incidents_get`, `nv_incident_get` | `group`, `Incident::{key, is_growing, intervals, download_ranges, kinds}` | regroup on index change, JSON shaping |
| `nv_incident_edit` | `EditLog` (store, replay, expiry, `revert`), `merge`, `split`, `add_camera`, `remove_camera` | JSON shaping; no expiry for edits of locked incidents |
| `nv_incident_lock`, `locked` | `EditLog`'s atomic store, `DeviceIndex` age and size eviction, `ThumbnailCache` eviction | the lock store and its matching rule, eviction exemptions, edit key rewriting on regroup, download on lock (C13) |
| `nv_camera_groups_get`/`_put` | `GroupStore`, `CameraGroups` (bounded, stored like layouts, name overrides) | JSON shaping |
| `nv_thumbnail_get`/`_put`/`_cancel` | `ThumbnailCache::{get, put}`, `LaneHandle::{request_thumbnail, cancel_thumbnails}` (newest first, 256, thumbnails before index work) | key mapping, `NV_PENDING` |
| `nv_download_*`, `download` | `Scheduler::{enqueue_incident, trash, set_paused, set_camera_paused, retry, retry_camera, forget, remove_device, resume_all, progress}`, `FailReason`, `Mp4Finalizer` | coalesced events (4 per second), derived `state` (C11; key dedupe and trash done with it, C15) |
| `nv_download_range`, `dest`, `share_safe` | `Scheduler` (jobs are per camera range already), `naming` | range jobs and folder names, the downloads registry for `dest`, share-safe names and MP4 times (C14) |
| `nv_edit_compute` `trim`, `in_ms`/`out_ms`, `nv_still_plan` | `edl`, `sync_samples`, playback tiles' answered pictures | the trim and still planners (C14) |
| `nv_playback_*`, playback events | tiles, `TilePipeline`, generations (`tile.rs`), audio hub, `Subscription`-style bounded queue, replay framing (`encode_replay_media`, `replay_seq`) | playback commands in `bc-protocol` and `bc-session`, play streams routed by message ID and seq, file-boundary handling, the shared clock and paced release, buffering policy, mock-nvr playback |
| `nv_sound_watch`, `sound_watch` | `Subscription`, audio decoding in `TileAudio` | a tile-less subscription that decodes audio only; stream-limit accounting |
| `nv_recordings_loudness`, `nv_sound_labels_put` | the level meter (`audio-pipeline`), `DeviceIndex` day files | the 1-second envelope store, the T3/T4 pattern detector, label merge and caps, sound kinds in incidents and search |
| `nv_edit_compute` | incident intervals, `nv_grid_layout`, `mp4` crate (reading sync samples) | the follow-cut and grid planners |

## 11. Existing code that conflicts or is missing

1. **Allowlist.** Done: 123, 5 and 7 are on the viewing tier, with request
   builders (`Session::replay_seek`, `start_play`, `stop_play`) and a play
   stream in `bc-session` (`DeviceSession::play`, `Play`: media routed by
   message ID and the replay `seq`, bounded, stop on drop and on close, at
   most `max_plays` per device). Rates other than 1× wait for B1.
2. **Lens in the index and incidents.** Done (MILESTONE-P.md, C1): the
   index is keyed by channel and lens (index format 2) and `CameraId` has a
   `lens` (old job files load with lens 0).
3. **Stable incident key.** Done (C2): `Incident::key` and `is_growing`;
   `id` still changes while an incident grows.
4. **Scheduler granularity.** Done (C6): per-camera pause and retry,
   `FailReason` codes beside the English detail, `remove_device`, and the
   device's size estimate.
5. **Recorded time on frames.** The demuxer keeps the video extra header
   (`Record::Video::extra_header`, `wall_clock_secs`), and download chunks
   now record each keyframe's seconds (`recordings::recorded_time`). Still
   open: whether those seconds are device-local or UTC (MILESTONE-P.md,
   B7), which the shared clock needs to place pictures on the index's
   timeline.
6. **First picture time of downloads.** Each camera job records its first
   picture's time on the keyframes' clock (`first_picture_wall_ms`);
   `file_start_ms` in device-local time follows once B7 is settled.
7. **Thumbnail priority.** Done (C4, C5): one search lane per device
   (`recordings::lane`) serves thumbnails first, then requested days, then
   a held today; the optional background block asks the same lane for
   its days (the whole-range `indexer` is no longer used by the core).
8. **The recordings owner in `viewer-core`** is done (MILESTONE-P.md,
   C11): sections 1 to 5 and the events of section 8 have a Rust API on
   `viewer_core::Core` (JSON in, JSON out, `RecError` for the new
   statuses) that the C functions (F1) wrap; locks (C13) and export (C14)
   are not in it yet. mock-nvr serves playback (123/5/7) at 1× with the
   observed burst-then-pace, across files.
9. **`NvStreamTier` carries Baichuan tiers** (`MOBILE`, `EXTERN`) in ABI 3.
   This proposal keeps them for live tiles and uses lens + main/sub for
   everything new (Decision 7).

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
  follow cut or trim can be remuxed without decoding.
- **Stills**: encode PNG or JPEG with WIC from the tile's shown texture or a
  picture decoded from the file.
- **Trash**: `IFileOperation` with `FOFX_RECYCLEONDELETE` on the folder
  `nv_download_trash` returns, after its `download_removed` event.
- **Paths**: the app passes its downloads folder (for example under the
  user's Videos known folder) as UTF-8; output names are already safe for
  Windows (`recordings::naming::sanitize`) and capped at 120 characters per
  component, so paths stay well below the long-path limit in ordinary
  folders.

## 13. Header additions (ABI 3, minor 4)

```c
#define NV_ABI_VERSION 3   /* unchanged */
#define NV_ABI_MINOR 4     /* was 3 (nv_tile_retry) */

/* NvStatus gains: */
NV_PENDING = 1,                /* accepted; the answer arrives as an event (thumbnails) */
NV_ERR_UNSUPPORTED = -8,       /* the device's backend cannot do this */
NV_ERR_NOT_CONFIGURED = -9,    /* nv_recordings_configure not called, or no downloads_dir */
/* (-7 is NV_ERR_NOT_PERMITTED, taken by talk-back in ABI 3.) */

/* New types: NvThumbKind, NvThumbnail, NvSeekMode, NvPlaybackState
 * (sections 4 and 6). New functions: the nv_recordings_* (including
 * nv_recordings_want and nv_recordings_hold_today), nv_incident* (including
 * nv_incident_lock), nv_camera_groups_*, nv_thumbnail_*, nv_download*
 * (including nv_download_trash and nv_download_range), nv_playback_*,
 * nv_sound_*, nv_tile_loudness, nv_edit_compute, and nv_still_plan
 * declarations above.
 * No existing function or struct changes. */

/* Live sound settings, JSON (unknown members ignored):
 * {"alarms": true, "loudness_history_s": 600}. Any thread outside
 * callbacks; applies to tiles opened afterwards. */
NvStatus nv_sound_configure(NvCore *core, const char *json);
```

`NV_PENDING` is the first positive status: apps that test `== NV_OK` treat
it as "not yet", and only `nv_thumbnail_get` returns it.

An earlier draft added two members at the end of `NvConfig`
(`sound_alarms_off`, `loudness_history_s`). That changes the struct's size,
which an addition under a minor version may not do (an app built against
minor 3 passes the shorter struct), so the two settings are a JSON call
instead.

## Decisions (2026-10-10)

The open questions of this proposal, as the user answered them, and what
changes in the contract above. Where a decision needs a measurement first,
it says so; nothing below depends on an unmeasured fact.

1. **Versioning: add a minor version.** `nv_abi_minor()` joins
   `nv_abi_version()`: the major number changes only for incompatible
   changes, the minor for additions (recordings, talk-back, later motion
   control and the local API), so changes are classified and apps can check
   for a feature. Recordings arrive as an addition, not ABI 4.
2. **Live view keeps running during playback.** Plays and live tiles share
   the device's one session; how many plays a device allows next to live
   view is measured before the limit is set (protocol.md, "Open questions").
   A capture of the official client showed four synchronized replay plays on
   one session with no refusal (so the session multiplexes several plays by
   message ID), but it also **stopped live view when its playback tab opened**,
   so it did not demonstrate live and replay media on one session at once. Our
   design still keeps live tiles running during playback; that one session can
   carry both live and replay media is the assumption to confirm when playback
   is built (protocol.md, "Open questions").
3. **Rates up to 16x,** as the official client offers. A capture of the client
   at 1×, 2×, 4×, 8×, and 16× settled the mechanism: it changes rate with
   **stop, seek, play** — the `playSpeed` field in the play request (command 5)
   — not a dedicated speed command, not keyframe-only playback (150), and not
   play-by-time (381/383). Higher rates delivered only about three times the 1×
   byte rate, not N×, so the device serves a thinned, keyframe-biased stream at
   speed rather than every frame faster; each play still begins with a burst
   and then paces. The frame-level detail (which frames are kept, the
   frames-per-second and keyframe ratio per rate, the exact `playSpeed` values)
   was encrypted in that capture and needs a decryptable one; the core can
   implement rate changes as stop/seek/play now and tune fast-rate decoding
   once the frames are read. Until then 0.25× to 2× by seeks and pacing.
4. **Synchronization to tens of milliseconds, not a second.** Every recorded
   video frame carries a microsecond timestamp, as in live view; only the
   anchor to wall-clock time comes from whole seconds at keyframes. Each
   keyframe bounds the anchor to a one-second window, and intersecting the
   windows of many keyframes narrows it (live view: about 15 ms from two
   minutes of a main stream's keyframes, protocol.md "Sub and main of one
   camera"). The shared clock uses that refined anchor per camera and file;
   the user can still nudge `offset_ms`, remembered per camera. To confirm on
   a real recording: the same refinement on recorded streams, and how far
   two cameras' clocks differ.
5. **A short stall at file boundaries is acceptable** unless a way to avoid
   it is found (for example asking for the next file before the current one
   ends; measured when playback is built). A capture of the official client
   showed a single play run across a recording-file boundary with no stop,
   seek, or re-play and no stall beyond its normal pacing — the device
   continues across files within one play — so following the client's own flow
   (one play per lane, left running) may need no boundary handling at all; this
   is observed only at the byte/timing level and is confirmed on a real
   recording when playback is built.
6. **Time zone: the NVR's.** Index and incident times are the device's own
   wall-clock time, which is what the cameras burn into their picture. The
   read-only queries are now identified: the client reads the device time and
   zone with command 104 (general system config), daylight saving with command
   106, and NTP with command 38 (protocol.md, "Device information"), and sends
   no write when the settings screen is closed without saving. Their reply
   fields (zone format, DST rule, NTP offset) still need a decryptable capture
   before the core parses them; until then the app passes `utc_offset_s`.
7. **Protocol neutrality:** ABI additions take a `lens` argument instead of
   new uses of `NV_TIER_MOBILE`/`NV_TIER_EXTERN`; the Baichuan tier names stay
   for compatibility, marked as such (docs/ARCHITECTURE.md "Other
   protocols").
8. **No background indexing.** The NVR keeps the real index; the core's index
   is a cache of what has been searched, so the timeline, calendar, search,
   and incident grouping across cameras answer at once without repeating
   device searches, and the computer's own labels (sound events) have a
   place. It fills on demand: a day or camera the user opens is searched and
   kept, and today is refreshed while it is shown. `nv_recordings_enable`
   remains, but no default pass runs.
9. **Deleting moves to the Trash** (macOS Trash, Windows Recycle Bin), never
   a permanent delete: the core offers it for whole incident folders, the
   app performs it through the system.
10. **Animal kinds stay open-ended:** `dog_cat` and any finer kinds devices
    report later pass through as their own kinds; an "animal" filter in the
    apps groups every animal kind, so new kinds need no contract change.
11. **Edits beyond the follow cut and grids** come later.
12. **Sound watching:** only cameras that are on screen, plus cameras the
    user adds to an "always listen for sound" list, keep their audio decoded
    for sound events (`nv_sound_watch` for the list).
13. **Older recordings are analyzed for sound only on request,** for the day
    or camera the user picks; no background pass.
14. **The ten sound kinds plus the classifier's own label** are enough for
    the first version.

## Decisions (2026-10-11)

The owner's review of sections 1 to 5, 8, and 9 before freezing them (F1),
and what changed above.

1. **Explicit calls for on-demand indexing.** `nv_recordings_want`
   ({cameras, days (at most 31), force}) maps to the lane's
   `request_days`; `nv_recordings_hold_today` maps to `hold_today`, one
   hold per app-chosen hold number (a window), released with NULL, by
   `nv_core_free`, or for a removed device. `nv_recordings_refresh` is
   **folded in**: a refresh is `want` with `"force": true`. Why: Decision 8
   needs a way for a view to say what it shows; queries that searched as a
   side effect would hide device traffic behind a read and could not tell
   "show" from "keep fresh". One call instead of two for the same lane
   request keeps the surface small; nothing was implemented yet, so
   dropping `refresh` costs nothing. The hold takes a hold number (an
   argument the review sketch did not have) so several windows can hold
   today independently and each release only its own.
2. **`days_back` and `pass_interval_s` stay, as an optional per-device
   `"background"` block, off by default**, which runs the existing
   `indexer`. `nv_recordings_enable` otherwise only names the channels and
   lenses that may be indexed. Why: some people want the index filled
   ahead of time and the code exists; off by default keeps Decision 8.
3. **Downloads are deduplicated by the incident's `key` while
   unfinished**; a finished download stays, and queuing the grown incident
   again makes a new folder (` (2)`). The download ID stays the incident's
   `id` at queue time and is opaque. Why: deduping by `id` made a second
   folder for every growth step of a live incident; deduping by `key` for
   finished downloads would refuse to fetch what was added later.
4. **Trash is in F1**: `nv_download_trash` forgets the download and
   returns its folder; the app moves it to the Trash or Recycle Bin once
   `download_removed` says nothing writes there. The core never deletes
   permanently. Why: Decision 9 needed a call, and the system's Trash is
   the platform's to use.
5. **Incident keys are opaque**; renaming or deleting a group changes the
   keys in it, and the contract says so. Why: the key format already
   changed once (the group hash), and apps that parse it would break.
6. **`recordings_index` reports the lane's states** (`idle`, `busy`,
   `waiting_device`, `ended`) with `pending_days`; failed days are their
   own event, `recordings_day_failed` (device, camera, day, reason), and
   are not retried automatically. Why: that is what the lane does; a
   `days_total` would be a count of a moving set of requests.
7. **Event volume is frozen as written.** Thumbnail events are batched
   only if the 1024-entry event queue overflows in testing. Why: the
   bounds are already per device and per download; batching is an
   additive change if needed.
8. **The four `device_info.recordings` flags are frozen** (`search`,
   `playback`, `download`, `thumbnails`). Why: they cover what a different
   backend might lack; more can be added as members.
9. **`nv_downloads_get` uses the buffer-too-small loop**, no paging. Why:
   at most 1000 downloads, and every other list call works the same way.
10. **Edits expire with `max_age_days` except for locked incidents**, and
    **locking** is new: a locked incident's edits, index days, and
    thumbnails are kept past `max_age_days` and exempt from eviction;
    locking queues its download; unlocking makes it ordinary and never
    deletes files. Locks are stored atomically, at most 1000 (`NV_ERR_LIMIT`
    at the cap, never dropped by the core), and follow merges, splits, and
    regrouping by overlap with the lock's extent (section 3, "Locked
    incidents"). Why: the NVR overwrites old video and the index forgets
    old days; a person who marks an incident as important expects it to be
    there next year, and only a download keeps the video. Matching by
    extent rather than key alone keeps a lock across group renames, and
    keeping split parts under one lock means edits never raise the count.
11. **Export is planned** (section 7a): trims (keyframe copy or exact, as
    an edit-list kind), range downloads through the same scheduler, still
    images (from a playback's shown pictures or a file), a destination per
    export, and share-safe output. Why: the user wants all of these. The
    core decides ranges, edit lists, and names; the apps decode, encode,
    and write with platform APIs, as for the follow cut. They are not part
    of F1; they ship as their own slices (MILESTONE-P.md).

Also corrected in this review, without a decision: ABI 3 minor 4
throughout; `NvConfig` no longer grows (the two sound settings became
`nv_sound_configure`, because a minor may not change a struct's size); the
new status codes are -8 and -9, because talk-back took -7; the incident key
example; `NV_ERR_LIMIT` at the download cap; the JSON camera argument of
pause and retry; derived download and camera states; the missing caps in
the limits table; and sections 10 and 11 brought up to date.

## Clarifications from the implementation (C11, 2026-10-11)

What building the recordings owner (MILESTONE-P.md, C11) settled where
the text above was silent or could be read two ways. Sections 1 and 11
above are updated; the rest is stated here.

1. **`utc_offset_s`** absent means 0: the core has no time-zone database,
   so the app passes the computer's offset (section 1).
2. **`background`** goes through the device's search lane (section 1)
   instead of a separate pass, so its days produce the same events, wait
   behind thumbnails, and stop after a refused login.
3. **Devices not added.** Enabling opens the device's index, so queries
   answer from it (and from earlier runs' files) before the device is
   added. `nv_recordings_want` for an enabled device that is not added
   keeps the request (it counts toward the 4096 camera-days) and searches
   once the device is added; `recordings_index` says `waiting_device`,
   also after `nv_remove_device` of an enabled device. A thumbnail of a
   device that is not enabled and added returns `NV_PENDING` and a
   `thumbnail` event `failed` follows.
4. **`nv_recordings_status`** answers `{"revision": N, "devices":
   [{"device", "state", "pending_days", "revision"}]}`, one entry per
   enabled device.
5. **`nv_incident_get`** answers `{"revision": N, "incident": {...}}`, the
   incident as in `nv_incidents_get`.
6. **`truncated`** is in every query answer (calendar, spans, events,
   incidents); `limit` counts a calendar's camera lists, a span list's
   spans, events, and incidents.
7. **`recordings_changed` at the end of a fill** is sent at once (when the
   lane leaves `busy`), even within a second of the previous one; while
   busy, one a second at most. `recordings_index` follows it.
8. **`nv_download_forget`** of a download with a camera job running (not
   paused, or still stopping) returns `NV_ERR_INVALID_ARGUMENT`.
9. **Trash is final.** A trashed download's job file is marked, so it is
   never resumed after a restart, nor returned by a later
   `nv_download_incident`, even if moving its folder to the Trash failed
   (a later download of the incident then takes a ` (2)` folder).
10. **Finished downloads after a restart** are not loaded (memory stays
    bounded), but `nv_download_incident` of the unchanged incident finds
    the finished folder on disk and returns it (`existing` true), as rule
    2 of "Download IDs" says.
11. **Changing settings.** The thumbnail cache and each device's index
    keep the limits they were opened with (the first configure, and each
    enable); a change of `downloads_dir`, `downloads`, or `naming` stops
    the scheduler (job files keep the progress) and starts a new one,
    which resumes from disk.
12. **Without `data_dir`** the index and thumbnails live in a scratch
    directory for the run (removed when the core stops); edits and groups
    live in memory.
