# Milestone P: work breakdown

How the playback milestone ([PLAYBACK.md](PLAYBACK.md), contract proposal in
[FFI-RECORDINGS.md](FFI-RECORDINGS.md)) is cut into slices, in what order,
which are unblocked now, which wait for a decryptable capture, and which can
run as parallel agents. Protocol facts come only from
[protocol.md](protocol.md); a slice that needs a fact not marked observed or
reported there leaves a documented seam instead of guessing.

**Versioning.** FFI-RECORDINGS.md was written as ABI 4. Its decision 1
replaced that with a minor version, and discovery (minor 1) and updates
(minor 2) have since taken the first ones, and `nv_tile_retry` has taken minor 3, so recordings arrive as **ABI 3,
minor 4** (`NV_ABI_MINOR` 4), with no existing declaration changed. "ABI
4" in that document reads as "the recordings minor".

## Where the core stands

Already in `crates/recordings` (with end-to-end tests against
`tools/mock-nvr` on a paused clock):

| Piece | Module | State |
| --- | --- | --- |
| Per-device index: calendar (142), files (14–16), events (272–274), day files, age and size bounds | `index` | done; keyed by channel only (no lens) |
| Background index passes, newest day first | `indexer` | done; decision 8 asks for on-demand filling instead |
| Incident grouping, camera groups, merge, split, add or remove a camera | `incident` | done; IDs change while an incident grows; no stored edits |
| Download scheduler: chunks, retries, stall detection, resume across restarts, per-device workers, blocking-thread disk writes | `scheduler` | done; per-incident pause and retry only, failures as free text, no device removal |
| Raw chunks, crash-safe join into MP4 | `chunk`, `finalize` | done |
| Cover thumbnails (298) in a bounded cache | `thumbs` | done; fetches share the FIFO search permit with indexing |
| Output names, `incident.json` | `naming` | done |
| Mock device: 142, 14–16, 272–274, 298, 143/144 with stall and disconnect faults | `mock-nvr::archive` | done; no playback (123/5/7) |

Nothing outside `crates/recordings` uses it yet: `viewer-core` has no
recordings owner and the C interface has no recordings functions.

## Facts: what each slice may rely on

