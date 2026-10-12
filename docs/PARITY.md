# Feature parity

The macOS app (`apps/macos`) and the Windows app (`apps/windows`) show the same
product. [UX.md](UX.md) is the requirement; this page says which app has what
today. The rule (CLAUDE.md, "Engineering habits"): a user-visible feature lands
on both apps, or this table says why not, and the table is updated in the same
change.

Values: `yes`, `no`, `no (planned)` (designed, not started), `partial (what
is missing)`, `n/a (why)`. Each cell was
read from the code on 2026-10-11 (menus, settings, gesture handlers, the call
and alert code), not only from the docs; "Notes" says where a cell rests on
less than that. Nothing here was run on a Windows machine.

## Features

### Layouts, windows, and the sidebar

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Split-pane layouts: split, drag dividers, drag a tile to swap, remove | yes | yes | Windows: Edit Layout (Ctrl+E) or the right-click menu; macOS: Layout menu and right-click menu. |
| Layout presets ("Start From" shapes) in Edit Layout | yes | yes | macOS: the edit bar's Start From pop-up; Windows: the edit bar and a "Layout preset" submenu in the tile menu. Unsure whether the two preset lists are identical. |
| Saved layouts, edits saved as they are made | yes | yes | Same JSON documents on both. |
| Layout rename, duplicate, delete | yes | yes | Sidebar context menu. |
| Groups (named sets of cameras shown as an even grid) | yes | yes | |
| All Cameras (unsaved grid following the devices) | yes | yes | |
| New Layout from Template (shapes with icons) | yes | yes | |
| Layout icons | yes | yes | |
| Several windows, any camera in any window, streamed once | yes | yes | |
| Open a layout, group, device, or camera in a new window | yes | yes | |
| Window restore (frame, content, sidebar state) | yes | yes | Windows self-test step `window-restore`; macOS covered by unit tests only. |
| Sidebar: devices, channels, detection symbols per camera | yes | yes | |
| Sidebar collapse and edge reveal with pin | yes | yes | Same timing (`SidebarReveal` on both). |
| Hide offline channels | yes | yes | macOS: View menu; Windows: checkbox in the sidebar. |
| Fill Tiles (picture fills the tile instead of fitting) | yes | yes | Per window, saved with the window (windows.json `fill`). macOS: Layout menu (mixed while tiles differ), `state.views`. Windows: the header's "Fill tiles" toggle (shows the window's own setting; no mixed state), `WindowState.Views`, each tile's `StageTile.WantsFill` read by the renderer; the 1x crop is `ZoomBase.Of(..., fill: true)` drawn through the zoom rule. UX.md "Fill Tiles". Self-test steps `fill-tiles` (Windows only), `fill-window-switch` on both. |
| Fill Tile per camera (tile menu, checked; the window's switch clears the choices) | yes | yes | Same rule and shared vectors (`TileViews` in NVRCore and NvrView.Tiles, `fixtures/synthetic/tile-views.json`). macOS: right-click menu and the header's "…" (`TileHeaderLayout` item `fill-tile`); Windows: right-click menu "Fill tile" (the Windows header does not use `TileHeaderLayout` yet). Self-test step `fill-per-tile`. |
| Drag pans a filled picture's crop at 1x, stopping at its edges | yes | yes | `TileZoom` at scale 1 on the filled base (same vectors). Pointer: open and closed hand on macOS, the hand pointer on Windows. Self-test step `fill-pan-drag` (through the pointer handlers' own entry points). |
| Drag on the shown tile header swaps tiles (every tile) | yes | yes | Picture drags still swap tiles with nothing hidden; Edit Layout swaps anywhere. Self-test step `fill-pan-header-swap`. |
| Zoom, crop and Fill choice per camera saved with the window and restored | yes | yes | windows.json `tileViews` (camera, scale, x, y, fill; at most 256 per window, clamped on read, damaged entries skipped; same keys on both). Reset Position (Windows "Reset position") in the tile menu; 0 and the reset control also recenter. On Windows the zoom is now per camera in a window (it was per tile). Self-test step `fill-pan-restore`. |
| Menu bar | yes | n/a (Windows draws a header in its title bar, with a trimmed right-click menu and a help overlay) | Platform difference by owner decision: macOS keeps its menu bar; Windows has the header (layout picker, quick actions, Settings, a "…" menu) and a separate Settings window. UX.md "Windows header", "Settings window". Windows self-test steps `header-*`, `settings-window-*`, `tile-menu-no-app-wide` are allowlisted as n/a. |
| Keyboard shortcut help | yes | yes | macOS lists keys in the menus; Windows has the F1 overlay. |

