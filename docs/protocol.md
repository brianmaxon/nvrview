# Baichuan protocol notes

Facts about the Reolink "Baichuan" protocol that nvrview depends on, each with
its provenance (see [CLEAN-ROOM.md](CLEAN-ROOM.md)):

- **observed**: confirmed on real hardware (device class and firmware family
  only)
- **reported**: from another project's notes or code, not yet confirmed
- **reported (client)**: reported (static analysis of the official desktop
  client), read from the client's shipped protocol library and scripts
  (macOS build, version 8.21.12) without running it; describes what the
  client does, not yet confirmed on hardware

Unless noted, observations come from a 16-channel NVR (firmware build family
2025) talking to the official desktop client on a LAN, and were verified by
decoding the resulting video with FFmpeg. Observations of camera motion
control, talk-back, playback, and the client's own download come from a later
capture of the official desktop client and an NVR (firmware build family
2026) while a user panned and tilted a PTZ camera, recalled presets, moved the
zoom and focus of a camera with a motorized lens, used talk-back, and played,
scrubbed and downloaded a recording; they are marked "observed (client
capture)" where the distinction matters.

## Transport

| Fact | Provenance |
| --- | --- |
| TCP, port 9000 by default. One TCP connection is one session. | observed |
| The official client opens **one session per NVR** and carries every channel it views over it. | observed |

## Message header

All integers little-endian.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | magic `f0 de bc 0a` |
| 4 | 4 | command number |
| 8 | 4 | body length (bytes after the header) |
| 12 | 4 | message ID, chosen by the client; replies echo it |
| 16 | 2 | status (replies: 200 OK, 300, 400, 401 …) |
| 18 | 2 | message class, **big-endian** unlike every other field: class `0x1465` is the bytes `14 65` (observed) |
| 20 | 4 | payload offset (only for classes `0x1464` and `0x0000`) |

| Class | Header size | Used for | Provenance |
| --- | --- | --- | --- |
| `0x1465` | 20 | client's first (nonce request) message | observed |
| `0x1466` | 20 | device's nonce reply | observed |
| `0x1464` | 24 | client messages after the handshake | observed |
| `0x0000` | 24 | device replies after the handshake | observed |

**Payload offset.** When non-zero, the body is an XML *extension* of that many
bytes followed by the payload. When zero, the whole body is the payload.
(observed)

**Replay media headers.** The media messages of a recording playback
(command 5, see "Playback") have a 24-byte header whose bytes 16–19 are not a
status and class: they hold a u32 little-endian `seq` number that the client
chose in the preceding command 123. Bytes 20–23 are the payload offset as
usual, and the message ID is the play request's. A parser must therefore not
decide the header length from bytes 18–19 alone: only classes `0x1465` and
`0x1466` have a 20-byte header. (observed (client capture))

Implementation rule (ours, derived from the above): a command-5 header is
always 24 bytes, whatever bytes 18–19 hold. A `seq` close to a Unix time has
a high half that can spell a known class (for example the bytes `14 64`
when `seq` is between `0x64140000` and `0x6414ffff`, an 18-hour span of
March 2023) or none at all, so reading it as a class would misframe the stream.
The decoder returns the u32 in bytes 16–19 as the replay `seq` for every
command-5 message; only the device's media messages mean it as `seq` (the
client's request and the device's first reply have a status and class
there, and the caller knows which message it holds).

## Login

1. Client sends command 1, empty body, class `0x1465`, message ID 0, status
   `0xdc12` (bytes `12 dc`). (observed)
2. Device replies (class `0x1466`) with
   `<Encryption><type>md5</type><nonce>…</nonce></Encryption>`, obfuscated
   with the fixed XOR scheme below. (observed)
3. Client sends command 1, class `0x1464` (24-byte header, payload offset 0),
   message ID 0, status 0, with a `<LoginUser version="1.1">` body (salted
   hashes of user name and password, `<userVer>1</userVer>`) and
   `<LoginNet version="1.1"><type>LAN</type><udpPort>0</udpPort></LoginNet>`,
   XOR-obfuscated with message ID 0. (observed)
4. Every later client message has status 0; message IDs start at 1 after login
   and increase. (observed)
5. Device replies status 200 on success. On failure, status 401, sometimes with
   `<LoginErrInfo><remainTimes>N</remainTimes>`: devices count failed
   attempts and lock out after too many. (observed)

**Hash.** `md5_hex_upper(s)[:31]`, the uppercase hex MD5 of `s`, truncated to
31 characters. User name hash: `md5(user + nonce)`; password hash:
`md5(password + nonce)`. (observed)

**XOR obfuscation** (handshake and login only): byte `i` of a body sent with
message ID `m` is XORed with `key[(m + i) mod 8] XOR (m mod 256)`, where `key`
is `1F 2D 3C 4B 5A 69 78 FF`. (observed)

### Login failures and limits

How the official client reads a failed login reply's header status. All
reported (client).

| Status | Meaning in the client |
| --- | --- |
| 401 | wrong user name or password |
| 406 | login refused |
| 407 | device (account) locked |
| 408 | device not initialized |
| 409 | too many users online (session limit reached) |
| 410 | too many client addresses connected |
| 411–418 | certificate, token, and password-reset conditions of cloud or token logins |
| 419 | device occupied |
| 501 | device not authorized |

| Fact | Provenance |
| --- | --- |
| A failed login may carry `<LoginErrInfo>` with `remainTimes` (attempts left before lockout), `unlockTime` (seconds until a lockout ends; the client shows it rounded up to minutes), and `useNickname`. | reported (client) |
| The client reports `unlockTime` > 0 as "locked, try again in N minutes", else `remainTimes` > 0 as a warning that attempts are running out, else a plain password error. | reported (client) |
| After a 401 or 407 with a non-zero `remainTimes` or `unlockTime`, the client stops reconnecting to that device automatically, so it does not spend the remaining attempts. | reported (client) |
| A live-view start (command 3) answered with status 430 means the stream is busy (the client marks the tile "stream busy"); 431 is a recording-encryption key mismatch. Any other non-200 status fails the start. | reported (client) |

## Encryption after login

| Fact | Provenance |
| --- | --- |
| Session key: the first 16 characters of `md5_hex_upper(nonce + "-" + password)`, as ASCII bytes. | observed |
| AES-128-CFB (128-bit feedback), IV `0123456789abcdef` (ASCII), restarted for every encrypted unit. | observed |
| Each extension and each XML body is encrypted as its own unit. | observed |
| Media payloads: if the extension contains `<encryptLen>N</encryptLen>`, the first N bytes of the payload are encrypted (as one unit) and the rest is plain. Without `encryptLen`, the payload is plain. | observed |
| Live view uses `encryptLen` 1024 on frame-start messages; continuation messages carry no extension and are plain. | observed |
| Recorded-clip download (143) by our own small client came with `encryptLen` 0. When the official client downloaded (143) or played (5) a recording, `encryptLen` was set on every message that starts a record: 1024 for keyframes, 32 for predicted frames (the record header), and the whole record (about 230–310 bytes) for audio. Continuation messages carried an extension with `binaryData` only and were plain. Why the two clients got different treatment is unknown; always decrypt exactly `encryptLen` bytes when it is present. | observed |
| The extension of cover-image replies (298) spells it `EncryptLen`, with a capital E; match the element name case-insensitively. | observed (client capture) |

## Live view

**Start: command 3**, one per channel, each with its own message ID:

```xml
<Preview version="1.1">
  <channelId>0</channelId>
  <handle>256</handle>
  <streamType>subStream</streamType>
  <policy>newestIFrame</policy>
</Preview>
```

**Stop: command 4**, same channel and handle, a new message ID:

```xml
<Preview version="1.1">
  <channelId>0</channelId>
  <handle>256</handle>
</Preview>
```

| Fact | Provenance |
| --- | --- |
| The device answers a start with status 200 and then streams media as command-3 messages carrying **the start request's message ID**; that ID is how channels are demultiplexed on one session. | observed |
| There is no separate empty reply to a start: the first command-3 message from the device already carries status 200, the `binaryData` extension, and media. | observed |
| A stop is answered by an empty command-4 reply with status 200. | observed |
| `handle` = `(stream tier << 8) | channelId`, with tiers `mainStream` 0, `subStream` 1, `mobileStream` 2, `externStream` 4. | observed |
| `mainStream`/`subStream` are the usual high/low tiers. Multi-lens cameras expose a second lens as `externStream` (high) and `mobileStream` (low). | observed |
| With `policy` `newestIFrame`, the first video record of every stream was a keyframe. | observed |
| Switching a tile to full screen: the client starts the high tier for that channel, then stops the low-tier streams it no longer shows. | observed |
| Audio is interleaved in every live stream; unmuting in the client sends nothing to the device. | observed |
| Observed concurrency: 6 sub-streams on one session, and 2 high-tier H.265 4K streams on one session. | observed |
| Live pictures arrive unevenly: in a two-minute recording of a 20 fps sub-stream, the arrival intervals had a median of 45 ms but a 95th percentile of about 118 ms and gaps up to about 300 ms, the pictures of a gap then arriving together, while the timestamps were evenly 50 ms apart. Decoded on arrival, such pictures land within one display refresh: a tile showed about half the device's frame rate. | observed |

## Media records (BCMEDIA)

The payloads of one stream, concatenated in order, form a sequence of records
aligned to 8 bytes. Records may span messages. Each starts with a 4-byte
little-endian magic that reads as ASCII:

| Magic (ASCII) | Record | Layout | Provenance |
| --- | --- | --- | --- |
| `1001`, `1002` | stream info | `+4` u32 record size, counted from the record start; `+8` u32 width; `+12` u32 height; `+17` u8 frame rate. Width and height can be 0 (seen on second-lens streams); take dimensions from the SPS instead. | observed |
| `00dc`, `10dc` | keyframe | 24-byte header: `+4` codec (`H264` or `H265`), `+8` u32 payload size, `+12` u32 extra-header size (always a multiple of 8, 0–184 seen), `+16` u64 timestamp (µs); payload (Annex-B NAL units) follows the extra header; padded to 8. | observed |
| `01dc`, `11dc` | predicted frame | as keyframe | observed |
| `..wb` (`05wb`, `15wb`, …) | audio | `+4` u16 payload size, repeated at `+6`; payload follows the 8-byte header; padded to 8; no timestamp. | observed |

A video magic reads `<stream index><frame type>dc`: the second character is
`0` for a keyframe (its payload starts with the parameter sets) and `1` for a
predicted frame; the first character is a stream index (0 and 1 seen). This
was checked by counting every video record in a live capture against the
first NAL unit type of its payload. Audio records also start with a digit
(`05wb`, `15wb`). Any `..wb` record is audio and any `..dc` record is video.
(observed)

Observed rates are lower than the advertised frame rate (about 4–15 fps on
sub-streams, 12–15 fps on high tiers); use the timestamps, not the info record.
(observed)

#### Sub and main of one camera

Measured with `bc-cli capture` and `bc-cli analyze` (docs/STREAM-TIMING.md):
one camera behind an NVR, sub and main on one session for 120 s, main stopped
and started again every 30 s (4 starts).

| Fact | Provenance |
| --- | --- |
| Sub and main share one timestamp clock. The sub-stream carried every third main picture: all 464 sub pictures had a main picture with the identical µs timestamp. | observed |
| Rates and GOPs: sub 4.17 fps with a keyframe every 40 pictures (9.6 s), main 12.5 fps with a keyframe every 24 pictures (1.92 s), against 10 and 30 fps advertised in the stream info. | observed |
| For pictures of the same moment, main arrived a median 36 ms *earlier* than sub (p10 157 ms earlier, p90 68 ms later). | observed |
| Starting main while sub plays: the first record came 24–65 ms after the request and the first picture, always a keyframe, 125–217 ms after it (median 215 ms), with no burst of buffered older pictures: the device starts a preview at a fresh keyframe. Sub's single start took 346 ms. | observed |
| Every video picture of both tiers had an extra header of at least 8 bytes, but only keyframes carried the wall-clock seconds in bytes 0–3; every predicted frame of both tiers carried the same other value there (`0x09000006`), not seconds. The µs timestamps are not Unix time (they differ from seconds × 10⁶ by a large constant). | observed |
| Over the keyframes, *pts − seconds × 10⁶* spread over 0.83 s (sub, 13 keyframes) and 0.99 s (main, 60 keyframes): one constant plus the truncated fraction of a second, so the µs timestamps follow the wall-clock seconds to within their 1 s resolution. One constant fits both tiers' keyframes; main's pin it to a window of about 15 ms. | observed |

#### Second lenses

Measured on an NVR on newer firmware with `bc-cli channels`, `bc-cli query
199`, and `bc-cli capture`/`analyze` of all four tiers (sub, main, mobile,
extern) of two multi-lens cameras: camera A with two fixed lenses, and
camera B with a wide lens and a pan-tilt-zoom telephoto lens (120 s and 30 s).

| Fact | Provenance |
| --- | --- |
| `streamSupport` (145) does not tell lenses apart. Most single-lens cameras on both NVRs listed `mainStream,subStream,externStream` (for them `externStream` is most likely a third quality tier of the same lens), while both multi-lens cameras listed only `mainStream,subStream`. | observed |
| Both multi-lens cameras nevertheless served `externStream` and `mobileStream` with full video when asked: the second lens. The official client opened camera B's second lens with `handle` 1028 (`externStream` on its channel). | observed; observed (client capture) |
| Cameras without a second lens that do not list `externStream` (a dual-sensor camera that stitches one panoramic picture, and a single-lens camera) answered an `externStream` or `mobileStream` preview with one stream-info record (size 0×0) and no video within 12 s. | observed |
| `Support` (199) gave camera A one `subItem` per lens (lens 1 with `ptzType` 3), but camera B, on older camera firmware behind the same NVR, no `subItem` at all; its item had `ptzType` 3 and a `channelType` other than `ipc`. So 199 identifies some multi-lens cameras, not all. | observed |
| Each tier pair of one lens shares one clock (sub/main: 2213 of 2213 and 557 of 557 matched pictures with identical timestamps). The second lens has timestamps of its own: none identical to the first lens's; camera A's second lens sat about 100 ms from the first lens with no measurable drift over 2 minutes (−20 to −28 ± 25 ppm) and ran at 20.17 fps against 20.02, camera B's within 4–16 ms. | observed |
| Stream info gave the second lens's size as 0×0 (dimensions come from the SPS). Starts: camera A's `mobileStream` took 2.05 s to its first picture (one sample), its `externStream` 160 ms; camera B's four tiers each 136–213 ms. Camera A's main sometimes began with a burst of 3–10 buffered pictures (camera B and the earlier single-lens camera: none). | observed |
| A second lens's `externStream` once took 1.18 s to its first picture. | observed |

How nvrview detects a second lens (ours, from the facts above): two or more
`subItem`s in 199; otherwise, for a channel that does not list
`externStream`, one short `externStream` preview: video within about 3 s
(configurable; the slowest `externStream` start seen was 1.18 s) means a
second lens. A stream-info record alone does not decide (the second lens's
own info record says 0×0 too); only the first picture does.
A refusal, a 400 included (the probe does not wait for a third-party
stream), or no picture within the window means a single lens. The
preview is stopped on every path, probes run one at a time per device on
its one session, and the answer is kept per device key and channel until
the channel's camera changes (`uid` in 145, else name and model from 318).

### Audio payloads

All observed in a live capture of several cameras on one NVR (about 6,500
audio records across both stream indexes, sub and high tiers).

| Fact | Provenance |
| --- | --- |
| Every audio record's payload is exactly one ADTS frame (sync `0xFFF`, MPEG-4, no CRC) whose frame length equals the record's payload size. | observed |
| The ADTS headers say AAC-LC, 16 kHz, mono, one raw data block per frame: 1024 samples, 64 ms per record. | observed |
| Frame sizes vary (variable bit rate); about 250 to 520 bytes were common. | observed |
| Audio records carry no timestamp. They are interleaved with video records (about 15.6 audio records per second against 4–15 video frames), so a record's position between video frames is the only timing information in the stream. | observed |
| Over 10 minutes of one camera's sub-stream (9,371 audio and 2,644 video records, no gaps), the audio ran at 16,000.33 samples per second of the camera's video time (about 20 ppm) and 16,000.06 per second of the receiving computer's clock; the camera's video clock and the computer's differed by about 8 ppm. A player at exactly 16 kHz would drift only about 0.07 s per hour from clock rates alone. (Earlier estimates of 16,005 to 16,013 from 64 s spans were dominated by measurement noise.) | observed |
| Audio records arrive with jitter: arrival times deviated from a steady rate by 67 ms (standard deviation), from 131 ms early to 177 ms late, over those 10 minutes on a wired LAN. | observed |

