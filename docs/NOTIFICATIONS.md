# Smart notifications

Status: **designed; the rules engine (`crates/alerts`) is built, and so is
its service in `viewer-core` with the C interface (ABI 3 minor 12;
[FFI.md](FFI.md), "Smart alerts")**: trigger intake, camera wakes and
picture acquisition, the verification request and answer, decisions, the
event log, and the seams for trigger inputs and outputs. Not built yet: the
apps' side (detector, views, notifications), the input and output modules
themselves, standby, the coordinator, and models. Each section says what is built, what is planned, and what is still
to be measured. Protocol facts are marked **observed** or **reported**
exactly as in [protocol.md](protocol.md); nothing here may depend on a
reported fact until it is observed.

This page is the design behind the "Notification verifier", "Static-scene
memory and repeat suppression", and "Triggers" rows of
[ARCHITECTURE.md](ARCHITECTURE.md). Model choices and their measurements
live in [VISION.md](VISION.md); sound alarms in [AUDIO.md](AUDIO.md) and
[PLAYBACK.md](PLAYBACK.md) ("Sound").

## Goals and non-goals

Goals:

- **Tell the user about the events that matter, quickly, and say why.**
  Every notification leads with facts (camera, time, what was seen, how it
  was confirmed) and a short explanation built from typed facts.
- **Fewer false alerts without missing the important ones.** A camera's own
  AI, a sound alarm, or an external sensor is a *trigger*; nvrview looks at
  the main stream, confirms what is there, and only then decides.
- **Some things always alert.** Smoke and CO alarm sounds, a verified person
  on an outside camera at night, and a doorbell press at night (the owner's
  defaults). Nothing learned, no feedback, no model, and no schedule can
  lower them; only an explicit, time-limited mute of one camera can.
- **Nothing is ever deleted.** Quiet and suppressed decisions go to the event
  log with their full step trace; suppression only means "no notification".
- **Transparent.** The app can show a check while it happens: the pictures,
  the boxes, and every step from trigger to decision.
- **Local first.** Everything runs on the user's own machines. Nothing leaves
  the local network unless the user configures an output that sends it.
- **Honest about gaps.** When no app is watching, the apps say "Not watching
  since …", never "all clear".

Non-goals:

- **Not a certified alarm system.** Camera microphones are not certified smoke
  or CO detectors; a missed sound must never be presented as "no alarm".
  Notifications are an aid, not monitoring.
- **No cloud service of our own,** no account, no relay. Cloud outputs exist
  only when the user configures one, with a warning.
- **No classes the model does not output.** Delivery uniforms, vehicle make
  or color, and faces are out of scope until a measured model exists. A
  "package" comes only from a camera's own AI.
- **No device configuration.** Checks only open viewing-tier streams and
  playback on the existing session (CLAUDE.md "Device safety"); nothing on
  this path sends a device command.

## What exists to build on

| Building block | Where | State |
| --- | --- | --- |
| A camera's own AI and motion pushes (192, then 33), as `detection` and `alarm` start and stop events | `bc-session`, `viewer-core`, FFI.md "Events" | built. `people` and `dog_cat` and `MD` are **observed**; `vehicle`, `face`, `package`, `visitor`, `IOAlarm`, `cry`, and comma lists are **reported** (client). `timeStamp` was always 0 (**observed**), so a push's time is its arrival time. |
| Pushes arrive on a session with no streams | protocol.md "Session upkeep" | **observed**: a stream-less NVR session received 33 pushes. The basis for standby. |
| Starting main while sub plays | protocol.md "Sub and main of one camera" | **observed**: the first main picture is a fresh keyframe 125–217 ms after the request (median 215 ms), with no buffered older pictures; a sub-stream start alone took 346 ms. Main GOP about 1.9 s, sub about 9.6 s. A cold start from a stream-less session is **not yet measured**. |
| Timeline cover images (298) and per-hit covers (653) | protocol.md | **observed** (client capture). Whether either serves "the last few seconds" of a file still being written is **open**. |
| A file still being written | protocol.md "Recordings" | **observed**: the day's last file is listed with an end time equal to the search time. Playing near its live edge is **not measured**. |
| The device's event log (272–274) | protocol.md | **observed**; used for catch-up after a gap. |
| Session limit | protocol.md, status 409 | **observed**: "too many users online". |
| Sound alarms (`smoke_alarm`, `co_alarm`, source `local_audio`) | `crates/sound-events`, `viewer-core` | built |
| Incidents (stable keys, gaps, camera groups) | `recordings::incident` | built |
| Tile alerts and alert insets | UX.md "Alerts", "Alert insets" | built on macOS; Windows has call cards only (PARITY.md) |
| Detector (RT-DETR with tiles), optional multimodal model | VISION.md | measured on stills; event-level recall on video not yet measured |
| The rules engine | `crates/alerts` | **built** (this page, "The rules engine") |

## The pipeline

```
 triggers ──► intake ──► gate ──► frame acquisition ──► verification ──► classification
 (camera AI,   (normalize,  (mute, schedule,  (wake streams,      (detector boxes,     (only what
  motion,       dedupe,      class, cooldown)  keyframe now and    k of n over          the model
  sound,        lateness)                      about 1 s later,    pictures and         really
  external)                                    region crops)       angles)              outputs)
                                                                         │
             decision ◄── timeliness ◄── out-of-the-ordinary score ◄─────┘
   (alert with severity / quiet log / suppress with a reason, and an explanation)
        │
        └──► delivery, the event log, and the live verification view (the trace)
```

The decisions live in one sans-I/O crate, `crates/alerts`: no clocks, no
threads, no I/O; every time is passed in. The host (`viewer-core` inside an
app, later perhaps a headless service) does the I/O: it wakes streams, fetches
and decodes pictures, runs the detector, and delivers.

### Trigger intake

Every trigger is normalized to one record (JSON under "Contracts"): a source
(`device`, `local_motion`, `local_audio`, `external`, `nvr_input`), a camera
(device, channel, lens) or none, a kind, a state (`start`, `stop`, `pulse`),
the event time, and the arrival time.

