# Playback

Milestone P: finding recorded events and getting them off the NVR, done the
way people actually use recordings. Protocol facts are in
[protocol.md](protocol.md) ("Recordings: search and download", "Playback",
"Cover images"); this is the design.

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

Per device, the core keeps a local index (a small embedded database under the
app's data directory, bounded by age and size):

- **Days with recordings** per channel (command 142, one request per month).
- **Recording files** per channel and day (14/15/16): time ranges, sizes,
  stream type.
- **Events** per channel and day (272/273/274): start, end, type (`md`,
  `people`, `vehicle`, `pir`, `package`, ... as the device reports them), and
  the file each belongs to.
- **Cover images** (298): one keyframe per event or file, decoded to a small
  thumbnail and cached.

The index fills in the background (recent days first, then older ones),
refreshes the current day periodically, and is shared by every window. All of
this is read-only toward the device.

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
  margin before and after (default 5 s), and its types.
- Users can merge, split, add or remove a camera, and rename an incident.

## Finding

- **Incident list** for a day or range, newest first, filterable by type
  (vehicle, animal, people, any motion), camera group, and time of day. Each
  row shows thumbnails from every camera involved, the time span, and types.
- **Timeline**: one lane per camera, zoomable from a week down to seconds
  with the trackpad or keyboard, events drawn as colored bars by type,
  incidents as bands across lanes, large hit targets, and a thumbnail strip
  under the cursor.
- **Synchronized multi-camera playback**: the cameras of an incident play
  side by side on one clock (the cameras' recorded times), scrub together,
  step frame by frame, and switch any lane to full size. Playback uses the
  observed seek-and-play flow (123, 5, 7), one stream per visible lane,
  within the device's session limits.

## Download

- **Unit of work**: one camera's time range, fetched with command 143 in
  chunks (default 60 s, cut on keyframes by the device). A chunk that stalls
  past a timeout or fails is retried, then the job resumes at the first
  missing chunk; completed chunks are never fetched twice.
- **Scheduling**: jobs from a whole incident are queued together. Devices
  work in parallel; per device, downloads share one session with a small
  concurrency limit so live view keeps its stream (whether a device can carry
  live and download traffic on one session at once is to be measured; the
  official client stops live view during playback).
- **Progress** per incident and per camera, pausable, survives app restarts.
- **No re-encoding** for plain downloads: chunks are joined and remuxed into
  MP4 (H.264 or H.265 video, AAC audio) with correct timestamps.

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

## Measured goals

| Measure | Target |
| --- | --- |
| Find an incident by type on a busy day | seconds, from the local index |
| One-minute clip, one camera | about as fast as the device sends it (observed ~20 MB in ~1.5 s on a LAN) |
| A three-camera incident | all angles queued in one action; parallel across devices |
| A stalled transfer | detected within seconds and resumed without user action |
| Live view during downloads | never interrupted |

## Open questions

- Animal event type names from the device (needs a capture with an animal
  event).
- Whether live view and 143 downloads can share a session, and the
  device's limits on concurrent downloads.
- How 143 reports errors, and how a play ends at the end of a file.
- Clock agreement between cameras behind one NVR (recorded times come from
  the NVR; offsets between cameras with their own clocks need measuring).
