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
142, 272–274 (recording search; see "Recordings: search and download").

## Camera motion control

Commands that move a camera, change its focus, or play sound through its
speaker. Each is marked with its tier (docs/ARCHITECTURE.md, "Device
commands"): **viewing** (read-only, always allowed), **motion** (camera
motion control, only where the user enabled it), or **configuration**
(changes settings; never sent). Unless marked observed, everything here is
reported (client); none of the motion commands has been seen on the wire yet.

**Request shapes.** Read-only queries in this area have no body: the request
is only an extension, `<Extension version="1.1"><channelId>N</channelId></Extension>`
(the client adds `chnType`, and `subChannel` for a second lens), and the
client waits up to 8 s for the reply. Commands that act send an XML body as
well. (reported (client); the extension-only query form is observed for
command 10 and the channel queries 527–551)

### Capabilities

| Fact | Provenance |
| --- | --- |
| `Support` (199) has a device-level `ptzMode` (`none`, `af`, `ptz`, `pt`, `p`) and per-channel `item` entries with `chnID`, `ptzType`, `ptzControl`, `ptzPreset`, `ptzPatrol`, `ptzTattern`, `autoPt`, `ipcAudioTalk`, `channelType`. | observed (field names); meanings reported (client) |
| `ipcAudioTalk` 1 marks a channel that supports talk-back: on two NVRs the client asked for the talk ability (command 10) of exactly the channels with `ipcAudioTalk` 1, and of no others. | observed |
| `ptzType` took the values 0, 1 and 3, and `ptzControl` several bitmasks, on different channels; their meaning is not established. The client reads `ptzPreset` bit 0 as "has presets", `ptzControl` bit 7 as "can report its position", bit 4 as digital zoom, bit 14 as manual focus, and a per-channel `autoFocus` of 1 as autofocus support; it shows a zoom and focus slider when the device's `ptzMode` is `af` or `ptz`. | values observed; meanings reported (client), medium confidence |
| `AbilityInfo` (151) lists, per module and channel, what the logged-in user may do, as `abilityValue` strings like `download_rw, fileFind_rw`. PTZ entries are `control`, `preset`, `cruise`, `track`, `decoder`, `ptzInfo`; audio has `talk`; replay has `replay`, `seek`; record has `download`, `fileFind`. | observed (form); PTZ entries reported (client) |

### Reading position and state (viewing)

| Command | Reply | Provenance |
| --- | --- | --- |
| 433 | `ptzCurPos` with integers `pPos`, `tPos`, `zPos` (pan, tilt, zoom; units and ranges unknown) | reported (client) |
| 294 | `zoom` and `focus`, each with `minPos`, `maxPos`, `curPos`; the client polls it about every 6 s while its PTZ panel is open | reported (client) |
| 190 | `PtzPreset` with `channelId`, `separateCfg`, `maxPresetNum`, and `presetList/preset` entries (`id`, `name`, `imageName`, `xpos`, `ypos`, `height`, `width`, `mode`) | reported (client) |
| 224 | `AutoFocus` with `channelId`, `disable` (whether continuous autofocus is off) | reported (client) |
| 742 | PTZ information: `pt`, `separateCfg` | reported (client) |
| 332, 35, 64 | guard (home position) settings, PTZ serial settings, patrol settings | reported (client) |
| 10 | `TalkAbility`: `duplexList/duplex` (`FDX` seen), `audioStreamModeList/audioStreamMode` (`followVideoStream`, `mixAudioStream` seen), and `audioConfigList/audioConfig` with `priority`, `audioType` (`adpcm` seen; the client also knows `g711`), `sampleRate` (16000 seen), `samplePrecision` (16), `lengthPerEncoder` (1024), `soundTrack` (`mono`) | observed |
| 542, 657, 693 | pushed by the device: PTZ state, PTZ status, talk status | reported (client) |

### Moving (motion)

**Command 18, PTZ control**, a body with no reply data:

```xml
<PtzControl version="1.1">
  <channelId>N</channelId>
  <speed>20</speed>
  <command>left</command>
</PtzControl>
```