### Records as the official client reads them

All reported (client).

| Fact | Provenance |
| --- | --- |
| The client never reads the first character of a `..dc` or `..wb` magic; when it resynchronizes after lost data it accepts any character from `0` to `@` there. Which stream a record belongs to comes only from the command-3 message ID, never from the magic. What the first character denotes remains unknown. | reported (client) |
| A third-party (ONVIF) camera behind an NVR on newer firmware sent its video records as `:0dc` (keyframe) and `:1dc` (predicted frame): first character `:` (0x3A), not a digit. With the first character taken from `0` to `@`, its sub and main streams parsed completely (8 keyframes and 442 predicted frames on main; H.264 High, progressive); a parser that required a digit lost sync after the first record. A native camera's stream in the same capture used `00dc`, `01dc`, and `05wb`. What the character means is still unknown (it is not the channel number). Parsers must accept `0` to `@` there. | observed (client capture) |
| For that camera, the NVR answered the official client's preview (3) with status 400 and an empty body, repeatedly; the client sent the same preview again about every 6 s (each time a stop (4) for the previous start, then a new start (3) with a new message ID), and after one of those 400s the NVR started sending media on the same message ID (0.8 s to 3.9 s later, three times in 90 s). So a 400 to a preview of a third-party camera is not final: the stream may follow on that message ID, and the client keeps retrying. Our own client's previews of that channel got the same 400. Media on that channel used `encryptLen` 1024 like native cameras. | observed (client capture); observed |
| Video second character: `0` keyframe, `1` predicted frame, as observed. A third value, `e`, is also accepted as a video record of a separate kind whose use is unknown. | reported (client) |
| When resynchronizing, the client also requires the codec field to read `H264` or `H265`; it takes the codec from each record, not from any query reply. | reported (client) |
| Video extra header, when at least 8 bytes: bytes 0–3 are a u32 little-endian count of seconds (apparently the device's wall clock). At the first keyframe after a stream (re)starts, the client takes that value × 1,000,000 as the frame's absolute time in µs and dates later frames by adding their timestamp difference from that keyframe. Bytes 4–5 can encode a resolution code; byte 6 holds flags. Bytes after the first 8 carry tag-length-value data that the client parses as detection metadata. | reported (client) |
| A record's size in the stream is the 8-byte-aligned payload size plus the extra-header size, after the 24-byte header. | reported (client) |

**How nvrview handles these.** The demuxer accepts any first character
from `0` to `@` in video and audio magics (resynchronization too) and gives
it no meaning. A preview answered with 400 is kept open, so media that
follows on its message ID is delivered; until media arrives, the session
stops it and sends the same start again (with a new message ID, as the
official client does) 6 s after each 400, and gives up
(reporting the refusal) 60 s after the first one. Both times are settings
(`SessionConfig::preview_retry_interval`, `preview_wait_limit`). After
that the refusal stays visible and one start is tried every
`preview_slow_retry` (60 s) until media arrives (docs/ARCHITECTURE.md,
"Previews that deliver nothing"). Other refusal statuses stay final at
once. The wait is reported to apps as
`tile_waiting` (docs/FFI.md).

### Audio in the official client

Adds to "Audio payloads" above. All reported (client).

| Fact | Provenance |
| --- | --- |
| The audio codec is chosen per record by the magic's second character: `5` (as in `05wb`, `15wb`) means AAC in ADTS framing; any other character selects the client's ADPCM path. | reported (client) |
| The ADPCM path: payloads under 480 bytes are IMA ADPCM blocks (a 4-byte prefix, then the predictor as i16 at payload offset 4 and the step index as u8 at offset 6, samples from offset 8); payloads of 480 bytes or more are decoded as 8-bit µ-law. Both decode to 8 kHz mono. The client's own talk-back audio is sent as `01wb` records in this ADPCM form. | reported (client) |
| AAC is decoded with no out-of-band configuration: sample rate and channel count come from each ADTS header. | reported (client) |
| Audio payloads over 2048 bytes are dropped, whatever the codec. | reported (client) |
| The client reads no audio timestamp from the stream and asks the device for no audio parameters for live view; the audio settings it can query belong to talk-back and alarm sounds. | reported (client) |
| The live-view start request has no audio field (`channelId`, `handle`, `streamType`, `policy`, optionally `preRec`); audio always arrives interleaved and muting is local. | reported (client) |
| Playback is paced by a local clock from each decoded frame's duration; audio frames that fall more than two frame durations behind are dropped, and the audio buffer is reset when the stream's time jumps. No explicit correction of the audio/video clock difference was found. | reported (client) |

## Session upkeep

| Fact | Provenance |
| --- | --- |
| Command 93, empty body, is answered with `<LinkType><type>LAN</type></LinkType>`. The official client sent it on each session at irregular intervals (seen between about 30 and 140 s after login). | observed |
| Whether a device drops idle sessions, and after how long, is **unconfirmed**. | open |
| The first capture ended before the client quit, so it showed no logout. When the client quit normally in a later capture, it sent command 382 (empty body) and then the logout (command 2) on each NVR session, 10–30 ms apart without waiting for the 382 reply, and closed TCP about 0.1 s after the logout reply (status 200). The 382 was answered 200 on the session that had played a recording and 405 on one that had not. | observed (client capture) |

**Keepalive and dead connections.** All reported (client); times are whole
seconds from the system clock.

| Fact | Provenance |
| --- | --- |
| Any bytes received on the session (replies, pushes, media) count as a sign of life. | reported (client) |
| Command 93 is the client's connection test and its only use of command 93. It is sent only when nothing has been received for at least 6 s, and at most once every 6 s; there is no fixed timer and no jitter. A session carrying live media therefore sends none. | reported (client) |
| Confirmed in a later capture: on an NVR session with no streams, command 93 went out every 5.8–6.3 s, paused for about 20 s while the device pushed alarm-state messages (command 33) every few seconds, and resumed afterwards. On the session carrying live view and playback, the client sent a single 93, in a gap between a playback and a download. The earlier 30–140 s spacing therefore reflects device pushes on that session. | observed (client capture) |
| The session is declared dead when nothing has been received for more than 10 s on a direct (address) connection, or 20 s on a UID (relayed) connection, but not within 6 s of opening it. The client then closes it and reconnects. | reported (client) |
| Reconnects are retried about every 2 s with no backoff and no limit for mains-powered devices (the client limits battery cameras to 5). Devices that are not yet open are retried every 10 s. | reported (client) |
| Each request has its own reply timeout; the logout's is 2 s (the connection test's is probably 8 s). A request that times out is reported as a timeout; the dead-connection rule above is what triggers a reconnect. | reported (client) |

**Logout.** Reported (client) unless marked observed.

| Fact | Provenance |
| --- | --- |
| Before closing a session the client sends **command 2**, class `0x1464`, status 0, payload offset 0, with body `<body><LoginUser version="1.1"><userVer>1</userVer></LoginUser></body>` (no user name or password), encrypted like any other body after login. | observed (client capture) |
| It waits for the reply (up to about 6 s) and then closes TCP. The client does not close a logged-in session any other way. | reported (client) |
| This happens on every close: quitting the app (all devices are closed, then the app waits 0.5 s before tearing down, and exits 1 s later) and before every reconnect after a dead connection. Removing a device was not traced as far. | reported (client); the quit case observed (client capture, see above) |
| An NVR answers this logout (body encrypted like any other after login) with status 200, and accepts a new login immediately afterwards; checked with five log-in, log-out cycles in a row. Whether skipping the logout keeps a login slot occupied was not measured. | observed |

nvrview sends the same logout on every graceful close, after stopping its
previews, and waits up to 2 s for the reply. Command 2 is on the read-only
allowlist because it only ends our own session: it carries no credentials and
changes no settings.

## Device information (useful for the UI)

Answered on the same session; bodies are XML. All observed.

| Command | Request | Reply |
| --- | --- | --- |
| 80 | empty | `VersionInfo` of the NVR (name, model, firmware) |
| 145 | (pushed by the device) | `ChannelInfoList`: per-channel state |
| 146 | empty | `StreamInfoList`: per-channel stream tiers with resolutions and codecs |
| 199 | empty | `Support`: capabilities including channel count |
| 318 | extension `<channelId>` | per-channel `VersionInfo`, including the channel's name and camera model |

Reply details:

| Fact | Provenance |
| --- | --- |
| The 146 reply carries status **300**, not 200, together with a complete body (seen on two NVRs). Treat 300 as success for this command. | observed |
| An NVR on newer firmware also answered 199 with status 300 and a complete body (one on older firmware with 200). Treat 300 as success for 199 too. | observed |
| `StreamInfoList` holds one `StreamInfo` per distinct stream configuration, not one per channel. Its `channelBits` is a bitmask of the channels that share it (bit *n* = channel *n*), and it contains one `encodeTable` per tier with `type` (`mainStream`, `subStream`), `resolution/width`, `resolution/height`, `videoEncType`, `defaultFramerate`, `defaultBitrate`, `defaultGop`, `framerateTable`, `bitrateTable`. | observed |
| `videoEncType` takes the values 0 and 1. | observed |
| The official client ignores `videoEncType` in the 146 reply; it reads only `channelBits` and, per `encodeTable`, `type`, resolution, the defaults and the two tables. `type` can also be `externStream` or `mobileStream`. | reported (client) |
| In the encoder configuration (command 56) the field of the same name means 0 = H.264, 1 = H.265, and the client knows no other value. That 146 uses the same coding is likely but unconfirmed; take the codec from the media records. | reported (client) |
| `ChannelInfoList` (145) is pushed with message ID 0 and lists every channel slot. Each `ChannelInfo` has `channelId`, `state` (`connect` for a slot with a camera, `none` for an empty one), `changed`, `uid`, `streamSupport` (a comma-separated tier list such as `mainStream,subStream,externStream`), `abilityChanged`, `wifiState`. It has no channel name; names come from 318. | observed |

**The device's time settings** are read with command **104**
(`NET_GET_SYSGENERAL_CFG`, the general system configuration, which carries the
device's time and time zone), command **106** (`NET_GET_DST_CFG`,
daylight-saving configuration), and command **38** (`NET_GET_NTPCFG`, NTP
configuration). When the user opened the NVR's Date &amp; Time settings screen
in the official client and closed it without saving, the client sent exactly
these three reads (each answered status 200) alongside the rest of a settings
page's reads (102 HDD, 511 users, 76 local link, 80 version, 26 color, 115
Wi-Fi, 574 sleep) and **no write command at all**: opening and closing the
screen without saving changed nothing on the device. The bodies of the replies
could not be decrypted in that capture (it had no login handshake), so the
field layout was not recorded then; the commands and their read-only use were.
(observed (client capture))

A later capture, which included the login and could be decrypted, showed the
Date & Time screen of a newer client version sending **only 104** (with the
usual 58, 102, 511, 76 and 80 reads): no 106 and no 38. So the DST rule and
the NTP fields are still unseen, but the 104 reply (status 200, about 516
bytes, also sent twice at login) is now known. It holds `SystemGeneral` with
`timeZone`, `osdFormat`, `year`, `month`, `day`, `hour`, `minute`, `second`,
`deviceId`, `timeFormat`, `language`, `deviceName`, `loginLock`, `lockTime`,
`allowedTimes` and `isDst`, followed by a `Norm` element (`norm`, e.g. `NTSC`).
`year` to `second` are the device's local time. `timeZone` is in seconds
**west** of UTC for standard time (a positive value is behind UTC), and
`isDst` 1 means daylight saving is in effect, adding one hour. On the one
device checked, `-timeZone + 3600` (with `isDst` 1) equalled the offset seen
in keyframe seconds (see "Rates, keyframes, and how a play ends"), so the
current UTC offset can be computed from 104 alone. (observed (client
capture); the offset formula is inferred and matched one device)

Commands seen but not needed for live view: 10, 44, 58, 76, 102, 104, 114, 151,
299, 511, 527–551, 574 (configuration and capability queries), 192 (event
subscription; see "Detection events"), and 14–16, 142, 272–274 (recording
search; see "Recordings: search and download"). Pushed by the device and not
needed for live view: 33 and 600 (detection events, with the message ID of
the 192 request), and 291, 623, 677, 723, 727 (message ID 0). Each time the
device pushed 145, the official client read 151, 146, 199, and the per-channel
44, 10, 318 and 299 again. (observed)

## Detection events

How a logged-in client learns, live, that a camera saw motion or a person,
vehicle, animal, and so on. Everything here is read-only: it subscribes this
session to pushes and changes no settings. Viewing tier. Observed in both
client captures (each with two NVRs of different firmware families; about
2.5 and 4 minutes of live view, playback and camera control); type names
and capability bits not seen there are reported (client).

### Subscribing: command 192

| Fact | Provenance |
| --- | --- |
| Right after login, on every NVR session, the client sends **192** (start alarm report) with an empty body and gets an empty status-200 reply. It carried message ID 4 on every session in both captures. | observed |
| The device's event pushes (33, and 600 below) then carry **the 192 request's message ID**, not 0. Other status pushes (291 floodlight, 623 sleep, 677 I/O, 723, 727) carry message ID 0. | observed |
| The first 33 arrived 40–250 ms after the 192 reply: a full snapshot of every channel's state. | observed |
| 193 (stop alarm report) exists but was never sent; the client simply logs out. Whether a device pushes 33 to a session that never sent 192 is unknown. | observed (not sent); open |

nvrview sends 192 once per session after login (viewing tier: it only asks
for pushes on our own session) and keys the pushes by command number, not by
message ID.

### Event state: command 33

```xml
<AlarmEventList version="1.1">
  <AlarmEvent version="1.1">
    <channelId>N</channelId>
    <status>MD</status>
    <AItype>people</AItype>
    <recording>0</recording>
    <timeStamp>0</timeStamp>
  </AlarmEvent>
  <!-- one AlarmEvent per channel slot -->
  <range><start>0</start><end>…</end></range>
</AlarmEventList>
```

| Fact | Provenance |
| --- | --- |
| 33 is a **state snapshot, not an event**: every push lists every channel slot of the NVR, idle ones included (`status` `none`, `AItype` `none`). A start is a channel's values leaving `none`; a stop is them returning to `none` in a later push. | observed |
| Pushes come only on change: of about 90 pushes, all but one differed from the previous push on that session. Gaps ranged from 0.2 s to about 60 s; no periodic refresh was seen. | observed |
| A multi-lens camera has one entry per lens, told apart by `subChannel` (0, 1). NVRs on newer firmware also send `subChannel` and a trailing `range` (`start`, `end`: the slot range this push covers); older firmware sent neither. | observed |
| `status` values seen: `none`, `MD`. `AItype` values seen: `none`, `people`, `dog_cat`. | observed |
| `AItype` is independent of `status`: one channel's pushes during a `people` detection went `MD`/`none`, `MD`/`people`, `MD`/`none`, `MD`/`people`, …, `none`/`people`, `none`/`none`; a `dog_cat` detection showed as `none`/`dog_cat` with no `MD` at all. So a detection's span is the span of its `AItype`, not of `MD`. | observed |
| `timeStamp` was always 0 and `recording` always 0 on both NVRs, so event times must be taken from the arrival time. | observed |
| The client matches `status` and `AItype` by case-insensitive substring, so a value may hold several names (comma lists of the form `IO,MD` appear among its constants). `status` names: `MD`, `videoLoss`, `blind`, `IOAlarm`, `visitor` (doorbell press), `package`, `answer`, `tamper`, `lmsg` (left message). `AItype` names: `people`, `vehicle`, `face`, `dog_cat`, `package`, `cry`. An optional `smartAiTypeList` of `smartAiType`/`type` carries `crossline`, `intrusion`, `loitering`, `legacy` (left object), `loss` (missing object). | reported (client) |
| 600 (`yoloWorldEventList`, with `range`) also arrives with the 192 message ID, on connect and about 1 s before each channel-list push (145); entries (`channel`, `subChannel`, `YoloWorldType`/`type`) belong to the user-defined object detection the client calls "YOLO world". Only empty lists were seen. | observed (empty); fields reported (client) |
| Bounding boxes are not in 33. Live detection boxes travel inside the video stream: the extension data after the first 8 bytes of a video record's extra header (see "Records as the official client reads them") is parsed as tag-length-value detection data, possibly LZ4-compressed. Its layout has not been traced. | reported (client) |