- **Camera AI and status.** The existing `detection` and `alarm` events.
  A device field may hold several comma-separated names (reported); each
  becomes its own kind, `none` is dropped, and an **unknown name passes
  through** as an "other" kind: it is logged, never alerts on its own, and its
  text never reaches an explanation (only `[a-z0-9_]`, at most 32 bytes, is
  kept). `visitor` (a doorbell press, reported) is its own evidence. `MD`
  alone is weak: it is quiet-logged unless the camera is set to check motion.
- **Local sound.** `smoke_alarm` and `co_alarm` from our own analysis are
  triggers and their own evidence.
- **Local motion** (our own pixel motion) is out of scope until standby needs
  it; it needs decoded frames, so it is not cheap there.
- **External triggers.** A motion, infrared, or beam sensor, a door or gate
  contact, or another system's event. They arrive through:
  - **a webhook** on the local API, `POST /api/v1/triggers/{source}` with a
    per-source token;
  - **MQTT**, as a client of the user's own broker, on topics the user sets;
  - **the NVR's own alarm inputs**: only **reported** today (`IOAlarm` in the
    33 status, and push 677 "I/O" seen but not decoded). They need a probe and
    protocol.md entries before any use;
  - **a home-automation bridge**: generically, through MQTT or the webhook;
    nothing specific to any product.
- **Late triggers.** A trigger that arrives more than 30 s after its event
  (for example a replayed MQTT message), or one found by a catch-up search of
  the event log after a reconnect, is **late**. Late triggers never produce a
  "happening now" alert; they are quiet-logged ("reported 4 min late"),
  except always-alert outcomes, which alert marked late and say how late.
- **Duplicates.** A trigger id seen before is suppressed as a duplicate.
- **Stops** end episodes (see "Timeliness") and have no decision of their own.

#### External triggers: mapping and confirmation

Each external source maps to the camera angles that cover its area, each with
its own region of interest and weight:

```json
{"key": "sensor-1", "name": "Sensor 1",
 "cameras": [{"camera": {"device": 1, "channel": 2, "lens": 0},
              "roi": [[0.1, 0.5], [0.6, 0.5], [0.6, 1.0], [0.1, 1.0]], "weight": 1.0},
             {"camera": {"device": 1, "channel": 3, "lens": 0}, "roi": [], "weight": 1.0}],
 "window_before_ms": 2000, "window_after_ms": 8000, "window_max_ms": 30000,
 "classes": ["person", "vehicle", "animal"], "require": "verified",
 "k": 2.0, "min_score": 0.5, "on_unavailable": "quiet"}
```

