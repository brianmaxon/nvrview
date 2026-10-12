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
| Fill Tile per tile (tile menu, checked; the window's switch clears the choices) | yes | yes | Same rule and shared vectors (`TileViews` in NVRCore and NvrView.Tiles, `fixtures/synthetic/tile-views.json`). macOS: right-click menu and the header's "…" (`TileHeaderLayout` item `fill-tile`); Windows: right-click menu "Fill tile" (the Windows header does not use `TileHeaderLayout` yet). Self-test step `fill-per-tile`. |
| Drag pans a filled picture's crop at 1x, stopping at its edges | yes | yes | `TileZoom` at scale 1 on the filled base (same vectors). Pointer: open and closed hand on macOS, the hand pointer on Windows. Self-test step `fill-pan-drag` (through the pointer handlers' own entry points). |
| A press-drag-release pans a zoomed or filled picture continuously, 1:1 with the pointer, until release | yes | yes | Windows: the drag goes on while the button is down even when capture is lost (the hover header appearing under the pointer) or a mouse drag starts a manipulation (only a touch pinch replaces a drag); a move without the button ends it. Self-test step `pan-continuous` (Windows through the handlers' pointer path: 60 moves with a manipulation start, the header shown and capture lost midway; WinUI cannot take real pointer events from inside the app without moving the user's pointer; macOS through the mouse handlers). |
| The tile's frame (outline, header) is the tile's whole area, zoomed or not, and the whole stage in focus and full screen | yes | yes | macOS: the chrome is the leaf rectangle. Windows: `TileChrome` follows `StageTile.Frame*` (the tile's area, `View*`), not the fitted picture. Self-test steps `tile-frame-zoom`, `tile-frame-focus`. |
| Drag on the shown tile header swaps tiles (every tile) | yes | yes | Picture drags still swap tiles with nothing hidden; Edit Layout swaps anywhere. Self-test step `fill-pan-header-swap`. |
| Zoom, crop and Fill choice per tile (two tiles of one camera are independent views), saved with the window and restored | yes | yes | Keyed by window content, tile identity and camera (`TileKey` on macOS, `TileViewKey` on Windows; the layout leaf's `tile` member, `assignTileIDs` / `TileIds.Assign` with the shared `tileIDs` vectors). A swap moves each view with its camera; focus, layout switches and stream switches keep it; a new camera in the tile starts at the default. windows.json `tiles` (layout, tile, camera, scale, x, y, fill; at most 256 per window, clamped on read, damaged entries skipped; same keys on both); the old per-camera `tileViews` is read once and migrated to the tiles showing each camera (shared `migrate` vectors). Reset Position (Windows "Reset position") in the tile menu; 0 and the reset control also recenter. Self-test steps `fill-pan-restore`, `zoom-per-tile`, `zoom-follows-swap`. |
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
| Responsive tile header: priorities, "…" overflow, right-click menu from the same model | yes | yes | UX.md "Tile header"; shared `TileHeaderLayout` with test vectors. macOS: `TileChromeView`; Windows: `TileChrome` and `StageView.Header.cs`. The PTZ buttons are in place on both but say "coming soon"; Record and Save picture work on both; Start Conversation and PTZ buttons from the `channels` capabilities. Windows differences: Entrance is unavailable (no entrance insets), Ctrl-click on the Speaker, no Camera menu. Self-test IDs `header-priority-narrow`, `header-overflow-menu`. |
| Call window picture as a regular tile (zoom, menu, header) | yes | yes | UX.md "A call in its own window". Both: `StageView` in single fixed tile mode inside the call window (Windows: `CallWindow.Stage`, one renderer per window). IDs `call-window-tile-zoom`, `call-window-tile-menu`. |
| Keys P, Shift+P, R, T, Option/Alt+arrows, Option/Alt with = - and 1–9 | partly | partly | UX.md "Capture and camera keys". macOS: P, Shift+P, R, T, ⇧⌘P, ⇧⌘R and the Camera menu (all of them work; no Option keys yet). Windows: P, Shift+P, R, T, Ctrl+Shift+P and Ctrl+Shift+R through `TileKeyMap` (all work; Ctrl+Shift+R records the selected tile's call while its call is open, and is R otherwise), no Camera menu, no Alt keys yet. |
| Save picture (full frame, zoomed area, fisheye view or original circle; PNG or JPEG; toast) | yes | yes | UX.md "Save picture". macOS: P, Shift+P, header button, menus, Settings > Capture. Windows: P, Shift+P, Ctrl+Shift+P, header button, menus (`StageView.Capture.cs`), Settings > Capture (folder picker, format, share-safe names, kept in view-settings.json); drawn offscreen by D3D11 (`PictureRender`, a renderer of its own on the shared device), encoded with WIC. Both read the naming vectors `fixtures/synthetic/capture-names.json` (generated by `crates/capture/tests/name_vectors.rs`) in their pure-rule tests (`PictureNaming`, `PictureRegion`). Windows differences: no shutter flash; Save Picture As opens the system picker at the Pictures library (the API cannot start in another folder); the toast button is "Show in folder". IDs `capture-picture-tile`, `capture-picture-zoomed`, `capture-picture-fisheye` on both (the Windows steps are in `SelfTest.Capture.cs`); `capture-picture-call`, `capture-picture-text-field` on macOS only. |
| Live recording to MP4 (Record from Main or Sub-stream, caps, disk guard, segments, recovery) | yes | yes | UX.md "Recording". The core writes the files (K4, F5); macOS: R, the header's Record, the tile menus (Record Main Stream, Record Sub-stream), the Camera menu, Stop All, Open Recordings Folder, Settings > Capture (`CaptureController`, the pure `CaptureSettings`, `RecordingText`, `RecordingBoard` with unit tests). The AAC encoder is `AACCaptureEncoder` (AVAudioConverter; a decode round trip in the unit tests). IDs on macOS: `capture-record-tile`, `capture-record-disk-low`, `capture-record-main-limit` (the sub-stream offered), `capture-encoder-priming`, and `capture-record-real-core` (the real core against the mock, the file read back with AVFoundation). Not in the self-test yet: `capture-record-cap`, `capture-record-reconnect`, `capture-record-survives-tile-close` (the core's tests cover caps, gaps and lifetime). Windows: R, Ctrl+Shift+R, the header's Record, the "…" and right-click menus (Record Main Stream, Record Sub-stream), Stop all recordings and Open recordings folder in the window header's "…" menu (Windows has no Camera menu), Settings > Capture (`CaptureController`, `CaptureSettingsPage`, the same `CaptureSettings`, `RecordingText` and `RecordingBoard` in NvrView.Shell with the macOS vectors). The AAC encoder is `AacCaptureEncoder` (the AAC encoder MFT; its delay is measured through the system's AAC decoder, `AacRoundTrip`). The Windows self-test runs on the real core against the mock, so every recording step reads the file back (`Mp4Inspector`: tracks, keyframe start, duration within 0.5 s of the events; the AAC samples are counted, not decoded). Same IDs on Windows; the encoder step also decodes a 1 kHz tone and checks pitch and level. Windows differences: "Record from" and the limits are a combo box and number boxes; the sixth Settings section is Capture. |
| Recording indicators (tile pill, call bar pill, window list) | yes | yes | macOS: the "● REC" pill on every tile of the camera (outside the hover header, so it stays), the call bar's "● Recording 0:42", a toolbar item "● N recording" with Stop and Stop All, and Camera > Recordings; Windows: "● N recording" in the window header. ID `capture-record-indicator`. Windows: the pill sits in the corner opposite the corner indicators (the top left instead of the header's top right, `RecordingIndicator.PillCorner`) and the title starts after it; the call bar (inset and call window) shows "● Recording 0:42"; the window header's "● N recording" is a button whose menu stops one recording or all and opens the folder. |
| Recording calls (Conversation, Visitor, You tracks; consent note; beside the transcript) | yes (no per-doorbell choice) | yes (no per-doorbell choice) | UX.md "Recording calls", AUDIO.md "Recording calls". macOS: the call window's Record and ⇧⌘R, "Record calls" in Settings > Alerts with the consent note (one setting for every doorbell; the per-doorbell Default, Always, Never is not built), the recording and the transcript share their stem. IDs `capture-call-record-button`, `capture-call-record-real-core` (four tracks, only Conversation enabled, all decoding, ended with the call); `capture-call-record-auto` is not in the self-test. Windows: the call window's Record and Ctrl+Shift+R (also in a viewer window while the selected tile's call is open), "Record calls" in Settings > Alerts & calls with the consent dialog, the recording and the transcript share their stem (`CallRecordingFiles.SharedStem`), old call recordings pruned to the newest 100 and 10 GB (`TranscriptStore.PruneRecordings`). |
| PTZ pad (press and hold, speed), presets, lens zoom and focus | no (planned) | no (planned) | UX.md "Pan, tilt, and zoom". IDs `ptz-pad-hold`, `ptz-deadman`, `ptz-preset-recall`, `ptz-lens-clamp`. |

### Recordings (milestone P)

| Feature | macOS | Windows | Notes |
| --- | --- | --- | --- |
| Recordings window (Window > Recordings, ⌥⌘R; Windows: Ctrl+Alt+R or Recordings in the header's "…" menu) | yes | yes | UX.md "Recordings"; Windows is W1/W2 (MILESTONE-P.md). The macOS self-test runs it on the fake core and, with `scripts/macos-real-selftest.sh`, on the real core against the mock (`recordings-real-core`); the Windows self-test runs the same window class on the Windows fakes (`FakeRecordings`, `FakePlayback`), the model and rules also in the shell unit tests with the macOS vectors. Not on Windows yet: the real-core steps, cover thumbnails (below), and the edits and groups steps (the fake does not model them).
| Cameras to search, per device and per camera, kept between launches | yes | yes | ID `recordings-enable`. |
| Days asked for as shown; today held per window while shown, released on another day or close | yes | yes | IDs `recordings-hold-today`, `recordings-navigate-day`, `recordings-window-close`. |
| Calendar of days with recordings; refresh; a failed day marked | yes | yes | ID `recordings-calendar-lanes`. The failed-day mark is not in the self-test. |
| Calendar paged to another month asks for that month's calendar alone (minor 7) | yes | yes | ID `recordings-calendar-month`. |
| Camera names as the camera list shows them (override, reported name, lens) | yes | yes | ID `recordings-camera-names`; the core also names cameras by the reported names (FFI.md "Recordings"). |
| Window layout: player in the middle, collapsible sidebar and inspector that give way in a narrow window | yes | yes | ID `recordings-layout`. |
| Timeline lanes per camera (spans, events by kind with badges, zoom, playhead; a click seeks the player or plays that camera) | yes | yes | IDs `recordings-calendar-lanes`, `playback-scrub`. |
| Recordings sound: loudness strip under each lane with a legend, sound events (alarms and labels) in lanes, sound badge, Sounds filter | yes | yes | IDs `recordings-loudness-lanes`, `recordings-sound-labels`; the same rules and vectors (`SoundKinds`, `LoudnessScale`). docs/UX.md "Recordings", Sound. Analyzing a recorded day (`nv_sound_analyze_day`) is not exposed (no audio source for recorded days yet); no app classifier puts labels yet. |
| Recordings: Listen for sounds per camera (`nv_sound_watch`, at most four per device, kept between launches) | yes | yes | ID `recordings-listen-for-sounds`; `SoundListening`. |
| Timeline wheel zoom around the pointer (each notch 1.25x, whole day to 48x), panning | yes | yes | ID `recordings-timeline-wheel-zoom`; the same rule and vectors (`TimelineViewport`). macOS: the wheel zooms eased, Shift and the wheel or a horizontal two-finger scroll pans, Option and a two-finger scroll or a pinch zooms, a vertical two-finger scroll scrolls the lanes. Windows: the wheel zooms (not eased), Shift and the wheel or a horizontal wheel pans, the scroll bar and a touchpad pan; no pinch. |
| Incident list with kind filters (Animals matches every animal kind), camera or group scope, Locked | yes | yes | ID `recordings-animal-filter`. |
| Cover thumbnails: keyframe decoded, shrunk, put back into the core's cache | yes | no (planned) | VideoToolbox and ImageIO; Windows: Media Foundation and WIC. ID `recordings-thumbnail`. The Windows window asks for covers and shows what the core holds; the keyframe decoder (Media Foundation, WIC) is the seam `RecordingsService.ThumbnailDecoder` and is not built, so a keyframe shows a placeholder. |
| Incident edits: rename, merge, split, add and remove a camera, revert | yes | yes (built; the self-test covers rename and revert only) | ID `recordings-edits`. |
| Lock and unlock (the video is downloaded; says when it is not kept) | yes | yes | ID `recordings-lock`. |
| Downloads panel: progress, pause, resume, retry, Show in Finder, forget, Move to Trash | yes | yes | Trash through `NSWorkspace.recycle` after `download_removed`; Windows: the Recycle Bin (FFI-RECORDINGS.md section 12). ID `recordings-downloads`. Retry is not in the self-test. |
| Camera groups for incidents | yes | yes | ID `recordings-groups`. |

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
| Update prompt release notes: scrollable, selectable, wrapping, bounded height, buttons always visible | yes | yes | Self-test step `updates-notes-scroll`. macOS: alert accessory view; Windows: ScrollViewer in the dialog. |
| Bundled update key and source (`update-defaults.json`; shown as "Bundled with this build" / "Bundled default") | yes | yes | Self-test step `updates-bundled-defaults`. macOS: in the signed bundle's Resources; Windows: next to the exe. |
| Hardware decoding, GPU rendering | yes | yes | VideoToolbox and Metal; Media Foundation and D3D11. |
| Message when the OS lacks an HEVC decoder | n/a (built in) | yes | Windows needs the HEVC Video Extension. |
| Recorded-video playback in the Recordings window (up to four cameras on one clock, grid with large and single views, transport with skip, speeds 0.25× to 16×, mute and volume, scrubber with the incident's events) | yes | yes | Milestone P, M2 (macOS) and W2; [PLAYBACK.md](PLAYBACK.md) is the design, docs/UX.md "Recordings" the screen. IDs `playback-open`, `playback-transport`, `playback-grid`, `playback-scrub`, `playback-close`, and `playback-real-core` (real core against the mock). Stepping and per-camera offsets are not built. Windows draws the playback with the window's own D3D11 renderer; its pictures are not yet checked in the self-test (the fake makes none; `playback-real-core` is a gap). |
| Remove from Incident and Split Here at the playhead | yes | yes (built, not in the self-test) | IDs `playback-remove-camera`, `playback-split-here`. |
| Follow cut and 2-up/4-up grids of a downloaded incident in the player (view menu; files decoded and drawn by the platform renderer, no re-encode; same transport; sound follows the shown camera) | yes | yes | Milestone P, M3 and W3; docs/UX.md "Recordings", "Follow and Grid". IDs `edit-download`, `edit-follow-cut`, `edit-grid`, `edit-back-to-cameras` on both (fake store, synthetic files; Metal and VideoToolbox on macOS, D3D11 and Media Foundation on Windows). Windows differences: sound through `MediaPlayer`, Space needs a click on the player first. |

## Open gaps

What the table shows that is not just a platform difference:

- Windows insets are call cards only: no smoke, CO or entrance insets, no
  swap with the stage, and no Entrance option.
- The layout preset lists on the two apps were not compared.
- Windows' captions use an older engine than the Mac's.

## The self-test comparison

Both apps have a `--selftest` built from numbered steps
(`apps/macos/Sources/NVRView/SelfTest.swift`,
`apps/windows/NvrView.App/SelfTest.cs` and `SelfTest.Capture.cs`). Each step carries a stable ID in a
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
