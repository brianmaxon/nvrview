# Media Source check for the fragmented MP4 output

A check that a real browser plays what `mp4_remux::Fragmenter` makes.
Nothing here uses the network; the page only reads files next to it.
`scripts/browser-check.sh` plays every sample headless in Google Chrome and
Playwright's WebKit and reports pass or fail per sample (optional in
`scripts/check.sh` with `NVRVIEW_BROWSER_TESTS=1`); the steps below are the
manual way. ffmpeg is no substitute: it accepted a stream the browsers
rejected.

1. Generate the samples (synthetic video and audio from `fixtures/synthetic`):

   ```
   cargo run -p mp4-remux --example mse_sample
   ```

   This writes `crates/mp4/tests/mse/out/` (git-ignored): per sample a `.bin`
   with the segments back to back and a `.json` with one record per segment
   (kind, track, MIME type with codecs, byte range, timing, keyframe flag,
   whether an init segment resets the buffer).

2. Serve this directory (the page needs `http://`, not `file://`, to fetch):

   ```
   cd crates/mp4/tests/mse
   python3 -m http.server 8000 --bind 127.0.0.1
   ```

3. Open `http://127.0.0.1:8000/` in Safari or Chrome, pick a sample, press
   Play. The log shows `isTypeSupported` for each MIME type, each init and
   append, and the verdict line says OK when playback reaches the end at about
   the expected time.

Samples (12 s each unless noted):

| sample | what it checks |
| --- | --- |
| `h264-video-audio-muxed` | one SourceBuffer, video and audio in one `moof` |
| `h264-video-audio-separate` | two SourceBuffers, one track per fragment |
| `h264-video-only` | video only |
| `h264-resolution-change` | 320x240 and 480x270 alternating every 2 s (8 s): each change brings an init with `reset: true` (and here a new level in the codec string); the page appends it to the same buffer in stream order, calling `changeType` first because the MIME type changed, and keeps going |
| `h265-resolution-change` | the same for H.265 |
| `h265-video-audio-muxed` | H.265 (`hvc1`); plays only where the browser has an H.265 decoder (Safari; Chrome with hardware support), otherwise the page reports it as not supported |

"Pace like a live stream" releases each media segment when its end time has
passed, and each init together with the media segment after it, as a
WebSocket sender would; unchecked, everything is appended at once. Segments
must reach the buffer in stream order: an init appended ahead of the previous
generation's media segments makes the decoder read those frames with the new
parameter sets, and Chrome and Safari stop with a decode error.

`?sample=NAME` plays a sample on load (`&live=0` appends it at once). When
playback ends or fails the page sets `document.body.dataset.result` to `ok`,
`fail` or `unsupported`, and the video element's `data-sizes` lists the
decoded picture sizes in order.
Segments are half a second long, so the live delay is about half a second plus
the browser's own buffering.