| Fact | Provenance |
| --- | --- |
| `command` is one of `left`, `right`, `up`, `down`, `leftUp`, `leftDown`, `rightUp`, `rightDown`, `zoomInc`, `zoomDec`, `focusInc`, `focusDec`, `irisInc`, `irisDec`, or `stop`. | reported (client) |
| Moves are continuous: the client sends the direction when a control is pressed and `stop` (with speed 1) when it is released. Its default speed is 20; its speed slider runs 1–64. A client must therefore always follow a move with `stop`, including on error and disconnect paths. | reported (client) |
| `auto` (continuous scan) and `startPatrol`/`stopPatrol` (with `patrolId`) are also motion; `clearPatrol` changes settings (configuration). | reported (client) |
| `focusInc`/`focusDec` are the only one-shot focus actions found; no "autofocus now" command exists in the client. | reported (client) |

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
| The client always writes `xpos`, `ypos`, `height`, `width` (floats) into each `preset`, `name` and `imageName` when set, and optionally `mode` (`global`, `center`, `area`). Whether a recall needs more than `id` and `command` is not known. | reported (client) |

**Other motion commands.**

| Command | Body | Tier | Provenance |
| --- | --- | --- | --- |
| 295 | `StartZoomFocus` with `channelId`, `command` (`zoomPos` or `focusPos`), `movePos`: move zoom or focus to an absolute position in the range from 294 (the client's slider) | motion | reported (client) |
| 468 | `ptzCurPos` with `pPos`, `tPos`, `zPos`: go to absolute coordinates | motion, but nvrview never chooses raw coordinates (it recalls presets instead) | reported (client) |
| 445 | `Ptz3DLocation` with `topLeftX`, `topLeftY`, `width`, `height` (floats), `speed`, `streamType`: aim at a rectangle drawn on the picture | motion | reported (client) |
| 545 | `pPos`, `tPos`, `zPos` and a rectangle: move to a tracking area | motion | reported (client) |
| 709 | manual focus with `xpos`, `ypos`, `height`, `width` (a focus region); no caller in the client's UI was found, so its effect is unknown | motion (unconfirmed) | reported (client) |
| 331 | guard (home position): `toGrd` goes there, `setGrd` saves it. The client writes `benable` and `timeout` into every 331 body, so even `toGrd` can rewrite settings | configuration (never sent, even to go home) | reported (client) |

**Configuration in this area (never sent):** 19 with `setPos` or `delPos`,
20 (set patrol), 331, 225 (turn continuous autofocus on or off), 708
(autofocus mode), 34 (PTZ serial settings).

### Talk-back (motion)

Sending sound to a camera's or doorbell's speaker. All reported (client)
unless marked.

1. Command 10 reads the talk ability (viewing; observed above).
2. Command **201** opens talk:

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
   `mixAudioStream`).
3. Audio goes out as command **202** messages, class `0x1464`, status 0, with
   the extension `<Extension version="1.1"><binaryData>1</binaryData><channelId>N</channelId></Extension>`
   and a payload of `01wb` records: both size fields hold the ADPCM length
   plus 8, then a 4-byte prefix `00 01 <n/2+2> 00` and an IMA ADPCM block
   (predictor, step index, samples).
4. Command **11** closes talk: no body, channel extension only.

| Fact | Provenance |
| --- | --- |
| Commands 12 and 203–205 (other talk stream variants) are not used by this client. | reported (client) |
| Not known: whether 202 payloads are encrypted, which message ID they carry, how they are paced, and whether the device answers them. The ability advertised 16 kHz ADPCM, while the client's ADPCM decoder outputs 8 kHz; which rate the talk records use is unconfirmed. | open |

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
capture. (observed)

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
| Nine entries, indexes 0–8, came back on the tenth day of a month for a channel that had recorded continuously, which fits "one entry per day with recordings, index = day of month − 1"; not confirmed. Other `type` values (probably event recordings) were not seen. | observed (meaning inferred) |
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
| `streamType` here is a number, not a name: 2 was sent while the 14 search of the same moment named `externStream`. The mapping of the numbers is not known. | observed |
| Command 273 sends `findAlarmVideo` with `channelId` and `fileHandle` and returns one page: `alarmVideoInfo` with `channelId`, `fileHandle`, `bFinished`, and an `alarmVideoList` of up to 50 `alarmVideo` entries. The client repeats 273 until `bFinished` is 1; every page but the last was full. | observed |
| Each `alarmVideo` has `fileName`, `bHasRecFile`, `bEncrypted`, `alarmType` (`md`, `vehicle`, `people` seen), `fileId` (the `Id` of the recording file from command 15 that contains the event), `startTime`, `endTime`. Events were about 10 to 25 s long. | observed |
| Command 274 with `channelId` and `fileHandle` closes the search; empty reply, status 200. | observed |
| The client also knows `destination` in the 272 request and `bDeleted` and `score` in `alarmVideo`. | reported (client) |

