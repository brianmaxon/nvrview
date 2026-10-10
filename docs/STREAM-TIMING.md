# Stream timing: capture, analyze, replay

A camera's sub-stream and main stream are separate previews of the same
picture. To switch a tile from sub to main without a visible jump or a
delay, the core needs to know how the two relate in time: whether their
video timestamps run on one clock, how much later main arrives than sub
for the same moment, and what happens when main is started while sub runs.
This page describes the tools that measure that, and how `mock-nvr` replays
what they record. The numbers themselves belong in
[protocol.md](protocol.md) once measured on hardware (aggregates only).

## 1. Capture

`bc-cli capture` records several tiers of one channel on **one session** and
**one clock** into an `NVRREC02` recording:

```sh
bc-cli --host 192.0.2.10 --user example-user --keychain SERVICE:ACCOUNT \
  capture --channel N --tiers sub,main --raw captures/timing.nvrrec \
  --seconds 600 --stagger 2 --restart main:30 --restart-gap 2
```

- The tiers start in the order given (`--tiers`, default `sub,main`),
  `--stagger` seconds apart (default 2).
- `--restart TIER:EVERY_S` stops that tier after `EVERY_S` seconds and starts
  it again `--restart-gap` seconds later (default 2), repeatedly, so one
  capture holds many start-latency samples. The gap must let the stop reach
  the device before the next start (the session would otherwise keep the old
  preview).
- Only preview start and stop are sent, through ordinary subscriptions;
  nothing is added to the command allowlist. The session is closed on every
  exit path (end of `--seconds`, Ctrl-C, errors).
- Every record is written with its arrival time; a "start requested" marker
  is written just before each start is requested, and a "stop requested"
  marker at each stop. Video records keep their extra headers, so the
  device's wall-clock seconds survive.
- A short per-tier summary (starts, stops, records, keyframes, gaps,
  dropped frames) is printed at the end. Dropped frames mean the writer fell
  behind; such a capture's latency figures are suspect.

Recordings of real devices hold real video: keep them in the git-ignored
`captures/` directory ([SANITIZATION.md](SANITIZATION.md)).

Limits of the method: arrival times are taken when a record leaves the
session's subscriber queue (after demultiplexing), not when its first byte
reached the socket. The queue holds back predicted frames and audio until
the first keyframe of each start, so a device that began with predicted
frames would show a later first record, not those frames.

## 2. Analyze

```sh
bc-cli analyze captures/timing.nvrrec > timing.md
```

prints a Markdown report (aggregates only: no absolute times, wall-clock
values, or picture contents). Every pair of recorded tiers is compared
(main against sub, extern against mobile, and the rest); the reference of
a pair is the first of sub, mobile, main, extern in it.

- **Tiers**: frame rate from timestamps and from arrivals, GOP length in
  frames and seconds, keyframe and other frame sizes, bit rate, timestamp
  drift against arrivals, and, over the keyframes (only they carry the
  wall-clock seconds), the spread of *pts − seconds × 10⁶* around its
  median and whether it stays within one second.
- **Clock relation**: per run of the other tier (start or discontinuity to
  the next), the median of *pts − arrival* minus the reference tier's over
  the same span. On one clock this is minus the latency difference and the
  same in every run; on separate clocks it is arbitrary and changes on
  restart. Also the relative drift (slope of the difference of the tiers'
  *pts − arrival*, ppm ± one standard error) per run of 10 s or more and
  pooled over every run, the count of pictures with identical timestamps in
  both tiers, and both tiers' keyframe spreads of *pts − seconds × 10⁶*
  relative to the reference's median: whether they overlap, and whether one
  constant fits both (the seconds are truncated, so each tier allows the
  constant in *[max − 1 s, min]*, widened by 100 ms either way for jitter
  between the seconds and the timestamp).
- **Latency difference**: for pictures of the same moment (nearest
  timestamps, closer than half the shorter frame interval of the two
  tiers), how much later the other tier arrived, corrected by the timestamp
  difference: median, p10, p90, jitter (p90 − p10), standard deviation.
- **Starts**: for each start request, the time to the first record, first
  video record, and first keyframe; whether the first video was a keyframe;
  the first keyframe's *age* (how much older than the run's steady latency
  it arrived); the *burst* (leading pictures that arrived older than the
  steady latency by more than two frame intervals: buffered frames); and
  *live* (the first picture back at the steady latency). Pictures in a
  burst are left out of the clock and latency comparison.
- **Verdict**: `shared clock: yes/no (evidence)`. Identical timestamps on
  at least 90% of the pictures of the same moment (10 at least) are
  decisive: separate clocks do not agree to the microsecond picture after
  picture. Otherwise "yes" needs per-run offsets under 2 s that agree
  within 200 ms, no significant relative drift (the pooled drift counts
  only when it exceeds 200 ppm *and* three standard errors: with a couple
  of hundred milliseconds of arrival jitter, runs of tens of seconds cannot
  measure drift to 200 ppm), and one wall-clock constant fitting both tiers
  when both carry the seconds. On "no",
  the report names the fallback, aligning the tiers by correlating motion in
  their decoded pictures, as future work; it is not implemented.

`analyze` also reads `NVRREC01` recordings (one stream: the per-tier part
only).

## 3. Replay

`mock-nvr --replay FILE.nvrrec [--replay-loop] [--replay-speed N]` serves a
recording of either version. The rules are in the module docs of
`tools/mock-nvr/src/replay.rs`; in short:

- `NVRREC01`: every preview, whatever its tier, plays the one stream from
  its start, on a clock that starts with that preview (unchanged).
- `NVRREC02`: each tier is served from its own entries, and **all previews
  share one replay clock**, which starts with the first preview any client
  starts on the server. A tier started at replay time *T* begins with its
  latest stream-info record, then its most recent keyframe that arrived at
  or before *T* within an unbroken run of that tier (no discontinuity or
  stop marker in between); the pictures from there up to *T* are due
  already and go out at once, as a burst, and later ones keep their
  recorded times. Without such a keyframe (the tier was stopped or not yet
  started at *T* in the recording), it starts at its next keyframe, at that
  keyframe's time. A tier the recording lacks plays the sub-stream (or the
  first recorded tier). Each recorded tier's size, frame rate, and codec
  are advertised in the stream-info reply (146).
- Speed divides every tier's times alike; looping shifts every tier's
  arrival times and video timestamps by the same pass length, so the
  recorded relation between the tiers holds in every pass.

The join rule is a model. Whether a real device starts a preview with a
burst of the current GOP or waits for the next keyframe, and how long the
start takes, is what the capture measures; once known, differences between
the device and this rule are to be noted here. The mock answers a start at
once, without the device's start latency (the recorded start latency is
still visible in a capture's entries, but the replay does not reproduce
it).

## Recording format

Both versions are defined in the module docs of
`crates/bc-protocol/src/recording.rs`. `NVRREC02` entries carry a 16-byte
head: `u64` arrival time (µs, one clock for every tier), a kind (record,
discontinuity, start requested, stop requested), the tier number as used in
preview handles (0 main, 1 sub, 2 mobile, 4 extern), two reserved bytes,
and a `u32` body length; the body of a record entry is one media record.
Readers skip unknown kinds. `soak` and `mock-nvr` accept both versions;
`soak` plays an `NVRREC02` recording's default tier (the sub-stream).