| Fact (protocol.md) | Status | Used by |
| --- | --- | --- |
| Searches 142, 14–16, 272–274; covers 298; download 143/144 | observed | C1–C6 |
| Playback at 1×: 123 `ReplaySeek`, 5 `FileInfo` by `Id` with `playSpeed` 1, 7 stop; media on the play's message ID with the 123 `seq`; burst then roughly real-time pace; play starts at the keyframe at or before `seekTime` | observed (client capture) | C9 |
| A play continues across a file boundary with no wire event | observed (timing and bytes only) | C9, C10 |
| Several plays on one session, demultiplexed by message ID | observed (client capture), up to four | C9, C10 |
| Rate change is stop, seek, play with `playSpeed` 1, 2, 4, 8, 16 | observed (client capture, decrypted) | C9 seam |
| Keyframes carry wall-clock seconds in extra-header bytes 0–3, in **UTC** (Unix epoch) in live view and replay; `seekTime` and all search times are device local; the first keyframe is at the seek time converted to UTC, or up to 1 s after it | observed (client capture, decrypted, and `bc-cli replay-probe`); the first capture's "equal to `seekTime`" reading is not reproduced | C7, C10 |
| A recording file `Id` embeds UTC (zero-based month); file `name` and search times are local | observed (client capture) | C1, C10 |
| `playSpeed` 2, 4, 8, 16: keyframes only, no audio; every keyframe up to 4×, every 2nd at 8×, every 4th at 16×; at most about two keyframes per second delivered, so the media clock runs at the requested rate; a burst, then pacing (at 1× about 4 to 5 s ahead of real time) | observed (client capture, decrypted) | B1 (done), C9 seam |
| How a play ends at the end of the recorded data; how 143 reports errors | **unknown**: the client always stopped its plays itself (7) when its loaded file list ran out; no device end message seen | B6, C9 seam |
| 5, 123, 7 report errors | no non-200 status was ever seen on them (all 200); a time with no recording is read from search results | C9 seam |
| Live media and replay media on one session at once | **observed** (`bc-cli replay-probe`): one play, and two simultaneous plays (one of them another camera), beside one live preview, all delivered; live rate unchanged; nothing refused. The earlier "status 52092 refusal" was our misreading of a replay header, not a device status. Live view beside more than two plays, and beside downloads, not measured | B5 |
| 104 reply (`timeZone` in seconds west of UTC for standard time, `isDst`, local date and time); 106 (DST rule) and 38 (NTP) | 104 **observed**; 106 and 38 **unseen** (the newer client's Date & Time screen read only 104) | B2 |
| Event-search (273) animal type name | **observed**: `dog_cat` | B3 |
| Event log and AI search 516–518, 653 (plain `textOfPicture` text matched on the NVR, `similarity`, hit fields, covers ending with a status-201 message, opening a hit = 14–16, 272–274, 123, 5); 519 and 759 | 516–518 and 653 **observed (client capture)**; 519 and 759 **reported (client)** only; status 442 on a 517 page unexplained | B4 |

## Slices

IDs are for reference in commits and agent prompts. "Unblocked" means every
fact the slice needs is observed or reported; seams for the rest are named.

### Core (Rust; builds and tests on macOS, Windows, Linux)

| ID | Slice | Depends on | Status | Agent tier |
| --- | --- | --- | --- | --- |
| C1 | **Lens in every key.** `CameraId` gains `lens` (0 or 1, default 0, serde default so old job files load); the index is keyed by channel and lens (index format 2; a version-1 index is a cache and is dropped and refetched); `IndexedChannel` maps lens to stream; thumbnail keys and job slugs include the lens | — | done (`recordings`; tested against the mock) | Opus (formats) |
| C2 | **Stable incident key** (`key`: group and earliest event, unchanged while the incident grows) and `growing` (end within the gap of the device's now) | C1 | done (`recordings`; tested against the mock) | Sonnet |
| C3 | **Stored edits and camera groups.** Edit log (merge, split, rename, add or remove a camera, revert) stored atomically, replayed after every regroup, expired with the index's age, capped at 10000; camera groups and camera-name overrides stored like layouts, bounded (64 × 64) | C1, C2 | done (`recordings`; tested against the mock) | Sonnet |
| C4 | **On-demand index** (decision 8): fill the days a view asks for (calendar months, then files and events per channel and day), refresh today only while a view holds it, no default background pass; revision counter and change ranges for `recordings_changed` | C1 | done (`recordings`; tested against the mock) | Opus |
| C5 | **One search lane per device with priority.** Thumbnails and user requests before background index work; a thumbnail queue per device, newest request first, at most 256 queued (oldest dropped and reported), cancel by key or all | C1 | done (`recordings`; tested against the mock) | Opus |
| C6 | **Scheduler control.** Failure reason codes (`stalled`, `session_closed`, `refused`, `no_recording`, `write_failed`, `join_failed`, `login_failed`, `other`) beside the English detail; per-camera pause and retry; `remove_device` (jobs wait as `waiting_device`, also while the device is offline, without spending attempts); `forget` with `delete_partial`; the device's size estimate in progress; at most 1000 known downloads. (Progress is a watch channel, newest state wins; the 4-per-second event limit belongs to the owner, C11) | — | done (`recordings`; tested against the mock) | Opus |
| C7 | **Recorded time on frames.** Chunk indexes keep each keyframe's wall-clock seconds (the demuxer already kept the extra header); `recordings::recorded_time` refines the offset across keyframes as in live view, and each camera job records its first picture's time. Optional throughout: a stream without the seconds still downloads. **Seam:** times stay on the keyframes' clock, because whether those seconds are device-local or UTC is not established (B7) | — | done (seconds observed on live and replay; on 143 downloads checked only against the mock) | Sonnet |
| C8 | **Edit decision lists** (follow cut, 2-up, 4-up) as pure planners over an incident and its finished files; keyframe positions read from the MP4's sync samples | C7 | done (`recordings::edl`: `plan_follow`, `plan_grid`; `recordings::sync_samples` reads stss, stts, ctts, and the edit list of the finalized MP4, bounded; the rules where FFI-RECORDINGS.md section 7 is silent are in the module documentation; unit tests only, no device). **Seam:** `SourceFile::file_start_ms` is supplied by the caller and must be on the events' local clock; until B7 is settled the caller passes `edl::fallback_file_start_ms` (the download range start), and the first picture's keyframe-clock time (C7) replaces it afterward. The planners do no conversion | Sonnet |
| C9 | **Playback on the session.** 123, 5, 7 on the viewing tier with request builders; `bc-session` play stream (`DeviceSession::play`: seek, play, media routed by message ID and replay `seq`, bounded buffer, stall timeout, stop on drop and close, at most `max_plays` per device); `mock-nvr` serves 123/5/7 at 1× with the observed burst-then-pace and continues across files. Seams: only `playSpeed` 1 is ever sent (B1); an empty message ends a play and a non-200 reply refuses it, as the decoder's rule (B6) | — | done at 1× (against the mock only) | Opus |
| C10 | **Playback engine** in `viewer-core`: playback tiles on the existing tile pipeline and generations, the shared clock, paced release less decode latency, `wait`/`free` sync, buffer bounds with stop and restart, per-camera anchor refinement and offsets, rates 0.25× to 2× by pacing | C7, C9 | unblocked except B1, B5 | Opus |
| C11 | **Recordings owner in `viewer-core`**: one per core, holding the per-device indexes, the search lanes, the scheduler, the thumbnail cache, the edit log and groups; follows device add, remove, login failure; produces the events of FFI-RECORDINGS.md section 8 | C1–C6 | unblocked | Opus |
| C12 | **Sound for recordings** (section 2a): loudness envelope store per camera and day, labels as events of `source` `local_audio` | C1, C11 | unblocked | Sonnet |

### C interface

| ID | Slice | Depends on | Status | Agent tier |
| --- | --- | --- | --- | --- |
| F1 | Minor 3, part 1: configuration, index and search, incidents and edits, camera groups, thumbnails, downloads (sections 1–5, 8, 9) with the C smoke test and cbindgen header | C11 | unblocked once C11 lands; the user reviews the contract first | Opus for the contract, Sonnet for shaping JSON |
| F2 | Minor 3, part 2: recorded playback (section 6) | C10, F1 | unblocked at 1× | Opus |
| F3 | Edit lists (section 7) and sound (section 2a) | C8, C12, F1 | unblocked | Sonnet |

F1 and F2 ship in one minor if they land together; otherwise F2 takes the
next minor.

### Apps

| ID | Slice | Depends on | Status | Agent tier |
| --- | --- | --- | --- | --- |
| M1 | macOS: incident list, timeline lanes, calendar, filters (an "animal" filter matches every animal kind), thumbnails (decode, shrink, put back), download panel; first against `FakeCore`, then the real core | contract frozen (FakeCore); F1 (real) | unblocked after review | Sonnet |
| M2 | macOS: playback tiles, scrubber, step, offsets | F2 | unblocked at 1× | Sonnet |
| M3 | macOS: follow cut and 2-up/4-up rendering with Metal and VideoToolbox | F3 | unblocked | Opus (first platform renderer) |
| W1–W3 | Windows: the same three, WinUI 3, Media Foundation, Direct3D 11 (FFI-RECORDINGS.md section 12) | as M1–M3 | unblocked after review | Sonnet (ports of M1–M3) |

### Blocked on the re-capture or on a measurement

| ID | Blocked item | Seam until then |
| --- | --- | --- |
| B1 | ~~Rates above 2×~~ (observed: `playSpeed` 1, 2, 4, 8, 16; keyframes only and no audio from 2×; see docs/protocol.md, "Rates, keyframes, and how a play ends") | rates 0.25× to 2× by pacing 1× plays (decision 3); `playSpeed` other than 1 is still never sent, so fast rates are unblocked for a later slice, not built |
| B2 | The device's own time zone: 104 is now known (`timeZone`, `isDst`, local time; current UTC offset is `-timeZone + 3600` when `isDst` is 1, matched on one device). Open: the layouts of 106 (DST rule) and 38 (NTP), which the newer client never read | the app passes `utc_offset_s`; a later slice may compute it from 104 |
| B3 | ~~Animal names in event-search replies~~ (observed: `dog_cat`) | kinds pass through unchanged; the apps' "animal" filter covers `dog_cat` and any later animal kind (decision 10) |
| B4 | ~~Event log and AI search~~ 516–518 and 653 observed (docs/protocol.md, "What the AI search sent on the wire"). Open: 519 and 759 (never sent by that client), status 442 on a 517 page, `pts` and `offset` semantics | not on the allowlist; nothing built; adding 516–518 and 653 needs the allowlist reason recorded in protocol.md |
| B5 | ~~Live and replay media on one session at once~~ (observed: one play, and two simultaneous plays with one of them another camera, beside one live preview were all accepted and delivered, and live view kept its rate; docs/protocol.md, "Live view and plays on one session"). The earlier "second play refused with status 52092" was our probe misreading a replay header (the play that "failed" was the restart of a file stopped moments earlier); the device refused nothing. Still open: live view beside more than two plays, live view beside a download, and any real per-camera or total limit. The misread message was the first play's media tagged with the second seek's `seq` (a fifth run); the session now routes play media by message ID and accepts any recent seek's `seq` | the design keeps live running (decision 2); plays limited by `max_plays_per_device` (default 4) and a refusal (any non-200 answer to 5) ends only that play and is shown as unavailable; downloads already share the session today and the end-to-end test checks live view keeps flowing in the mock. Next: a run with three or four plays and one with a download, with the user's permission |
| B6 | How a play ends at the true end of the recorded data (the official client always stopped first with 7 when its loaded file list ran out, and no device end message was seen); error replies of 143 (5, 123 and 7 were always 200) | C9 reports `ended` when the core's clock passes the last indexed recording, and `refused` for any non-200 status |
| B7 | ~~Whether keyframe seconds are local or UTC~~ (observed UTC, live and replay, in the official client's decrypted capture and nvrview's probe; `seekTime`, file `name` and search times are local; the file `Id` is UTC). The first capture's "equal to `seekTime`" reading is unexplained; UTC is confirmed only on the devices measured | recorded times stay on the keyframes' own clock (`recordings::recorded_time`); conversion to the index's local time (with `utc_offset_s` or 104) is now unblocked |

## Order and parallel agents

```
C1 ─┬─ C2 ── C3 ─┐
    ├─ C4 ───────┼─ C11 ── F1 ── M1, W1
    └─ C5 ───────┘           │
C6 ──────────────────────────┘
C7 ─┬─ C8 ─────────────── F3 ── M3, W3
C9 ─┴─ C10 ─────────────── F2 ── M2, W2
C12 (after C11) ─────────── F3
```

- **Wave 1, in parallel** (separate worktrees; they touch different files):
  C1→C2→C3→C4→C5 in `recordings` (one agent, in order, because they share
  the index and incident types), C6 in `recordings::scheduler` (one agent;
  rebases on C1's `CameraId`), C7 in `bc-protocol::media` and
  `recordings::chunk` (one agent), C9 in `bc-protocol`, `bc-session`, and
  `mock-nvr` (one agent).
  (Done: C1–C7 and C9, in sequence on one branch.)
- **Wave 2**: C11 (needs C1–C6), C10 (needs C7, C9), C8 (needs C7). C10 and
  C11 both touch `viewer-core`; run them in sequence or agree on module
  boundaries first. C8 and C10 can be built on the keyframe clock now, but
  placing pictures and file offsets on the index's (local) timeline waits
  for B7.
- **Wave 3**: F1, then M1 and W1 in parallel; F2 then M2 and W2; F3 then M3
  and W3. The apps can start against `FakeCore` as soon as the contract is
  frozen.
- Merging: branches that both touched `Cargo.lock` get it regenerated, not
  merged textually.

## The re-capture

Done. The capture of the official client with the login included was made
and decrypted; its facts are in docs/protocol.md (marked observed (client
capture)) and in the tables above. What each planned step settled:

1. Plays at 1×, 2×, 4×, 8×, 16× (B1): settled, see B1.
2. Play the most recent recording until it reaches the present (B6): the
   client stopped each play itself when its loaded file list ran out, so the
   true end of data was not reached. Still open.
3. Seek to a time with no recording, and a day with none (B6): the client
   searched the empty day (a normal reply with an empty file list) and sent no
   play, so no error reply was provoked. The client never sends a seek to a
   time without recording; still open.
4. A day with an animal event (B3): settled (`dog_cat`).
5. The Date & Time screen (B2, B7): that client version read only 104, so
   104 is settled and 106 and 38 are still open; B7 is settled by the plays.
6. A live view beside a playback and a download (B5): not in the capture (the
   client stops live view when the playback tab opens). B5 was measured with
   nvrview's own probe instead, see below.
7. AI search (B4): a typed-phrase search, its covers and opening a hit were
   captured; type, danger-level and track searches were not.

B5 is measured with nvrview's own read-only client instead, with the
user's permission to use real hardware: `bc-cli replay-probe` (one session,
one live preview, then 1 to 4 simultaneous plays at 1×, each at most 10 s,
the run capped at 90 s; it stops at the first refusal, error, stall, or
live disruption). Its runs settled live plus one play and live plus two
plays (one of them another camera). Its reported "refusal" was a misread
of a replay header, so its status reading needs fixing before another run
(three or four plays; a download beside live view).

What a further capture would still need: a play left running past the end
of the last recording (B6), a 143 failure, 106 and 38 (a client version or
screen that reads them), 519 and 759, and a repeat of a phrase search that
returns status 442 on a 517 page.