These three searches give a playback UI what it needs: a calendar (142), a timeline of
continuous recording (14–16), and event markers (272–274).

### Playback commands in the client (not yet observed)

All reported (client); none of these appeared in a capture yet.

| Command | Use | Body |
| --- | --- | --- |
| 5 / 7 | play a recording file / stop | `FileInfo` with `channelId`, `Id` or `name`, `uid`, `supportSub`, `playSpeed`, `streamType`, optional `startTime` |
| 123 | seek during playback | `ReplaySeek` with `channelId`, `seq`, `seekTime` |
| 150 | keyframe-only playback | not traced |
| 381 / 382 / 383 | play by time range / stop / seek (newer form) | `ReplayByTimeV2` with `startTime`, `endTime`, `playSpeed`, `streamType`, `logicChnBitmap`, offsets, `durationList` |
| 8 / 9 | download a whole file by name / stop | `FileInfo` with `uid`, `Id`, `name`, `channelId`, `supportSub`, `streamType` |
| 143 / 144 | download a time range / stop | below |
| 476; 492–494 | paged download; paged search open, next, close | not traced |
| 303 | **delete recording files** | configuration tier: never sent |

### Download by time range: command 143

Named "download cut" in the client, with 144 as its stop. Observed on an NVR (firmware build family 2025) by a small read-only
client that sent only login and this command, and verified by decoding the
result. The official client was not captured downloading; that it uses 143
too is unconfirmed.

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
| First comes a control reply (payload offset 0) whose encrypted body is a `FileInfoList` with `sizeL`: the device's estimate of the window's size in bytes. | observed |
| Then media messages: an extension (payload offset non-zero) and a payload of BCMEDIA records, exactly as in live view. `encryptLen` was 0 on every media message, so the payload is plain. | observed |
| The download ends with a command-143 message with an empty body. | observed |
| Other messages (alarm and status pushes) arrive on the same session during a download and must be skipped by command number and message ID. | observed |
| `streamType` alone selects the lens and tier: `mainStream`, `subStream`, and on a multi-lens camera `externStream` (second lens, high) and `mobileStream` (second lens, low). Changing `channelId` or `logicChnBitmap`, or adding logical-channel elements, never selected the second lens. `uid` is optional for the first lens; it was sent with the camera's ID for the second lens, as the official client does in its searches. | observed |
| The cut is aligned to the recording's keyframes: a 5 s window came back as 6–8 s of video (one whole GOP of 6 s on a main stream, 8 s on a sub-stream). Video timestamps (µs) are the frame times; the advertised frame rate is not. | observed |
| Audio records can precede the window's first picture by whole fragments (up to about 12 s was seen), and audio has no timestamp, so a downloader must align audio to video itself (for example, drop audio ahead of the first keyframe). | observed |
| A window with no recording ends normally (empty 143) without a single video record. | observed |
| Second-lens streams carry audio as `15wb`; any `..wb` record is audio. | observed |
| Cost, for a 5 s window: about 1.5 MB read for a high-resolution H.265 main stream and 0.42 MB for its H.264 sub-stream, about 2 s per fetch including login. One session served several such downloads one after another. | observed |
| A download can be stopped early with command 144 (empty body). | reported (client) |
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

- Camera motion control (needed before any code sends it): the commands and
  bodies are now reported (client); none is observed. Read-only checks first:
  199, 151, 433, 294, 190 and 224 on a PTZ or zoom camera, to map `ptzType`
  and the `ptzControl` bits and learn the units of `pPos`/`tPos`/`zPos` and
  `movePos`. Then a capture of the official app using move and stop, preset
  recall, the zoom and focus sliders, and talk-back, to confirm 18, 19
  (`toPos`), 295, and the 201 → 202 → 11 sequence (message IDs, encryption,
  pacing, sample rate). What 709 does is unknown.
- Recordings: whether 15 must be repeated for days with many files; the
  meaning of `dayType` `index` and of the numeric `streamType` in 272; whether
  the official client downloads with 143 (a capture of its download); the
  playback commands 5/7, 123 and 381–383, none yet observed; how a 143 error
  (bad stream name, no disk) is reported.
- How many sessions an NVR accepts. Its HTTP API refused new connections
  after six logins without logout (observed); the limit for port 9000
  sessions is not measured.
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