### Stage, focus, and listening

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Focus a tile (animated), main stream after the aligned switch | yes | yes | |
| Full screen | yes | yes | System transition on macOS; the window's full-screen presenter on Windows. |
| Full screen chrome and pointer auto-hide after 3 s | yes | yes | Alert headers and indicators stay. |
| Keys: F, S, arrows, Space, 0, M | yes | yes | |
| Enter dismisses an alert, else focuses | yes | yes | |
| Listen from any tile (speaker button, M, menu) | yes | yes | Windows: Ctrl+click listens to several; macOS: a modifier. |
| Level meter on the audible tile, mono meter on hover | yes | yes | UX.md also describes an optional edge meter column; neither app's code has one (a doc item that was never built, or not found). |
| Retry button after 30 s without a picture | yes | yes | Both on the viewer clock. |
| Tile status in words (connecting, reconnecting with countdown, waiting) | yes | yes | |
| Frame rate display | yes | partial (diagnostics overlay, Ctrl+D) | macOS: View > Show Frame Rates; the Windows overlay is a different readout. |
| Digital zoom: mouse wheel, drag to pan | yes | yes | |
| Zoomed picture grows over the bars until it fills the tile (or the full-screen stage), then crops | yes | yes | Same geometry and test vectors (TileZoom in NVRCore and NvrView.Tiles); self-test step `zoom-fills-tile` on both. Fill Tiles uses the same rule from the filled base on both. |
| Trackpad or touchpad pinch zooms | yes | yes | Windows: a touchpad pinch arrives as Ctrl+wheel; a touch screen's two-finger pinch zooms around the point between the fingers (`StageView.PinchDigital`, steps bounded as the Virtual PTZ pinch is). Windows self-test step `zoom-touch-pinch`. |
| Zoom map (outline of the visible region) | yes | yes | macOS `ZoomInsetView`; Windows a map in `TileChrome` placed by `ZoomMap.Frame` (same rule: bottom left of the zoomed picture as drawn, at most 96 wide, a quarter of its width, a third of its height), fading with the chrome. Windows self-test step `zoom-map`. |
| Reset zoom control in the tile header | yes | yes | A magnifier button at the header's right, shown while the tile is zoomed (or a virtual camera moved); also resets the virtual camera, as 0 does. Windows self-test step `zoom-reset-control`. |
| Quality per tile: Auto, Clear, Fluent, Balanced; saved in the layout | yes | yes | Auto follows zoom past 2x on both. |
| Second lens of a multi-lens camera as its own tile | yes | yes | |

### Fisheye

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Views: Original, Panorama, Double panorama, Quad, Virtual PTZ | yes | yes | Saved in the layout leaf on both. |
| Image circle found automatically | yes | yes | Both self-tests check it within 1% of the synthetic fixture. |
| Mount (ceiling, wall, desk) in the tile menu | yes | yes | |
| Mount picker in Settings | yes | yes | macOS: Settings > Cameras; Windows: Settings window, Cameras section. |
| Drag aims or turns the view | yes | yes | |
| Wheel or pinch zooms the Virtual PTZ view | yes | yes | Windows also pinches with two fingers on a touch screen. |
| Trackpad two-finger scroll turns the view | yes | n/a (no precise-scroll gesture on Windows) | |
| 0 resets the virtual camera | yes | yes | |

