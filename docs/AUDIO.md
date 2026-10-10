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
- **Playout delay**: 280 ms after the median delivery by default. It must
  cover one record (64 ms: a record is sent only after its last sample) plus
  the late tail of the jitter (177 ms above median measured). Note that with
  pictures shown on arrival the video itself jitters by the same ±67 ms, and
  audio lags the typical picture by the playout delay, more than lip-sync
  tolerances (tens of ms). Delaying video by a similar jitter buffer and
  passing that as the presentation map would let the playout delay shrink to
  the record length plus a margin; that is a video-path decision.
- **Tap**: the decoded samples before playout resampling, as 16 kHz mono
  (downmixed, and resampled when the source is not 16 kHz, as for 8 kHz
  ADPCM), with each block's camera time, the arrival wall-clock time of the
  record that completed it, its epoch, and a discontinuity flag. 16 kHz mono
  is what speech models (Qwen-ASR, Whisper-class models) take. Taps work on
  any number of tiles, audible or not, through bounded lock-free queues that
  drop the oldest blocks with a counter; a slow consumer never touches
  playout.

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

## C interface (planned, ABI 2)

Sketch, to be finalized when implemented:

- `nv_audio_set_output(core, sample_rate, channels)`
- `nv_tile_audio_enable(core, tile, enabled, volume)`
- `nv_audio_render(core, float *out, frames, host_time_ns)`: called from the
  app's real-time audio callback; mixes audible tiles; never blocks, locks,
  or allocates. `host_time_ns` is when `out[0]` will be heard, on the same
  clock as the core's arrival times (the core must define that clock for the
  app, or convert).
- `nv_tile_audio_tap(core, tile, enabled)` plus an `on_audio_tap` callback
  delivering borrowed 16 kHz mono blocks with timestamps.
- `audio_stats` events: buffer level, rate estimate, A/V offset, ratio
  deviation, resyncs, underruns, overflow, records decoded, concealed, and
  rejected, tap drops.

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
  shows no trend. Also: resync after a 3 s audio gap and a 6 s stall, and a
  check that the ratio correction stays small under jitter.
- Decoder accuracy against FFmpeg; fuzzed records through the decoder and a
  whole tile never panic; taps drop oldest and never disturb playout; the
  render path, under a counting allocator, never allocates or frees, through
  playback, skips, underruns, and slot reuse.
