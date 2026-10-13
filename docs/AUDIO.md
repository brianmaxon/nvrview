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
- **Sync tests** (`crates/audio-pipeline/tests/sim.rs`, a simulated
  camera, network, and device on a simulated clock): long-run behavior is
  proven directly in minutes of simulated time, a few seconds each. The
  timestamper and the presentation estimator land on exact, known clock
  ratios; with exactly linear clocks and no jitter the whole loop settles
  and stays stationary (every click within 5 ms, the rate estimate right,
  the correction equal to the true skew, so the error cannot grow), for the
  measured clocks and for the worst case (800 ppm audio, 30 ppm camera to
  host, 50 ppm device); with the measured jitter every click stays within
  ±40 ms and jitter does not swing the correction; after one stall and
  after several in a row the latency equals that of the same run without
  them; the same checks pass with every clock and counter started 3 hours
  and a week in; and the adaptive delay settles on its target and stays.
  The original 3-hour runs are one ignored test, run by hand only when a
  sync problem is suspected:
  `cargo nextest run -p audio-pipeline -E 'test(/^sim::/)' --run-ignored only --no-capture`.
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

## Talk-back (sending)

The other direction: the app's microphone, or system text-to-speech for a
typed reply, through a doorbell's or camera's speaker (UX.md, "Doorbell
calls"). It is camera motion control, off per camera until the app enables
it (FFI.md, "Talk-back"). The app pushes mono float samples at its own
rate; the core resamples them to 16 kHz with the same windowed-sinc
resampler as playout, the device session encodes them as IMA ADPCM
(1024 samples, 64 ms, per record; docs/protocol.md, "Talk-back (motion)")
and sends one record per 64 ms of its own clock, with a bounded buffer
that drops rather than queues when the app pushes faster than real time.
Received talk-back records (`01wb`) are still never played.

**The tail.** The device plays talk audio about a second behind and drops
whatever it has not played when the talk closes (observed; docs/protocol.md,
"Talk-back (motion)"): closing right after the last record cut the last
second or so of every reply. So a talk that ends normally (a draining stop:
a typed reply sent in full, or the user letting go of Talk) stays open
after its last audio and sends silence, encoded and paced like any other
record, for `SessionConfig::talk_tail` (default 1.5 s: 24 records), then
closes. Audio pushed during the tail continues the same talk without a new
start (and without counting against the start limit). Aborts close at
once: withdrawing the permission, closing the tile or call, shutdown, a
device error, the idle and length limits. The `talk` event's `tail_ms`
reports the silence sent; `sent_ms` counts audio only.

**Echo in the captions.** What the device plays comes back through its own
microphone about a second later, after the tail, so the visitor's audio
(and its captions) can contain our own words. The apps filter those captions
(docs/UX.md "Captions and echo"): for 3 s after the talk's `stopped` event,
which already comes after the 1.5 s tail, and while talk-back runs, a visitor
caption that closely matches what we said or typed is dropped. This is a text
filter for the captions only; the audio itself is unchanged.

## Echo cancellation

While the user talks back, the camera's audio may be playing through the
computer's speakers, and the microphone hears it. Sent back, the visitor
hears themselves a second or more later; captioned, the visitor's words show
up again as the user's. Both apps use the operating system's voice
processing (acoustic echo cancellation and noise suppression) on the
microphone while Talk is on, so the user can talk while the visitor is heard
(full duplex). This is app code (Apple and Windows APIs); the core is not
involved, and the microphone's audio reaches the core already processed.

**Rules (shared, `EchoCancellation` in NVRCore and NvrView.Tiles, same test
vectors).** Echo cancellation counts as active only when the platform
confirms all of it: the voice-processing stream was set up, the system
reports its echo canceller present and on, and the canceller's reference is
the output that plays the camera. Anything not confirmed (an older system
that cannot report, a microphone without a canceller, a canceller turned
off, a reference on another output device) is the fallback. Each outcome is
logged once per run ("Talk-back microphone: echo cancellation on", or
"... unavailable (why); captions of the microphone pause while the camera
plays"), and the call's status says "Echo cancellation on" or "No echo
cancellation" while the microphone is on.

**The caption gate (`MicrophoneEchoGate`).** Talk-back itself is never
gated: what the microphone hears always goes to the camera. Only the
microphone's captions are:

- Without echo cancellation (the fallback, and how both apps worked before):
  the half-duplex gate. While the camera is audible and its level peaks over
  0.03, and 0.4 s after, the microphone is not captioned. The visitor still
  hears their own voice back from the computer's speakers, as before.
- With echo cancellation: no half-duplex gate, only a short safety duck when
  the camera is very loud (a peak over 0.5, and 0.15 s after). A canceller is
  weakest exactly then (speakers distort, so the echo is no longer a linear
  copy of the reference) and a residue would be captioned as the user's
  words. Ordinary speech from the camera is captioned on both sides at once.

**macOS.** Voice processing (`AVAudioInputNode.setVoiceProcessingEnabled`)
must see what the speakers play, so the microphone runs on the same
`AVAudioEngine` as playback: while Talk is on, playback moves to an engine
with voice processing enabled on its input (which enables it on the output
too) and the microphone is tapped there (`AudioPlayer.startVoiceProcessing`).
Other apps' audio is ducked as little as the system allows
(`voiceProcessingOtherAudioDuckingConfiguration`, minimum level, no advanced
ducking); automatic gain control is left on. Measured on one Mac (macOS 26;
`VoiceProcessingTapTests`, opt-in with `NVRVIEW_AUDIO_DEVICE_TESTS=1`):

- Enabling voice processing takes about 1 s and pauses the engine that is
  playing for 0.3 to 0.5 s. An engine that had it cannot turn it off again
  (its output then reports 0 channels and does not start), so each mode is
  an engine of its own, and the main mixer must exist before voice
  processing is enabled (otherwise the output unit fails to initialize, error
  -10875).
- Switching from a prepared plain engine to a prepared voice-processing one
  pauses playback for 0.07 to 0.11 s; switching back for 0.85 to 1 s.
- A built engine that was not started does not run the input device (no
  microphone in use).
- The voice-processing input has 9 channels, all carrying the same
  processed signal; the first is converted to 16 kHz.

So the switches happen where they hurt least: the voice-processing engine is
built (not started) when a call opens, if the microphone is already allowed;
Talk only switches to it; after Talk it stays until the call ends with nothing
taken from the microphone (the system shows the microphone in use), so
pressing Talk again is instant; playback goes back to a plain engine when
the call ends (switching back drops about a second of audio, so it is not
done between Talks). The slow work runs on a queue of its
own, never on the main thread, and never two render blocks on the core at
once. A device change while voice processing runs rebuilds it (at most 5
times in 30 s); when that fails the player goes back to plain playback, the
microphone to an engine of its own, and the call to the fallback. If voice
processing cannot start at all, the microphone runs on its own engine as
before.

**Windows** (11 22H2 and later; earlier versions capture as before and get
the fallback). The WASAPI capture stream asks for the communications
category (`IAudioClient2::SetClientProperties`,
`AudioCategory_Communications`), which is what makes Windows apply voice
processing (the microphone driver's, or the one Windows provides on recent
versions). After `Initialize`,
`IAcousticEchoCancellationControl::SetEchoCancellationRenderEndpoint` points
the canceller at the endpoint the app plays the camera on (the `AudioOutput`
device); a canceller without that control listens to the default
communications output, which counts only if it is the same device.
`IAudioEffectsManager::GetAudioEffects` says whether
`AUDIO_EFFECT_TYPE_ACOUSTIC_ECHO_CANCELLATION` is there and on; where it
cannot be asked, echo cancellation counts as unknown, so the fallback. The
render stream is unchanged.

**What is not verified by a test.** The self-tests (step
`call-echo-cancellation`) check the configuration: what the microphone
reported, what the call shows and logs, and which caption rule runs (on
macOS also the captions themselves with a fake doorbell level). Whether the
echo is actually removed needs a person, a doorbell, and speakers: real echo
cannot be produced or judged in a self-test.

## Recording calls

> **Built** in the core (K4, F5) and on both apps (M5, W5: the AAC encoder
> and the call window's Record; CAPTURE.md, "Implementation status (M5)",
> "(W5)").

A recorded call holds the doorbell's video and three audio tracks in one
alternate group, so a player plays one of them: **Conversation** (enabled,
first), **Visitor** and **You** (both disabled, for listening to one side).

- **Visitor** is the camera's audio as received: AAC passed through
  untouched, placed on the video's time line by the same timeline that
  places it for playout (`audio-pipeline`). Audio in other codecs
  (reported only) is decoded and encoded by the app's AAC encoder.
- **You** is exactly what the doorbell got: the 16 kHz PCM the talk sender
  encodes to IMA ADPCM, one 1024-sample record at a time, the silent tail
  included, each stamped with the instant it went out on the core's clock.
  The talk sender hands these to a bounded observer (keyed by device and
  channel) that drops its oldest records rather than delay talk-back. The
  microphone audio in it has already been through the system's echo
  cancellation ("Echo cancellation"); a typed reply is the synthesized
  voice.
- **Placing You.** The recorder keeps its own presentation map for the
  camera, the median of host arrival time minus camera timestamp over the
  recent video, and maps each send instant to camera time with it. So the
  user's words sit in the file where the doorbell received them, within
  ±40 ms (checked on a simulated clock). The doorbell plays them about a
  second later; the file does not model that delay.
- **Conversation** is Visitor and You at unity gain through a soft limiter,
  resampled to 48 kHz with the playout resampler, in 1024-sample frames,
  and silence where nobody talks. The app encodes it, and You (and Visitor
  when it is not AAC), as AAC-LC 48 kHz mono at 96 kbps; the encoder's
  priming becomes an edit list.
- **Echo is left in.** The doorbell's microphone hears its own speaker, so
  the Visitor track (and the mix) can hold the user's words again about a
  second after they were said. The recording keeps what was heard; only the
  captions are filtered ("Echo in the captions", under "Talk-back
  (sending)").
- **Bounds.** At most 10 s of PCM waits for the encoder per track; beyond
  that the track gets silence and `encode_dropped_ms` counts it. A call
  recording stops at 2 hours or 2 GB.

## Speech recognition (later)

The core provides the tap; recognition runs outside it at first, in the app
or a helper process, because model runtimes are platform-specific (MLX or
Core ML on macOS, ONNX Runtime/DirectML on Windows; the Windows app does not deploy Windows ML, the Windows App SDK's ML component, until the first feature that runs a model adds it back). Results come back as
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
- viewer-core, end to end against mock-nvr (AAC clicks, 30 ms jitter), on a
  simulated clock: the core's renderer, driven like a device on the core's
  clock, plays each click within a few ms of its picture's arrival plus the
  reported playout delay, with the same numbers every run; taps, talk-back,
  and argument checks. viewer-ffi: `nv_audio_render` from its own thread,
  in lockstep with the simulated clock, under a counting allocator while
  the app changes format, volume, audibility, and taps: no allocation. The
  C smoke test renders from a pthread in real time.
- Decoder accuracy against FFmpeg; fuzzed records through the decoder and a
  whole tile never panic; taps drop oldest and never disturb playout; the
  render path, under a counting allocator, never allocates or frees, through
  playback, skips, underruns, and slot reuse.
