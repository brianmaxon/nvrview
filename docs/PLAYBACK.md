# Playback

Milestone P: finding recorded events and getting them off the NVR, done the
way people actually use recordings. Protocol facts are in
[protocol.md](protocol.md) ("Recordings: search and download", "Playback",
"Cover images"); this is the design. The work breakdown (slices, order,
what is blocked on a capture) is [MILESTONE-P.md](MILESTONE-P.md).

## What goes wrong today (official desktop app)

- Finding an event means tiny timelines and small targets, no thumbnails, and
  a bare multi-camera view.
- One event seen by several cameras has to be found and downloaded camera by
  camera.
- Downloads block the rest of the app, and they stall or time out midway.
- Each download lands in its own folder with a name that is hard to read.
- Following action from camera to camera, or showing two or four angles side
  by side, has to be assembled by hand in a video editor afterwards.

## Principles

1. **Incidents, not files.** The unit is an *incident*: one real-world event
   (an animal crossing the yard, a vehicle arriving) with every camera that
   saw it, not one camera's recording file.
2. **Search by what happened.** Motion and detection events (vehicle, people,
   animal, ...) are the index; files are an implementation detail.
3. **Downloads never get in the way.** They run in the background, in
   parallel across devices, resume after any failure, and never interrupt
   live view.
4. **Output you can use as is.** One folder per incident with readable names,
   standard MP4 files, and optional ready-made edits (follow cut, 2-up/4-up).
5. **Shared core, platform rendering.** Indexing, search, incident grouping,
   download scheduling, and edit decisions are shared Rust; thumbnails,
   composition, and encoding use each platform's GPU and hardware encoder.

## Index

Per device, the core keeps a local index under the app's data directory,
bounded by age and size. It is one small file per day (every channel's files
and events of that day, replaced atomically) plus a calendar of recorded
days, held in memory for queries: searches return a whole day of one channel
at a time, so a day is the natural unit of refresh and expiry, and plain
files need no C library on any platform.

- **Days with recordings** per channel (command 142, one request per month).
- **Recording files** per channel and day (14/15/16): time ranges, sizes,
  stream type.
- **Events** per channel and day (272/273/274): start, end, type (`md`,
  `people`, `vehicle`, `pir`, `package`, ... as the device reports them), and
  the file each belongs to.
- **Cover images** (298): one keyframe per event or file, decoded to a small
  thumbnail and cached.
  The core fetches the keyframe (`recordings::thumbs::cover_thumbnail`) and
  keeps it in an on-disk cache bounded by entry count (default 4096) and total
  bytes (default 64 MB), least recently used first. The core does not decode
  pictures: the app decodes the keyframe, shrinks it, and may store the small
  image in its place, which is how thousands of thumbnails fit. Entries are
  keyed by device, channel, stream, and requested range; an empty answer (no
  recording in the range) is not cached.

The index is a cache of what views asked for, not a copy of the device's
(FFI-RECORDINGS.md, decision 8). One search lane per device
(`recordings::lane`) sends one search at a time over the device's session,
choosing each time: cover thumbnails first (newest request first, at most
256 waiting, cancelable), then the days a view asked for (newest day first,
one month calendar or one camera-day at a time, never searching a finished
day twice unless the user refreshes), then today again while a view shows
it and its entry is older than a minute. So a long fill delays what is on
screen by at most one search. Nothing is searched in the background
otherwise. The index is shared by every window, keyed by camera (channel
and lens). All of this is read-only toward the device.

- **Sound** per channel: a loudness envelope (peak and RMS per second) and
  sound labels as events of our own (see "Sound" below).

Animal events: the device's own type names for animals have not been observed
yet (open question). Until they are, "animal" can also come from on-device
classification of cover images and sampled frames (Core ML on macOS), stored
as our own detections next to the device's.

## Incidents

Events from all cameras are grouped into incidents:

