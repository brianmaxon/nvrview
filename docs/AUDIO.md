# Audio

Listening is a first-class feature: audio that stays in sync with video for
hours, and a clean feed of the same audio for local speech recognition
(keyword spotting, transcription) later. Facts about the stream are in
[protocol.md](protocol.md#audio-payloads); this is the design.

## What the stream gives us

- One ADTS AAC-LC frame per audio record: 16 kHz, mono, 1024 samples (64 ms).
- **No audio timestamps.** Only the record's position between timestamped
  video frames.
- The camera's audio clock is close to, but not exactly, 16 kHz in video
  time (about 20 ppm off over a 10-minute measurement: 0.07 s per hour), and
  the output device's clock differs from both by its own small error.
- Arrival jitter is large by comparison: about 67 ms standard deviation,
  peaks near ±150 ms on a wired LAN. The playout buffer must absorb that.
- The official client plays audio by a local clock with no drift correction
  (reported), and its audio slides far out of sync over long sessions. Clock
  rates alone explain only a fraction of a second per hour, so the likely
  cause is latency that accumulates in its buffer after network stalls and is
  never removed (a hypothesis). The design below removes both: it steers the
  buffer back to its target continuously and resyncs after stalls.

## Principles

1. **All decisions in shared Rust**, as for video: decoding, timestamping,
   drift correction, and taps are the same on every platform. Apps only play
   samples the core hands them.
2. **Never drift.** Correct continuously with inaudible rate adjustments; resync
   hard only after a real interruption.
3. **Never block the session.** Audio paths are bounded and drop with
   counters, like video.
4. **The real-time audio thread never waits.** The app's audio render callback
   reads from a lock-free buffer; it never takes locks or allocates.

## Pipeline (per tile, in viewer-core)

```
records ─▶ ADTS parse ─▶ AAC-LC decode ─▶ timestamp ─▶ ┬─▶ drift-corrected resampler ─▶ ring ─▶ app render callback
 (interleaved with video)        (16 kHz f32)          └─▶ tap (16 kHz mono f32, timestamped) ─▶ consumers (ASR, recording)
```

The pipeline is the sans-I/O crate `crates/audio-pipeline` (no threads, no
I/O, no platform APIs; the caller supplies every time). viewer-core runs one
`TileAudio` per tile on the tile's thread and hands the app's audio callback
the crate's `Renderer`.

- **Decode** in the core with a pure-Rust AAC-LC decoder (Symphonia's AAC
  decoder, MPL-2.0, used unmodified as a dependency; compatible with our
  MIT OR Apache-2.0 licensing). It matches FFmpeg's decode of the synthetic
  fixtures to 84–89 dB SNR (16-bit rounding of the reference), at 16 kHz mono
  and 48 kHz stereo. The codec is chosen per record by the magic's second
  character (`stream_index % 10`): `5` is AAC with rate and channels from each
  ADTS header; anything else takes the reported IMA ADPCM / µ-law path (8 kHz
  mono, unconfirmed on hardware). Payloads over 2048 bytes are dropped. A
  record with a valid ADTS header that fails to decode, or a corrupt record in
  an AAC stream, becomes 1024 samples of silence instead of a reset: the
  sample count is the stream's only audio clock, so it must not lose frames.
  Platform decoders were the alternative; they would split timestamping and
  the tap per platform.
- **Timestamp**: a record is assumed to be sent once its last sample is
  captured, after every picture captured before then. So with `x` its last
  sample's number, `P` the timestamp of the picture before it and `U` of the
  next picture, `P ≤ T(x) < U` where `T(x) = t0 + x / rate` in camera video
  time. The slope (the camera's real audio rate) is an exponentially
  weighted least-squares fit of `P` against `x` (60 s window, a prior at the
  nominal rate for the first seconds); the offset is the midpoint of the
  largest `P − line` and the smallest `U − line`, both slowly leaking
  envelopes. In simulation this recovers 16,013 Hz within 0.3 Hz and places
  samples within a few milliseconds at 4–15 fps. Records more than 0.5 s
  off the line are ignored; four in a row (a gap in the audio, a jump in the
  video timestamps) start a new epoch, as does a discontinuity or a format
  change.
- **Presentation**: the target for each sample is the moment the picture
  with the same camera time is shown. The caller may supply that mapping;
  otherwise the tile estimates it, assuming pictures are shown on arrival:
  the *median* of `arrival − send time` over the last 120 s, with each audio
  record's send time from the timeline above, and the host-to-camera clock
  ratio from a long (30 min) fit kept within ±200 ppm of 1. The median is
  stable under the measured jitter, and stalls and the bursts after them only
  add delay to a few deliveries, so the reference cannot ratchet upward.
- **Play out**: the app tells the core its output format (sample rate,
  channels) and pulls samples from its real-time render callback with the
  host time at which they will be heard. Each tile has a lock-free
  single-producer ring of output-rate frames; the renderer publishes which
  frame plays at which host time, so the producer knows when the next frame
  it writes will be heard and compares that with its target
  (presentation + **playout delay**). A windowed-sinc resampler runs at the
  feed-forward ratio (estimated audio rate, through the clock ratio, over the
  output rate) plus a slow PI correction of the filtered error (60 s
  proportional time constant, 5 s error filter; four times faster for the
  first minute while the estimates settle). The ratio never leaves ±0.5% of
  nominal. Because the ring is written contiguously, arrival jitter does not
  reach the error at all; only real offsets do. An error over 250 ms
  (60 ms in the first minute), or audio that is already late when written
  (after a stall longer than the buffer), is fixed by a short fade, a jump
  (drop queued audio, then write silence or skip input so the next sample is
  on time), and a fade back.
- **Underruns keep the schedule.** When a record arrives too late, the
  renderer plays silence and then *drops* the late frames as they arrive, so
  every later frame still plays at its time. Latency never grows by the
  length of an underrun; this is the opposite of a player that resumes where
  it stopped, whose latency ratchets up with every stall.
- **Playout delay**: how long after its picture each sample is heard. Video
  is never delayed (live view, above all full screen, shows the newest
  picture), so audio trails the picture by this delay, and it adapts to
  each stream to be as short as its network allows (`crates/audio-pipeline`
  `delay.rs`). It must cover a record's length (64 ms: a record is sent only
  after its last sample), its lateness against the presentation reference,
  and the output's lead (the renderer reads about 20 ms ahead of the time
  it is heard). The target is the 99th percentile, over the last 600 s of
  arrivals, of the *per-second maximum* lateness, plus those two, kept
  within 100–500 ms: every record arrives in time in all but about one
  second in a hundred, a stall (a second or two of late records) every
  few minutes does not raise it, and a rough patch that recurs every ten
  minutes or so is remembered between occurrences. (A plain 99th
  percentile of records underran every few seconds in the simulation: late
  records come singly, spread over many seconds. A 300 s window forgot a
  rough patch in a replayed LAN recording between its occurrences.)
  Lateness is kept across stream restarts (the network has not changed).
  The floor is a record plus the lead plus slack; the ceiling is where
  audio stops feeling like the picture's sound and a network needing more
  is stalling, which resyncs handle. The delay starts at 250 ms; after
  every (re)start it steps up by a 50 ms warm-up allowance that fades over
  two minutes, because the presentation reference is still a young median
  then and its early errors arrive as lateness (without it, most underruns
  in the replayed recording fell in the first two minutes after each
  restart). It moves toward the target at most 2 ms per second
  (approaching it with a 10 s time constant), and playout feeds that rate
  forward into its resampling ratio, so a change is a 0.2% tempo change
  (3.5 cents), never a jump. In simulation: 100 ms on a clean network
  (0–10 ms jitter), about 280 ms under the measured LAN jitter, about
  400 ms under jitter half again as large; every click stays within
  ±40 ms of its moving target. A caller can still fix the delay
  (`TileConfig::playout_delay_us`).
