# Milestone P: work breakdown

How the playback milestone ([PLAYBACK.md](PLAYBACK.md), contract proposal in
[FFI-RECORDINGS.md](FFI-RECORDINGS.md)) is cut into slices, in what order,
which are unblocked now, which wait for a decryptable capture, and which can
run as parallel agents. Protocol facts come only from
[protocol.md](protocol.md); a slice that needs a fact not marked observed or
reported there leaves a documented seam instead of guessing.

**Versioning.** FFI-RECORDINGS.md was written as ABI 4. Its decision 1
replaced that with a minor version, and discovery has since taken minor 1,
so recordings arrive as **ABI 3, minor 2** (`NV_ABI_MINOR` 2), with no
existing declaration changed. "ABI 4" in that document reads as "the
recordings minor".

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
| Rate change is stop, seek, play with `playSpeed` | observed (timing and bytes only) | C9 seam |
| Keyframes carry wall-clock seconds in extra-header bytes 0–3 | observed (live view); matches `seekTime` at replay start (observed) | C7, C10 |
| `playSpeed` values above 1, which frames a fast rate keeps, frames per second per rate | **blocked**: encrypted in the capture | B1 |
| How a play ends at the end of the last file; how 5, 123 and 143 report errors | **unknown**: never seen | C9 seam |
| Live media and replay (or download) media on one session at once | **unmeasured**: the client stops live view in its playback tab | B5 |
| Reply fields of 104, 106, 38 (device time zone, DST, NTP) | **blocked**: encrypted | B2 |
| Event-search (273) type names for animals | **blocked**: encrypted; live push says `dog_cat` | B3 |
| Event log and AI search 516–519, 759, 653 | **reported (client)** only, never on the wire | B4 |

## Slices

IDs are for reference in commits and agent prompts. "Unblocked" means every
fact the slice needs is observed or reported; seams for the rest are named.

### Core (Rust; builds and tests on macOS, Windows, Linux)

