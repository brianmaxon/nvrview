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
decoding the resulting video with FFmpeg.

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
| Live view uses `encryptLen` 1024 on frame-start messages; continuation messages carry no extension and are plain. Recorded-clip download has been seen with `encryptLen` 0. | observed |

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

### Audio payloads

All observed in a live capture of several cameras on one NVR (about 6,500
audio records across both stream indexes, sub and high tiers).

| Fact | Provenance |
| --- | --- |
| Every audio record's payload is exactly one ADTS frame (sync `0xFFF`, MPEG-4, no CRC) whose frame length equals the record's payload size. | observed |
| The ADTS headers say AAC-LC, 16 kHz, mono, one raw data block per frame: 1024 samples, 64 ms per record. | observed |
| Frame sizes vary (variable bit rate); about 250 to 520 bytes were common. | observed |
| Audio records carry no timestamp. They are interleaved with video records (about 15.6 audio records per second against 4–15 video frames), so a record's position between video frames is the only timing information in the stream. | observed |
| Counting samples against the video timestamps over about 64 s gave 16,005 to 16,013 samples per second of video time, not exactly 16,000: the cameras' audio and video clocks differ slightly, and a player that plays audio at a local 16 kHz drifts by seconds per hour. | observed |

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
| The client sent no logout command before the end of the capture. | observed |

**Keepalive and dead connections.** All reported (client); times are whole
seconds from the system clock.

| Fact | Provenance |
| --- | --- |
| Any bytes received on the session (replies, pushes, media) count as a sign of life. | reported (client) |
| Command 93 is the client's connection test and its only use of command 93. It is sent only when nothing has been received for at least 6 s, and at most once every 6 s; there is no fixed timer and no jitter. A session carrying live media therefore sends none. | reported (client) |
| The observed 30–140 s spacing of command 93 (above) fits this rule only if the device sent nothing for 6 s before each one; worth rechecking against a capture. | open |
| The session is declared dead when nothing has been received for more than 10 s on a direct (address) connection, or 20 s on a UID (relayed) connection, but not within 6 s of opening it. The client then closes it and reconnects. | reported (client) |
| Reconnects are retried about every 2 s with no backoff and no limit for mains-powered devices (the client limits battery cameras to 5). Devices that are not yet open are retried every 10 s. | reported (client) |
| Each request has its own reply timeout; the logout's is 2 s (the connection test's is probably 8 s). A request that times out is reported as a timeout; the dead-connection rule above is what triggers a reconnect. | reported (client) |

**Logout.** Reported (client) unless marked observed.

| Fact | Provenance |
| --- | --- |
| Before closing a session the client sends **command 2**, class `0x1464`, status 0, with body `<body><LoginUser version="1.1"><userVer>1</userVer></LoginUser></body>` (no user name or password). Encryption of this body was not traced; presumably like any other body after login. | reported (client) |
| It waits for the reply (up to about 6 s) and then closes TCP. The client does not close a logged-in session any other way. | reported (client) |
| This happens on every close: quitting the app (all devices are closed, then the app waits 0.5 s before tearing down, and exits 1 s later) and before every reconnect after a dead connection. Removing a device was not traced as far. | reported (client) |
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
| `StreamInfoList` holds one `StreamInfo` per distinct stream configuration, not one per channel. Its `channelBits` is a bitmask of the channels that share it (bit *n* = channel *n*), and it contains one `encodeTable` per tier with `type` (`mainStream`, `subStream`), `resolution/width`, `resolution/height`, `videoEncType`, `defaultFramerate`, `defaultBitrate`, `defaultGop`, `framerateTable`, `bitrateTable`. | observed |
| `videoEncType` takes the values 0 and 1. | observed |
| The official client ignores `videoEncType` in the 146 reply; it reads only `channelBits` and, per `encodeTable`, `type`, resolution, the defaults and the two tables. `type` can also be `externStream` or `mobileStream`. | reported (client) |
| In the encoder configuration (command 56) the field of the same name means 0 = H.264, 1 = H.265, and the client knows no other value. That 146 uses the same coding is likely but unconfirmed; take the codec from the media records. | reported (client) |
| `ChannelInfoList` (145) is pushed with message ID 0 and lists every channel slot. Each `ChannelInfo` has `channelId`, `state` (`connect` for a slot with a camera, `none` for an empty one), `changed`, `uid`, `streamSupport` (a comma-separated tier list such as `mainStream,subStream,externStream`), `abilityChanged`, `wifiState`. It has no channel name; names come from 318. | observed |

Commands seen but not needed for live view: 10, 44, 58, 76, 102, 104, 114, 151,
192, 299, 511, 527–551, 574 (configuration and capability queries), and 14–16,
142, 272–274 (recording search and playback).

## Open questions

- Idle timeout on the device side. The client's side is now reported
  (command 93 after 6 s of silence; dead after 10 s on a direct connection);
  confirm on hardware, including whether a device answers 93 while streaming
  and why the observed spacing was 30–140 s.
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
