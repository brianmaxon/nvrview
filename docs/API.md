# Local API

One local server, built once in the shared core, behind every client that is
not the native app itself: the web viewer, a phone companion app, scripts,
home-automation systems, and AI agents over MCP. This document is the design;
nothing here is built yet. See [ARCHITECTURE.md](ARCHITECTURE.md) (goals 4, 5,
7; extension points; feature ideas) and [FFI.md](FFI.md) for the native
interface it complements.

## Principles

1. **Off until the user turns it on.** By default the server listens on
   loopback only, or not at all. Listening on the local network is a separate,
   explicit switch, and the server never listens on a public interface or
   opens a router port.
2. **Every client is paired.** No anonymous access, not even read-only.
3. **Least privilege.** Each paired client holds grants for exactly what it
   needs; motion control is granted per client and per camera; device
   configuration cannot be granted at all.
4. **The devices see one client.** All API clients share the server's one
   login per device and its shared streams; adding viewers never adds device
   sessions.
5. **Untrusted text stays data.** Anything that came from a device, a camera
   picture, a microphone, or another client is returned as data and never
   becomes an instruction, a command argument, or speech (see "Agents").
6. **Bounded.** Clients, streams, requests, queues, and logs all have limits;
   a slow or hostile client loses its own data, never anyone else's.

## Shape

| Surface | Transport | Used by |
| --- | --- | --- |
| REST, JSON, versioned (`/api/v1/...`) | HTTPS | everything: cameras, layouts, incidents, downloads, snapshots, settings that belong to nvrview (never to devices) |
| Events | WebSocket (`/api/v1/events`) | live device state, detections, triggers, transcripts as they happen |
| Live media | WebSocket per stream (`/api/v1/live/{camera}/{tier}`) | fragmented MP4 segments (init, then media) for Media Source Extensions; pass-through, no re-encoding |
| Recorded media | HTTPS with ranges | downloaded clips and edits (MP4) |
| Web app | HTTPS, static files embedded in the server | the web viewer |
| MCP | Streamable HTTP (`/mcp`) | AI agents, through the tools listed below |

The server is a Rust crate hosted by the core (`crates/local-api`, planned),
so the macOS app, the Windows app, and a headless service all expose the same
API.

## Transport security

- **TLS on the local network.** At first start the server creates its own key
  and certificate (no public certificate authority can issue one for a LAN
  address). Clients learn the certificate's fingerprint during pairing and pin
  it; a different certificate later is refused, not clicked through. Plain
  HTTP is accepted on loopback only.
- **DNS-rebinding and cross-site defenses.** Requests must carry an expected
  `Host` (the server's addresses and its local name) and, for browsers, a
  matching `Origin`; there are no CORS grants; browser sessions use
  `HttpOnly`, `Secure`, `SameSite=Strict` cookies.
- **Content Security Policy** on the web app: no inline scripts, no third-party
  origins, no network access except the server itself.
- **Discovery** (optional): Bonjour/mDNS advertises the service on the local
  network so clients can find it; it reveals the service name only.

## Pairing and credentials

1. On the Mac (never remotely), the user chooses "Pair a device", names it,
   and picks its grants. The app shows a short code and a QR code for two
   minutes. The QR code carries the server address, the certificate
   fingerprint, and a one-time secret.
2. The new client scans the QR code (or types the address and code) and
   proves the secret. The server issues a long random credential bound to
   that client record.
3. The server stores only a hash of each credential. Credentials can be
   revoked one by one from the app; revoking closes the client's open
   connections at once.

Failed pairing attempts are rate-limited and end the pairing window after a
few failures. Browser clients exchange their credential for a short-lived
session cookie.

## Grants

| Grant | Allows |
| --- | --- |
| `cameras` | camera list, status, layouts (read) |
| `live` | live streams and snapshots, optionally limited to listed cameras |
| `recordings` | the index, incidents, cover images, recorded playback |
| `downloads` | starting downloads and edits, fetching the results |
| `detections` | detection events and history |
| `transcripts` | transcripts and keyword events (sensitive; separate on purpose) |
| `app` | app actions: focus a tile, switch a layout, edit nvrview's own layouts |
| `motion:{camera}` | camera motion control on that camera (preset recall, moves, focus); the per-camera grant automation needs (ARCHITECTURE.md, "Device commands") |
| `speak:{camera}` | doorbell text-to-speech from the user's templates on that camera |

There is no grant for device configuration, for pairing other clients, or for
changing grants: those are done only in the app on the server machine.

## Limits

Per client: concurrent live streams (default 4), concurrent downloads, request
rate, and event queue length. Per server: total paired clients and total live
viewers. Media WebSockets drop to the next keyframe when a client falls
behind, as the native renderer does. Every motion command, speak request,
pairing, and revocation goes to a bounded audit log viewable in the app.

## Agents (MCP)

Tools map onto the same grants; an agent is just another paired client.

| Tool | Grant | Notes |
| --- | --- | --- |
| `list_cameras`, `get_status` | `cameras` | |
| `search_incidents`, `get_incident` | `recordings`, `detections` | results carry camera names and event types as data |
| `get_snapshot` | `live` | returns an image, never text read from it |
| `get_transcript` | `transcripts` | |
| `focus_tile`, `show_layout` | `app` | |
| `recall_preset` | `motion:{camera}` | preset id only; asks the user on the Mac first unless the grant says "without confirmation" |
| `speak_template` | `speak:{camera}` | a template id and typed slots (time, camera name, a choice from a list); never free text |

Prompt-injection boundaries:

- Device-provided strings (channel names, event types), detection labels,
  text recognized in pictures, and transcripts are untrusted. Tool results
  return them as fields of structured data, never mixed into instructions.
- No tool takes free text that reaches a device or a speaker. The only
  arguments that act are ids from lists the server published and typed slots
  validated by the server.
- Motion and speech are rate-limited, logged, and by default confirmed on the
  Mac; a confirmation shows exactly what will happen, on which camera.
- An agent cannot widen its own grants, pair clients, or reach configuration.

## Threats considered

| Threat | Defense |
| --- | --- |
| Another device on the LAN probing the server | off by default; pairing required; TLS; rate limits |
| A web page in a LAN browser attacking the server (CSRF, DNS rebinding) | Host and Origin checks, SameSite cookies, no CORS, pinned certificate |
| A compromised TV or tablet | per-client grants and stream limits; revocation from the app |
| A leaked credential | stored hashed; revocable; browser sessions short-lived |
| Prompt injection through camera names, transcripts, or text in pictures | data-only results; no free-text actions; confirmations; logs |
| A client exhausting device logins | impossible by design: clients never get device sessions |
| Exposure to the internet | never listens on public interfaces; remote use goes through the user's own VPN |

## Open questions

- Which HTTP stack (for example `axum` on `hyper`) and TLS library
  (`rustls`), within the license and size budget.
- How the Windows and headless builds store the server key and client
  records (the macOS app can use the Keychain).
- Whether browsers on smart TVs accept a pinned self-signed certificate well
  enough, or need a one-time trust step the pairing flow explains.