- **Start**: playout waits until the estimated presentation map has
  settled and has a second of deliveries for its median (the first 3 s of
  a stream; an explicit map is used at once).
  Placing audio on the rough early map and correcting it a second later
  cost two or three audible jumps after every (re)start.
- **Soak** (`tools/soak`, a real 10-minute LAN recording looped for 24 h of
  simulated time, each loop a restart): with the fixed 280 ms delay, the
  filtered A/V error after each start's first minute ranged −15.8 to
  +35.1 ms (mean +0.4), up to 47.9 ms in the first minute, with 436 resyncs
  (all in the first seconds after a restart) and 144 underruns (one per
  loop seam). With the adaptive delay, the warm-up allowance, and playout
  waiting for a settled map: −15.1 to +34.9 ms (mean +1.1), up to 27.2 ms
  in the first minute, no resyncs, the same 144 seam underruns, and a
  delay of 256 ms. The rate estimate at the end of every pass is
  16,000.12 Hz; a run that ends seconds after a restart reports the young
  estimate of the new epoch (16,009 Hz), which is not a bias.
- **Tap**: the decoded samples before playout resampling, as 16 kHz mono
  (downmixed, and resampled when the source is not 16 kHz, as for 8 kHz
  ADPCM), with each block's camera time, the arrival wall-clock time of the
  record that completed it, its epoch, and a discontinuity flag. 16 kHz mono
  is what speech models (Qwen-ASR, Whisper-class models) take. Taps work on
  any number of tiles, audible or not, through bounded lock-free queues that
  drop the oldest blocks with a counter; a slow consumer never touches
  playout. The same 16 kHz mono samples feed the core's sound analysis
  (alarm patterns and loudness, `viewer_core::sound`) on every tile with
  audio, tapped or not (`TileAudio::set_mono_output`, `mono_block`).