- Events overlap or follow each other within a gap (default 10 s, adjustable)
  on cameras the user groups as covering the same area (for example "front of
  the house": three cameras). Without groups, any cameras.
- An incident keeps the union of its events' time ranges per camera plus a
  margin before and after (default 5 s), and its types. A download takes
  one continuous range per camera, from its first event to its last, with
  the margins.
- Grouping is deterministic: the same events give the same incidents and
  IDs.
- Users can merge, split, add or remove a camera, and rename an incident.

## Finding

- **Incident list** for a day or range, newest first, filterable by type
  (vehicle, animal, people, any motion), camera group, and time of day. Each
  row shows thumbnails from every camera involved, the time span, and types.
- **Timeline**: one lane per camera, zoomable from a week down to seconds
  with the trackpad or keyboard, events drawn as colored bars by type,
  incidents as bands across lanes, large hit targets, and a thumbnail strip
  under the cursor. Under each camera's lane, a thin loudness lane shows
  where something was loud, and sound labels appear as markers like device
  events.
- **Filter and search by sound** as by video type: "barking, last week",
  "any alarm sound today", or "loud moments on this camera".
- **Synchronized multi-camera playback**: the cameras of an incident play
  side by side on one clock (the cameras' recorded times), scrub together,
  step frame by frame, and switch any lane to full size. Playback uses the
  observed seek-and-play flow (123, 5, 7), one stream per visible lane,
  within the device's session limits.

## Device AI search

Newer NVR firmware runs its own search over its recordings: by event type
(people, vehicle, animal, package, visitor, ...), by danger level, by a
tracked object across cameras, and by a typed phrase ("AI video search"). The
commands, request fields, and hit fields are in protocol.md ("Event log and
AI search: commands 516–519, 759, 653"), read from the official client but
**not yet seen on the wire**. The plan, in order:

1. **Capture it.** The official client's AI search screen with a packet
   capture running: a type search, a danger-level search, a track ("follow
   this person"), a text search, opening a hit's detail and cover image.
   Record the observed request and reply bodies, timings, and paging in
   protocol.md, and confirm how support is advertised (199
   `eventLogVersion`).
2. **Read back results with our own client.** Type, danger-level, and track
   searches (516 → 517 pages → 518, 519 counts, 759 detail, 653 cover) are
   read-only viewing commands; add them to the allowlist once observed, then
   a `bc-cli eventlog` command, then the core. Hits become incidents like the
   272–274 index's, with their extra fields (`dangerLevel`, `yoloWorldType`,
   the 759 description text and tracks) kept for filtering and display.
3. **Use it for notifications, if it can be read soon enough.** Measure how
   long after an event its hit appears in a 516 search, how costly a
   repeated narrow search (last minute, chosen cameras) is for the NVR, and
   what the hit adds over the live detection push (command 33): a danger
   level, an object type beyond person/vehicle/animal, a description, or a
   track across cameras. If a hit arrives within seconds, a notification can
   wait briefly for it to add detail or to confirm (the verifier in
   ARCHITECTURE.md, with the device's own judgment as one more signal), and
   it fails open: no hit in time never suppresses a notification. Searches
   run at most a few times a minute per NVR, one at a time, never while the
   device is busy serving playback, within the session limits.
4. **Text search is matched by the NVR.** The official client's AI search
   screen was observed sending the typed phrase itself to the NVR (a plain
   text field with a similarity cut-off, docs/protocol.md "What the AI search
   sent on the wire"); no encoder model ran on the computer and no feature
   vector was sent, and the NVR returned ranked hits. nvrview can therefore
   send the same text with no model of its own, and needs no vendor server
   (a vendor server could record the search texts, so it stays out). Older
   firmware, or other client versions, may use a feature vector instead
   (reported from the client's code); if an NVR does not return hits for
   plain text, text search is simply not offered for it, and nvrview never
   bundles, downloads, or redistributes the vendor's model. Results are a
   similarity ranking over all detected objects with a low cut-off, not a
   class filter, so the UI should show the score and let the user raise the
   cut-off. Type, danger, and track searches do not depend on any of this.
5. **Fast thumbnails.** The official client fetches one cover per hit, one
   at a time, which makes its search slow. Ours fetches covers only for hits
   on screen, in parallel up to the device's measured limit, and caches them.

## Sound

The official desktop app's playback ignores audio, although recordings
carry it. Sound finds events that video misses or cannot see: an alarm in
another room, glass breaking, a dog barking at night, a shout. It is an aid
for finding and labeling events, never a replacement for alarm systems or
monitoring; a missed sound is never shown as "no alarm".

- **Loudness lane.** The core keeps a per-second loudness envelope per
  camera, from live audio (every tile with audio, audible or not, and
  cameras the user keeps watching for sound) and from recordings as they are
  downloaded or played. Drawn under each timeline lane, it shows at a glance
  where to look, with no classifier at all. About 170 KB per camera per day.
- **Alarm patterns.** A deterministic detector in the core recognizes the
  standard smoke-alarm (three beeps, T3) and CO-alarm (four beeps, T4)
  rhythms of a tone near 3 kHz, with no model; it is tested on synthetic
  audio, and a camera's alarm test button is the hardware check.
  Implemented, with the loudness envelope, in `crates/sound-events`, and
  run by the core on every live tile with audio, which reports alarms as
  `detection` events (docs/FFI.md, "Events"; recordings are not analyzed
  yet): T3 is 0.5 s on, 0.5 s off, three times, then
  1.5 s off (ISO 8201, ANSI/ASA S3.41, NFPA 72); T4 is 0.1 s on, 0.1 s off,
  four times, then about 5 s off (NFPA 720, now NFPA 72; UL 2034). The tone
  band defaults to 2.9 to 3.5 kHz, durations are accepted within 30% plus
  40 ms, and two linked cycles are needed before an episode is reported
  (about 7.5 s after a T3 starts, 10 s after a T4). On synthetic audio it
  finds every alarm down to a tone 5 dB below broadband noise, with AAC-like
  coding, resampling, reverberation, and ±20% timing jitter, and reports
  none of 25 distractors (steady tones, ring cadences, microwave and backup
  beepers, alarms outside the band, speech-like noise, music-like chords).
  On 10 minutes of real camera audio it reported nothing, and found alarms
  mixed into that audio at down to -40 dBFS, also after an AAC round trip.
  Cost: about 0.1 ms of CPU per second of audio for the detector and 0.01 ms
  for the envelope (Apple M3 Max, release build; `cargo run --release -p
  sound-events --example bench`), about 0.01% of one core per camera. In
  the core, with the conversion to 16 kHz mono, it adds about 10 µs to
  each 64 ms record on the tile's thread, 0.016% of one core (`cargo run
  --release -p viewer-core --example sound_cost`; decoding the record
  takes about 7 µs).
  Not yet checked: a real alarm through a real camera microphone.
- **Sound labels.** On macOS, SoundAnalysis's built-in classifier labels
  sirens, glass breaking, barking, a baby crying, knocking, and similar
  sounds; the app hands the labels to the core, which stores them as events
  (`source` `local_audio`) next to the device's. They join incidents, filter
  the list and timeline, and can feed triggers and the notification
  verifier like any other detection. Device detections of sound (such as a
  camera's own baby-cry detection) are used where a camera has them.
- **Older recordings** are analyzed on demand (a day or a camera the user
  picks), or by an optional slow background pass, newest first, one stream
  per device, only while the device is otherwise idle.

The C interface for this is in [FFI-RECORDINGS.md](FFI-RECORDINGS.md),
section 2a.

## Download

- **Unit of work**: one camera's time range, fetched with command 143 in
  chunks (default 60 s, cut on keyframes by the device). A chunk that stalls
  past a timeout or fails is retried, then the job resumes at the first
  missing chunk; completed chunks are never fetched twice.
- **Chunks overlap.** The device starts each window at the keyframe at or
  before its start, so a chunk repeats the end of the previous one (up to a
  GOP). Windows are requested so that each ends where the next begins, and
  joining drops frames at or before the previous chunk's last timestamp.
  Raw chunks (Annex-B video, ADTS audio from the first keyframe on, and an
  index of timestamps) are kept in the incident folder's `.parts` until the
  MP4 is written.
- **Buffering.** Downloads arrive unpaced, and slowing one would stall live
  view on the same connection, so the session buffers a bounded amount and
  stops (144) a download whose consumer falls that far behind; the chunk is
  fetched again. Chunk writes (in batches), their `fsync`, and job file
  saves run on blocking threads, never on an async worker: a worker stuck
  in a slow disk flush also holds up the tasks queued behind it, which can
  be the session's reader. Measured before this rule, on a 6-core Windows
  computer under CPU and disk load (debug build), live frames reached the
  session up to 1 s late while the mock sent them on time. The end-to-end
  test checks live view during a three-camera download (no gap over
  400 ms) in simulated time, and, opt-in, in real time with every core busy
  (`cargo test -p recordings --test e2e -- --ignored`).
- **Scheduling**: jobs from a whole incident are queued together. Devices
  work in parallel; per device, downloads share one session with a small
  concurrency limit (one download and one search at a time by default) so
  live view keeps its stream. One NVR carried live view and a play on one
  session at once with no drop in the live frame rate (observed with
  `bc-cli replay-probe`, docs/protocol.md); live view beside a download is
  still to be measured (the official client stops live view during
  playback, though it ran searches and cover requests beside its
  download).
- **Progress** per incident and per camera, pausable, survives app restarts.
- **No re-encoding** for plain downloads: chunks are joined and remuxed into
  MP4 (H.264 or H.265 video, AAC audio) with correct timestamps.
- **Joining** (`recordings::finalize`) is crash-safe in two steps. The chunks
  are written to a fragmented MP4 (`joined.fmp4` in the camera's parts
  folder, flushed and synced every fragment of up to 2 s), with the overlap
  dropped by timestamp and each audio frame placed right after the video frame
  it followed (audio has no timestamp; audio that followed a repeated
  video frame is dropped with it). Then the fragmented file is remuxed into a
  standard faststart MP4 under a temporary name, renamed into place, and
  deleted. A killed process leaves either the fragmented file (it plays up to
  its last complete fragment; `recover_fragmented` finishes it) or a finished
  MP4, never a partial output name, and the raw chunks stay until the job is
  forgotten, so an interrupted join is simply redone. A change of codec or
  parameter sets in the middle of an incident fails the join (a file has one
  `moov`); handling it by splitting the output is not done yet.

## Output

One folder per incident, named so it sorts and reads well, for example:

```
2026-10-10 07.42 vehicle — Front, Side, Gate/
  2026-10-10 07.42.05 Front.mp4
  2026-10-10 07.42.11 Side.mp4
  2026-10-10 07.42.20 Gate.mp4
  incident.json        (cameras, events, types, times; for tools and search)
```

Camera names are the user's own channel names; the pattern is configurable.

- **Locking an incident** keeps it: its edits, its index days, and its
  thumbnails no longer expire with age, and its video is downloaded, since
  the NVR overwrites old recordings. Unlocking makes it ordinary again and
  never deletes downloaded files.
- **Deleting** moves an incident's folder to the Trash (Recycle Bin on
  Windows); nothing is deleted permanently.
- **Export**: trim to in and out points inside an incident or while
  scrubbing (a fast keyframe copy, or exact with a re-encode); download any
  time range of chosen cameras, not only detected incidents; save the
  current picture, or one per camera, as PNG or JPEG; choose the
  destination of each export; and a **share-safe** option that leaves
  camera and device names out of file names and overlays, writes no
  `incident.json` or device details, and no metadata the core would
  otherwise write into MP4 files. The core plans ranges, cuts, and names;
  the app encodes and writes with the platform's APIs
  ([FFI-RECORDINGS.md](FFI-RECORDINGS.md), section 7a).

## Ready-made edits

Built from the downloaded files, on the GPU, with the hardware encoder:

- **Follow cut**: one video that switches to whichever camera currently has
  the action, from each camera's event intervals (optionally refined by
  on-device motion measurement), with short holds to avoid flicker. Cuts on
  keyframes need no re-encode; frame-accurate cuts re-encode.
- **2-up / 4-up**: the incident's cameras side by side, synchronized on camera
  time, rendered with the same Metal grid renderer as live view (offscreen),
  encoded with VideoToolbox. Per-camera time offsets can be nudged if clocks
  disagree.
- **Labels**: optional camera name and time burned in.

Edit decisions (which camera when, layout, offsets) are computed in the shared
core as an edit list, so the Windows app renders the same edits with its own
compositor and encoder.

## Review mode: live view during playback

Live view and recorded playback share the device's one session. Measured
beside live view, a device served two plays fully and degraded a third
silently (no refusal; protocol.md, "Live view and plays on one session").
The official client, which stops live view when its playback tab opens,
ran four plays on the same device.

- **One or two cameras of a device:** live view keeps running.
- **Three or four cameras of one device** (a 4-up review, or a follow cut
  over several angles): that device's live tiles pause while the playback
  holds them, labeled "Live paused while reviewing", and resume as soon as
  the playback ends or drops to two cameras. Other devices are untouched.
- **Alerts keep working:** detection and alarm events do not depend on the
  video, so they keep arriving. A doorbell ring resumes that camera's live
  tile for the call; if that leaves the playback one play short, the
  camera added last is shown as unavailable until the call ends.
- **Setting:** "Pause live view while reviewing: when needed (default) /
  always".
- **To measure first:** that the device serves four plays with our live
  view paused (one `bc-cli replay-probe` run with live view off).

## Measured goals

| Measure | Target |
| --- | --- |
| Find an incident by type on a busy day | seconds, from the local index |
| One-minute clip, one camera | about as fast as the device sends it (observed ~20 MB in ~1.5 s on a LAN) |
| A three-camera incident | all angles queued in one action; parallel across devices |
| A stalled transfer | detected within seconds and resumed without user action |
| Live view during downloads | never interrupted |

## Open questions

- Device AI search (above): the wire format is now observed (516 to 518,
  plain-text search matched by the NVR). Still open: how soon a hit appears
  after an event, the device's cost and limits for repeated searches, and why
  a 517 page is sometimes answered with status 442.
- (Settled.) The animal event type name is `dog_cat` in the live push and in
  event-search replies.
- Whether live view and 143 downloads can share a session, the device's
  limits on concurrent downloads, and how many plays it allows beside live
  view. A capture showed four synchronized replay plays on one session with
  no refusal (with live view stopped by the client), and nvrview's own probe
  showed two simultaneous plays (one of them another camera) beside live view
  on one session, accepted with the live frame rate unchanged (B5 in
  docs/MILESTONE-P.md); more than two plays beside live view and
  live-plus-download are still unconfirmed.
- How 143 reports errors, and how a play ends at the end of a file. A play is
  now observed to continue across a file boundary with no wire event; the end
  of the last file was not reached, and no seek or play ever returned an error
  status (a time with no recording is read from the search results, not from a
  play error).
- (Settled.) Fast rates (2×–16×) are served as keyframes only with no audio,
  switched by stop/seek/play with `playSpeed` 2, 4, 8, 16: every keyframe up
  to 4×, then every 2nd (8×) and 4th (16×), at most about two keyframes per
  second, so the media clock runs at the requested rate (docs/protocol.md,
  "Rates, keyframes, and how a play ends").
- Clock agreement between cameras behind one NVR (recorded times come from
  the NVR; offsets between cameras with their own clocks need measuring).
- Which cameras to keep watching for sound when they are not on screen
  (each costs one sub-stream on the device), and whether older recordings
  are analyzed by default.
- How well camera microphones pick up an alarm in another room (test with
  the alarm's test button), and how SoundAnalysis's labels fare on
  compressed 16 kHz camera audio (measure before relying on a label).
