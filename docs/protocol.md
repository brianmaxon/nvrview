# Baichuan protocol notes

Facts about the Reolink "Baichuan" protocol that nvrview depends on, each with
its provenance (see [CLEAN-ROOM.md](CLEAN-ROOM.md)):

- **observed**: confirmed on real hardware (device class and firmware family
  only)
- **reported**: from another project's notes or code, not yet confirmed

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

## Session upkeep

| Fact | Provenance |
| --- | --- |
| Command 93, empty body, is answered with `<LinkType><type>LAN</type></LinkType>`. The official client sent it on each session at irregular intervals (seen between about 30 and 140 s after login). | observed |
| Whether a device drops idle sessions, and after how long, is **unconfirmed**. | open |
| The client sent no logout command before the end of the capture; whether quitting sends one or only closes TCP is **unconfirmed**. | open |

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
| `videoEncType` takes the values 0 and 1. That 0 is H.264 and 1 is H.265 is **unconfirmed**; take the codec from the media records. | open |
| `ChannelInfoList` (145) is pushed with message ID 0 and lists every channel slot. Each `ChannelInfo` has `channelId`, `state` (`connect` for a slot with a camera, `none` for an empty one), `changed`, `uid`, `streamSupport` (a comma-separated tier list such as `mainStream,subStream,externStream`), `abilityChanged`, `wifiState`. It has no channel name; names come from 318. | observed |

Commands seen but not needed for live view: 10, 44, 58, 76, 102, 104, 114, 151,
192, 299, 511, 527–551, 574 (configuration and capability queries), and 14–16,
142, 272–274 (recording search and playback).

## Open questions

- Keepalive interval and idle timeout.
- Logout command, if any.
- Audio codec of `05wb`/`15wb` records (AAC with ADTS framing is expected; not
  yet checked for live streams).
- Behavior at the device's limits: how many concurrent streams per session and
  per device.