| ID | Slice | Depends on | Status | Agent tier |
| --- | --- | --- | --- | --- |
| C1 | **Lens in every key.** `CameraId` gains `lens` (0 or 1, default 0, serde default so old job files load); the index is keyed by channel and lens (index format 2; a version-1 index is a cache and is dropped and refetched); `IndexedChannel` maps lens to stream; thumbnail keys and job slugs include the lens | — | unblocked | Opus (formats) |
| C2 | **Stable incident key** (`key`: group and earliest event, unchanged while the incident grows) and `growing` (end within the gap of the device's now) | C1 | unblocked | Sonnet |
| C3 | **Stored edits and camera groups.** Edit log (merge, split, rename, add or remove a camera, revert) stored atomically, replayed after every regroup, expired with the index's age, capped at 10000; camera groups and camera-name overrides stored like layouts, bounded (64 × 64) | C1, C2 | unblocked | Sonnet |
| C4 | **On-demand index** (decision 8): fill the days a view asks for (calendar months, then files and events per channel and day), refresh today only while a view holds it, no default background pass; revision counter and change ranges for `recordings_changed` | C1 | unblocked | Opus |
| C5 | **One search lane per device with priority.** Thumbnails and user requests before background index work; a thumbnail queue per device, newest request first, at most 256 queued (oldest dropped and reported), cancel by key or all | C1 | unblocked | Opus |
| C6 | **Scheduler control.** Failure reason codes (`stalled`, `session_closed`, `refused`, `no_recording`, `write_failed`, `join_failed`, `login_failed`, `other`) beside the English detail; per-camera pause and retry; `remove_device` (jobs wait as `waiting_device`, also while the device is offline, without spending attempts); `forget` with `delete_partial`; the device's size estimate in progress; at most 1000 known downloads. (Progress is a watch channel, newest state wins; the 4-per-second event limit belongs to the owner, C11) | — | unblocked | Opus |
| C7 | **Recorded time on frames.** Chunk indexes keep each keyframe's wall-clock seconds (the demuxer already kept the extra header); `recordings::recorded_time` refines the offset across keyframes as in live view, and each camera job records its first picture's time. Optional throughout: a stream without the seconds still downloads. **Seam:** times stay on the keyframes' clock, because whether those seconds are device-local or UTC is not established (B7) | — | done (seconds observed on live and replay; on 143 downloads checked only against the mock) | Sonnet |
| C8 | **Edit decision lists** (follow cut, 2-up, 4-up) as pure planners over an incident and its finished files; keyframe positions read from the MP4's sync samples | C7 | unblocked | Sonnet |
| C9 | **Playback on the session.** 123, 5, 7 move to the viewing tier with request builders; `bc-session` gains a play stream (seek, play, media routed by message ID and replay `seq`, bounded queue, stop on every exit path, coalesced restarts); `mock-nvr` serves 123/5/7 at 1× with the observed burst-then-pace and continues across files. Seams: `playSpeed` other than 1 is refused until B1; end of last file and error replies are surfaced as "ended" / "refused" from whatever the mock and later hardware show | — | unblocked at 1× | Opus |
| C10 | **Playback engine** in `viewer-core`: playback tiles on the existing tile pipeline and generations, the shared clock, paced release less decode latency, `wait`/`free` sync, buffer bounds with stop and restart, per-camera anchor refinement and offsets, rates 0.25× to 2× by pacing | C7, C9 | unblocked except B1, B5 | Opus |
| C11 | **Recordings owner in `viewer-core`**: one per core, holding the per-device indexes, the search lanes, the scheduler, the thumbnail cache, the edit log and groups; follows device add, remove, login failure; produces the events of FFI-RECORDINGS.md section 8 | C1–C6 | unblocked | Opus |
| C12 | **Sound for recordings** (section 2a): loudness envelope store per camera and day, labels as events of `source` `local_audio` | C1, C11 | unblocked | Sonnet |

### C interface

| ID | Slice | Depends on | Status | Agent tier |
| --- | --- | --- | --- | --- |
| F1 | Minor 2, part 1: configuration, index and search, incidents and edits, camera groups, thumbnails, downloads (sections 1–5, 8, 9) with the C smoke test and cbindgen header | C11 | unblocked once C11 lands; the user reviews the contract first | Opus for the contract, Sonnet for shaping JSON |
| F2 | Minor 2, part 2: recorded playback (section 6) | C10, F1 | unblocked at 1× | Opus |
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
| B1 | Rates above 2×: `playSpeed` values, which frames are kept, frames per second | rates 0.25× to 2× by pacing 1× plays (decision 3); `playSpeed` other than 1 is never sent |
| B2 | The device's own time zone (104, 106, 38 replies) | the app passes `utc_offset_s` |
| B3 | Animal names in event-search replies | kinds pass through unchanged; the apps' "animal" filter covers `dog_cat` and any later animal kind (decision 10) |
| B4 | Event log and AI search (516–519, 759, 653) | not on the allowlist; nothing built |
| B5 | Live and replay (or download) media on one session at once; how many plays a device allows beside live view | the design keeps live running (decision 2); plays limited by `max_plays_per_device` (default 4); downloads already share the session today and the end-to-end test checks live view keeps flowing in the mock |
| B6 | How a play ends at the end of the last file; error replies of 5, 123, 143 | C9 reports `ended` when the core's clock passes the last indexed recording, and `refused` for any non-200 status |
| B7 | Whether the wall-clock seconds in keyframe extra headers are the device's local time or UTC | recorded times stay on the keyframes' own clock (`recordings::recorded_time`); conversion to the index's local time waits |

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
- **Wave 2**: C11 (needs C1–C6), C10 (needs C7, C9), C8 (needs C7). C10 and
  C11 both touch `viewer-core`; run them in sequence or agree on module
  boundaries first.
- **Wave 3**: F1, then M1 and W1 in parallel; F2 then M2 and W2; F3 then M3
  and W3. The apps can start against `FakeCore` as soon as the contract is
  frozen.
- Merging: branches that both touched `Cargo.lock` get it regenerated, not
  merged textually.

## The re-capture

What the next packet capture of the official client must contain so B1–B3,
B6, and B7 can proceed. Start the capture **before** launching the client (quit
it first), so the logins are in it and the bodies can be decrypted locally;
the capture stays in the git-ignored `captures/` directory.

1. Open the playback tab on one camera; play at 1× for 20 s, then 2×, 4×,
   8×, 16× for at least 20 s each (B1: `playSpeed` values, frames and
   keyframes per rate).
2. Play the most recent recording at 1× until it reaches the present, and
   let it run 30 s more (B6: how a play ends).
3. Seek to a time with no recording, and pick a day with none (B6).
4. Pick a day with an animal event and filter by animal (B3: the type name
   in 273 replies).
5. Open the device's Date & Time screen and close it without saving (B2:
   104, 106, 38 replies). With step 1's plays, this also settles B7: the
   keyframes' seconds against the play's `seekTime` and the device's zone.
6. If the client allows it, keep a live view of another camera open in a
   second window during the playback and a download (B5).
7. If the NVR has it, the AI search screen: a type search, a danger-level
   search, a track, opening one hit and its cover (B4).

B5 can also be measured with nvrview's own read-only client, without the
official app: one session, one live preview plus one play or download, with
the user's permission to use real hardware for that measurement.