How nvrview reads the pushes: it keeps the last snapshot per session and
compares each new one with it, slot by slot (`channelId` and `subChannel`).
Each name in `AItype` that appears is a detection start and each that
disappears a stop; `MD` appearing or disappearing in `status` is motion
starting or stopping; other `status` names (doorbell press, video loss,
and so on) start and stop the same way as alarms. A snapshot that leaves a
slot out changes nothing for that slot (a `range` may cover only part of
the slots). When the session drops, everything active is stopped, since
the next session's first push is a fresh snapshot.

### Which channels detect what

| Fact | Provenance |
| --- | --- |
| `Support` (199) lists one `item` per channel slot, numbered by `chnID` from 0 (the same numbering as `channelId` in 145 and 33; one item for each of the `channelNum` slots, empty slots included with `aitype` 0). On newer NVR firmware, the item of a multi-lens camera also holds one `subItem` per lens, each with its own `chnID` (0 and 1, apparently the lens that 33 calls `subChannel`), `aitype`, and PTZ fields; the item's own `chnID` and `aitype` come before them. | observed |
| `Support` (199) gives each channel `item` an integer `aitype`, which the client reads as a bitmask: bit 0 AI present, 1 people, 2 vehicle, 3 face, 4 dog/cat, 6 other, 17 package, 22 baby cry, 23 user-defined (YOLO world) objects. Other bits are configuration features (5 detection area, 7 AI tracking, 8 stay time, 9 sensitivity, 10 target size, 24 always-on video, 25 tamper alarm). | reported (client); the copy from `aitype` to the client's bitfield was not traced, medium confidence |
| The bitmask fits the captures: `AiCfg` (299) listed `dog_cat` in `detectType` on exactly the channels with bit 4, and left out `vehicle` on a channel without bit 2. Channels with `aitype` 7 (AI, people, vehicle) produced `people` events. | observed |
| A separate per-channel `aiAnimalType` (0 or 1) marks support for the wildlife subtypes below. Other per-channel fields seen: `smartAI`, `eventTypeVersion`, `thumbnail`. | observed (fields); meaning of `aiAnimalType` reported (client) |
| `AiCfg` (299, read per channel at login) has `detectType` (comma list such as `people,vehicle,dog_cat`) and AI tracking settings. Read-only use is viewing tier; 300 writes it (configuration, never sent). | observed |
| `AbilityInfo` (151) has no AI-specific entries; its `alarm` module holds only permissions such as `motion_rw`. | observed |

