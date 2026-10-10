# Audio

Listening is a first-class feature: audio that stays in sync with video for
hours, and a clean feed of the same audio for local speech recognition
(keyword spotting, transcription) later. Facts about the stream are in
[protocol.md](protocol.md#audio-payloads); this is the design.

## What the stream gives us

- One ADTS AAC-LC frame per audio record: 16 kHz, mono, 1024 samples (64 ms).
- **No audio timestamps.** Only the record's position between timestamped
  video frames.
- The camera's audio clock is not exactly 16 kHz in video time (16,005 to
  16,013 samples per second were measured). Playing it at a local 16 kHz
  drifts by seconds per hour, which is what the official client appears to
  do: its audio slides out of sync over long sessions.

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

- **Decode** in the core with a pure-Rust AAC-LC decoder (Symphonia's AAC
  decoder, MPL-2.0, used unmodified as a dependency; compatible with our
  MIT OR Apache-2.0 licensing). Checked against FFmpeg's decode of the same
  frames in tests. Platform decoders were the alternative; they would split
  timestamping and the tap per platform.
- **Timestamp**: each frame's sample position (a running sample count) is
  mapped to the camera's video clock by a smoothed linear fit between sample
  counts and the timestamps of the video frames around each audio record. The
  fit's slope is the camera's real audio rate; it is re-estimated continuously
  and reset on a discontinuity (reconnect, gap, decoder error).
- **Play out**: the app tells the core its output format (sample rate,
  channels) and pulls samples from its real-time render callback. The core
  resamples 16 kHz to that rate with a ratio steered by a small controller so
  that each sample comes out when the video frame with the same camera time is
  on screen (plus a fixed lip-sync allowance). Corrections are limited to
  about ±0.5% (inaudible); an error above about 250 ms (after a stall) is fixed
  by a short fade, a jump, and a fade back.
- **Tap**: the decoded 16 kHz mono samples, before resampling, with each
  block's camera time and wall-clock time. 16 kHz mono is what speech models
  (Qwen-ASR, Whisper-class models) take, so no conversion is needed. Taps work
  on any number of tiles, audible or not, through bounded queues that drop the
  oldest blocks with a counter.

Only one tile is audible at a time by default (the focused one); volume and
mute are per tile.

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
  app's real-time audio callback; mixes audible tiles; never blocks.
- `nv_tile_audio_tap(core, tile, enabled)` plus an `on_audio_tap` callback
  delivering borrowed 16 kHz mono blocks with timestamps.
- `audio_stats` events: buffer level, rate estimate, A/V offset, drops,
  resyncs.

## Tests

- Synthetic fixtures: AAC-LC 16 kHz tones and clicks aligned to frame-numbered
  video, served by mock-nvr interleaved like a real device, with an adjustable
  audio clock skew and arrival jitter.
- A simulated multi-hour run at 16,013 Hz against a 48 kHz output clock that
  must keep the A/V offset within ±40 ms the whole time.
- Decoder accuracy against FFmpeg; resync after injected gaps; taps never
  block playback; no allocation or locking in the render path.