- **An external trigger never alerts on its own** (`require: verified`, the
  owner's decision). It wakes the mapped angles and samples pictures from
  **2 s before to 8 s after** the trigger; a retrigger of the same source
  while the window is open extends it to 8 s after the retrigger, **at most
  30 s after the first**. Retriggers join the open check rather than starting
  another.
- **k of n across angles.** It becomes an alert only when an allowed class is
  found **in the region** (the box's foot point inside the polygon, and inside
  the camera's own zones) in pictures whose weights add up to k (default 2):
  two pictures of one angle, or one picture each of two angles.
- **Nothing seen** goes to the quiet log ("Sensor 1 fired; the cameras did not
  confirm it (0 of 3 pictures)") and counts toward that source's false-trigger
  rate. **No picture at all** (cameras offline, the detector timed out)
  follows the source's fail policy: quiet log by default, or alert marked
  unverified.
- Only an explicit per-source setting, `require: none`, lets a sensor alert
  without confirmation (for example a contact while the household is away);
  such alerts are marked unverified. Off by default.
- **Per-source health:** fired, retriggers, confirmed, nothing seen, could not
  look, unconfirmed alerts, last fired. The settings screen shows the
  false-trigger rate.
- **Endpoint security.** Off by default; local network only, never a public
  interface. Each source has its own token, paired like an API client but
  with a grant `trigger:{source}` that allows **only** posting triggers;
  tokens are stored hashed and revocable. Rate-limited per source (for
  example 10 a minute, bursts of 20), bodies capped, every request in the
  bounded audit log. Payload fields are data only: never spoken, never a
  command argument, never an agent instruction, never shown as text. The
  source is the token's, never the body's. A trigger can at most open
  viewing-tier streams.

#### MQTT input

`crates/mqtt-triggers` is a subscribe-only MQTT 3.1.1 client (written for
this project on rustls with the same crypto provider as the update client;
MQTT 5 is not used, and brokers that speak only 5 are not supported). The host
starts one per MQTT source with `start(config, credentials, sink, options)`
and stops it with `Handle::stop`. The sink receives
`{source_id, kind, state, sender_id, at_ms}`; the kind comes from the source's
settings, never from a payload, and `sender_id` is `mqtt:` plus the source key.

- **Connection.** Plain TCP (port 1883 by convention) or TLS (8883), with the
  broker's CA pasted as PEM or the built-in public roots. The username and
  password come from the host's keychain at start and are never part of the
  stored settings. Keepalive 30 s, clean session, client id `nvrview` unless
  set. A lost connection reconnects with exponential backoff (1 s to 60 s)
  and jitter; a login the broker rejects is **not** retried (it would lock
  the account), and the source shows "login rejected" until restarted.
- **Local brokers only.** A literal public address is refused when the source
  is saved; a name is resolved at each connection and refused unless it
  resolves to a loopback, private, link-local, or shared (carrier-grade NAT range)
  address. `allow_public` overrides this per source.
- **Mapping.** One exact topic (wildcards `+` and `#` only when the source
  allows them; use them for one logical sensor, since state is per source).
  Level mode treats the payload as a binary sensor: `ON`, `true`, `1` mean on
  and `OFF`, `false`, `0` mean off (case-insensitive, trimmed; both lists are
  settings). Off to on is a start and on to off is a stop (each can be turned
  off); a repeat of the current state is ignored. Pulse mode turns every "on"
  into one pulse. For JSON payloads a dot path (`state`, `sensor.state`,
  `items.0.state`) locates the value.
- **Retained messages** are ignored by default, so a stale "on" held by the
  broker cannot fire at connect. A source can instead use the retained value
  to seed its state (without a trigger) or treat it like any message.
- **Debounce.** A change within N ms (default 500) of the previous trigger is
  dropped and counted; the state is not updated by it.
- **Safety.** Payloads over 1 KiB are dropped unread, at most 10 messages a
  minute (bursts of 20) are examined per source, and a payload is only ever
  compared with the on and off lists: it is never copied into a trigger,
  explanation, or log line. If the connection drops while a sensor is on, no
  stop is invented; episode timeouts end it.
- **Audit.** Per-source counters: connects, reconnects, failures, messages,
  triggers, and one counter for each reason a message produced nothing
  (oversized, rate limited, retained, unrecognized, repeat, transition not
  counted, debounced, wrong topic). Connection problems are logged at most
  once every 30 s with a count of the held-back ones.

A home-automation system publishes a sensor's state to the broker in one of
two generic ways. Either an automation that publishes on each change:

```yaml
automation:
  - alias: Publish example sensor state
    trigger:
      - platform: state
        entity_id: binary_sensor.example_motion
    action:
      - service: mqtt.publish
        data:
          topic: example/sensors/motion
          payload: "{{ trigger.to_state.state }}"   # "on" or "off"
```

or the `mqtt_statestream` integration with `base_topic: example`, which
publishes `example/binary_sensor/example_motion/state` (and attributes under
sibling topics). In nvrview, add a source with that topic, kind `motion`, and
the default mapping; map it to the cameras that cover the area like any
external source. Publish with retain off, or leave the default of ignoring
retained messages in place.

### Gate

Cheap, before any picture is fetched, in this order: camera **muted**; the
kind is unknown; then, unless an always-alert rule could apply, the camera's
**schedule**; motion on a camera that does not check motion; the **class is
off** for the camera; a **cooldown** for this camera and class. A trigger
stopped at the gate is still logged, with its reason. An always-alert
candidate (smoke or CO; a doorbell press at night; a person or motion on an
outside camera at night) passes everything but a mute.

### Frame acquisition

Recognition runs on the **main stream**: VISION.md's recall numbers come from
stills of 1280×720 or more, and small distant figures are where models fail.

1. **The picture at the trigger.** Start main on the existing session (reuse
   a running tile's stream). The first picture is a fresh keyframe about
   0.2 s later (observed with the sub-stream running; a cold start is to be
   measured). Decode only that picture.
2. **About 1 s later**, a second picture, decoded forward through the GOP (a
   keyframe-only decoder may restart main for a second fresh keyframe, about
   0.2 s, or wait for the next natural one, about 1.9 s). The pair tells
   moving from parked.
3. **Crops:** the whole frame plus 4 tiles (tiles gave +16 points of recall
   on small people in VISION.md), plus the region's bounding crop when there
   is one.
4. **Before the trigger**, for external triggers and late pushes, from the
   NVR's recording rather than a local buffer: a cover image for the window,
   playback of the file still being written, or a download by time range for
   the clip. All three are **to be probed**: whether they serve a window that
   ended seconds ago, and with what lag. An NVR that records only on motion
   has nothing before its own trigger; the UI says so.
5. **Bounds:** at most 4 checks at once per device and 8 per process; more
   wait, ordered by severity. Pictures are compressed access units held only
   until the check ends; streams opened for a check stop when it ends.

### Verification

- **Detector:** RT-DETR R18 with tiles by default (R50 selectable), mapped to
  person; vehicle (car, truck, bus, motorcycle); bicycle (separate); animal
  (dog, cat; other animals gated, since VISION.md measured 15 % animal false
  positives from foliage ungated).
- **k of n:** a class is **verified** when pictures with a box of that class
  at or above the score floor, with the foot point in the region and zones,
  reach k (default 2 of 2 for a camera's own trigger; across angles with
  weights for external triggers). One hit is **weak**: rules may still alert
  one severity lower, but weak never satisfies an always-alert rule.
- **Thresholds** per class, camera, and day or night (starting points from
  VISION.md: person 0.5 with tiles, 0.6 at night; vehicle 0.6, stricter at
  night for headlights and infrared glare), tuned by feedback within bounds.
- **Cheap filters first:** static-scene memory, repeat suppression, and masks
  (ARCHITECTURE.md).
- **Agreement with the camera's AI:** a camera `people` trigger our check
  confirms says "The camera's AI agreed". When our check sees nothing, the
  owner's rule applies: **quiet log**, except that on a camera marked **"trust
  the camera's AI"** an always-alert case alerts anyway, marked
  **unverified**. When no picture could be checked at all, always-alert cases
  fail open (alert, unverified) and other cases alert one severity lower,
  unverified.

### Classification limits

Only what the model really outputs: person, vehicle, bicycle, animal.
`package` and `face` come only from a camera's own AI (and say so in the
explanation); COCO's suitcase, backpack, and handbag are not "package". The
optional multimodal step may *describe* more in a caption, but a caption is
never evidence and never drives a decision.

### Out-of-the-ordinary: rules first

Rules are deterministic and explainable; each yields a severity: low, normal,
high, or critical. Below the lowest alerting severity (normal by default) an
event goes to the quiet log.

- **Exposure per camera:** outside, perimeter, or inside, plus zones
  (normalized polygons tested by a box's foot point; include zones and
  exclude masks) and the entrance mark already used by the insets. Outside
  and perimeter both count as "outside" for the always-alert rules.
- **Night:** from sunset + 30 min to sunrise − 30 min, computed from a coarse
  location the user enters (rounded to 0.1°, about 11 km, stored locally)
  with the NOAA solar equations, in pure arithmetic in the core. The core has
  no time-zone database: the host passes its current UTC offset, which
  already includes daylight saving time; the answer for an instant does not
  depend on it. Close to the poles a day can be all night (polar night) or
  have no night (polar day), and margins can swallow a short summer night.
  Without a location: 22:00–06:00 local time.
- **Global mode:** home (people on inside cameras are expected), away
  (everything one step more urgent, at most high), night (treated as night
  whatever the sun does).
- **Per-camera schedules:** always, only at night, or local-time windows by
  weekday (a window crossing midnight belongs to the day it starts).
- **Severity table** (verified or self-evident): person outside normal, at
  night high; person inside low at home, normal in night mode, high away;
  vehicle outside normal; animal low; package normal; doorbell press high;
  smoke or CO high; anything else low.
- **Always-alert rules** (owner defaults, each can be turned off): smoke and
  CO alarms; a verified person on an outside camera at night; a doorbell
  press at night. Their outcome is always critical. Nothing learned, no
  feedback, no model, and no quiet hours lower them; only a mute of one
  camera for at most 24 h stops them.

### Out-of-the-ordinary: learned second

Learned baselines are advisory and come later. They may promote a low
severity to normal ("Rare here at this hour (about 1 in 40).") or demote a
normal one to low ("Common here at this hour."), one step, never touching an
always-alert outcome.

- **Rate model:** events per camera, class, and hour of the week from the
  recordings index over 4–8 weeks (requested with `nv_recordings_want`, at
  most 31 days a call), smoothed with a Gamma–Poisson prior pooled over
  neighboring hours, the same hour on other days, then the camera's overall
  rate. Cold start: rules only.
- **Novelty:** a class rarely seen on this camera; a position outside the
  usual heat map; an unusual count; later, an unusual path across cameras.

### Timeliness: episodes

One **episode** per camera and class, joined to the recordings' **incident
key** so that alerts, playback, and the incident list agree.

```
Pending ──alert──► Active ──seen again──► Lingering ──misses and the camera's stop, or idle──► Settled
   └──quiet, or no decision in time──► Settled
```

- **Cooldown** per camera and class escalates: 1 min after the first alert,
  then 5 min while activity continues. Within a cooldown a trigger is
  suppressed ("already alerted 28 s ago") unless it breaks through: a higher
  severity, or the episode's first always-alert outcome. Always-alert
  outcomes are never suppressed: inside a cooldown they update the existing
  alert instead of starting a new one.
- **Updates, not new alerts:** "still present" after the cooldown, a higher
  severity, the same class on another camera of the same incident ("Also seen
  after Camera 1"), each raises the alert's revision.
- **Still present:** while an episode is active or lingering, the host
  re-checks every 10 s (sub-stream first) for at most 10 min; 2 misses and the
  camera's own stop settle it, as does 2 min without activity. Loitering
  (dwell above a per-camera threshold at night) raises severity once (later).
- **Dedupe:** the same incident and class is one alert; triggers that land on
  one incident merge and each adds to the explanation.

### Decision and explanation

The outcome is one of **alert** (severity, alert id, revision, late,
unverified, the always-alert rule if any), **quiet** (a reason), or
**suppress** (a reason), always with an explanation built by templates from
the trace, never from model prose or trigger text:

> Person on Camera 2 at 22:30 (night, perimeter camera). Verified in 2 of 2
> pictures (0.75). The camera's AI agreed. Always alerts: a person on an
> outside camera at night.

> Sensor 1 fired at 13:00. Not alerted: the cameras did not confirm it (0 of
> 3 pictures).

Camera and sensor names are the user's own names from settings (or "Camera
N"), cleaned of control characters and capped at 40 characters.

## Where it runs

**App-hosted first** (the owner's decision): the engine runs inside a desktop
app's `viewer-core`, using the app's session per NVR, its hardware decoders,
and its platform model runtime (Core ML on macOS, Windows ML on Windows). An
app only watches while it runs and the machine is awake.

### A coordinator among running apps

Several apps on one local network (a desktop, a laptop) would otherwise each
check and notify. Instead they **elect one smart-alert processor**:

- **Discovery.** Apps announce themselves on the local network (mDNS, then
  the local API with mutual pairing as API clients) and only coordinate with
  apps that watch the same devices (a fingerprint of the device set, not the
  devices' identities).
- **Election.** Candidates rank by a tuple, highest wins, computed the same
  way everywhere so every candidate agrees without a vote (see "Choosing the
  processing app: measured capability" below): the user's override, preferred
  tier (a future always-on headless service ranks above every desktop app),
  measured detector speed, staying power, connection to the devices, current
  load, then earliest start and instance id as tie-breaks.
- **Lease.** The processor holds a lease it renews every 5 s; it expires after
  15 s. Followers keep their device sessions (each app already has its own
  login per NVR) but do not wake streams for checks or deliver; they show the
  processor's alerts and live traces through the local API.
- **Startup check.** An app that opens listens for a processor for one lease
  period before it may claim; if one exists it becomes a follower.
- **Handoff.** A processor that closes hands off explicitly to the next in
  rank, passing its open episodes (keys, cooldowns, alert ids) so updates
  stay updates. A processor that vanishes is replaced when its lease expires;
  the new one runs a catch-up search of the event log for the gap, and those
  triggers are late.
- **Split brain.** Two processors that see each other resolve by rank at
  once; for at most one lease period a duplicate notification is possible,
  and alert ids carry the processor's instance id so duplicates are
  recognizable in the log.
- **A headless service later** joins as a candidate of the preferred tier and
  wins the election whenever it runs; nothing else changes. (It needs its own
  decoder and model runtime; a second login per NVR is the cost unless apps
  relay through it, per API.md.)
- **"Not watching".** When no processor runs, every app shows "Smart alerts:
  not watching since hh:mm" (and the standby view the same), never "all
  clear". Phones cannot know this on their own: an optional heartbeat to the
  push service lets the user set up a dead-man check on their side.

### Choosing the processing app: measured capability

When several apps on the local network can process smart alerts, each one
advertises a capability score, and every app ranks the candidates the same
way, so they agree without voting:

1. **The user's override** wins: "Prefer this computer for smart alerts".
2. **Measured detector speed.** On first start and after each update, the app
   runs the detector on a few synthetic frames and records the time per frame
   (this captures the GPU, neural engine or NPU, memory and drivers together,
   which specifications alone do not), plus whether it has the memory for the
   optional multimodal check.
3. **Staying power.** On AC power and not set to sleep ranks above a laptop on
   battery; a computer that has stayed reachable longer, from its own recent
   history, ranks above one that comes and goes.
4. **Connection to the devices.** A wired connection on the devices' network
   ranks above Wi-Fi, which ranks above a VPN.
5. **Current load.** An app showing a large live grid ranks slightly below an
   idle one.

A running processor is replaced only when another candidate is clearly
better (by a margin), so two similar computers do not hand the role back and
forth. A future always-on service still ranks above every desktop app unless
the user overrides it.

### Sessions and logins

One session per device per app carries events, streams, playback, and
downloads; a check only adds previews to it and never adds a login. Waking N
cameras means N main previews; the per-device cap applies (4 by default).
Playback alongside live view was observed with up to 4 plays at one seek
time; pre-trigger fetches for several cameras are batched at one seek time.
A refused login stays terminal (existing rule) and the processor says so once.

## Standby: event-driven viewing

Keep each device session with its event subscription (192) and **no
streams**; a trigger wakes only the relevant cameras (main for the pictures,
sub if a view is wanted), and streams stop when the episode settles.

- **The apps** get a **Standby** toggle (menu bar on macOS, tray on Windows)
  and an option to switch to standby **automatically on battery** (offered;
  off until the user turns it on). In standby, tiles freeze on their last
  picture, dimmed; devices stay connected; a trigger wakes and checks the
  relevant cameras; an alert notifies with a snapshot (later a clip) and can
  show the verification view.
- **Network loss:** a session drop ends every active detection (existing
  behavior); standby shows "Not watching since hh:mm"; on reconnect a
  catch-up search yields late triggers and one summary ("While disconnected:
  3 person events on 2 cameras").
- **Wake budget** (trigger to decision; target p95 under 3 s; critical rules
  may alert after the first picture and update after the second):

  | Step | Number | Status |
  | --- | --- | --- |
  | Real event to 33 push | the camera's AI delay | **to probe** |
  | Push to main start request | about 1 ms | ours |
  | Main start to first keyframe | 125–217 ms with sub running | observed; **cold start to probe** (sub alone took 346 ms) |
  | Decode one keyframe | a few ms in hardware | to measure |
  | Inference, R18 whole frame and 4 tiles | 53 ms on the Neural Engine | VISION.md; Windows to measure |
  | Second picture | +1 s by design | |
  | Delivery | ntfy on the local network | to measure |

- **To measure:** standby idle traffic per NVR; traffic per wake; laptop
  energy in standby against live with 4 and 16 tiles; cold and warm wake
  latency.

## Verification view and event log

Every check can be watched while it happens and read back later.

### The live view

- **Where.** In the camera's tile, as a band across its bottom (or a side
  panel in a large tile). Per a setting, and automatically when the camera is
  not on screen, as a **verification inset** in the viewer window, the same
  mechanism as the doorbell call insets (draggable, edge snapping, position
  per window), or as a small **non-activating window** that never takes focus
  (like the doorbell call window's ring behavior). Several checks at once
  stack, newest on top, with a count when they overflow.
- **What.**
  - The live picture of each mapped angle (the stream the check opened), with
    the detector's boxes and class and score drawn as pictures arrive; hits,
    boxes below the floor, and boxes outside the region are drawn
    differently (and labeled, not by color alone).
  - A **step-by-step log** of every check, from the trace: the trigger
    received (source, kind, time, late or not); the gate's result (mute,
    schedule, class, cooldown); cameras woken (time to the first picture);
    each picture sampled per angle, with its detections above or below the
    floor and in or out of the region; k-of-n progress per class, angle, and
    overall; the rules (night and its basis, exposure, zone, mode,
    always-alert); rarity and novelty inputs when they exist; timeliness
    (episode, incident, cooldown, update); the decision with its explanation;
    and delivery results per output.
- **Timing.** It stays at least **60 s after the decision**, longer while the
  episode is active; a close button on each, Esc when focused, and "Dismiss
  all". It **never steals focus**.
- **Settings:** where to show (tile, inset, window), whether to show checks
  that end quiet or suppressed (default: alerts and always-alert candidates
  only), and the minimum time on screen.

### The event log

An **Events** view (a sidebar entry and a window of its own) lists every
decision, quiet and suppressed included, newest first: time, camera or
sensor, class, decision, severity, and the explanation's first sentence.

- Filters: camera, source (camera AI, sound, sensor), decision (alert, quiet,
  suppressed), severity, date range, and text over the explanations.
- Opening an entry shows the full trace (the same renderer as the live view),
  its snapshots with boxes, and links to jump to the recording at that time or
  to the incident.
- Feedback (below) is given here as well as on notifications.
- **Retention:** the log keeps 90 days by default, rotated by count and size;
  snapshots 7 days; entries marked "keep for evaluation" until deleted. All
  local, under the app's data directory.

### Data

The trace is the engine's typed, ordered list of steps (see "Contracts"),
emitted as each step happens (intake, each picture, the decision) so the view
renders live, and stored with the decision. It is capped at 64 steps per
check: picture and progress steps beyond the budget are counted in one
`omitted` step, and the steps that carry the decision always fit.

### Parity and self-tests

PARITY.md lists the view, inset, window, and log as planned for both apps.
Self-test ideas, with shared IDs: `verify-view-tile` (a scripted check renders
in the tile band with boxes), `verify-view-inset` (camera off screen: an inset
appears, does not take focus, and stacks a second check),
`verify-view-dismiss` (60 s minimum on the simulated clock, close and dismiss
all), `events-log-filter` (filters and opening a trace), `events-log-jump`
(jump to the recording at the decision time).

## Delivery

- **Desktop:** macOS `UNUserNotificationCenter` with a snapshot and actions;
  Windows App SDK `AppNotificationManager` (an unpackaged app needs a
  registered app identity: a parity risk to check early). Both also raise
  the existing tile alerts and insets; critical alerts raise the tile alert as
  smoke and CO do today.
- **Phone push (each opt-in, several allowed):**
  - **self-hosted ntfy** (the owner's choice for phones): priorities
    critical 5, high 4, normal 3; actions as callbacks to the local API, which
    work on the local network or a VPN;
  - **MQTT** to the user's broker: events and alerts on topics, optional
    generic discovery for home-automation software;
  - **a generic outgoing webhook** (the decision JSON).
- **Pictures in pushes: on by default** (the owner's choice), **per output**.
  The settings screen warns when an output's endpoint may be outside the
  local network (anything but loopback, private and link-local addresses, and
  `.local`, `.lan`, `.home.arpa` or single-label names), since the picture
  then leaves the house.
- **Clip** (about 10 s, downloaded by time range after the episode settles) as
  an update, linked through the local API.
- **Actions:** open live view; open the recording; mute the camera for 15 min,
  1 h, or until morning; useful or not useful; acknowledge.
- **Quiet hours** per output and severity: critical always passes; normal and
  high are held for a morning digest.
- **Escalation** for always-alert outcomes: repeat every 2 min until
  acknowledged, at most 5 times.
- Delivery results go into the trace (`delivery` steps).

### Home Assistant output

`crates/ha-output` drives Home Assistant's REST API from decisions. It is a
separate crate; the core hosts it (directly or on its bounded-queue worker)
and records each result as a `delivery` step such as "Home Assistant:
`input_boolean.example_alert` pulsed (200)".

- **Helper entity pulse** (for voice-assistant routines that trigger on a
  helper turning on): read `GET /api/states/<entity_id>`; if the state is
  already `on`, turn it off first; `POST /api/services/input_boolean/turn_on`;
  after the pulse gap (default 2 s) `turn_off`. The reset exists because a
  routine fires on an off-to-on change: a helper left on (a failed earlier
  turn-off, or someone toggling it) would swallow the next alert. Success is
  HTTP 200 with a JSON list. If the final turn-off fails, the result says the
  entity may be left on, and the next pulse resets it.
- **Other actions:** plain `turn_on` (left on), `script.turn_on` with the facts
  as script variables, and `notify.<service>` with a message from a template.
  Templates use only `{camera}`, `{class}`, `{time}`, `{severity}`, and
  `{explanation}`, filled in one pass from typed alert facts (control
  characters removed, 200 characters at most), so no trigger payload text is
  ever sent, spoken, or executed. Entity ids and service names are validated
  before they reach a request path.
- **Mapping** (settings): rules match on class, trigger source id, camera,
  minimum severity, and decision (alerts only by default) and name an action.
  The mode is first match (default) or all matches (one call per distinct
  target). Each target has a cooldown (default 30 s, overridable per rule)
  so one event does not speak twice; a repeat inside it reports "skipped
  (cooldown)". Updates of an alert (revision above 1) are not sent again.
- **Reliability:** each request has a timeout (5 s) and, on network errors and
  5xx, up to 3 tries with doubling backoff from 500 ms (waits go through the
  injected clock). 4xx is never retried: 401 or 403 reads "token rejected",
  404 "entity not found". Error lines in the log are limited to 5 a minute with
  a count of the rest. Delivery runs on a worker with a bounded queue; a full
  queue drops the alert and the host records a failed delivery.
- **Address and token:** `https://` anywhere with certificate checks; plain
  `http://` only for hosts on the local network (the same rule as the
  picture warning above). Redirects are never followed. The long-lived access
  token comes from the host's keychain on each start, is sent only as
  `Authorization: Bearer`, is never stored or logged by the crate, and prints
  as redacted.

## Feedback and learning

- **Collection:** useful or not useful on the notification, in the event log,
  and in the incident inspector. "Not useful" asks why: nothing there, wrong
  type, real but not interesting here or now, duplicate.
- **Effects** (bounded, per camera and class, visible and reversible, with
  "reset learning"): *nothing there* raises that camera's threshold for the
  class by 0.05 (at most +0.15) and offers a mask at the box; *not
  interesting* lowers the weight of that hour of the week; *wrong type* only
  counts; *useful* lowers the threshold one step, never below the floor. At
  most one step per day. **Never applied to always-alert rules.**
- **Storage,** local only: the decision log (rotated by count and size),
  feedback, baseline counts (cameras × classes × 168 hours), tuning, and the
  snapshot cache.

## Privacy

- Nothing leaves the local network unless the user configures an output.
  Pictures in pushes are a per-output choice with a warning for endpoints
  that may be outside it.
- The location is coarse (0.1°), entered by the user, stored locally, and used
  only for sunrise and sunset.
- Feedback-labeled pictures form a **local** evaluation set the user can
  export for the vision lab; it is never committed, and only aggregate metrics
  appear in docs (SANITIZATION.md).
- Trigger payloads never become text that is shown, spoken, or executed.

## Contracts

All JSON uses stable snake_case names; the trace carries a `version` (now 1).

**Trigger** (input; the webhook body is only `{"kind", "state"?, "at_ms"?}`,
the source comes from the token):

```json
{"id": 12, "source": "device", "camera": {"device": 1, "channel": 3, "lens": 0},
 "kind": "people", "state": "start", "at_ms": 1718900000000,
 "received_ms": 1718900000000, "catch_up": false}
```

**Decision** (an event to the apps and over the local API; one line in the
log; the body of webhook and MQTT outputs). `decision` is `alert`, `quiet`,
or `suppress`; quiet and suppress carry a `reason` instead of the alert
fields:

```json
{"trigger_id": 12, "camera": {"device": 1, "channel": 3, "lens": 0},
 "class": "person", "decision": "alert", "severity": "critical",
 "alert_id": 4, "rev": 1, "late": false, "unverified": false,
 "always": "night_outside_person", "incident": "i-…",
 "explanation": "Person on Camera 4 at 22:30 (night, outside camera). …",
 "decided_ms": 1718900001300,
 "trace": {"version": 1, "trigger_id": 12, "steps": [
   {"at_ms": 1718900000000, "step": "trigger_received", "source": "device",
    "kind": "people", "class": "person", "camera": {"device": 1, "channel": 3, "lens": 0},
    "event_ms": 1718900000000, "late": false, "age_ms": 0},
   {"at_ms": 1718900000000, "step": "gate", "result": "pass_always", "rule": "night_outside_person"},
   {"at_ms": 1718900000000, "step": "sampling", "cameras": [{"device": 1, "channel": 3, "lens": 0}],
    "from_ms": 1718899998000, "until_ms": 1718900010000, "classes": ["person"], "k": 2.0},
   {"at_ms": 1718900000250, "step": "frame_sampled", "camera": {"device": 1, "channel": 3, "lens": 0},
    "frame_ms": 1718900000200, "detections": [{"class": "person", "score": 0.75,
    "above": true, "in_region": true, "hit": true}], "more": 0},
   {"at_ms": 1718900000250, "step": "progress", "class": "person", "weighted": 1.0,
    "needed": 2.0, "hits": 1, "n": 1, "angles": 1},
   {"at_ms": 1718900001300, "step": "checked", "check": {"status": "verified", "class": "person",
    "hits": 2, "n": 2, "angles": 1, "score": 0.75, "moving": null}, "agreed_with_device": true},
   {"at_ms": 1718900001300, "step": "rules", "class": "person", "night": true, "basis": "sun",
    "exposure": "outside", "entrance": false, "mode": "home", "support": "verified",
    "always": "night_outside_person", "severity": "critical", "unverified": false},
   {"at_ms": 1718900001300, "step": "timeliness", "admission": "new", "alert_id": 4},
   {"at_ms": 1718900001300, "step": "decision", "decision": "alert", "severity": "critical",
    "alert_id": 4, "rev": 1, "late": false, "unverified": false, "always": "night_outside_person"}]}}
```

Other steps: `retrigger`, `camera_woken` (`first_picture_ms`), `rarity`
(`one_in`, `effect`), `delivery` (`output`, `kind`, `ok`), `omitted`
(`steps`). Gate results: `pass`, `pass_always`, `muted`, `schedule`,
`class_off`, `cooldown`, `motion_only`, `unknown_kind`, `unknown_source`,
`no_camera`, `duplicate`. Quiet reasons: `not_confirmed`, `nothing_seen`,
`outside_region`, `could_not_look`, `low_severity`, `late`, `motion_only`,
`unknown_kind`, `no_decision`. Suppress reasons: `muted`, `schedule`,
`class_off`, `cooldown`, `duplicate`, `unknown_source`, `no_camera`.

**Feedback:**

```json
{"alert_id": 4, "vote": "down", "reason": "nothing_there"}
```

**Settings** (`alerts.json`): `cameras[]` (camera, name, exposure,
entrance, `trust_camera_ai`, classes, zones, schedule, `verify_motion`),
`always_alert` (`smoke_co`, `night_outside_person`, `doorbell_night`, all on),
`mode`, `location` (or null), `night` (30 and 30 min, fallback 22:00–06:00),
`sources[]` (the mapping above), `timing` (cooldowns 60 s and 300 s,
staleness 30 s, settle 120 s, re-check 10 s for at most 600 s, 2 misses),
`verification` (k 2, score floor 0.5, window 2 s before and 10 s after),
`min_alert_severity` (normal), `outputs[]` (kind, enabled, endpoint, minimum
severity, pictures on), `standby` (automatic on battery off, 4 wakes per
device), `learning` (on, promote at 1 in 20, demote at 1 in 2). Validation
never fails: it clamps every value into range, rounds the location to 0.1°,
drops unusable sources and duplicate cameras, and lists what it changed.

### C interface

Built as ABI 3 minor 12; [FFI.md](FFI.md), "Smart alerts", is the
contract (it adds `nv_alert_snapshot_get` and `nv_alerts_trigger_put` with
the trigger's ID, and names the settings' `enabled`, `utc_offset_min`,
`acquisition`, and `inputs`). Standby (`nv_standby_set`) and the
coordinator's `processor_state` are not built. The design as first
proposed, modeled on the thumbnail get-and-put pattern:

```c
NvStatus nv_alerts_configure(NvCore*, const char *settings_json);
NvStatus nv_alerts_get(NvCore*, const char *query_json, char *out, size_t cap, size_t *len);
NvStatus nv_alert_frames_get(NvCore*, const char *request_key, NvThumbnail *info,
                             uint8_t *out, size_t cap, size_t *len);   /* keyframe .. target AUs */
NvStatus nv_alert_detections_put(NvCore*, const char *request_key, const char *boxes_json);
NvStatus nv_alert_snapshot_put(NvCore*, uint64_t alert_id, const uint8_t *jpeg, size_t len);
NvStatus nv_alert_feedback(NvCore*, const char *feedback_json);
NvStatus nv_alert_mute(NvCore*, const char *camera_json, uint32_t seconds);
NvStatus nv_standby_set(NvCore*, bool on);
NvStatus nv_trigger_put(NvCore*, const char *trigger_json);           /* app-side sources, tests */
```

New events: `verify_request` (request key, camera, picture list, crops),
`verify_step` (one trace step, for the live view), `alert` (a decision, new
or updated by revision), `standby_state`, `processor_state` (this app's role
in the coordinator, and since when nothing watches), `trigger_rejected`. The
app decodes, runs its model, and returns boxes; the core does the rest. A
`verify_request` with no answer in time counts as "could not look" (fail open
for always-alert cases).

## The rules engine (`crates/alerts`)

Built and tested; wired to `viewer-core` (below).

- `trigger`: normalization (device field lists, unknown kinds, external
  bodies with keys checked and future times clamped).
- `engine`: `intake` (gate, then "sample these angles in this window",
  "decide now", "merged into an open check", or a decision), `camera_woken`
  and `sample` (live trace steps, early "enough seen"), `decide` (rules,
  rarity, timeliness, decision, explanation), `observe` (still present),
  `tick` (settles idle episodes, ends mutes, decides checks the host never
  finished), `mute` (at most 24 h).
- `rules`, `sun`, `episode`, `verify` (k-of-n counting; the `Sample`
  detector input and the `RarityModel` trait with a no-baseline stand-in are
  where later slices plug in), `external` (windows, source health), `trace`,
  `explain`, `settings`, `zone`.
- Bounded: 1024 episodes, 256 open checks, 4096 remembered trigger ids, 64
  trace steps, 8 detections per picture step.

## The service in `viewer-core`

Built (`viewer_core::alerts_owner`; the C contract is FFI.md "Smart
alerts"):

- **Intake**: a tap on the core's event queue turns `detection`, `motion`,
  and `alarm` events into triggers; external sources reach it through
  `ExternalTriggers` (a bounded queue input modules such as MQTT feed,
  rate-limited per source) and the app through `nv_alerts_trigger_put`.
- **Acquisition**: one wake per camera, shared by the checks that need it
  and by any tile showing the same stream; the first keyframe, a picture
  about 1 s later, and for external checks more every 2 s while the window
  is open; a cover image (298) of the last 10 s when no picture came in
  3 s. Caps per device (`standby.max_wake_per_device`) and per process
  (8), waiting cameras ordered by priority (always-alert candidates
  first), a re-check hold given up for a new check.
- **Verification**: `verify_request` events; boxes from the app become the
  engine's samples; no answer in 5 s counts as not checked.
- **Timeliness**: still-present re-checks every `recheck_period_s` while an
  alerted episode lasts, for at most `recheck_limit_s`; the stream stops
  when the episode settles.
- **Outputs**: modules implementing `AlertOutput`, built from `outputs[]`
  by kind (the engine's kinds, `home_assistant` included); each result is
  a `delivery` step. Delivery is asynchronous (a `DeliveryDone` handle,
  30 s limit); outputs that send pictures wait up to 2 s for the app's
  snapshot.
- **Storage**: the decision log, snapshots, and feedback under
  `data_dir/alerts/`, each bounded (FFI.md).

## Testing and evaluation

- **Engine** (built): unit tests for every rule and state transition on
  explicit times; sunrise and sunset against reference values from an
  independent implementation of the NOAA equations (mid and high latitudes,
  both hemispheres, polar day and night, and offsets that differ only by
  daylight saving time); golden explanation strings and trace JSON; a
  scripted day (camera AI, a smoke alarm, an external sensor with and without
  confirmation, a reconnect gap with late events); property tests that an
  always-alert outcome is never suppressed and that cooldowns never shrink
  within an episode.
- **End to end against `mock-nvr`** (built for the service:
  `crates/viewer-core/tests/alerts.rs` on the simulated clock with a
  scripted detector: external trigger to alert with trace, not confirmed,
  no answer failing open, smoke and mutes, caps, a shared tile stream, a
  reconnect mid-check, snapshots and the log, and a property test that
  woken streams always stop; still planned: the webhook, catch-up, and
  the coordinator): scripted 33 pushes, synthetic
  streams, and a scripted detector (boxes keyed by camera and time), covering
  wake, request, boxes, alert, settle, stream stop; network loss and catch-up;
  never a second login; the webhook (token, rate limit, region, window,
  k-of-n across angles); the coordinator's election, handoff, and lease
  expiry on simulated clocks.
- **Apps:** self-test steps with `// parity:` markers on both apps for the
  notification, its actions, standby wake, the verification view, the event
  log, and the feedback round trip.
- **Evaluation** (private data stays local; only aggregates in docs): replay
  labeled event clips through the engine; event-level precision and recall
  per class, day and night; false alerts per camera per day; **missed
  always-alert events (target 0)**; trigger-to-alert latency (target p95
  under 3 s); per-sensor confirmation rate.

## Slices

| # | Slice | Depends on |
| --- | --- | --- |
| S0 | This page; ARCHITECTURE.md pointer; PARITY.md rows | — |
| S1 | Hardware probes: push delay, cold wake, pre-trigger fetch, standby idle traffic, alarm-input pushes, the reported push kinds, a day/night state if one exists | — |
| S2 | `crates/alerts`: rules engine, trace, explanations, settings (**done**) | S0 |
| S3 | Verification maths: crop and tile plan, box merge, per-class thresholds, IoU tracking across pictures and angles, moving vs parked, static memory, repeat suppression | S2 |
| S4 | `viewer-core` orchestration: wake streams, picture collection, caps, still-present re-checks, catch-up, decision log store, trace events; ABI minor; `mock-nvr` end-to-end | S2, S3 |
| S5 | Baselines: index backfill, Gamma–Poisson rates, novelty map | S2 |
| S6 | macOS: Core ML runtime, notifications and actions, settings, verification view, event log, feedback | S4 |
| S7 | Windows: Windows ML, the same contract, app notifications, settings, verification view, event log, feedback | S4 |
| S8 | Standby in both apps ("not watching since", energy and traffic) | S4, S1 |
| S9 | Coordinator: discovery, pairing between apps, ranked election, lease, handoff, startup check | S4, local API |
| S10 | Minimal local API: pairing, the `trigger:{source}` grant, webhook, action callbacks, audit log, limits | S2 |
| S11 | Outputs: ntfy, MQTT (also as trigger input), webhook; quiet hours, escalation, digests | S4 |
| S12 | External triggers end to end: mapping UI, pre-trigger fetch, cross-angle k-of-n, source statistics | S3, S4, S10, S1 |
| S13 | Feedback learning | S6, S7 |
| S14 | Optional multimodal verifier (borderline band, captions after the fact, budget, fail open) | S4 |
| S15 | Evaluation harness; pick k-of-n and threshold defaults | S3 |
| S16 | A headless service as a preferred candidate (decoder, model runtime, packaging) | S9 |

## Decisions

| Question | Decision |
| --- | --- |
| Where it runs | **The app first.** Then a **coordinator among running apps** on the local network electing one smart-alert processor, with handoff on close and a check on startup; designed so an always-on headless service can later join as a preferred candidate. Honest "not watching" when no app runs. |
| Always-alert defaults | **Smoke and CO alarms; a verified person on an outside camera at night; a doorbell press at night.** Everything else follows the rules and score. |
| The camera's AI says person, our check disagrees | **Quiet log.** Exception: on a camera marked "trust the camera's AI", an always-alert case alerts, marked unverified. |
| External motion and beam sensors | **Visual confirmation required**: window 2 s before to 8 s after, extended by retriggers to at most 30 s after the first. `require: none` only by explicit per-source choice. |
| Night | **Sunset + 30 min to sunrise − 30 min** from a coarse user-entered location (0.1°); **22:00–06:00** without one. |
| Phone push | **Self-hosted ntfy**; **MQTT and webhooks** as well. |
| Pictures in pushes | **On**, per output, with a warning when the endpoint may be outside the local network. |
| Standby in the apps | **A toggle**, plus **automatic on battery** as an option. |
| Live verification view | **Shown** in the tile, an inset, or a non-activating window; live pictures with boxes and a step log; at least 60 s after the decision; never steals focus; every check in a browsable event log. |
| Learning | On, bounded, visible, resettable; never touches always-alert rules (proposed default). |
| Multimodal verifier | Off; when on, only for the borderline band and captions (proposed default). |
| Retention | Event log 90 days, snapshots 7 days, evaluation pictures until deleted (proposed default). |
