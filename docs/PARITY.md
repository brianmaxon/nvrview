# Feature parity

The macOS app (`apps/macos`) and the Windows app (`apps/windows`) show the same
product. [UX.md](UX.md) is the requirement; this page says which app has what
today. The rule (CLAUDE.md, "Engineering habits"): a user-visible feature lands
on both apps, or this table says why not, and the table is updated in the same
change.

Values: `yes`, `no`, `partial (what is missing)`, `n/a (why)`. Each cell was
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
| Fill Tiles (picture fills the tile instead of fitting) | yes | no | macOS Layout menu, `state.fill`. Windows has no equivalent. |
| Menu bar | yes | n/a (Windows uses the right-click menu and a help overlay) | UX.md "Windows notes". |
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
| Zoomed picture grows over the bars until it fills the tile (or the full-screen stage), then crops | yes | yes | Same geometry and test vectors (TileZoom in NVRCore and NvrView.Tiles); self-test step `zoom-fills-tile` on both. Windows has no Fill Tiles, so only the fitted case applies there. |
| Trackpad or touchpad pinch zooms | yes | partial (touchpad pinch arrives as Ctrl+wheel and works; a touch screen's two-finger pinch does not zoom digital zoom) | UX.md "Windows notes". |
| Zoom map (outline of the visible region) | yes | no | `ZoomInsetView` exists on macOS only. |
| Reset zoom control in the tile header | yes | partial (0 key and the right-click "Reset zoom" item only) | |
| Quality per tile: Auto, Clear, Fluent, Balanced; saved in the layout | yes | yes | Auto follows zoom past 2x on both. |
| Second lens of a multi-lens camera as its own tile | yes | yes | |

### Fisheye

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Views: Original, Panorama, Double panorama, Quad, Virtual PTZ | yes | yes | Saved in the layout leaf on both. |
| Image circle found automatically | yes | yes | Both self-tests check it within 1% of the synthetic fixture. |
| Mount (ceiling, wall, desk) in the tile menu | yes | yes | |
| Mount picker in Settings | yes | yes | macOS: Settings > Cameras; Windows: Settings dialog, "Fisheye cameras". |
| Drag aims or turns the view | yes | yes | |
| Wheel or pinch zooms the Virtual PTZ view | yes | yes | Windows also pinches with two fingers on a touch screen. |
| Trackpad two-finger scroll turns the view | yes | n/a (no precise-scroll gesture on Windows) | |
| 0 resets the virtual camera | yes | yes | |

### Alerts, indicators, and detections

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Corner indicators per tile, with 2 s hold | yes | yes | |
| Indicator corner setting | yes | yes | macOS: Detection Indicators menu; Windows: Settings and right-click menu. |
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
| Movable insets: drag the column, 24 pt edge snap, position per window, Reset Position | yes | no (planned) | UX.md "Alert insets". macOS: `InsetPlacement` (unit tests), saved as `insetAnchor` in windows.json; self-test steps `inset-drag-snap`, `inset-drag-resize`, `inset-drag-restore`, `inset-drag-multi`. |

### Calls, talk-back, and captions

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Doorbell press opens a call with the doorbell's live video | yes | yes | Windows: a call card with the picture, in the inset corner. |
| Call controls over the stage when the doorbell fills it (focused or only tile) | yes | yes | Windows step `call-stage-controls`. |
| Start Conversation / End Conversation without a press | yes | yes | Tile menu on both. |
| Unmute control for a silent call | yes | yes | |
| Talk (hold or Keep talking), microphone through the camera speaker | yes | yes | Rate limited and logged by the core on both. Letting go drains: the core sends a 1.5 s silent tail before closing, so the last words play. |
| No talk-back limit while a call is open (20 starts a minute otherwise) | yes | yes | Both apps call `nv_talk_set_conversation` when the call opens (and on each start) and when it ends. Self-test step `call-talk-conversation`. |
| Allow talk-back per camera (off by default) | yes | yes | macOS: Settings > Cameras; Windows: tile menu. |
| "Allow talk-back for <camera>?" prompt on Talk or Send when it is off (Allow saves and continues, Cancel sends nothing and keeps the typed text) | yes | yes | Self-test step `call-talk-permission-prompt`. macOS: alert sheet; Windows: ContentDialog. |
| Typed reply spoken by the system voice | yes | yes | |
| Live captions of both sides | yes | partial (older desktop recognizer; none where Windows lacks one for the language) | macOS: Apple SpeechTranscriber; Windows: `System.Speech` stand-in for the planned Moonshine engine. |
| Call ends after 60 s of inactivity, gives audio back | yes | yes | |
| Unmute on ring even when not listening (per doorbell) | yes | no | Windows README "Not yet on Windows". |
| Save call transcripts (event log and one text file per call, bounded folder, Open Transcripts Folder) | yes | yes | UX.md "Transcript files". macOS: Settings > Alerts; Windows: Settings. Pure naming, text and pruning in `CallTranscriptFile` (NVRCore) and `TranscriptFiles` (NvrView.Tiles) with the same test vectors. Self-test step `call-transcript-file`. |
| Swap an inset with the stage (click, Tab, Esc) | yes | no | Depends on alert insets. |
| Call pop-out window (Pop Out / Pop In, the call kept whole) | yes | no (planned) | UX.md "A call in its own window". macOS: a non-activating panel, on top with a remembered pin; self-test steps `call-popout-open`, `call-popout-talk`, `call-popin`, `call-popout-end`. A call in its own window never ends by inactivity; Pop In restarts the minute (the Windows call model already has the rule). |
| Open calls in their own window: global setting and per-doorbell choice | yes | no (planned) | macOS: Settings > Alerts > Doorbell calls and Settings > Cameras ("Calls open in"); `CallPresentation.resolve` (unit tests); self-test steps `call-default-ring`, `call-default-conversation`. |
| Call window placement (where I left it, or top right of the active screen) | yes | no (planned) | macOS: Settings > Alerts > Doorbell calls ("Open call windows"); `CallWindowPlacement` (unit tests) and self-test step `call-window-frame`. |

### Devices, settings, and updates

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Add, edit, remove devices; password in the OS store | yes | yes | Keychain on macOS, Credential Manager on Windows. |
| Reconnect a device | yes | yes | |
| Find Devices on This Network (list, Added, Not supported yet, Search Again) | yes | yes | Windows may raise a Firewall prompt. |
| Settings: Devices | yes | yes | |
| Settings: Cameras (per-camera options) | yes | partial (fisheye mount only; talk-back is in the tile menu) | |
| Settings: Alerts (inset corner, save transcripts, call windows) | yes | partial (the call cards' corner and saving transcripts, in Settings; no call windows) | |
| Settings: Updates (source, trusted keys, automatic checks, Check Now) | yes | yes | |
| Check for Updates in a menu | yes | yes | |
| Self-update from a signed folder or server | yes | yes | Windows unpacks into a new folder next to the running one. |
| Hardware decoding, GPU rendering | yes | yes | VideoToolbox and Metal; Media Foundation and D3D11. |
| Message when the OS lacks an HEVC decoder | n/a (built in) | yes | Windows needs the HEVC Video Extension. |
| Recorded-video playback | no | no | Out of scope for now (ARCHITECTURE.md); [PLAYBACK.md](PLAYBACK.md) is the design. |

## Open gaps

What the table shows that is not just a platform difference:

- Windows insets are call cards only: no smoke, CO or entrance insets, no
  swap with the stage, and no Entrance option.
- Windows lacks "unmute on ring".
- Windows has no zoom map, no header reset-zoom control, no Fill Tiles, and
  digital zoom does not follow a touch screen's pinch.
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