Only one tile is audible at a time by default (the focused one); volume and
mute are per tile (mute keeps the tile's timing; making a tile silent
releases its output slot). Measured on Apple silicon: one tile's decode,
timestamping, and resampling to 48 kHz stereo take about 0.1% of one core,
and its share of rendering about 0.01%.

## Speech recognition (later)

The core provides the tap; recognition runs outside it at first, in the app
or a helper process, because model runtimes are platform-specific (MLX or
Core ML on macOS, ONNX Runtime/DirectML on Windows). Results come back as
timestamped text that the UI can align with video. A Rust-side engine can
replace this later without changing the tap. Nothing leaves the machine.

## C interface (ABI 2)

Specified in [FFI.md](FFI.md#audio) and [nvrview.h](nvrview.h):

- `nv_audio_set_output(core, sample_rate, channels)`
- `nv_audio_render(core, out, frames, channels, host_time_ns)`: called from
  the app's real-time callback; mixes audible tiles; never blocks, locks,
  allocates, or frees. It takes the channel count the buffer was sized for
  (unlike the first sketch), so a callback still running in the old format
  after `nv_audio_set_output` gets silence instead of an overrun.
- `host_time_ns` is on the core's clock, which is the platform host clock
  (`nv_clock_now_ns`; on macOS `mach_absolute_time` in nanoseconds, the
  unit of `AudioTimeStamp.mHostTime`), so arrival times and render times
  need no conversion between clocks.
- The renderer does not take each `host_time_ns` literally: it keeps a
  smoothed render clock, a second-order loop that follows the host times
  with the frames rendered as its clock (it locks within a few dozen calls,
  then takes 5% of each call's error). Host times that jitter by
  milliseconds per call average out, and an app whose calls come further
  apart than the frames they render (a timer loop passing `now + 10 ms`
  and sleeping 10 ms: measured 12.2 ms per call on macOS; a Windows timer
  tick is 15.6 ms) gets a frame period to match (within 0.5× to 2× of
  nominal), so neither causes resyncs. A host time more than 50 ms from
  the estimate is a real jump (rendering stopped): the clock starts over
  there and the usual resync rules apply. Before this, such a timer loop
  resynced every few seconds and its tile's level dropped to 0 for
  seconds at a time.
- `nv_tile_audio_set(core, tile, audible, volume)` (audible and volume in
  one call; volume 0 is the mute that keeps the slot).
- `nv_tile_audio_tap(core, tile, enabled)` plus the `on_audio_tap` callback
  delivering borrowed 16 kHz mono `NvAudioBlock`s with camera time, arrival
  wall-clock time, epoch, and a discontinuity flag.
- `audio_stats` events: buffer level, rate estimate, A/V error, playout
  delay and its target, ratio deviation, resyncs, underruns, overflow,
  records decoded, concealed, rejected, and talk-back records ignored, tap
  blocks and drops.

In viewer-core each tile thread owns its `TileAudio`; the core owns the
`AudioOutput`, and the `Renderer` changes hands between the control thread
(which replaces it on a format change) and the real-time thread through two
lock-free single-slot queues, so a replaced renderer is always freed off the
real-time thread. Records with stream index 1 (`01wb`, the client's
talk-back form, reported) are counted and never played.

## Tests

In `crates/audio-pipeline` unless noted.

- Synthetic fixtures (`fixtures/synthetic/make-audio.sh`): AAC-LC tones,
  clicks, and 48 kHz stereo, with FFmpeg's decodes as references.
- mock-nvr interleaves audio records with video like a device, with an
  adjustable audio clock skew and arrival jitter; its tests check the
  placement over TCP, and its schedule drives an end-to-end test here (real
  AAC clicks at 16,013 Hz heard within about 1 ms of their target).
- Simulated three-hour runs against a 48 kHz output clock with jitter like
  the measurement (65 ms SD, heavy-tailed): the measured skews, the 16,013 Hz
  worst case, and a 1.5 s stall every 5 minutes. Every click stays within
  ±40 ms of its target after the first minute, and the 10-minute mean offset
  shows no trend. Also: resync after a 3 s audio gap and a 6 s stall, a
  check that the ratio correction stays small under jitter, and no resync
  or silence with host times jittering ±5 ms per call or from 10, 12.2,
  and 15.6 ms timer loops rendering 10 ms per call.
- viewer-core, end to end against mock-nvr (AAC clicks, 30 ms jitter): the
  core's renderer, driven like a device on the host clock, plays each click
  within a few ms of its picture's arrival plus the reported playout delay;
  taps, talk-back, and argument checks. viewer-ffi: `nv_audio_render` from
  its own thread under a counting allocator while the app changes format,
  volume, audibility, and taps: no allocation. The C smoke test renders
  from a pthread.
- Decoder accuracy against FFmpeg; fuzzed records through the decoder and a
  whole tile never panic; taps drop oldest and never disturb playout; the
  render path, under a counting allocator, never allocates or frees, through
  playback, skips, underruns, and slot reuse.
