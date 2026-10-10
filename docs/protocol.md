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
| Video second character: `0` keyframe, `1` predicted frame, as observed. A third value, `e`, is also accepted as a video record of a separate kind whose use is unknown. | reported (client) |
| When resynchronizing, the client also requires the codec field to read `H264` or `H265`; it takes the codec from each record, not from any query reply. | reported (client) |
| Video extra header, when at least 8 bytes: bytes 0–3 are a u32 little-endian count of seconds (apparently the device's wall clock). At the first keyframe after a stream (re)starts, the client takes that value × 1,000,000 as the frame's absolute time in µs and dates later frames by adding their timestamp difference from that keyframe. Bytes 4–5 can encode a resolution code; byte 6 holds flags. Bytes after the first 8 carry tag-length-value data that the client parses as detection metadata. | reported (client) |
| A record's size in the stream is the 8-byte-aligned payload size plus the extra-header size, after the 24-byte header. | reported (client) |

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

### Animal type names

| Name | Where | Provenance |
| --- | --- | --- |
| `dog_cat` | 33 `AItype`, 299 `detectType`, and the 272 search request | observed |
| `dog_cat`, also `dog`, `cat` | recognized in recording-file `recordType` and event-search types | reported (client) |
| `dogCat`, `animal`, `pet` | the client's own JSON and UI names (`petCount` in 519) | reported (client); not wire names |
| `other`, `buck`, `bear`, `boar`, `fox`, `female_deer`, `raccoon`, `turkey`, `wolf` | wildlife subtypes: `animalTypes` in the paged search 492, `animalType` in its results, `detectedAnimalTypes` in a recording file's `FileInfo` | reported (client) |

## Camera motion control

Commands that move a camera, change its focus, or play sound through its
speaker. Each is marked with its tier (docs/ARCHITECTURE.md, "Device
commands"): **viewing** (read-only, always allowed), **motion** (camera
motion control, only where the user enabled it), or **configuration**
(changes settings; never sent). Unless marked observed, everything here is
reported (client). Observed facts come from the client capture described at
the top: a PTZ lens of a multi-lens camera, a camera with a motorized zoom
and focus lens (no pan or tilt), and a camera with a speaker, all behind one
NVR.

**Request shapes.** Read-only queries in this area have no body: the request
is only an extension, `<Extension version="1.1"><channelId>N</channelId><chnType>0</chnType></Extension>`
(the client adds `subChannel` for a second lens), and the client waits up to
8 s for the reply. (observed for 10, 190, 294, 340 and the channel queries
527–551; the timeout is reported (client))

Commands that act (18, 19, 295, 201) put their XML body in the *payload*:
the header's payload offset is the length of an extension
`<Extension version="1.1"><channelId>N</channelId></Extension>` (no
`chnType`), and the payload is the `<body>…</body>` XML, encrypted as its own
unit like any body. Every one of them was answered with an empty reply,
status 200, carrying no state. (observed)

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
| 294 timing | Read when the PTZ panel opened for a channel and again about 6 s after a preset recall or a zoom move (6.2, 6.2 and 6.4 s seen), but not after focus steps and never on a fixed period. This **contradicts** the static reading of a 6 s poll; it fits a 6 s timer restarted by some actions. | observed (client capture) |
| 294 state | After a zoom move the camera's focus `curPos` changed by itself (refocus after zoom). After two preset recalls on the PTZ lens the zoom `curPos` was unchanged. | observed (client capture) |
| 190 | `PtzPreset` with `channelId`, `separateCfg` (holding `preset` 0), `maxPresetNum` (64), `maxPresetPicNum` (64), and `presetList/preset` entries with only `id` (from 0), `name`, `imageName`; no coordinates or `mode`. Read once when the preset list opened. | observed |
| 340 | `PtzAutoTest` with `testStat` (0 seen): the pan/tilt self-test state. Read once, together with 294, when the PTZ panel opened on the PTZ lens; not read for the zoom-only camera. Viewing tier. | observed (client capture) |
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
| Moves are continuous: the client sent the direction once when a control was pressed and `stop` with `speed` 1 when it was released, 1.3–1.5 s later in the capture; nothing was repeated while the button was held. Each command was answered by an empty status-200 reply in 17–56 ms. A client must therefore always follow a move with `stop`, including on error and disconnect paths. | observed |
| The move speed in the capture was 40; the client's built-in default is 20 and its speed slider runs 1–64, so 40 was presumably a stored user setting. | observed (40); default and range reported (client) |
| On a multi-lens camera whose second lens pans and tilts, the client addressed 18 to the plain `channelId` (no `chnType` or `subChannel`) while showing that lens's `externStream`. | observed (client capture) |
| `auto` (continuous scan) and `startPatrol`/`stopPatrol` (with `patrolId`) are also motion; `clearPatrol` changes settings (configuration). | reported (client) |
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

**Other motion commands.**

| Command | Body | Tier | Provenance |
| --- | --- | --- | --- |
| 295 | `StartZoomFocus` with `channelId`, `command` (`zoomPos` or `focusPos`), `movePos`: move zoom or focus to an absolute position in the range from 294. Moving the zoom slider sent one 295 per move, not a stream (`movePos` 11, then 0 about 1.3 s later, on a 0–28 range); the focus buttons sent one 295 per click, stepping `movePos` by 1 from the current focus position. Replies were empty, status 200, after 100–220 ms. | motion | observed |
| 468 | `ptzCurPos` with `pPos`, `tPos`, `zPos`: go to absolute coordinates | motion, but nvrview never chooses raw coordinates (it recalls presets instead) | reported (client) |
| 445 | `Ptz3DLocation` with `topLeftX`, `topLeftY`, `width`, `height` (floats), `speed`, `streamType`: aim at a rectangle drawn on the picture | motion | reported (client) |
| 545 | `pPos`, `tPos`, `zPos` and a rectangle: move to a tracking area | motion | reported (client) |
| 709 | manual focus with `xpos`, `ypos`, `height`, `width` (a focus region); no caller in the client's UI was found, and it was not sent in the capture, so its effect is unknown | motion (unconfirmed) | reported (client) |
| 331 | guard (home position): `toGrd` goes there, `setGrd` saves it. The client writes `benable` and `timeout` into every 331 body, so even `toGrd` can rewrite settings | configuration (never sent, even to go home) | reported (client) |

**Configuration in this area (never sent):** 19 with `setPos` or `delPos`,
20 (set patrol), 331, 225 (turn continuous autofocus on or off), 708
(autofocus mode), 34 (PTZ serial settings).

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
predictor 0, step index 0.

| Fact | Provenance |
| --- | --- |
| The first 202 followed the 201 reply after about 0.2 s; the 11 followed the last 202 after about 50 ms. | observed |
| Records went out at a mean spacing of 64.0 ms (median 62 ms, from 5 to 144 ms between messages), which is 1024 samples at 16 kHz: the talk audio is 16 kHz ADPCM, as the ability advertised. (The 8 kHz of the client's ADPCM *decoder*, reported above, applies to received audio only.) | observed |
| The device answered **every** 202 with an empty 202 reply, status 200, message ID 0, after 4–118 ms (median 29 ms). | observed |
| No talk-status push (693) arrived during talk. | observed (client capture) |
| Commands 12 and 203–205 (other talk stream variants) are not used by this client. | reported (client) |

## Recordings: search and download

Everything in this section is read-only: it lists or copies recordings and
changes nothing on the device. All of it belongs to the viewing tier.

**Times.** Every time in these bodies is a block of six elements,
`<year>`, `<month>` (1–12), `<day>`, `<hour>`, `<minute>`, `<second>`, in the
device's local wall-clock time with no zone or offset. (observed)

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
| `name` and `Id` are opaque file identifiers; treat them as strings. (In the capture `Id` looked like `<tier>-<channel>-0-<packed start time>-<suffix>`, with the start time in UTC and a 0-based month, and `name` like a channel number plus the local start time; neither is needed to download by time.) | observed |

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
| Command 273 sends `findAlarmVideo` with `channelId` and `fileHandle` and returns one page: `alarmVideoInfo` with `channelId`, `fileHandle`, `bFinished`, and an `alarmVideoList` of up to 50 `alarmVideo` entries. The client repeats 273 until `bFinished` is 1; every page but the last was full. | observed |
| Each `alarmVideo` has `fileName`, `bHasRecFile`, `bEncrypted`, `alarmType` (`md`, `vehicle`, `people` seen), `fileId` (the `Id` of the recording file from command 15 that contains the event), `startTime`, `endTime`. Events were about 10 to 25 s long. | observed |
| Command 274 with `channelId` and `fileHandle` closes the search; empty reply, status 200. | observed |
| The client also knows `destination` in the 272 request and `bDeleted` and `score` in `alarmVideo`. | reported (client) |
| The full `alarmType` list the client sent was `md, pir, other, package, io, people, face, vehicle, dog_cat, visitor, cry, crossline, intrude, loitering, legacy, loss, answer`, and `eventAlarmType` added `nonmotorveh`, `tamper`, `lmsg` (and dropped `package`). Results seen: `md`, `people`, `vehicle`. | observed |
| A 272 search covers one channel (one lens, chosen by `streamType`) and one day; the playback tab runs one per channel it shows. There is no cross-channel form. | observed (requests); reported (client) |

These three searches give a playback UI what it needs: a calendar (142), a timeline of
continuous recording (14–16), and event markers (272–274).

### Event log and AI search: commands 516–519, 759, 653

The client's NVR "event log" window and its "AI video search" (search by
type, or by a typed phrase) do not use 272–274. They use the event-log
commands, which search **all chosen channels in one search**. None of them
appeared in either capture; everything here is reported (client) and read
from the client's request builders and reply parsers. All are viewing tier.

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
| Text search: the typed phrase is turned into a feature vector on the computer by a downloaded text-encoder model, or, when a vendor feature flag is on, by the vendor's server (which can also record search texts). The vector, not the text, goes to the NVR in 516, with a `similarity` threshold. If the NVR reports a feature-version conflict (`version`, `text`), the client re-encodes with the other model version. | reported (client) |
| Thumbnails: the newer search screen (the client's "AI video search") fetches one 653 cover per hit with a recording, through a queue that runs **one request at a time**, and its summary card loads the first 12 hits. The older desktop window instead asks 298 (cover preview, sub-stream, one frame over the hit's time range, 16 s timeout) per row. Per-hit, one-at-a-time image fetches are the clearest reason the client's AI search feels slow. | reported (client) |
| The paged search 492–494 (`paginationSearch`: `channel`, `order`, `fileType`, `photoReplay`, `alarmInTypes`, `animalTypes`, `startTime`, `endTime`; items with `recordType`, `animalType`, `thumb`, `Id`, `fileSize`) carries thumbnails and wildlife subtypes; it appears to serve snapshot-based cameras and was not traced further. | reported (client) |
| Nearby commands that are not viewing tier and are never sent: 300 (write `AiCfg`), 343 (write AI detection settings), 765 (set user-defined object labels). The client's "set text feature" step during a text search was not traced to a wire command; it stays off the allowlist until it is. | reported (client) |

### Playback

The client's playback commands. 5, 7, 123, 143, 144, 298 and 382 were
observed (client capture); the rest are reported (client) and did not appear.

| Command | Use | Body | Provenance |
| --- | --- | --- | --- |
| 5 / 7 | play a recording file / stop | 5: `FileInfo` with `channelId`, `Id`, `uid`, `supportSub` (0), `playSpeed` (1), `streamType`; no `startTime` was sent. 7: `FileInfo` with `channelId`, `name`, `streamType` | observed |
| 123 | set the start position of the next play | `ReplaySeek` with `channelId`, `seq`, `seekTime` (a time block) | observed |
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
6. Pacing: about 2 to 4.4 s of video arrived within the first 0.25 s, then
   roughly real time (about 1.1 s of video per second). Whether the device or
   TCP flow control set that pace was not determined.
7. Scrubbing was **7** (stop, answered empty, status 200), then a new 123 with
   a new `seq` and `seekTime`, then a new 5. Seven plays were made this way;
   none ran to the end of a file, so how a play ends is unknown.

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
| Other projects report command 13 for downloads and 14–16 as a download path; in the client, 14–16 are the file search above and 13 is not sent. | reported |

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

## Open questions

- Camera motion control: 18 (four directions and stop), 19 `toPos`, 295,
  294, 190, 340 and the 201 → 202 → 11 talk sequence are now observed. Still
  open: the units of `pPos`/`tPos`/`zPos` (433 was never sent, so no
  position was seen); whether the camera really keeps moving without a stop,
  and for how long (needs position readings or video analysis); whether a
  recall needs more than `id` and `command`; how long a recall takes (its
  reply comes before the camera arrives); diagonal moves, 18 zoom and focus
  commands, and speeds other than 40 and 1; what 709 does; whether any camera
  supports an autofocus command (no channel in the capture advertised
  `autoFocus`); the meaning of the `01wb` prefix `00 01 02 00` in talk
  records; and whether talk-back needs 202 at real-time pace or tolerates
  bursts.
- Recordings: whether 15 must be repeated for days with many files; the
  meaning of `dayType` entries for days without recordings and the full
  mapping of the numeric `streamType` in 272; how a play (5) ends at the end
  of a file; whether the device or TCP sets the replay pace; why 123 carried
  `channelId` 0; why our own client's 143 media had `encryptLen` 0 when the
  official client's did not; the playback commands 381 and 383 (unused by
  the client so far) and why it sends 382 at quit; how a 143 error (bad
  stream name, no disk) is reported.
- Detection events: 192 → 33 and the type names `people` and `dog_cat` are
  observed. Still open: whether a device pushes 33 without 192; how
  `vehicle`, `face`, `package`, a doorbell press (`visitor`) and two types
  at once look in a push (the client expects comma lists); whether
  `timeStamp` is ever non-zero (a camera logged in directly, rather than
  through an NVR, may differ); the layout of the in-stream detection data
  (boxes) in video extra headers; what non-empty 600 lists hold; and the
  meaning of `aitype` bits not named above.
- Event log and AI search (516–519, 759, 653): not yet seen on the wire.
  Open: whether the NVRs observed so far accept them (only the newer one advertises
  `eventLogVersion`), the page size and how long a page takes, whether
  hits include the NVR's own detections for cameras without AI, the cover
  image format, and the wire command behind the client's "set text
  feature" step.
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
  start); the numbers are not.
- A second message magic, bytes `a0 cb ed 0f`, which the client's receive
  path accepts besides `f0 de bc 0a` (reported); when devices send it is
  unknown.