### Alerts, indicators, and detections

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Corner indicators per tile, with 2 s hold | yes | yes | |
| Indicator corner setting | yes | yes | macOS: Detection Indicators menu; Windows: Settings > Alerts & calls and the header's "…" menu. |
| Indicator symbols on sidebar rows | yes | yes | |
| Device detections (motion, person, vehicle, animal, face, package, visitor) | yes | yes | From the core. |
| Local sound detection (smoke and CO alarm) badge | yes | yes | Sound analysis is in the core (`viewer-core`); Windows shows a slate badge at the bottom left as well. |
| Tile alert: red border (steady with reduced motion), header with reason, count, times, Dismiss | yes | yes | |
| Alert survives the stop; Enter or Dismiss clears it on every lens | yes | yes | |
| Session event log of alerts | yes | yes | Not verified where each app shows it. |
| Alert insets for cameras not on screen (doorbell, smoke, entrance people) | yes | partial (calls only: a call card with the doorbell's live picture) | Windows has no smoke or entrance insets (UX.md, apps/windows/README.md "Calls"). |
| Inset picture drawn by the window's own renderer, last, from the shared tile | yes | yes | Self-test step `alert-inset-in-grid`; Windows also checks it after a device loss (`call-inset-device-loss`). |
| Inset corner setting (Settings, else opposite the indicators) | yes | yes | macOS: Settings > Alerts; Windows: Settings ("Doorbell call cards"). Windows step `call-inset-corner`. |
| Entrance option per camera | yes | no | Depends on alert insets for detections. |
| Movable insets: drag the column, 24 pt edge snap, position per window, Reset Position | yes | yes (call cards) | UX.md "Alert insets". `InsetPlacement` (NVRCore, NvrView.Tiles) with the same test vectors; saved as `insetAnchor` in windows.json (`SavedWindow.InsetX/InsetY` on Windows); self-test steps `inset-drag-snap`, `inset-drag-resize`, `inset-drag-restore`, `inset-drag-multi` on both. The Windows steps drive the card's own press/move/release code with synthetic points (pointer events are not injected), and Reset Position through the method its menu item calls; a click on a card's picture does nothing on Windows (no swap). |

### Calls, talk-back, and captions

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Doorbell press opens a call with the doorbell's live video | yes | yes | Windows: a call card with the picture, in the inset corner. |
| Call controls over the stage when the doorbell fills it (focused or only tile) | yes | yes | Windows step `call-stage-controls`. |
| Start Conversation / End Conversation without a press | yes | yes | Tile menu on both. |
| Unmute control for a silent call | yes | yes | |
| Talk (hold or Keep talking), microphone through the camera speaker | yes | yes | Rate limited and logged by the core on both. Letting go drains: the core sends a 1.5 s silent tail before closing, so the last words play. |
| Echo cancellation while talking (full duplex), "Echo cancellation on" / "No echo cancellation" in the call, half-duplex caption gate as the fallback | yes | yes | AUDIO.md "Echo cancellation". macOS: voice processing on the playback engine; Windows: communications capture with the canceller's reference set to the playback device, confirmed through the effects list. Shared rules in `EchoCancellation` (NVRCore, NvrView.Tiles) with the same test vectors. Self-test step `call-echo-cancellation` checks the configuration with a fake microphone (macOS also the captions with a fake doorbell level); whether echo is really removed was not verified by ear. |
| No talk-back limit while a call is open (20 starts a minute otherwise) | yes | yes | Both apps call `nv_talk_set_conversation` when the call opens (and on each start) and when it ends. Self-test step `call-talk-conversation`. |
| Allow talk-back per camera (off by default) | yes | yes | macOS: Settings > Cameras; Windows: tile menu and Settings > Cameras. |
| "Allow talk-back for <camera>?" prompt on Talk or Send when it is off (Allow saves and continues, Cancel sends nothing and keeps the typed text) | yes | yes | Self-test step `call-talk-permission-prompt`. macOS: alert sheet; Windows: ContentDialog. |
| Typed reply spoken by the system voice | yes | yes | |
| Live captions of both sides | yes | partial (older desktop recognizer; none where Windows lacks one for the language) | macOS: Apple SpeechTranscriber; Windows: `System.Speech` stand-in for the planned Moonshine engine. |
| Caption echo filter: the visitor caption that repeats our own talk-back or typed reply (the doorbell hears its speaker) is hidden, not saved, not activity | yes | yes | UX.md "Captions and echo". Pure `CaptionEcho` (NVRCore) and `CaptionEcho` (NvrView.Tiles) with the same test vectors; self-test step `call-caption-echo`. |
| Call ends after 60 s of inactivity, gives audio back | yes | yes | |
| Unmute on ring even when not listening (per doorbell) | yes | yes | macOS: Settings > Cameras (`CameraOptions.unmuteOnRing`). Windows: Settings, "Unmute on ring even when not listening, per doorbell" (a check box per camera), kept in `view-settings.json` as `unmuteOnRing`; `CallAudio.Press`. Windows self-test step `unmute-on-ring`. |
| Save call transcripts (event log and one text file per call, bounded folder, Open Transcripts Folder) | yes | yes | UX.md "Transcript files". macOS: Settings > Alerts; Windows: Settings. Pure naming, text and pruning in `CallTranscriptFile` (NVRCore) and `TranscriptFiles` (NvrView.Tiles) with the same test vectors. Self-test step `call-transcript-file`. |
| Swap an inset with the stage (click, Tab, Esc) | yes | no | Depends on alert insets. |
| Call pop-out window (Pop Out / Pop In, the call kept whole) | yes | yes | UX.md "A call in its own window". macOS: a non-activating panel, on top with a remembered pin. Windows: a WinUI window with its own swap chain and `StageRenderer` (joins device-loss recovery; checked after a simulated loss), always on top while pinned, shown without activation for a ring. Self-test steps `call-popout-open`, `call-popout-talk`, `call-popin`, `call-popin-new-window`, `call-popout-end` on both. A call in its own window never ends by inactivity; Pop In restarts the minute. Unverified on Windows: real multi-monitor placement, focus behavior of a real ring, and always-on-top over a full-screen app. |
| Open calls in their own window: global setting and per-doorbell choice | yes | yes | macOS: Settings > Alerts > Doorbell calls and Settings > Cameras ("Calls open in"). Windows: Settings (Alerts & calls: the checkbox; Cameras: a picker per channel) and the tile menu ("Calls open in"), kept in `view-settings.json`. `CallPresentation.resolve` / `CallPresentations.Resolve` (unit tests); self-test steps `call-default-ring`, `call-default-conversation`. |
| Call window placement (where I left it, or top right of the active screen) | yes | yes | Settings > "Open call windows" on both; `CallWindowPlacement` (unit tests) and self-test step `call-window-frame`. Windows keeps the frame in physical pixels. |

### Tile header, capture, and camera motion

Planned in [CAPTURE.md](CAPTURE.md). `no (planned)` means designed and not
started on that app.

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Responsive tile header: priorities, "…" overflow, right-click menu from the same model | yes | no (planned) | UX.md "Tile header"; shared `TileHeaderLayout` with test vectors. macOS: `TileChromeView`, with Save picture, Record and the PTZ buttons in place but saying "coming soon"; Start Conversation and PTZ buttons from the `channels` capabilities. Self-test IDs `header-priority-narrow`, `header-overflow-menu` (macOS only for now). |
| Call window picture as a regular tile (zoom, menu, header) | yes | no (planned) | UX.md "A call in its own window". macOS: `StageView` in single fixed tile mode inside the call window. IDs `call-window-tile-zoom`, `call-window-tile-menu` (macOS only for now). |
| Keys P, Shift+P, R, T, Option/Alt+arrows, Option/Alt with = - and 1–9 | partly | no (planned) | UX.md "Capture and camera keys". macOS: P, Shift+P, R, T, ⇧⌘P, ⇧⌘R and the Camera menu (T works; the others say "coming soon"; no Option keys yet); Windows: the F1 overlay. |
| Save picture (full frame, zoomed area, fisheye view or original circle; PNG or JPEG; toast) | no (planned) | no (planned) | UX.md "Save picture". IDs `capture-picture-tile`, `capture-picture-zoomed`, `capture-picture-fisheye`, `capture-picture-call`, `capture-picture-text-field`. |
| Live recording to MP4 (Record from Main or Sub-stream, caps, disk guard, segments, recovery) | no (planned) | no (planned) | UX.md "Recording". IDs `capture-record-tile`, `capture-record-cap`, `capture-record-disk-low`, `capture-record-reconnect`, `capture-record-survives-tile-close`. |
| Recording indicators (tile pill, call bar pill, window list) | no (planned) | no (planned) | macOS: toolbar item and Camera > Recordings; Windows: "● N recording" in the window header. ID `capture-record-indicator`. |
| Recording calls (Conversation, Visitor, You tracks; consent note; beside the transcript) | no (planned) | no (planned) | UX.md "Recording calls", AUDIO.md "Recording calls". IDs `capture-call-record-button`, `capture-call-record-auto`, `capture-encoder-priming`. |
| PTZ pad (press and hold, speed), presets, lens zoom and focus | no (planned) | no (planned) | UX.md "Pan, tilt, and zoom". IDs `ptz-pad-hold`, `ptz-deadman`, `ptz-preset-recall`, `ptz-lens-clamp`. |

### Recordings (milestone P)

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Recordings window (Window > Recordings, ⌥⌘R) | yes | no (planned) | UX.md "Recordings"; Windows is W1 (MILESTONE-P.md). The macOS self-test runs it on the fake core and, with `scripts/macos-real-selftest.sh`, on the real core against the mock (`recordings-real-core`). |
| Cameras to search, per device and per camera, kept between launches | yes | no (planned) | ID `recordings-enable`. |
| Days asked for as shown; today held per window while shown, released on another day or close | yes | no (planned) | IDs `recordings-hold-today`, `recordings-navigate-day`, `recordings-window-close`. |
| Calendar of days with recordings; refresh; a failed day marked | yes | no (planned) | ID `recordings-calendar-lanes`. The failed-day mark is not in the self-test. |
| Timeline lanes per camera (spans, events by kind with badges, zoom, click selects the incident) | yes | no (planned) | ID `recordings-calendar-lanes`. |
| Incident list with kind filters (Animals matches every animal kind), camera or group scope, Locked | yes | no (planned) | ID `recordings-animal-filter`. |
| Cover thumbnails: keyframe decoded, shrunk, put back into the core's cache | yes | no (planned) | VideoToolbox and ImageIO; Windows: Media Foundation and WIC. ID `recordings-thumbnail`. |
| Incident edits: rename, merge, split, add and remove a camera, revert | yes | no (planned) | ID `recordings-edits`. |
| Lock and unlock (the video is downloaded; says when it is not kept) | yes | no (planned) | ID `recordings-lock`. |
| Downloads panel: progress, pause, resume, retry, Show in Finder, forget, Move to Trash | yes | no (planned) | Trash through `NSWorkspace.recycle` after `download_removed`; Windows: the Recycle Bin (FFI-RECORDINGS.md section 12). ID `recordings-downloads`. Retry is not in the self-test. |
| Camera groups for incidents | yes | no (planned) | ID `recordings-groups`. |

### Devices, settings, and updates

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Add, edit, remove devices; password in the OS store | yes | yes | Keychain on macOS, Credential Manager on Windows. |
| Reconnect a device | yes | yes | |
| Find Devices on This Network (list, Added, Not supported yet, Search Again) | yes | yes | Windows may raise a Firewall prompt. |
| Settings: Devices | yes | yes | |
| Settings: Cameras (per-camera options) | yes | yes (calls open in, unmute on ring, talk-back, fisheye mount) | Windows: Settings window, Cameras section. |
| Settings: Alerts (inset corner, save transcripts, call windows) | yes | yes (in the Devices dialog: the call cards' corner, saving transcripts, calls in their own window and where they open) | |
| Settings: Updates (source, trusted keys, automatic checks, Check Now) | yes | yes | |
| Check for Updates in a menu | yes | yes | |
| Self-update from a signed folder or server | yes | yes | Windows unpacks into a new folder next to the running one. |
| Bundled update key and source (`update-defaults.json`; shown as "Bundled with this build" / "Bundled default") | yes | yes | Self-test step `updates-bundled-defaults`. macOS: in the signed bundle's Resources; Windows: next to the exe. |
| Hardware decoding, GPU rendering | yes | yes | VideoToolbox and Metal; Media Foundation and D3D11. |
| Message when the OS lacks an HEVC decoder | n/a (built in) | yes | Windows needs the HEVC Video Extension. |
| Recorded-video playback (synchronized cameras, scrubber) | no (planned) | no (planned) | Milestone P, M2 and W2; [PLAYBACK.md](PLAYBACK.md) is the design. Finding and downloading recordings is under "Recordings". |

## Open gaps

What the table shows that is not just a platform difference:

- Windows insets are call cards only: no smoke, CO or entrance insets, no
  swap with the stage, and no Entrance option.
- The layout preset lists on the two apps were not compared.
- Windows' captions use an older engine than the Mac's.

## The self-test comparison

Both apps have a `--selftest` built from numbered steps
(`apps/macos/Sources/NVRView/SelfTest.swift`,
`apps/windows/NvrView.App/SelfTest.cs`). Each step carries a stable ID in a
comment directly above its code:

```text
// parity: fisheye-circle
```

An ID is a short kebab-case string; the steps that check the same thing on the
two apps share one. `python3 -I scripts/check-parity.py` (part of
`scripts/check.sh`, right after the sanitization step) collects the IDs and
fails on an ID present in one app only unless `scripts/parity-allow.txt` lists
it as `id  platform  reason`, with the platform that has the step and a reason
starting `n/a:` (it makes no sense on the other platform) or `gap:` (the other
app should get it). A stale allowlist entry fails too. `--list` prints the
matrix.

A new self-test step gets an ID in the same change, a new user-visible feature
gets a row above, and a gap that is closed loses its allowlist line. The
headless demos (`NvrViewDemo`, `NvrView.Demo`) test the renderers, not the
apps, and are not compared.