How nvrview uses this: the core reads each `item`'s `aitype` and its
`subItem`s (lens number from the sub-item's `chnID`, and its `aitype`). A
channel whose item has two or more sub-items is reported to the app as a
multi-lens camera (`lenses`, and `lens_detects` per lens, in the `channels`
event, docs/FFI.md), with `detects` the union of the item's and the lenses'
kinds. Without sub-items (older firmware, single-lens cameras) only the
item's `aitype` is used; a second lens can then still be found by the
probe in "Second lenses", without per-lens kinds.

### Animal type names

| Name | Where | Provenance |
| --- | --- | --- |
| `dog_cat` | 33 `AItype`, 299 `detectType`, the 272 search request, and `alarmType` in 273 replies | observed |
| `dog_cat`, also `dog`, `cat` | recognized in recording-file `recordType` and event-search types | reported (client) |
| `dogCat`, `animal`, `pet` | the client's own JSON and UI names (`petCount` in 519) | reported (client); not wire names |
| `other`, `buck`, `bear`, `boar`, `fox`, `female_deer`, `raccoon`, `turkey`, `wolf` | wildlife subtypes: `animalTypes` in the paged search 492, `animalType` in its results, `detectedAnimalTypes` in a recording file's `FileInfo` | reported (client) |

## Fisheye cameras

A fisheye (360°) camera sends its raw circular image; dewarping it into
panoramas or virtual views is the client's job. Measured with `bc-cli query
199`, `bc-cli query 146` and `bc-cli capture`/`analyze` (sub and main, 12 s)
of a ceiling-mounted fisheye camera behind an NVR on newer firmware, and one
decoded main picture. Nothing here sends anything beyond the read-only
queries and a preview.

| Fact | Provenance |
| --- | --- |
| `Support` (199) gives every channel `item` a `fishEye` (1 for the fisheye camera, 0 for every other channel) and a `channelType` string: `fisheye_ipc` for the fisheye camera, `ipc` for ordinary cameras, `Doorbell` for a doorbell. The fisheye item had no `subItem`. | observed |
| The fisheye camera's item also had `ptzType` 0, `ptzControl` 64, `autoPt` 1, and `ipcAudioTalk` 1. What `ptzControl` 64 and `autoPt` mean for a camera without a motor is unknown; nvrview sends it no motion commands. | observed; meaning open |
| Its streams are square: `StreamInfo` (146) listed main 2560×2560 (25 fps advertised) and sub 1024×1024 (10 fps advertised) in one entry, and main 2048×2048 with the same sub in another. The camera's channel list (145) named `mainStream,subStream` only. | observed |
| Delivered: sub 1024×1024 at 10.46 fps with a keyframe every 40 pictures (3.84 s); main 2560×2560 H.264 at 12.50 fps (half the advertised 25) with a keyframe every 24 pictures (1.92 s). As elsewhere, take the rate from the timestamps. | observed |
| The 146 entry with the 2560×2560 main stream had `videoEncType` 1, yet the delivered main stream was H.264 (High profile): one more reason to take the codec from the media records. | observed |
| The picture is the lens's image circle on black. Fitted to the black-to-image boundary of three main pictures (and one scaled to the sub size), the circle's center sat at (0.501, 0.502) of the width and height and its radius at 0.471 of the width, the same in every picture to within 0.001; the corners outside it read luma 0. | observed |
| The camera draws its text overlay (name and date/time) in the black corners outside the circle, not over the image. | observed |
| Ceiling mount: the floor is in the middle of the circle and the walls around its rim. The lens's field of view is not reported by any query seen; nvrview assumes 180° (the rim is the horizon). | observed (mount); field of view assumed |

How nvrview uses this: the core passes `fishEye` (or a `channelType` of
`fisheye_ipc`) to the apps as `fisheye` in the `channels` event
(docs/FFI.md). The apps find the circle in the decoded picture themselves
(docs/RENDERING.md, "Fisheye dewarping"), since no query reports it, so a
camera that frames its circle differently still works.

## Camera motion control

A second client capture on a newer NVR (same firmware family, a later build)
repeated the PTZ session: a PTZ camera's panel, the PTZ lens of a multi-lens
camera with presets and a patrol button, and a motorized-lens camera. Rows
below cite it as "observed (second capture)".

Commands that move a camera, change its focus, or play sound through its
speaker. Each is marked with its tier (docs/ARCHITECTURE.md, "Device
commands"): **viewing** (read-only, always allowed), **motion** (camera
motion control: the user's own controls, talk-back with the camera's
permission, automation only with a per-camera grant; rate-limited and
logged), or **configuration** (changes settings; never sent). Unless marked observed, everything here is
reported (client). Observed facts come from the client capture described at
the top: a PTZ lens of a multi-lens camera, a camera with a motorized zoom
and focus lens (no pan or tilt), and a camera with a speaker, all behind one
NVR.

**Request shapes.** Read-only queries in this area have no body: the request
is only an extension, `<Extension version="1.1"><channelId>N</channelId><chnType>0</chnType></Extension>`
(the client adds `subChannel` for a second lens), and the client waits up to
8 s for the reply. (observed for 10, 190, 294, 340 and the channel queries
527–551; the timeout is reported (client)) In the second capture the
requests for the PTZ lens of a multi-lens camera (190, 340, 64) carried
`<chnType>0</chnType><subChannel>1</subChannel>`, the lens field being
`subChannel` (observed (second capture)).

Commands that act (18, 19, 295, 201) put their XML body in the *payload*:
the header's payload offset is the length of an extension
`<Extension version="1.1"><channelId>N</channelId></Extension>` (no
`chnType`), and the payload is the `<body>…</body>` XML, encrypted as its own
unit like any body. Every one of them was answered with an empty reply,
status 200, carrying no state. (observed) In the second capture the
extension of 18, 19 and 20 for the PTZ lens of a multi-lens camera held
`channelId` and `<subChannel>1</subChannel>` (no `chnType`); for every other
camera it was the plain `channelId`. (observed (second capture))

**What nvrview must do.** From these observations: build every motion body
itself from a fixed template (never forward client-supplied XML); send `stop`
after every move on every path, including errors, timeouts and disconnects,
and also stop on its own after a short maximum hold time, because a move is
a single command with no repeats, so the camera presumably keeps moving
until it gets a stop (not measured: the capture has no position readings);
clamp `movePos` to the `minPos`–`maxPos` the camera reported in 294; send 19
only with `toPos`. The official client sent one command per user action and
never repeated a move while a button was held, so a rate limit of a few
commands per second per camera costs nothing.

### Capabilities

| Fact | Provenance |
| --- | --- |
| `Support` (199) has a device-level `ptzMode` (`none`, `af`, `ptz`, `pt`, `p`) and per-channel `item` entries with `chnID`, `ptzType`, `ptzControl`, `ptzPreset`, `ptzPatrol`, `ptzTattern`, `autoPt`, `ipcAudioTalk`, `channelType`. | observed (field names); meanings reported (client) |
| `ipcAudioTalk` 1 marks a channel that supports talk-back: on two NVRs the client asked for the talk ability (command 10) of exactly the channels with `ipcAudioTalk` 1, and of no others. | observed |
| `ptzType` took the values 0, 1 and 3, and `ptzControl` several bitmasks, on different channels; their meaning is not established. The client reads `ptzPreset` bit 0 as "has presets", `ptzControl` bit 7 as "can report its position", bit 4 as digital zoom, bit 14 as manual focus, and a per-channel `autoFocus` of 1 as autofocus support; it shows a zoom and focus slider when the device's `ptzMode` is `af` or `ptz`. | values observed; meanings reported (client), medium confidence |
| What the client offered per channel, against `Support`: on a PTZ lens with `ptzType` 3, `ptzControl` 239 (bit 7 set, bit 4 clear) and `ptzPreset` 3, it sent moves (18), read zoom and focus (294) and the self-test state (340), and listed and recalled presets (190, 19); it never read the position (433) although bit 7 was set. On a camera with `ptzType` 1, `ptzControl` 1 and `ptzPreset` 0 it offered zoom and focus (294, 295) but no moves or presets, although the NVR's `ptzMode` was `none`. No channel had an `autoFocus` element, and no autofocus command was sent. So `ptzType` 1 appears to mean "zoom and focus only" and 3 "pan, tilt, zoom"; the client evidently decides per channel, not from `ptzMode`. | observed (client capture); meanings inferred |
| `AbilityInfo` (151) lists, per module and channel, what the logged-in user may do, as `abilityValue` strings like `download_rw, fileFind_rw`. PTZ entries are `control`, `preset`, `cruise`, `track`, `decoder`, `ptzInfo`; audio has `talk`; replay has `replay`, `seek`; record has `download`, `fileFind`. | observed (form); PTZ entries reported (client) |

### Reading position and state (viewing)

| Command | Reply | Provenance |
| --- | --- | --- |
| 433 | `ptzCurPos` with integers `pPos`, `tPos`, `zPos` (pan, tilt, zoom; units and ranges unknown). Not sent by the client in the capture, even while panning a PTZ camera that sets `ptzControl` bit 7. | reported (client) |
| 294 | `PtzZoomFocus` with `channelId`, then `zoom` and `focus`, each with `maxPos`, `minPos`, `curPos` (integers). Ranges differ by camera: a PTZ lens reported zoom 1000–6000 and focus 1–100; a motorized-lens camera zoom 0–28 and focus 0–164. The units are the camera's own steps. | observed |
| 294 in the second capture | The same ranges again (zoom 1000–6000 and focus 1–100 on a PTZ camera; 0–28 and 0–164 on a motorized-lens camera), read when each panel opened and once more about 40 s later on the lens camera. The PTZ lens of the multi-lens camera was not read with 294. | observed (second capture) |
| 294 timing | Read when the PTZ panel opened for a channel and again about 6 s after a preset recall or a zoom move (6.2, 6.2 and 6.4 s seen), but not after focus steps and never on a fixed period. This **contradicts** the static reading of a 6 s poll; it fits a 6 s timer restarted by some actions. | observed (client capture) |
| 294 state | After a zoom move the camera's focus `curPos` changed by itself (refocus after zoom). After two preset recalls on the PTZ lens the zoom `curPos` was unchanged. | observed (client capture) |
| 190 | `PtzPreset` with `channelId`, `separateCfg` (holding `preset` 0), `maxPresetNum` (64), `maxPresetPicNum` (64), and `presetList/preset` entries with only `id` (from 0), `name`, `imageName`; no coordinates or `mode`. Read once when the preset list opened. | observed |
| 190 in the second capture | For the PTZ lens of a multi-lens camera the request carried `chnType` 0 and `subChannel` 1; the reply carried only `channelId` (no lens field), three presets (ids 0, 1, 2; id 0 had an empty `imageName`) and the same maxima (64, 64). The client read it again, with 64, three times in the next 130 s. | observed (second capture) |
| 340 | `PtzAutoTest` with `testStat` (0 seen): the pan/tilt self-test state. Read once, together with 294, when the PTZ panel opened on the PTZ lens; not read for the zoom-only camera. Viewing tier. In the second capture the request for a multi-lens camera's PTZ lens carried `subChannel` 1. | observed (client capture; second capture) |
| 64 | `PtzCruise` (patrol settings; viewing): `channelId`, `intervalTime` (3600), `maxKeyPosCount` (4), `separateCfg/cruise` (1), `defSchedule` (a 168-character string of `1`), and `cruiseList` (empty when no patrol was set). Read with `chnType` 0 and `subChannel` 1 when the patrol panel opened and before each patrol edit. | observed (second capture) |
| 421 | `imageFileInfo` with `channelId` and `imageName` (`preset_NN`) fetches a preset's picture: the reply gives `fileSize` and `delete` 0, then a binary-data message with the JPEG. Viewing; read once when the preset list opened. | observed (second capture) |
| 723 | pushed by the device (`id` 0): `coordinatePointList/coordinatePoint` with `channel`, `subChannel` (0 seen, while the user drove lens 1), `xpos`, `ypos` (0–1) and `type` `normal`. Seen on connect and then several times while the multi-lens camera's panel was open, some of them 1–4 s after a recall; not tied to moves. Meaning not established (apparently a point on the other lens's picture). | observed (second capture); meaning open |
| 224 | `AutoFocus` with `channelId`, `disable` (whether continuous autofocus is off). Not sent in the capture. | reported (client) |
| 742 | PTZ information: `pt`, `separateCfg`. Not sent in the capture. | reported (client) |
| 332, 35, 64 | guard (home position) settings, PTZ serial settings, patrol settings | reported (client) |
| 10 | `TalkAbility`: `duplexList/duplex` (`FDX` seen), `audioStreamModeList/audioStreamMode` (`followVideoStream`, `mixAudioStream` seen), and `audioConfigList/audioConfig` with `priority`, `audioType` (`adpcm` seen; the client also knows `g711`), `sampleRate` (16000 seen), `samplePrecision` (16), `lengthPerEncoder` (1024), `soundTrack` (`mono`) | observed |
| 542, 657, 693 | pushed by the device: PTZ state, PTZ status, talk status. None arrived while the capture's camera panned, recalled presets, zoomed, focused or talked. | reported (client) |

### Moving (motion)

**Command 18, PTZ control**, a body with no reply data (observed):

```xml
<PtzControl version="1.1">
  <channelId>N</channelId>
  <speed>40</speed>
  <command>left</command>
</PtzControl>
```

| Fact | Provenance |
| --- | --- |
| `command` is one of `left`, `right`, `up`, `down`, `leftUp`, `leftDown`, `rightUp`, `rightDown`, `zoomInc`, `zoomDec`, `focusInc`, `focusDec`, `irisInc`, `irisDec`, or `stop`. | reported (client); `left`, `right`, `up`, `down`, `stop` observed |
| Diagonals: none was sent in the second capture either, so the names `leftUp`, `leftDown`, `rightUp`, `rightDown` remain reported, not observed. | observed (absence) |
| Speeds in the second capture: 40 for the four directions (left 1.9 s, right 2.1 s, up 2.0 s, down 2.2 s from move to stop), 1 for the lowest slider setting (up, held 3.7 s) and 64 for the highest (up, held 1.3 s). `stop` was always `speed` 1. Replies came in 3–62 ms. | observed (second capture) |
| The PTZ camera's zoom and focus: no zoom or focus command (18 `zoomInc`/`zoomDec`/`focusInc`/`focusDec`, or 295) was sent to the pan-and-tilt cameras in the second capture, although 294 reported zoom 1000–6000 and focus 1–100 for one of them. How the client zooms a pan-and-tilt camera is therefore still not observed. | observed (absence) |
| Moves are continuous: the client sent the direction once when a control was pressed and `stop` with `speed` 1 when it was released, 1.3–1.5 s later in the capture; nothing was repeated while the button was held. Each command was answered by an empty status-200 reply in 17–56 ms. A client must therefore always follow a move with `stop`, including on error and disconnect paths. | observed |
| The move speed in the capture was 40; the client's built-in default is 20 and its speed slider runs 1–64, so 40 was presumably a stored user setting. | observed (40); default and range reported (client) |
| On a multi-lens camera whose second lens pans and tilts, the client addressed 18 to the plain `channelId` (no `chnType` or `subChannel`) while showing that lens's `externStream`. **On the newer NVR (second capture) it added `subChannel` 1 to the extension of 18, 19 and 20 and to the reads (190, 340, 64) for that lens**, while the body's `channelId` stayed the plain channel. A client that sends the plain channel to such a camera may therefore move the other lens: the form without `subChannel` is observed only in the first capture. | observed (client capture; second capture) |
| `auto` (continuous scan) is also motion; `clearPatrol` changes settings (configuration). | reported (client) |
| Patrol: `startPatrol` and `stopPatrol` are 18 bodies with `command`, `patrolId` (0) and `speed` 3 (not the move speed), sent once each and answered empty, status 200, in 28–59 ms. A `startPatrol` was sent five times in one session (one per click of the patrol button) and one `stopPatrol` ended it. No position reading shows what a patrol does or whether it ends by itself. **Document only: nvrview does not send these.** | observed (second capture) |
| Patrol editing: around the patrol starts the client wrote the patrol list with 20 (`PtzCruise`: `cruiseList/cruise` with `patrolId`, `enable`, `name`, `valueTable`, and 16 `keyPos` slots of `keyPosId`, `presetId` (-1 for an empty slot), `speed` 32, `dwellTime` 10, `trackTime`, `trackEnable`). Three 20 writes were seen (answered empty, 200, in 136–178 ms), each preceded by a 64 read. This is configuration: never sent. | observed (second capture) |
| No "autofocus now" command exists in the client, and none was sent in the capture. On a camera with a motorized lens, the client's focus buttons did not use 18 `focusInc`/`focusDec`: each click sent 295 `focusPos` with the current focus position plus or minus 1 (below). | reported (client); observed (client capture) |

**Command 19, presets.** One command number for recall, save, and delete;
only the body's `command` tells them apart:

```xml
<PtzPreset version="1.1">
  <channelId>N</channelId>
  <presetList>
    <preset>
      <id>1</id>
      <command>toPos</command>
    </preset>
  </presetList>
</PtzPreset>
```

| Fact | Provenance |
| --- | --- |
| `toPos` recalls a preset: **motion**. `setPos` saves the current position as a preset and `delPos` deletes one: **configuration**. The allowlist cannot judge 19 by its number; nvrview must build recall bodies itself and never forward any other `command`. | reported (client) |
| A recall as the client sent it: one `preset` with `id`, `command` `toPos`, the preset's `name` (from 190), `xpos`, `ypos`, `height`, `width` all `0.000000e+00`, and `mode` `global`. The reply was empty with status 200 after 106–140 ms, carrying no state; it does not say when the camera arrives. | observed |
| `imageName` was not sent in a recall. Whether a recall needs more than `id` and `command` is not known. | observed; open |
| Second capture, the PTZ lens of a multi-lens camera, four recalls (ids 0, 2, 1, 2; each answered empty, status 200, in 89–113 ms): the first two carried the preset's `name` (the second also its `imageName`), the last two carried **only** `id`, `command` `toPos`, the four zero coordinates and `mode` `global` (no `name`, no `imageName`) and were accepted just the same. The extension held `subChannel` 1. The recall form the core builds (with `name`) is also observed. So `name` is optional. | observed (second capture) |

**Other motion commands.**

| Command | Body | Tier | Provenance |
| --- | --- | --- | --- |
| 295 | `StartZoomFocus` with `channelId`, `command` (`zoomPos` or `focusPos`), `movePos`: move zoom or focus to an absolute position in the range from 294. In the second capture: zoom clicks stepped `movePos` 1, 2, 3, 2, 1 and a slider drag sent 25 and then 0 (range 0–28); focus went to 0 and 164, the two **edges** of the 0–164 range, and both were answered 200 (empty), then stepped down 29, 28, ... 23 one click at a time. No edge was refused. Replies took 58–250 ms. Moving the zoom slider sent one 295 per move, not a stream (`movePos` 11, then 0 about 1.3 s later, on a 0–28 range); the focus buttons sent one 295 per click, stepping `movePos` by 1 from the current focus position. Replies were empty, status 200, after 100–220 ms. | motion | observed |
| 468 | `ptzCurPos` with `pPos`, `tPos`, `zPos`: go to absolute coordinates | motion, but nvrview never chooses raw coordinates (it recalls presets instead) | reported (client) |
| 445 | `Ptz3DLocation` with `topLeftX`, `topLeftY`, `width`, `height` (floats), `speed`, `streamType`: aim at a rectangle drawn on the picture | motion | reported (client) |
| 545 | `pPos`, `tPos`, `zPos` and a rectangle: move to a tracking area | motion | reported (client) |
| 709 | manual focus with `xpos`, `ypos`, `height`, `width` (a focus region); no caller in the client's UI was found, and it was not sent in the capture, so its effect is unknown | motion (unconfirmed) | reported (client) |
| 331 | guard (home position): `toGrd` goes there, `setGrd` saves it. The client writes `benable` and `timeout` into every 331 body, so even `toGrd` can rewrite settings | configuration (never sent, even to go home) | reported (client) |

**Configuration in this area (never sent):** 19 with `setPos` or `delPos`,
20 (set patrol), 331, 225 (turn continuous autofocus on or off), 708
(autofocus mode), 34 (PTZ serial settings).

### What nvrview will send

> **Implemented in the core (Z1); not yet reachable from the apps**
> (docs/CAPTURE.md, "Pan, tilt, and zoom"; docs/FFI.md, "Camera motion
> (PTZ)"). 18, 19 and 295 are on the allowlist in the motion tier and 294,
> 190 and 340 in the viewing tier, each with a fixed template
> (`bc_protocol::ptz`) and an allowlist test. Tested only against the mock
> NVR's PTZ model, never on hardware: the mock's answers to what was not
> observed (a refused command, an out-of-range position, an unknown preset)
> are its own choices, not facts.

Only the forms observed above, each built by the core from a fixed
template:

| Command | What nvrview sends | Tier | Based on |
| --- | --- | --- | --- |
| 18 | `PtzControl` with `command` `left`, `right`, `up` or `down` and a `speed` of 1 to 64, then `stop` (with `speed` 1, as the client did). One move per press; nothing repeated while it is held. | motion | observed |
| 19 | `PtzPreset` with one `preset` whose `command` is `toPos` (recall), with the `id` from 190 and the other fields as the client sent them. Never `setPos` or `delPos`. | motion | observed |
| 295 | `StartZoomFocus` with `zoomPos` or `focusPos` and a `movePos` clamped to the `minPos`–`maxPos` of the last 294 reply. | motion | observed |
| 294 | read zoom and focus ranges and positions: when the PTZ controls open, and about 6 s after a zoom move or a preset recall, as the client did | viewing | observed |
| 190 | read the preset list when the presets open | viewing | observed |
| 340 | read the pan and tilt self-test state with 294 on a pan-and-tilt camera | viewing | observed |

- **Stop.** Because a move is one command and the camera presumably keeps
  moving until `stop` (not measured), the core sends `stop` on every path
  (release, error, timeout, disconnect, tile close, session drop), 1 s
  after the app stops renewing a held move, and after 30 s of holding.
- **Not sent until observed:** the diagonals (`leftUp`, `leftDown`,
  `rightUp`, `rightDown`), 18 `zoomInc`/`zoomDec` and
  `focusInc`/`focusDec`, `auto`, patrols, 433, 468, 445, 545, 709, and
  autofocus. The second capture newly observed, and the core still does not
  send: 18 `startPatrol`/`stopPatrol`, 20 (patrol write, configuration), the
  64 and 421 reads, and the `subChannel` form of the extension for a
  multi-lens camera's PTZ lens (18, 19, 190, 340). Until the core carries a
  lens field, a multi-lens camera's second lens cannot be addressed, and the
  plain-channel form may move the wrong lens on newer NVRs.
- Which controls a channel gets comes from 199 (`ptzType`, `ptzPreset`;
  see "Capabilities"), so a channel without them is never sent a motion
  command.

### Talk-back (motion)

Sending sound to a camera's or doorbell's speaker. Observed (client capture)
on a camera behind an NVR, while the client showed that camera's live view,
unless marked.

1. Command 10 reads the talk ability (viewing; observed above). The client
   read it at login, not again before talking.
2. Command **201** opens talk, with the body as payload after a channel
   extension (see "Request shapes"); answered empty, status 200, in about
   65 ms:

   ```xml
   <TalkConfig version="1.1">
     <channelId>N</channelId>
     <duplex>FDX</duplex>
     <audioStreamMode>followVideoStream</audioStreamMode>
     <audioConfig>
       <audioType>adpcm</audioType>
       <sampleRate>16000</sampleRate>
       <samplePrecision>16</samplePrecision>
       <lengthPerEncoder>1024</lengthPerEncoder>
       <soundTrack>mono</soundTrack>
     </audioConfig>
   </TalkConfig>
   ```

   The client fills it from the ability reply (`duplex` `FDX` or `HDX`;
   `audioStreamMode` `followVideoStream`, `onlyAudioStream` or
   `mixAudioStream`; the alternatives are reported (client)).
3. Audio goes out as command **202** messages, class `0x1464`, status 0,
   **message ID 0**, with the extension
   `<Extension version="1.1"><binaryData>1</binaryData><channelId>N</channelId></Extension>`
   (encrypted like any extension; payload offset is its length) and a
   **plain** payload (no `encryptLen`) holding exactly one `01wb` record.
4. Command **11** closes talk: no body, channel extension only; answered
   empty, status 200.

The `01wb` record, as observed in every message: the magic, then both u16
size fields 520; a 4-byte prefix that was always `00 01 02 00`; the IMA ADPCM
state (i16 little-endian predictor, u8 step index, one zero byte); then 512
bytes of 4-bit samples, which is 1024 samples. The first record started from
predictor 0, step index 0. Every 202 payload was exactly 528 bytes: the
8-byte record header and the 520 bytes, so no padding.

**How the samples are coded** (observed (client capture), measured by
decoding all 135 records of the session with the standard IMA ADPCM step
and index tables):

- Each byte holds two samples, **high nibble first**. Decoded high nibble
  first, the state at the end of every record equaled the state in the next
  record's header, predictor and step index both, in 134 of 134 cases; decoded
  low nibble first, in none.
- So the encoder runs continuously across records: a record's header is the
  encoder state before its first sample (not an extra sample), carried over
  from the previous record. With the 8-byte headers removed, the records
  concatenate into a plain IMA ADPCM stream starting from state 0, the same
  coding FFmpeg's `adpcm_ima_ssi` decoder reads (high nibble first, standard
  tables, no block headers); nvrview's tests check its encoder against that
  decoder.

**Exact requests** (observed (client capture); channel numbers aside, as
sent):

- 201's extension is the XML declaration, then
  `<Extension version="1.1">` with only `<channelId>` (no `chnType`), each
  element on its own line. Its payload is the XML declaration, `<body>`, the
  `TalkConfig` above, `</body>`. The body has no `priority`, although the
  ability lists one.
- 11 is the same extension alone: the header's payload offset equals the
  body length, and nothing follows.
- 202's extension is the XML declaration, then `<Extension version="1.1">`
  with `<binaryData>1</binaryData>` and `<channelId>`, 131 bytes before
  encryption.
- 201 and 11 carried ordinary session message IDs (the next free ones);
  only 202 used ID 0. The device answered 201 and 11 with the request's ID.
- Every `TalkAbility` reply seen (every talk-capable channel of two NVRs,
  read several times) had `duplex` `FDX` only, `followVideoStream` as the first (on
  some channels the only) `audioStreamMode`, and one `audioConfig`: `adpcm`,
  16000, 16, 1024, `mono`, `priority` 0. The reply body has no
  `channelId`. The client's 201 repeated the first `duplex`, the first
  `audioStreamMode`, and that `audioConfig`.

| Fact | Provenance |
| --- | --- |
| The first 202 followed the 201 reply after about 0.2 s; the 11 followed the last 202 after about 50 ms. | observed |
| Records went out at a mean spacing of 64.0 ms (median 62 ms, from 5 to 144 ms between messages), which is 1024 samples at 16 kHz: the talk audio is 16 kHz ADPCM, as the ability advertised. (The 8 kHz of the client's ADPCM *decoder*, reported above, applies to received audio only.) | observed |
| The pacing is real time: over the session's 8.6 s, the n-th record went out within -12 to +104 ms of n × 64 ms after the first, and ended 5 ms early. Gaps ran 55–72 ms for most records, with the occasional long gap (up to 144 ms) followed by a short one (down to 5 ms): the client catches up after a stall instead of drifting, and never ran more than about two records ahead of or behind the clock. | observed (client capture) |
| The device answered **every** 202 with an empty 202 reply, status 200, message ID 0, after 4–118 ms (median 29 ms). | observed |
| No talk-status push (693) arrived during talk. | observed (client capture) |
| Commands 12 and 203–205 (other talk stream variants) are not used by this client. | reported (client) |
| The device plays talk audio roughly a second behind what it has received, and **discards what it has not played when the talk is closed** (11). A 3.6 s typed reply sent completely and in real time (3.6 s from the first 202 to the close, every record acknowledged) was heard only for about its first 2.4 s on a doorbell. | observed (nvrview, 2026-10) |

**What nvrview does with this** (the reason the allowlist grew): 10 joins the
viewing tier (a read-only query); 201, 202 and 11 form the talk-back part of
the motion tier and are built only for a channel the user enabled talk-back
on. nvrview sends talk only to channels whose 199 item has `ipcAudioTalk`
1 and whose ability offers the one configuration observed (`adpcm`, 16000,
16, 1024, `mono`); builds the 201 body itself from that template (never
from client-supplied XML); sends 202 in real time from a local clock, one
1024-sample record per 64 ms, catching up a short stall as the client
does but restarting its clock after a longer one rather than bursting, and
drops audio rather than queueing it without bound; and sends
11 on every exit path (stop, drop, session close, a refused or failed
record), since nothing observed ends a talk but 11. Because the device
drops what it has not played at the close, a talk that ends normally (all
pushed audio sent, or the user let go of Talk) stays open for a tail of
1.5 s after its last audio, sending silent records at the same pace, and
only then sends 11; audio pushed during the tail continues the same talk
(no new 201). Aborts (permission withdrawn, tile closed, shutdown, a
device error) send 11 at once.

## Recordings: search and download

Everything in this section is read-only: it lists or copies recordings and
changes nothing on the device. All of it belongs to the viewing tier.

**Times.** Every time in these bodies is a block of six elements,
`<year>`, `<month>` (1–12), `<day>`, `<hour>`, `<minute>`, `<second>`, in the
device's local wall-clock time with no zone or offset. (observed) The one
exception is the recording file `Id`, which embeds UTC (see command 15 below).

### What the official client sent when its playback tab opened

Observed in a capture of the official desktop client and an NVR,
on the session that was already carrying live view. The client stopped the
live stream of the selected channel (command 4), then ran three searches over
that same session, interleaved, each with its own message ID. The whole
exchange took about 3.5 s. No playback or download was started in that
capture. A later capture showed the same sequence (searches of 142 three
times and of 14–16 and 272–274 twice each) and then the playback and
download described under "Playback" and "Download by time range". (observed)

**1. Which days have recordings: command 142.** One request per month:

```xml
<DayRecords version="1.1">
  <startTime><year>2026</year><month>1</month><day>1</day><hour>0</hour><minute>0</minute><second>0</second></startTime>
  <endTime><year>2026</year><month>1</month><day>31</day><hour>23</hour><minute>59</minute><second>59</second></endTime>
  <DayRecordList>
    <DayRecord><index>0</index><channelId>N</channelId><uid>UID-EXAMPLE</uid></DayRecord>
  </DayRecordList>
</DayRecords>
```

| Fact | Provenance |
| --- | --- |
| The reply repeats `startTime`/`endTime` and, per `DayRecord` (`index`, `channelId`), a `dayTypeList` of `dayType` entries, each with an `index` and a `type` (`normal` was the only value seen; the client also knows `none` and `alarm`). | observed; other types reported (client) |
| For a channel that records continuously, the number of entries equaled the current day of the month (counted in the device's local time) in two captures on consecutive days, with one more entry on the second day; indexes ran from 0 and included the current day. This fits "one entry per day with recordings, index = day of month − 1", and corrects an earlier reading here that was off by one day. Other `type` values (probably event recordings) and gaps for days without recordings were not seen. | observed (meaning inferred) |
| `uid` is the camera's device ID from `ChannelInfo` (command 145). The client sent the same request three times in a row. | observed |

**2. Recording files of one day: commands 14, 15, 16** (open, fetch, close).
All three use the same message ID.

Command 14 opens a search:

```xml
<FileInfoList version="1.1">
  <FileInfo>
    <uid>UID-EXAMPLE</uid>
    <searchAITrack>1</searchAITrack>
    <channelId>N</channelId>
    <logicChnBitmap>255</logicChnBitmap>
    <streamType>mainStream</streamType>
    <recordType>manual, sched, io, md, people, face, vehicle, …</recordType>
    <fileRecordType><i>manual</i><i>sched</i><i>md</i>…</fileRecordType>
    <startTime>… 00:00:00 of the day …</startTime>
    <endTime>… 23:59:59 of the same day …</endTime>
  </FileInfo>
</FileInfoList>
```

| Fact | Provenance |
| --- | --- |
| `recordType` is a comma-separated list of recording causes; `fileRecordType` lists the same causes as `<i>` items plus a few more (`answer`, `nonmotorveh`, `IPCInputOutput`). The client asked for every cause it knows. | observed |
| `streamType` takes the live-view stream names (`externStream` was seen); the client searched the stream the tile was showing. | observed |
| The 14 reply is a single summary `FileInfo`: `channelId`, `handle` (0 here), empty `name`, `containsAudio`, `recordType` (the causes present that day, e.g. `manual ,io ,md ,sched`), `supportSub`, `sizeL`/`sizeH` 0, and the requested times. | observed |
| Command 15 sends `FileInfo` with `channelId`, `uid`, `searchAITrack`, and the `handle` from the 14 reply. Its reply lists the day's files, one `FileInfo` each: `channelId`, `handle`, `name`, `Id`, `streamType`, `containsAudio`, `recordType`, `sizeL` and `sizeH` (low and high 32 bits of the file size in bytes), `supportSub`, `encrypt`, `startTime`, `endTime`. | observed |
| A single 15 returned every file of a day of continuous recording (a few dozen files, about 19 KB of XML); whether a busier day needs repeated 15 requests until an empty reply is not known. | observed |
| Recording files run about 30 to 60 minutes; the first one of a day can start on the previous day. | observed |
| In the client, 14, 15 and 16 are named search *open*, *next* and *close*, so 15 is presumably repeated for further pages. The reply `FileInfo` can also carry `uid`, `fileType` (`h264`, `mp4`, `flv`, `jpg`), `playSpeed`, `fileSize` and `typeExtension`, and 14 can carry `fileType` `jpg` to search snapshots. | reported (client) |
| Command 16 with the same body closes the search; the reply is empty with status 200. The next search was handed the same `handle` 0 again. | observed |
| `name` and `Id` are opaque file identifiers; treat them as strings. (In the capture `Id` looked like `<tier>-<channel>-0-<packed start time>-<suffix>`, with the start time in UTC and a 0-based month, and `name` like a channel number plus the local start time; neither is needed to download by time.) A later capture confirmed this: the `Id` embeds UTC with a zero-based month (`<tier>-<channel>-0-<yy><month-1><dd><hhmmss>-<suffix>`), while the file `name` and every search time are device local; 7 names the file by its local `name`, 5 by the UTC `Id`. | observed (client capture) |
| A file still being written was listed with `recordType` `sched` and an `Id` suffix other than `00000`; after it closed it was listed with suffix `00000` and `recordType` `none`. Playing it by the `00000` form worked. The meaning of the suffix is inferred, not known. | observed (client capture) |
| Selecting the current day again simply repeats 14, 272, 15, 273 pages, 16 and 274 for that day; there is no separate "extend" command. The reply to 15 lists every file of the day, and the last file's `endTime` equals the time of the search (a file still being written ends "now"), so the list grows only when searched again. A day without recordings is answered with a normal 14 reply, a 15 reply that is an empty `FileInfoList` element, and an empty 16; the client sent that search and no play. | observed (client capture) |

**3. Motion and detection events of one day: commands 272, 273, 274** (open,
fetch pages, close). All three use the same message ID. Unlike the other
searches, the request body travels as the *payload* of the message: the
header's payload offset is non-zero, the extension is
`<Extension version="1.1"><channelId>N</channelId></Extension>`, and the
payload is the XML body below, encrypted like any other body. Replies carry an
ordinary (encrypted) body with no extension. (observed)

```xml
<findAlarmVideo version="1.1">
  <channelId>N</channelId>
  <uid>UID-EXAMPLE</uid>
  <logicChnBitmap>255</logicChnBitmap>
  <streamType>2</streamType>
  <notSearchVideo>0</notSearchVideo>
  <startTime>… 00:00:00 of the day …</startTime>
  <endTime>… 23:59:59 of the same day …</endTime>
  <alarmType>md, pir, other, package, io, people, face, vehicle, …</alarmType>
  <eventAlarmType><i>md</i><i>pir</i>…</eventAlarmType>
</findAlarmVideo>
```

| Fact | Provenance |
| --- | --- |
| The 272 reply is `findAlarmVideo` with `channelId` and a `fileHandle` (a small number that increased by one with each new search). | observed |
| `streamType` here is a number, not a name: 2 was sent while the 14 search of the same moment named `externStream`, and 0 while it named `mainStream`. The full mapping is not known. | observed |
| Command 273 sends `findAlarmVideo` with `channelId` and `fileHandle` and returns one page: `alarmVideoInfo` with `channelId`, `fileHandle`, `bFinished`, and an `alarmVideoList` of up to 50 `alarmVideo` entries. The client repeats 273 until `bFinished` is 1; every page but the last was full. A later capture on newer firmware saw pages of about 100 entries, requests sent back to back about every 0.15 s, and up to 14 pages for one busy day; the client then sent 274. | observed (client capture) |
| Each `alarmVideo` has `fileName`, `bHasRecFile`, `bEncrypted`, `alarmType` (`md`, `vehicle`, `people` seen), `fileId` (the `Id` of the recording file from command 15 that contains the event), `startTime`, `endTime`. Events were about 10 to 25 s long. | observed |
| Command 274 with `channelId` and `fileHandle` closes the search; empty reply, status 200. | observed |
| The client also knows `destination` in the 272 request and `bDeleted` and `score` in `alarmVideo`. | reported (client) |
| The full `alarmType` list the client sent was `md, pir, other, package, io, people, face, vehicle, dog_cat, visitor, cry, crossline, intrude, loitering, legacy, loss, answer`, and `eventAlarmType` added `nonmotorveh`, `tamper`, `lmsg` (and dropped `package`). Results seen: `md`, `people`, `vehicle`. A later capture sent the same two lists in full (272 request fields: `channelId`, `uid`, `logicChnBitmap`, `streamType`, `notSearchVideo`, `startTime`, `endTime`, `alarmType`, `eventAlarmType`) and saw `dog_cat` in replies, so the animal type name in 273 is `dog_cat` (no species-specific name). | observed (client capture) |
| The client opened an event by 123 with `seekTime` equal to the event's `startTime` (no pre-roll), then 5 with the `fileId` from the 273 hit as the file `Id` (the 5 body carries the real `channelId`; 123's stays 0). | observed (client capture) |
| A 272 search covers one channel (one lens, chosen by `streamType`) and one day; the playback tab runs one per channel it shows. There is no cross-channel form. | observed (requests); reported (client) |

These three searches give a playback UI what it needs: a calendar (142), a timeline of
continuous recording (14–16), and event markers (272–274).

### Event log and AI search: commands 516–519, 759, 653

The client's NVR "event log" window and its "AI video search" (search by
type, or by a typed phrase) do not use 272–274. They use the event-log
commands, which search **all chosen channels in one search**. None of them
appeared in the first two captures, so the tables below are reported (client),
read from the client's request builders and reply parsers. A later capture of
the AI search screen on an NVR with recent firmware confirmed part of it; those
facts are in "What the AI search sent on the wire" after this section's
tables, marked observed (client capture). All are viewing tier.

| Command | Use | Body (request → reply) |
| --- | --- | --- |
| 516 | open a search | `findEventLog` → a search `handle` |
| 517 | fetch the next page | `findEventLog` with `handle`, `maxEventCount` → `handle`, `bFinished`, `eventLogList` of `eventLog` |
| 518 | close the search | `handle` |
| 519 | counts per type | `EventLogBrief` with `logType`, `startTime`, `endTime` → `mdCount`, `peopleCount`, `vehicleCount`, `petCount`, `visitorCount`, `packageCount`, `cryCount`, … and `dailyEventCounts` |
| 759 | one hit's detail | `EventLogDesc` with `descId` → `detailId`, a text description, `structInfoList` of `structInfo` (`trackId`, `clipInfo`, `imageName`) |
| 653 | one hit's cover image | `exportCoverPic` with `channelId`, `pts`, `startTime`, `endTime`, apparently sent after a channel extension like the channel queries → `fileSize`, then the image data |

**516 request** (`findEventLog version="1.1"`), the elements the client can
write:

- `logTypeBits`, `notSearchVideo`, `onlySearchCluster`, `desc` (sort
  order), `mode`, `startTime`, `endTime` (time blocks as above);
- `alarmType` (comma list) and `eventAlarmType` with `<i>` items: `md`,
  `pir`, `other`, `package`, `io`, `people`, `face`, `vehicle`, `dog_cat`,
  `visitor`, `cry`, `crossline`, `intrude`, `loitering`, `legacy`, `loss`,
  `answer`, `nonmotorveh`, `IPCInputOutput`;
- `devices` with one `deviceInfo` per channel (`uid` and `logicChnBitmap`,
  or a `channelID`): the channels to search;
- for text search: `encoderVersion`, `similarity`, `textFeature` (a
  base64 feature vector of the typed phrase), `filterList` of `item`
  (`text`, `feature`), `textOfPicture`;
- also `category`, `yoloWorldTypeList`/`subType`, `dangerLevelList`
  (`normal`, `attention`, `potentialDanger`, `danger`) and `trackList`/
  `trackItem`/`trackId` (following one tracked object across cameras).

**Each hit** (`eventLog`) has `uid`, `channelID`, `logicChn`, `fileName`,
`bHasRecFile`, `bDeleted`, `bEncrypted`, `alarmType`, `startTime`,
`endTime` (and `newStartTime`, `newEndTime`), `score`, `pts`, `offset`,
`typeExtension`, `yoloWorldType`, `dangerLevel`, `detailId`, `passid`,
`linkage`, `source`, `linkageType`, and a `trackList`. There are no bounding
boxes in a hit; the closest are `structInfo` entries of the 759 detail.

| Fact | Provenance |
| --- | --- |
| Support is advertised per NVR as a set of bits (event log as a whole, and per type: motion, people, vehicle, animal, package, visitor, cry, crossline, nonmotorized vehicle, tamper, …), most likely the 199 `eventLogVersion`, which only newer NVR firmware sent. The mapping from that element to the bits was not traced. | reported (client); `eventLogVersion` observed |
| The desktop event-log window opens one search for every channel with the selected types ORed into one set, `notSearchVideo` 1 and `desc` 0, then calls 517 until `bFinished`, dropping duplicate hits (same camera, channel, start and end). | reported (client) |
| The client allows **120 s for each 517 page** (25 s for open, close and 519; 30 s in the newer search screen; the desktop values are timeout arguments, presumably seconds), which suggests the device can take long to answer. | reported (client) |
| Text search, as read from the client's code: the typed phrase is turned into a feature vector on the computer by a downloaded text-encoder model, or, when a vendor feature flag is on, by the vendor's server (which can also record search texts), and the vector goes to the NVR in 516 with a `similarity` threshold; on a feature-version conflict (`version`, `text`) the client re-encodes with the other model version. **Superseded on the wire:** the client version captured later sent the typed text itself (`textOfPicture`) with `similarity`, `encoderVersion` 0 and no `textFeature` or `filterList`, and the NVR returned hits, so on that NVR and client the NVR matches the text and no encoder runs on the computer. The vector path may still exist for other firmware or client versions. | reported (client); superseded by observed (client capture) |
| Thumbnails: the newer search screen (the client's "AI video search") fetches one 653 cover per hit with a recording, through a queue that runs **one request at a time**, and its summary card loads the first 12 hits. The older desktop window instead asks 298 (cover preview, sub-stream, one frame over the hit's time range, 16 s timeout) per row. Per-hit, one-at-a-time image fetches are the clearest reason the client's AI search feels slow. | reported (client) |
| The paged search 492–494 (`paginationSearch`: `channel`, `order`, `fileType`, `photoReplay`, `alarmInTypes`, `animalTypes`, `startTime`, `endTime`; items with `recordType`, `animalType`, `thumb`, `Id`, `fileSize`) carries thumbnails and wildlife subtypes; it appears to serve snapshot-based cameras and was not traced further. | reported (client) |
| Nearby commands that are not viewing tier and are never sent: 300 (write `AiCfg`), 343 (write AI detection settings), 765 (set user-defined object labels). The client's "set text feature" step during a text search was not traced to a wire command; it stays off the allowlist until it is. | reported (client) |

#### What the AI search sent on the wire

Observed (client capture), decrypted, from the AI search screen of the
official desktop client on an NVR with recent firmware, one session, one
search typed at a time. Viewing tier.

| Step | Wire |
| --- | --- |
| open | 516 `findEventLog`; the reply is `findEventLog` with a `handle` and `maxEventCount` 50 |
| page | 517 `findEventLog` with `handle` and `maxEventCount` 50; the reply is `eventLogInfo` (`handle`, `bFinished`, `eventLogList`). The client kept one or two page requests in flight and sent the next as soon as a page arrived: page 1 came 0.1 to 1.3 s after the 516, later pages 0.2 to 1.8 s apart |
| end | `bFinished` 1 with an empty `eventLogList`, then 518 with the `handle` (empty reply, status 200) |
| cover | 653 `exportCoverPic` (`channelId`, `pts`, `startTime`, `endTime` of the hit): status 200 with `fileSize`, then media messages (extension `binaryData`, 38,000 bytes of JPEG each), then an empty message with status **201** as the terminator. 12 covers per search, strictly one at a time (the next request about 30 ms after the 201); each was a JPEG of 1920 by 720 and 127 to 214 KB |
| open a hit | not a special command: 14, 15, 16 for the hit's channel and day (the file `Id` comes from 15), 272 to 274 for that range, 123 with `seekTime` = the hit's `startTime`, 5 with that `Id` (the hit's recording had a sub-stream, `supportSub` 1, and that stream was played), 7 at the end. No 759, 519 or 298 |

| Fact | Provenance |
| --- | --- |
| 516 request fields written: `logTypeBits` 1, `notSearchVideo` 1, `onlySearchCluster` 0, `desc` 0, `chnbits` (all channels set), `mode` 2, `encoderVersion` 0, `textOfPicture` (the typed text), `similarity` 43, `category` (empty), `startTime` (seven days back, 00:00:00), `endTime` (today, 23:59:59), and an empty `devices` element. | observed (client capture) |
| The text goes to the NVR as plain text and the NVR does the matching: no `textFeature` vector and no `filterList` were sent, and nothing ran on the computer. A different phrase is only a different `textOfPicture`; the UI's "animal" type was not a request field. | observed (client capture) |
| Each `eventLog` hit had exactly `channelID`, `bHasRecFile`, `bEncrypted`, `yoloWorldType`, `score`, `pts`, `offset`, `startTime`, `endTime`: no `uid`, `fileName`, `alarmType` or bounding box. `yoloWorldType` values seen: `animal`, `person`, `motor vehicle` (a different vocabulary from the 272 `alarmType` names). `score` ran from the request's `similarity` (43) upward to about 60, so `similarity` acts as the cut-off. | observed (client capture) |
| One phrase returned hundreds of hits over 14 pages (plus an empty finishing page), sorted by time, mostly of types other than the one the phrase named: the search is a similarity ranking over all detected objects with a low threshold, not a class filter. | observed (client capture) |
| `pts` is in the recording's frame-timestamp clock (microseconds); for the hit that was opened, `pts` lay a few seconds after the first keyframe of its play, and `offset` was 19 s. That `pts` and `offset` locate the best-matching frame inside the hit is an inference. | observed (client capture); meaning inferred |
| A 517 request can be answered with status **442** and an empty body, 0.3 s after the request. It happened on the second page of two searches, for two particular phrases, while other phrases completed; both 516 requests had been answered 200 with a handle and page 1 had returned a few low-score hits. No timeout or other status appeared. After the 442 the client sent 518 (once, or twice in quick succession) and showed an error; its Retry button opened a fresh 516 with the previous successful phrase, not the failed one, and that search completed. What 442 means is unknown. | observed (client capture); meaning unknown |
| A whole successful search, from the 516 to the 518, took about 17 s. | observed (client capture) |

### Playback

The client's playback commands. 5, 7, 123, 143, 144, 298 and 382 were
observed (client capture), and 13 on older firmware (see "Plays on older
firmware"); the rest are reported (client) and did not appear.

| Command | Use | Body | Provenance |
| --- | --- | --- | --- |
| 5 / 7 | play a recording file / stop | 5: `FileInfo` with `channelId`, `Id`, `uid`, `supportSub` (0 for a main-stream play, 1 for a sub-stream play), `playSpeed` (1, 2, 4, 8 or 16), `streamType`; no `startTime` was sent. 7: `FileInfo` with `channelId`, `name`, `streamType` | observed |
| 123 | set the start position of the next play | `ReplaySeek` with `channelId`, `seq`, `seekTime` (a time block) | observed |
| 13 | one file's details by `name`; on older firmware, sent before every play, which goes out on its message ID | `FileInfo` with `channelId`, `name`, `supportSub`, `playSpeed`, `streamType` (see "Plays on older firmware") | observed (client capture) |
| 298 | cover images for the timeline | `CoverPreview` with `channelId`, `uid`, `logicChnBitmap`, `desc` (0), `streamType`, `startTime`, `endTime`, `frameList/frameNo` (1) | observed (client capture) |
| 382 | stop a play by time range | empty; sent once per NVR session just before the logout when the client quit | observed (client capture) |
| 150 | keyframe-only playback | not traced | reported (client) |
| 381 / 383 | play by time range / seek (newer form) | `ReplayByTimeV2` with `startTime`, `endTime`, `playSpeed`, `streamType`, `logicChnBitmap`, offsets, `durationList` | reported (client); not used for playback in the capture |
| 8 / 9 | download a whole file by name / stop | `FileInfo` with `uid`, `Id`, `name`, `channelId`, `supportSub`, `streamType` | reported (client); not used |
| 143 / 144 | download a time range / stop | below | observed |
| 476; 492–494 | paged download; paged search open, next, close | not traced | reported (client) |
| 303 | **delete recording files** | configuration tier: never sent | reported (client) |

**How the official client played a recording** (observed (client capture)):

1. Stop the channel's live view (4) and run the searches above.
2. Send **123** `ReplaySeek` with a new `seq` and the wanted `seekTime`; it is
   answered empty, status 200. The `seq` values were increasing 32-bit numbers
   chosen by the client, close to the current Unix time. `channelId` was 0,
   although the recording belonged to another channel.
3. Send **5** about 0.5 s later, naming the recording file by `Id` (from
   command 15). The first reply has status 200, a `binaryData` extension, and
   a 32-byte payload holding a stream-info record (`1001`).
4. Media follows as command-5 messages with the play request's message ID and
   the 123 `seq` in header bytes 16–19 (see "Replay media headers"). Every
   media message has an extension: `binaryData` plus `encryptLen` where a
   record starts, `binaryData` alone on continuation chunks. The payload is
   BCMEDIA, as in live view (H.264 and AAC in `05wb` records here).
5. The first keyframe's wall-clock seconds (video extra header) matched
   `seekTime` exactly in five plays and lay 1 s before it in two, with a GOP
   of about 1.1 s: play starts at the keyframe at or before `seekTime`.
   (Superseded: measured later, on devices whose local time is not UTC, the
   seconds are UTC and the first keyframe is at or up to 1 s **after** the
   seek time converted to UTC; see "Rates, keyframes, and how a play ends"
   below. Why this early capture read differently is not explained.)
6. Pacing: about 2 to 4.4 s of video arrived within the first 0.25 s, then
   roughly real time (about 1.1 s of video per second). Whether the device or
   TCP flow control set that pace was not determined.
7. Scrubbing was **7** (stop, answered empty, status 200), then a new 123 with
   a new `seq` and `seekTime`, then a new 5. Seven plays were made this way;
   none ran to the end of a file, so how a play ends is unknown.

**How nvrview plays (ours, from the facts above).** 123, 5 and 7 are on the
allowlist, viewing tier: they position and stream a replay on our own
session and stop it, and change nothing on the device (the observed
client's playback used exactly these three). A play is the observed
sequence: 123 with a `seq` and the start time, its empty status-200 reply,
then 5 naming the file by `Id`, with `playSpeed` 1, 2, 4, 8 or 16 (the
observed values; no other is sent); 7 on every exit path, naming the file. `seq` numbers increase
from the current Unix time, as the client's did; 123's `channelId` is 0, as
the client sent it. With no observation of how a play ends, `bc-session`
takes an empty message as the end; a non-200 reply without the play's `seq`
is a refusal (the probe's one reported refusal turned out to be a misread
media header, see "Live view and plays on one session"; the one refusal
seen since is older firmware's status 400 with an empty body, see "Plays on
older firmware"); `supportSub` is 1 on a sub-stream play and 0 on
a main-stream play, as the client sent it. To start several plays together
nvrview follows the client's recipe (see "Four plays at once"): all seeks
back to back with consecutive `seq` numbers and one `seekTime`, a pause of
about 0.5 s, then all plays back to back (`DeviceSession::play_group`). The
number of plays per device is capped by `max_plays` (default 4, see "Live
view and plays on one session"). Not sent: 150, 381–383 (not observed in
use), and any `playSpeed` other than the five observed. On a device whose
file search lists no `Id` (older firmware) every play takes the by-name
form instead (see "Plays on older firmware").

#### Speed, file boundaries, and concurrent plays

A second capture of the official client's playback tab added the facts below.
It began after the sessions had logged in (live view was already open), so its
session key could not be recovered and **XML field values and BCMEDIA record
headers in it could not be decrypted**: everything here is read from the
unencrypted message header (command, message ID, status, body length, payload
offset), timing, and byte volume only. Field-level detail (the `playSpeed`
value, frame rates, keyframe counts) is therefore still unconfirmed. All
observed (client capture).

| Fact | Provenance |
| --- | --- |
| **Changing speed is stop, seek, play**, the same three commands as scrubbing: every rate change (1×, 2×, 4×, 8×, 16× were exercised) was a 7, then a 123 with a new `seq` and `seekTime`, then a 5. No dedicated speed command, no keyframe-only command (150), and no play-by-time (381/383) appeared. The rate rides in the 5 body's `playSpeed`; the play request grew by one byte at two-digit speeds, consistent with that. | observed (client capture) |
| **Faster rates deliver far less than N× the data.** Per play, the command-5 media delivered (D→C bytes over the play's wall time): 1× about 1.0 MB/s, 2× about 1.2, 4× about 2.5, 8× about 2.5, 16× about 3.0. The rate rises then plateaus at roughly 3× the 1× byte rate, not 16×, so the device serves a thinned (keyframe-biased) stream at higher speeds rather than every frame faster. Which frames are kept could not be read there (records encrypted); a later decrypted capture answered it (next subsection: keyframes only, a stride that keeps the media clock at the requested rate). | observed (client capture) |
| **Each play starts with a burst, then paces.** A few MB arrived in the first ~0.5 s (about 3 MB at 1×, up to ~8 MB at 4×–8×), then media came in roughly 2 s chunks about 2 s apart — the same burst-then-pace shape as a single-speed play, at every rate. | observed (client capture) |
| **A play crosses a recording-file boundary with no visible event.** One 1× play ran for about 180 s (one command-5 stream, one status-200 reply then only media) spanning a file boundary, with no stop/seek/replay from the client, no status change, no empty message, and no stall beyond the normal ~2 s pacing. So the device continues across files within a single play; the boundary is not marked on the wire. (The boundary time inside the stream could not be read.) | observed (client capture) |
| **One session carried several plays at once.** Up to four simultaneous command-5 plays ran on the one session, each with its own message ID (two started together, and four within 40 ms), all answered status 200 with no refusal, demultiplexed by message ID like live streams. The synchronized multi-view plays were low bit rate (about 0.05 MB/s each, media every ~4 s) against about 1 MB/s for a single main-stream play, so the client plays the **sub-stream** for synchronized views. No concurrency limit was reached at four. | observed (client capture) |
| **The client stops live view when the playback tab opens.** Live media (command 3) flowed only before playback began; once the playback tab was open, no command-3 media arrived on either NVR for the rest of the capture (the client sent a stop (4) for each live stream). So this capture shows concurrent *replay* streams but not live and replay media together; whether one session can carry both at once is still unmeasured (the download step was not exercised here). | observed (client capture) |
| **No error status appeared.** Across the whole capture every seek (123) and play (5) was answered status 200 (the only non-200 statuses were the 300 that commands 146 and 199 use for large replies). A seek to a time with no recording produced no error reply; the client decides "no recording" from the search results (142/14–16/272–274) and does not start a play. How a play or seek reports an error is therefore still unknown. | observed (client capture) |

#### Rates, keyframes, and how a play ends

Observed (client capture), from a later capture of the official client that
included the login, so every body and BCMEDIA record could be decrypted. One
session, one NVR on recent firmware, one play at a time, a 4K main stream
recorded at 25 frames per second with a 2.0 s GOP (one keyframe in 50 frames).

| Fact | Provenance |
| --- | --- |
| `playSpeed` in the 5 body took exactly the values 1, 2, 4, 8 and 16 (decimal integers). Every rate change was 7, then 123 with a new `seq` and `seekTime`, then 5; no 150, 381 or 383. | observed (client capture) |
| **At `playSpeed` 1 a play carries every video frame and the audio records. At 2 and above it carries video keyframes only and no audio records.** | observed (client capture) |
| Which keyframes are kept: every keyframe at 2× and 4× (timestamps 2.0 s apart), every second one at 8× (4.0 s apart), every fourth at 16× (8.0 s apart). So the media clock (frame timestamps against wall time) advanced at the requested rate: 1.0, 2.0, 4.0, 8.0, 16.0 media seconds per wall second. | observed (client capture) |
| The device delivered at most about two keyframes per wall second: 1 per second at 2×, 2 per second at 4×, 8× and 16×. A 4K keyframe was about 1 MB, so the data rate plateaued near 2 MB/s from 4× up (about 1 MB/s at 1× and 2×). | observed (client capture) |
| Each play opened with a burst, then paced: about 4 s of video within the first second at 1× (6 s in two plays), and 4, 8, 10 and 9 keyframes at 2×, 4×, 8× and 16×; after that the arrival rate was constant, in groups about every 1 s (2× to 16×) or 2 s (1×). At 1× the delivery then stayed about 4 to 5 s ahead of real time for the whole play. The first picture arrived 0.2 to 0.45 s after the 5, in the same instant as its 200 reply. | observed (client capture) |
| A sub-stream play at 1× on the same NVR had 60 ms frame spacing (16.7 frames per second), a GOP of 3.84 s, about 0.09 MB/s, and messages about every 4 s. | observed (client capture) |
| **Keyframe seconds are UTC.** The wall-clock seconds in a keyframe's extra header are Unix epoch (UTC) in both live view and replay (a live keyframe's seconds equaled the packet's arrival time in UTC to within a second). `seekTime` and all search times are device local. In every play the first keyframe was at the seek time converted to UTC, or up to 1 s after it, on a device not on UTC; this reproduces nvrview's own probe measurement. The earlier capture's "equal to `seekTime`" reading is not reproduced. | observed (client capture) |
| **No play was ended by the device.** All plays ended with the client's own 7 (empty reply, status 200). In each, the last device message was an ordinary media message: no empty message, no status 300 or 201, no other command. The client stopped when its playhead reached the end of the file list it had last loaded (a list ends at the time of its search, see command 15), though the device had data past that point and the recording kept growing. Re-searching, then playing from the old end, ran until the new list end. A play running to a real end of data was not observed, so what the device sends then is unknown. | observed (client capture); end-of-data unknown |
| 5, 123 and 7 were answered status 200 every time. Other non-200 statuses in that capture were 400 on 44 and 318 and a few others for channels without a feature (empty body), 300 for large replies of 146 and 199, 442 on one 517 (see above) and 201 ending each 653. No 143, 144, 106, 38 or live view occurred after the playback tab opened. | observed (client capture) |
| Unsolicited messages on the event subscription's message ID besides 33: 600 `yoloWorldEventList` with a `range` (`start`, `end`, 0 to 23; probably the supported hit-type slots), and once at login 145, 291, 623, 677, 723 and 727 (not decoded further). | observed (client capture) |

#### Four plays at once: the official client's recipe

Observed (client capture), from a decrypted capture of the official client
on recent NVR firmware: one session, one NVR, a 2x2 playback grid. Live view
ran only before the playback tab opened; every play in it is a sub-stream
play (no main-stream play occurred, so the main-stream facts above are not
contradicted, only not tested here).

| Fact | Provenance |
| --- | --- |
| Four simultaneous plays on one session, all sub-stream, with no live view, were all served at `playSpeed` 1 and at `playSpeed` 4, with no refusal, no non-200 status and no limit field in any reply. A fifth play was not tried. | observed (client capture) |
| **The client starts N plays as:** 7 for each running play; then N seeks (123, consecutive `seq`, the SAME `seekTime`, `channelId` 0) sent back to back (within 10 to 50 ms) without waiting for replies; about 0.5 to 0.6 s later N plays (5, one per camera, each with its own message ID, the same `playSpeed`) sent within about 25 ms. The seek count equals the play count. A seek of all playing cameras and a speed change are the same sequence again (stop all, seek all, play all). | observed (client capture) |
| A sub-stream play's 5 carries `supportSub` 1 and the sub-stream file `Id` (first number 1) that contains the seek time; the file search for it used `streamType` subStream. A main-stream play's 5 carried `supportSub` 0 (earlier captures). | observed (client capture) |
| All media messages of all plays carry the `seq` of the latest seek the device has processed (the last of a batch of seeks), not the `seq` of their own play; the first seeks' `seq` numbers never appear on any media. Routing is by the 5's message ID. A 123 sent while plays run re-tags their media but does not move them. | observed (client capture) |
| In a four-up start the first picture arrived 0.3 to 0.5 s after the 5 for the first play and 0.8 to 1.2 s for the others; at 4x 0.65 to 0.7 s for all four. Four sub-stream plays at 1x ran 4 to 9 s ahead of real time with media about every 4 s (0.03 to 0.2 MB/s each). | observed (client capture) |
| **A sub-stream play at `playSpeed` 4 kept every video frame and the audio records** (frame spacing and keyframe ratio unchanged, 4 s GOP) and advanced the media clock at 4.0 s per wall second, 0.18 to 0.5 MB/s per play. (A 4K main stream at 4x kept keyframes only and no audio.) Whether a sub-stream thins at 8x or 16x was not tested. | observed (client capture) |
| The first keyframe of a sub-stream play lay within about 4 s of the seek time (up to 2 s before it occurred), because the GOP is 3.4 to 8 s. | observed (client capture) |
| 5, 7 and 123 were answered status 200 in every batch of four. | observed (client capture) |

Differences from nvrview's earlier probe, which stalled or starved its
fourth play: it sent `supportSub` 0 on sub-stream plays, and interleaved
seek, play, seek, play with a different `seekTime` per play, at different
times. Which of these matters (or whether the live preview alone does) is
settled only by the runs below.

#### Live view and plays on one session

Measured with nvrview's own read-only client (`bc-cli replay-probe`) on one
NVR with recent firmware: one session, one live preview of a camera's
sub-stream, then plays at `playSpeed` 1 of finished sub-stream recordings
beside it. Only the file search (14–16), preview (3, 4), seek (123), play (5)
and stop (7) were sent. A first run stopped after one play because the
probe's stall threshold was too strict for the burst below (since fixed). Three
further runs each did two stages: one play beside live view, then a second
stage with a second play; in the first of them the second play was the same
file on the same camera, in the other two it was another camera's recording.
All observed.

**Correction.** An earlier version of this section said the second play of
the same file was "refused with status 52092". That was our own probe
misreading a message, not a device refusal. In every run, the play that the
probe reported as failed was the **restart of the file it had stopped moments
earlier** (stage 2 stops stage 1's play and starts it again), while the
concurrent play (the same file in one run, another camera's file in the other
two) was accepted and delivered media. The reported "status" varied from run
to run (52092, 58201, 58472) and matched the low 16 bits of a number near the
session's replay `seq` base, that is, bytes 16–17 of a replay media header
(see "Replay media headers"), not a device status. A fifth run logged the
whole message: it was **media** (a 663-byte message with a media extension)
on the first play's message ID whose header carried the `seq` of the
**second** play's seek, sent just after the first play started. So the
device tagged the running play's media with the latest seek's `seq`, and the
probe, which then accepted only the play's own `seq`, took it for a
refusal. (Every seek carried `channelId` 0, as the official client sends it;
whether a seek with the play's own channel would keep the tags apart was not
tried.) nvrview now routes a play's media by message ID and accepts the
`seq` of any recent seek. The measurements below are observed.

| Fact | Provenance |
| --- | --- |
| **One session carried live view and a play at once.** With the live preview running, the 123 seek and the 5 play were accepted and the play's media arrived beside the live media on the same connection, told apart by message ID; nothing was refused and the connection stayed up. | observed |
| **At least two simultaneous plays beside live view delivered media**, in all three later runs (in one run both on the same camera, the same file as the restarted play; in two runs a second camera). The device refused nothing: no non-200 answer to a 123 or 5 was seen. The first play of each such stage was cut short by the probe on its first media message, which carried the second seek's `seq` (above), so its delivery was not measured. More than two plays beside live view, and downloads beside live view, were not measured. | observed |
| **Two plays beside live view delivered fully; a third did not, silently.** After the session began routing play media by message ID, a run of 1, 2 and 3 simultaneous plays at `playSpeed` 1 (sub-stream recordings of up to three cameras, beside one live sub-stream preview) gave: one play, normal; two plays (two cameras), both delivered their whole window (about 12 s of video in 8 s) and live view held its rate; three plays, one delivered normally, one stopped after its first burst (about 4 s of video), and one delivered nothing at all. No play was refused, no non-200 status came, and the session stayed up; every play was stopped with 7. Live view kept its rate throughout. Whether the limit counts plays per session or per device, and whether main-stream plays lower it, was not measured. | observed |
| **With no live view, three simultaneous plays delivered fully and a fourth stage did not, silently.** A run of `bc-cli replay-probe --no-live` (no live preview at all; one session; sub-stream recordings of three cameras and an earlier recording of the first, stages of 1, 2, 3 and 4 plays at `playSpeed` 1, 8 s windows) gave: one, two and three simultaneous plays all delivered their whole window (about 12 s of video in 8 s each; one play of the three-play stage, with a 20 s burst, was ahead). In the four-play stage one play delivered normally, one delivered only about 4 s of video in 8 s (stalled after its burst), one delivered nothing at all, and one delivered normally. No play was refused, no non-200 status came, nothing was logged at warning level, and the session stayed up; every play was stopped with 7. Beside live view the same NVR served two plays and degraded the third (previous row), so, as an inference from two runs (not a controlled comparison), the live preview takes one of about four serving slots: the review-mode cap is three plays per device with live view off, two with it on. Whether it counts plays per session or per device was not measured. | observed |
| **A running play's media can carry a later seek's `seq`.** With two plays started back to back on one session (seek, play, seek, play, every seek with `channelId` 0), the first play's media arrived on its own message ID tagged with the second seek's `seq`. | observed |
| **Live view kept its rate** in all runs: the live sub-stream ran at 9.8 to 10.0 frames per second alone and 10.0 to 10.1 while one play ran, with no gap of a whole second; one second during a play's burst carried about half the usual frames and a neighboring second made up for it (for example 5 then 15). In the two-play stages it ran at 11 to 13 frames per second over a window of about a second, which is within the second-to-second spread. | observed |
| **The burst can be long.** A play's first picture came 0.29 to 0.36 s after the request, and within the first second the device sent 8 s of video (80 frames at 10 frames per second, about 235 KB of sub-stream; 11.9 s of video in 7.7 s over the stage). Then nothing came for the 4 s the probe waited, consistent with the play running 8 s ahead of real time; whether it resumed at real time was not observed. The client capture's 1× bursts held 2 to 4.4 s of video (about 4 s on a main stream in the later one). | observed |
| **Keyframe wall clock in UTC, recording times local.** The live keyframes' wall-clock seconds matched the host's UTC clock within a second, and in every run the first play's first keyframe read `seekTime` (device local, like every search time) plus the device's UTC offset, less 1 s (the second play in the same runs read plus 1 s): the seconds in the extra header are UTC on this device, and the keyframe is within about a second of the seek time. The official client's decrypted capture agrees (see "Rates, keyframes, and how a play ends"). | observed || **`supportSub` 1 on the sub-stream plays did not lift the cap** (eighth run, `--no-live`, interleaved seek and play as before, sub-stream recordings of three cameras and an earlier recording of the first, stages of 1 to 4 plays). One, two and three plays delivered their whole window; in the four-play stage one play delivered about 4 s of video in 8 s (stalled after its burst), one delivered nothing, and two delivered normally: the same pattern as the seventh run without `supportSub`. No refusal, no non-200 status. | observed |
| **Seeking all plays together at one time, then playing all, served three plays cleanly** (ninth run: `supportSub` 1, `--no-live`, `--batched`: all seeks back to back at one `seekTime`, about 0.5 s pause, all plays back to back). Stages of one, two and three plays on recordings that all held the seek time delivered fully (the three-play stage: 16.0, 11.9 and 16.0 s of video in 7.5 s, first pictures 0.46 to 0.47 s after the plays, against 0.15 to 0.72 s interleaved), no refusal. The fourth stage was **not a test of four at once**: its fourth recording was an earlier file of the first camera, which shares no time with the other three, so the probe sent each seek at its own recording's start plus 5 s (the last being that earlier file's time). Only the play of that earlier file delivered; the three plays of the later files delivered nothing at all, silently, with no refusal. | observed |
| **Inference (not proven):** the device keeps one seek position per session and a play starts from the position that is current when its 5 is processed; a play whose file does not hold that time gets no media and no error. That fits the ninth run's fourth stage, the official client's use of one `seekTime` for all plays of a batch (each play's file is chosen to hold it), and the earlier tags (every play's media carry the latest seek's `seq`). Whether four plays on four recordings that all hold one seek time are served (with or without live view) was **not measured**; the next probe needs four recordings with a common time (four cameras, `--plays 0,1,2,3` style) and `--batched`. | inference |
| **Four plays at one seek time, all delivered** (tenth run: `supportSub` 1, `--no-live`, `--batched`, four cameras' recordings that all held the seek time). Stages of one to four plays all delivered fully; with four, each play gave 11.9 to 16.0 s of video in 7.2 s, first pictures 0.75 to 0.79 s after the plays, no stall, no refusal. This supports the inference above: the earlier "third/fourth play stalls" came from plays whose recordings did not hold the session's (latest) seek position, not from a play limit. | observed |
| **Four plays at one seek time beside live view, all delivered** (eleventh run: as the tenth, with one live sub-stream preview running). Every stage up to four plays delivered fully (11.9 to 16.0 s of video in 7.3 s each with four), first pictures 0.72 to 0.74 s after the plays, and live view held its rate (10.0 frames per second against 9.8 alone, 9.5 after). So the "two plays beside live view" of the sixth run was the same seek-position effect (its third play's recording did not hold the seek time), not a limit. | observed |

#### Plays on older firmware

Everything above was measured on NVRs with recent firmware. On an NVR with
older firmware nvrview's play was refused: every 5 was answered at once
with status **400** and an empty body (observed with nvrview's own client:
its log showed each play of that NVR's session ending on such a reply right
after the plays started; plays on a recent-firmware NVR worked). A decrypted
capture of the official client on that NVR (login included, one session,
nothing lost) then showed how that firmware plays: one camera at a time,
main stream at 1x, sub-stream at 1x, then 2x, 4x, 8x and 16x, a switch to
the main stream at 16x, another camera at 1x, a short download, and the
quit. All observed (client capture) unless marked otherwise.

| Fact | Provenance |
| --- | --- |
| **The file search lists no `Id`.** The 15 reply's `FileInfo` entries carry `channelId`, `handle`, `name`, `streamType`, `containsAudio`, `recordType`, `sizeL`, `sizeH`, `supportSub`, `encrypt`, `startTime` and `endTime`, but no `Id` (recent firmware lists `name` and `Id`). A sub-stream search (`streamType` subStream) lists the sub-stream's own files, whose `name` differs from the main stream's file of the same time. The 14, 15, 16 and 142 bodies the client sent match recent firmware's. | observed (client capture) |
| **A play is 123, then 13, then 5 on the 13's message ID.** Every play (nine, at every speed and both tiers): 123 `ReplaySeek` (`channelId` 0, `seq`, `seekTime`; the same body as on recent firmware), answered empty, status 200, within 35 ms; 0.53 to 0.56 s later a **13** (`FileInfoList`/`FileInfo` with `channelId`, `name`, `supportSub`, `playSpeed`, `streamType`, in that order), answered status 200 within 25 to 65 ms with one `FileInfo`: `channelId`, `handle` 0, `name`, `containsAudio`, `recordType`, `supportSub`, `sizeL`, `sizeH`, `startTime`, `endTime` of that file; then, 15 to 30 ms after that answer, the **5 with the same message ID as the 13**. | observed (client capture) |
| **The 5 names no file.** Its `FileInfo` holds `channelId`, `supportSub`, `streamType`, `startTime` and `playSpeed`, in that order; no `Id`, `uid` or `name`. `startTime` is the **file's** start time (as the search and the 13 answer gave it), not the seek time. `supportSub` is 0 on a main-stream play and 1 on a sub-stream play, in both the 13 and the 5 (the rule recent firmware's 5 follows); `playSpeed` took 1, 2, 4, 8 and 16. | observed (client capture) |
| The play's replies match recent firmware's: a first reply with status 200, a `binaryData` extension and a 32-byte stream-info payload, 40 to 140 ms after the 5, then media on the 5's message ID whose header bytes 16–19 carry the seek's `seq`. No play was refused or ended by the device; each ended with the client's 7. | observed (client capture) |
| **The stop (7) is the same** as on recent firmware: `FileInfo` with `channelId`, `name` and `streamType`, its own message ID, answered empty with status 200. | observed (client capture) |
| **Speed, tier and camera changes** are 7, 123, 13, 5 again with the new `playSpeed`, stream (the 13 naming that tier's file) or camera; nothing else is sent. | observed (client capture) |
| Data per play (bytes only; frames were not decoded): main stream 1x about 0.6 MB/s; sub-stream 1x 0.09, 2x 0.08, 4x 0.18, 8x 0.34, and 16x 0.07 MB/s (34 messages in 12 s, consistent with a thinned stream); main stream 16x about 0.6 MB/s. | observed (client capture) |
| **The download is the same as recent firmware's** (and as nvrview's for a first lens): 143 with `logicChnBitmap` 255, `channelId`, `supportSub` 0, `streamType` mainStream, `startTime`, `endTime` and no `uid`; a first reply (status 200) with `sizeL`, `sizeH` and `FileCount` 1; media; an empty status-300 end about 1 s later for a 16 s window. The client then sent 144 (empty) on a new message ID, answered 200. | observed (client capture) |
| 298 cover requests had no `uid`; each was answered empty with status 200, media, then an empty status-300 end, as on recent firmware. | observed (client capture) |
| 382 (sent at quit, as on recent firmware) was answered **405** with an empty body. | observed (client capture) |
| No 150, 381 or 383, and no other non-200 status on 5, 7, 13 or 123. | observed (client capture) |

**How nvrview chooses the form.** The device tells it in its own file
search: a file listed with an `Id` is played as on recent firmware (5 naming
the `Id`); a file listed without one is played by name (13 naming the file,
its 200 answer, then 5 on the 13's message ID naming the file's start time),
per file and so per device, with no model or version table. 13 is on the
allowlist, viewing tier: it reads one file's details and changes nothing. A
13 answered with another status ends the start as a refusal, and no 5 is
sent. Not verified on that hardware with nvrview's own client yet (the
mock's older mode reproduces the observed answers: `tools/mock-nvr
--older-playback`).

### Cover images: command 298

Observed (client capture). While a download ran, the client asked for one
cover image for each of four consecutive time ranges of the day (the first
range spanned the first recording file; together they covered the day up to
the current time). Each request got: an empty status-200 reply; then media
messages with the request's message ID, one `1001` stream-info record and one
keyframe (about 260 KB from a main stream), with `EncryptLen` 1024 on the
keyframe's first message; then an empty message with status **300** that
ends the reply. Viewing tier.

### Download by time range: command 143

Named "download cut" in the client, with 144 as its stop. Observed on an NVR (firmware build family 2025) by a small read-only
client that sent only login and this command, and verified by decoding the
result. The official client was later captured downloading a one-minute clip
of a recording it had just played: it uses 143 with exactly this body
(observed (client capture)).

Request, class `0x1464`, payload offset 0, body encrypted like any other:

```xml
<FileInfoList version="1.1">
  <FileInfo>
    <uid>UID-EXAMPLE</uid>
    <logicChnBitmap>255</logicChnBitmap>
    <channelId>N</channelId>
    <supportSub>0</supportSub>
    <streamType>mainStream</streamType>
    <startTime>…</startTime>
    <endTime>…</endTime>
  </FileInfo>
</FileInfoList>
```

| Fact | Provenance |
| --- | --- |
| Every reply is a command-143 message carrying the request's message ID. | observed |
| First comes a control reply (payload offset 0) whose encrypted body is a `FileInfoList` with `sizeL` (and `sizeH`): the device's estimate of the window's size in bytes. The official client's download reply also carried `FileCount` (1). The estimate was within 3 % of the bytes that followed. | observed |
| Then media messages: an extension (payload offset non-zero) and a payload of BCMEDIA records, exactly as in live view. For our own client `encryptLen` was 0 on every media message; for the official client it was set on every record start (see "Encryption after login"). | observed |
| The download ends with a command-143 message with an empty body. In the official client's download that message had status **300**; the status in our own client's downloads was not noted. | observed |
| The official client sent 144 (empty body) about 0.2 s after that end message anyway, answered empty, status 200. | observed (client capture) |
| A one-minute main-stream window (H.264, about 20 MB) arrived as 60.4 s of video, starting at the requested second, in about 1.5 s: downloads are not paced to real time. Meanwhile the client ran searches and cover requests (298) on the same session. | observed (client capture) |
| Other messages (alarm and status pushes) arrive on the same session during a download and must be skipped by command number and message ID. | observed |
| `streamType` alone selects the lens and tier: `mainStream`, `subStream`, and on a multi-lens camera `externStream` (second lens, high) and `mobileStream` (second lens, low). Changing `channelId` or `logicChnBitmap`, or adding logical-channel elements, never selected the second lens. `uid` is optional for the first lens; it was sent with the camera's ID for the second lens, as the official client does in its searches. | observed |
| The cut is aligned to the recording's keyframes: a 5 s window came back as 6–8 s of video (one whole GOP of 6 s on a main stream, 8 s on a sub-stream). Video timestamps (µs) are the frame times; the advertised frame rate is not. | observed |
| Audio records can precede the window's first picture by whole fragments (up to about 12 s was seen), and audio has no timestamp, so a downloader must align audio to video itself (for example, drop audio ahead of the first keyframe). | observed |
| A window with no recording ends normally (empty 143) without a single video record. | observed |
| Second-lens streams carry audio as `15wb`; any `..wb` record is audio. | observed |
| Cost, for a 5 s window: about 1.5 MB read for a high-resolution H.265 main stream and 0.42 MB for its H.264 sub-stream, about 2 s per fetch including login. One session served several such downloads one after another. | observed |
| A download can be stopped early with command 144 (empty body). | reported (client); 144 itself observed |
| Other projects report command 13 for downloads and 14–16 as a download path; in the client, 14–16 are the file search above, and 13 reads one file's details before a play on older firmware (see "Plays on older firmware"); it was not seen in a download. | reported; 13 observed (client capture) |

**Why not the HTTP API.** The same NVR's HTTP API was measured as a
download path and rejected: its file download always streams a whole
recording file from the first byte (no `Range`, and the `start`/`seek`
parameters other clients send were accepted and ignored), at about 800 kB/s,
with files packed in 60-minute units; its server-side cut returned a name
that no request could retrieve; its search answers a window longer than a
day with a per-day bitmap instead of files, and a two-hour window took
minutes. Its token logins also count against the device's session limit:
six logins without a logout made the device refuse new connections. RTSP
serves live view only, and ONVIF offered no replay service. (observed)

## LAN discovery

How the official desktop client finds devices on the local network ("search
LAN for devices"). All facts below are **observed (client capture)**: a
capture of the official desktop client searching twice, on a LAN with NVRs
and cameras of several product classes, unless marked otherwise. Discovery uses UDP only and no login.

Two exchanges were on the wire. In the client capture only the first one was
answered; our own probes later drew answers to both (see "Our own search").

### Search request and device record (UDP 2000 → UDP 3000)

| Fact | Provenance |
| --- | --- |
| The request is the 4-byte UDP payload `aa aa 00 00`, sent to the subnet's broadcast address, destination port **2000**, from an ephemeral source port. The client sent each request twice back to back, and repeated the pair about every 11 s while its search screen was open. | observed (client capture) |
| Every device answers each request copy with one 388-byte UDP datagram, unicast to the requester's address at destination port **3000**, whatever the request's source port was (the client had its own socket on port 3000). Replies arrived within about 10–100 ms. | observed (client capture) |
| A reply's source port is 2000 on some devices and an ephemeral port (fixed per device across the capture) on others, NVRs included. | observed (client capture) |
| A reply carries no transaction ID, magic, or checksum, and repeats byte for byte for the same device; it cannot be matched to a request except by timing. Duplicates (two per request pair) are normal. | observed (client capture) |
| NVRs send the same 4-byte request themselves, from source port 3000 to the limited broadcast address (255.255.255.255) port 2000, about every 2 s, and other hosts on the network send it too: a listener on port 2000 sees other searchers, not device announcements. | observed (client capture) |

The 388-byte record. Strings are ASCII, NUL-terminated and NUL-padded within
the field; integers little-endian. Field sizes are the distance to the next
field seen in use, so a longer string than any observed may be allowed.
Every byte not listed was zero in every reply.

| Offset | Size | Content | Provenance |
| --- | --- | --- | --- |
| 0 | 50 | zero | observed (client capture) |
| 50 | 1 | 1 on every camera, 2 on every NVR: device kind | observed (client capture) |
| 51 | 1 | small integer, differing by product class (cameras, doorbells, NVRs); meaning unknown | observed (client capture) |
| 52 | 1 | 1 on cameras; a larger value on NVRs; meaning unknown | observed (client capture) |
| 54 | 1 | 1 on cameras; 16 on NVRs, matching the channel count of the 16-channel NVR the rest of this document describes: channel count | observed (client capture) |
| 57 | 1 | 1 on every device; meaning unknown | observed (client capture) |
| 80 | 6 | the MAC address, binary | observed (client capture) |
| 104 | 4 | `aa aa 00 00`, the request's bytes | observed (client capture) |
| 108 | 20 | the device's IPv4 address, dotted decimal | observed (client capture) |
| 128 | 2 | u16: the device's TCP port for this protocol (9000 on every device), followed by 2 zero bytes | observed (client capture) |
| 132 | 32 | the device name the user gave it | observed (client capture) |
| 164 | 64 | the MAC address as text, `xx:xx:xx:xx:xx:xx`; upper case on cameras, lower case on NVRs | observed (client capture) |
| 228 | 32 | the device UID, 16 characters; empty on one camera | observed (client capture) |
| 260 | 32 | a short lower-case product-class tag with underscores; set on some camera classes, empty on others and on NVRs | observed (client capture) |
| 292 | 96 | on NVRs, the MAC address as 12 upper-case hex digits without separators; empty on cameras | observed (client capture) |

Not in the record: the model, firmware version, HTTP, HTTPS, RTSP, or ONVIF
ports, and any subnet mask or gateway. Those come from the session after
login ("Device information"). Whether the record is identical for a device
on firmware other than the families seen here is unknown.

### Second probe (UDP 2015)

In the same search, the client also sent a second kind of probe. No device
answered it in the client capture (no other UDP reached the client); a
battery-powered camera answered our own (see "Our own search").

| Fact | Provenance |
| --- | --- |
| Destination UDP port **2015**; 73-byte payload; to the subnet broadcast address (and, in the first search, also to 255.255.255.255), each copy from its own ephemeral source port. | observed (client capture) |
| A burst of 21 probes about 25 ms apart per search. Each probe went out two or three times with identical bytes; consecutive probes differed only in the transaction ID, which increased by one per probe and continued from one search to the next. | observed (client capture) |

Probe header, 20 bytes, all u32 little-endian:

| Offset | Field | Provenance |
| --- | --- | --- |
| 0 | magic: the bytes `3a cf 87 2a` | observed (client capture) |
| 4 | body length (53, the bytes after the header) | observed (client capture) |
| 8 | 1 in every probe | observed (client capture) |
| 12 | transaction ID | observed (client capture) |
| 16 | checksum of the (encrypted) body: CRC-32 with the reflected polynomial `0x04C11DB7` (zlib's), but initial value 0 and no final XOR (zlib's `crc32` uses `0xFFFFFFFF` for both); it matched in every probe | observed (client capture) |

Body obfuscation: the body is processed as 32-bit little-endian words (the
last one partial). Word *i* is XORed with `(K[i mod 8] + tid) mod 2^32`,
written little-endian, where `tid` is the header's transaction ID and `K` is

`1F2D3C4B 5A6C7F8D 38172E4B 8271635A 863F1A2B A5C6F7D8 8371E1B4 17F2D3A5`

(reported by other projects; confirmed against the client capture). The
first word is the fixed TCP XOR key's first four bytes read as a
little-endian word, recovered from the capture by known plaintext; the whole
table was confirmed by every probe in the capture decoding to the same
well-formed XML:

```xml
<P2P><C2D_S><to><port>N</port></to></C2D_S></P2P>
```

where `N` is a port number that was not one of the probes' source ports, and
no traffic arrived on it during the capture. (observed (client capture))

### WS-Discovery from NVRs

| Fact | Provenance |
| --- | --- |
| NVRs send a standard ONVIF WS-Discovery `Probe` (SOAP 1.2, WS-Discovery 2005/04, `Types` `tdn:NetworkVideoTransmitter`) to 239.255.255.250:3702 about every 6–7 s, presumably to find ONVIF cameras to add. The official client sent none. | observed (client capture) | <!-- sanitize:allow (the standard WS-Discovery multicast group) -->

### Our own search

Sent by nvrview's own test script from a computer on the same subnet, after
the facts above were recorded: two search requests (`aa aa 00 00` to the
subnet broadcast, port 2000) and, separately, two rounds of the UDP 2015 probe
(each one probe sent twice, with a random transaction ID and our own listening
port as `N`). Nothing else was sent.

| Fact | Provenance |
| --- | --- |
| Every device that answered the official client answered the two requests with two identical 388-byte records each, byte for byte the same as in the client capture, to our socket bound to port 3000 **on the interface address**. Replies came within 15–320 ms. | observed |
| While the official client was running on the same computer, it held UDP port 3000 on the wildcard address, and binding the wildcard address again failed even with `SO_REUSEADDR` and `SO_REUSEPORT`; binding port 3000 on the interface's own address with `SO_REUSEADDR` succeeded and received the replies (macOS). | observed |
| One device that never answers the port-2000 request, a battery-powered camera, answered each UDP 2015 probe copy from source port 2015 to the port named in the probe (`N`), not to the probe's source port. The answer has the same 20-byte header (magic, body length, 1, the probe's transaction ID echoed, CRC-32 as above) and the same body obfuscation keyed by that transaction ID. No other device answered. | observed |
| Its body: `<P2P><D2C_S_R><bat><low>…</low><pct>…</pct><c>…</c><p>…</p></bat><uid>…</uid></D2C_S_R></P2P>`, with `pct` the battery charge in percent (100 at the time) and `uid` the device UID; `low` and `c` were 0 and `p` was 2, meanings unknown. It carries no name, address, model, or TCP port: the address is the datagram's source. | observed |

## Open questions

- Camera motion control: 18 (four directions and stop), 19 `toPos`, 295,
  294, 190, 340 and the 201 → 202 → 11 talk sequence are now observed. Still
  open: the units of `pPos`/`tPos`/`zPos` (433 was never sent, in either
  capture, so no position was seen); whether the camera really keeps moving
  without a stop, and for how long (needs position readings or video
  analysis; `bc-cli ptz-probe` is built to observe it); how long a recall
  takes (its reply comes before the camera arrives); diagonal moves and 18
  zoom and focus commands (absent from both captures); what 723 means; how
  a recall to an id outside the 190 list is answered (the core never sends
  it); what 709 does; whether any camera
  supports an autofocus command (no channel in the capture advertised
  `autoFocus`); the meaning of the `01wb` prefix `00 01 02 00` in talk
  records (only one record size was seen); whether talk-back needs 202 at
  real-time pace or tolerates bursts; whether it needs a live view of the
  channel to be running (the client always had one: `followVideoStream`);
  what a device answers to a 201 on a channel without talk-back, to a
  second 201, or to 202 without 201; and whether the received-audio ADPCM
  path (above, reported low nibble first) is in fact coded high nibble first
  like talk-back records.
- Recordings: whether 15 must be repeated for days with many files; the
  meaning of `dayType` entries for days without recordings and the full
  mapping of the numeric `streamType` in 272; whether the device or TCP sets
  the replay pace; why 123 carried `channelId` 0; why our own client's 143
  media had `encryptLen` 0 when the official client's did not; the playback
  commands 381 and 383 (unused by the client so far) and why it sends 382 at
  quit; how a 143 error (bad stream name, no disk) is reported. A play across a
  file boundary is observed to continue with no wire event; a play across a
  gap with no recording at all (whether the device skips it instantly, waits
  it out, or ends the play) was not observed, and the mock skips it
  instantly; and rates
  2×–16× are served by stop/seek/play with `playSpeed` 2, 4, 8, 16 (keyframes
  only, no audio; settled, see "Rates, keyframes, and how a play ends").
  Four plays at once are observed for the official client (see "Four plays
  at once"); still open there: a fifth play, main-stream plays four-up,
  plays beside live view with that recipe, and sub-stream 8x and 16x.
  Still open: how a play (5) *ends* when it reaches the true end of the
  recorded data (the official client always stopped it first, so no device
  end message has been seen), and what the device does when asked to play
  past the end of the last file.
- Time settings: the 104 reply is now known (`timeZone` in seconds west of
  UTC for standard time, `isDst`, local date and time). Still open: the
  layouts of 106 (DST rule) and 38 (NTP server and offset), which the newer
  client's Date & Time screen did not read; whether `timeZone` and `isDst`
  describe every device the same way (checked on one).
- Time bases: whether keyframe seconds are UTC on every device and firmware
  (observed UTC on the devices measured, including a recent-firmware NVR in
  the official client's capture); why the very first capture read as equal to
  `seekTime`; what the recording `Id` suffix after the packed time means.
- Detection events: 192 → 33 and the type names `people` and `dog_cat` are
  observed. Still open: whether a device pushes 33 without 192; how
  `vehicle`, `face`, `package`, a doorbell press (`visitor`) and two types
  at once look in a push (the client expects comma lists); whether
  `timeStamp` is ever non-zero (a camera logged in directly, rather than
  through an NVR, may differ); the layout of the in-stream detection data
  (boxes) in video extra headers; what non-empty 600 lists hold; and the
  meaning of `aitype` bits not named above.
- Event log and AI search (516–518, 653): seen on the wire on a recent NVR
  (see "What the AI search sent on the wire"); the text goes to the NVR as
  `textOfPicture`, so no "set text feature" command was needed there. Still
  open: why a 517 page is sometimes answered status 442 with an empty body
  (the phrase may matter; the meaning is unknown); whether older NVR
  firmware accepts these commands or needs the feature-vector path; what 519
  and 759 return (never sent by that client); the exact meaning of `pts`
  and `offset`; and whether hits include the NVR's own detections for
  cameras without AI.
- How many sessions an NVR accepts. Its HTTP API refused new connections
  after six logins without logout (observed); the limit for port 9000
  sessions is not measured.
- Idle timeout on the device side. The client's side is now reported
  (command 93 after 6 s of silence; dead after 10 s on a direct connection),
  and the 6 s rule is observed. Still open: whether the device itself drops a
  silent session, and after how long.
- Logout: confirmed that an NVR answers command 2 with 200; not yet measured
  whether it frees the login slot sooner than a bare TCP close.
- What the first character of a `..dc`/`..wb` magic denotes. The client
  ignores it.
- Whether any camera model sends audio in another codec or sample rate (all
  observed so far: AAC-LC 16 kHz mono). The client also handles ADPCM and
  µ-law at 8 kHz (reported).
- Behavior at the device's limits: how many concurrent streams per session and
  per device. The statuses are reported (409, 410 at login; 430 at stream
  start); the numbers are not. Four simultaneous replay plays on one session
  were seen with no refusal (observed (client capture)), and nvrview's probe
  saw live view beside two simultaneous plays (one of them another camera)
  with nothing refused. Still open: live view beside more than two plays,
  beside a download, and what the message was that the probe once took for
  a "status 52092" refusal (see "Live view and plays on one session").
- LAN discovery: what record bytes 51, 52 and 57 mean; whether devices on
  other firmware send a different record size; why the battery camera did
  not answer the official client's UDP 2015 probes (asleep at the time, or
  something about the probe's port); what `low`, `c` and `p` in `D2C_S_R`
  mean, and whether a battery camera listens on port 9000 at all while it
  sleeps.
- A second message magic, bytes `a0 cb ed 0f`, which the client's receive
  path accepts besides `f0 de bc 0a` (reported); when devices send it is
  unknown.
