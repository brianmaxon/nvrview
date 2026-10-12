# Interface requirements

What the macOS app must feel like, written from the official app's pain
points. Each item is a requirement for milestone M3 and later, not a
suggestion; [ARCHITECTURE.md](ARCHITECTURE.md) has the goals and
[PLAYBACK.md](PLAYBACK.md) the playback screens.
[PARITY.md](PARITY.md) lists which of these each app has today.

## Layouts

- Layouts are split panes: any tile splits in two (side by side or stacked),
  any divider drags to resize, a tile dragged onto another swaps cameras,
  and removing a tile lets its neighbor take the space. No fixed set of
  grids.
- The same camera can appear in several tiles and windows; it is streamed
  once.
- Several windows, each with its own layout, on any display.
- A **layout** is a saved arrangement: its splits, divider positions, and
  which camera is in which tile. Edits are saved back to it as they are made.
- A **group** is a named set of cameras with no arrangement: showing it lays
  its cameras out as an even grid. Edits to that grid stay in the window
  until saved as a new layout, so a group is a quick way to see some cameras
  together and a starting point for a layout.
- **All Cameras** is the unsaved even grid of every camera (every lens of
  each channel) of every online channel of every connected device, in
  sidebar order, up to 64 (the most a layout can hold). A window shows it
  when it has nothing else to show: the first window before any layout
  exists, a window whose layout or group was deleted, and a new window
  opened with no other window active. Channels known to be offline are left
  out, and so are devices that are not connected yet (they are added when
  they connect). While the window is unedited it follows the devices: when a
  device connects or drops, or a channel goes online or offline, the grid is
  rebuilt from the new list (only when the list actually changes; cameras
  that stay keep their streams). Splitting, swapping, resizing, or entering
  Edit Layout stops the following. With no online camera the window shows
  its empty state. A device's context menu, "Show This Device's Cameras in
  New Window", opens the same kind of grid for that device alone.
- **New Layout from Template** (File menu and the sidebar) offers ready-made
  shapes, smallest first, each shown with its icon and tile count. The new
  layout is filled with the online cameras in All Cameras order (empty tiles
  when there are fewer cameras than tiles), saved as an ordinary layout, and
  opened in Edit Layout. The templates:
  - even grids of 1, 4 (2 x 2), 9 (3 x 3), 16 (4 x 4), 25 (5 x 5), and 36
    (6 x 6) tiles; 2 side by side; 2 stacked;
  - 6: one large tile (2/3 of the width and height) top-left, a column of 2
    to its right, a row of 3 along the bottom;
  - 8: one large tile (3/4 of the width and height) top-left, a column of 3
    to its right, a row of 4 along the bottom;
  - 10: two large tiles side by side across the top half, two rows of 4
    small tiles in the bottom half;
  - 13: on a 4 x 4 grid, a 2 x 2 large tile top-left, a 2 x 2 block of small
    tiles to its right, two rows of 4 below;
  - 14: two large tiles across the top (2 of 5 rows), three rows of 4 below;
  - 32: on a 7 x 5 grid, a 2 x 2 large tile top-left, a 5 x 2 block of small
    tiles to its right, three rows of 7 below.
  Every template is an ordinary split tree (nested splits with ratios), so
  its tiles resize, split, and swap like any layout's.
- **Layout icons** show a layout's shape: every tile of its tree as a small
  rounded rectangle with a gap between tiles, in one color that follows the
  sidebar's tint (and dark mode and selection). Each saved layout's row has
  one and it follows edits; a group's row shows the even grid for its
  camera count; the template menu items show their shapes.

## Listening from any tile

- Hovering a tile shows a speaker button in the tile's header. It fades in
  at once and fades out a few seconds after the pointer stops or leaves; it
  never covers the picture for longer than that.
- One click unmutes that camera instantly (audio is already flowing to the
  core; unmuting only routes it to the speakers) and mutes the previously
  audible tile, unless the user holds a modifier to listen to several.
- Works everywhere, including full screen and full-screen focus on one
  camera. (The official app cannot unmute in full screen.)
- Keyboard: a shortcut toggles listening on the focused tile.
- While a tile is audible, a small signal-level meter shows that sound is
  arriving: a thin, partly transparent green bar along the top of that tile
  by default, optionally a slim meter column along the edge of the grid that
  follows the audible tile. It shows level only, never a waveform of
  content, and stays visible when the header fades.
- Hovering any muted tile shows the same meter in monochrome (white on
  gray), fading with the header, so the user can see which cameras have
  sound before unmuting one.
- The level comes from the core (`nv_tile_audio_level`: the decoded audio's
  level before volume, audible or not), not from a second decode. The app
  reads it only for meters on screen, about once per display refresh.

## Full screen and focus

- Entering and leaving full screen, and focusing one tile, are continuous
  animations of the same live tiles. Video keeps playing through the
  transition: no tile is recreated, no stream restarts, nothing goes black
  or redraws from scratch. (The official app redraws tiles sluggishly during
  this transition.)
- How: the Metal grid keeps presenting every refresh while its layout
  rectangles are animated; drawable size changes are coalesced so the
  layer never stalls; the app uses the system's native full-screen
  transition rather than rebuilding the window.
- Focusing a tile switches it to the main stream only after the animation,
  and the sub-stream keeps showing until the main stream's first keyframe
  is decoded, so the picture sharpens in place instead of blinking.
- Leaving focus does the reverse: the sub-stream is already running for the
  grid, so the tile shrinks back without a gap.
- Focus belongs to one tile of one window. Other windows (and other tiles)
  showing the same camera keep their layout; because a camera is streamed
  once, they share its tile and so show the main stream while any window
  focuses it, scaled into their own rectangles, and return to the
  sub-stream when the last focus ends.
- The second lens of a multi-lens camera is a camera of its own (its own
  tile), on the lens's own tiers: `mobile` in the grid, `extern` focused,
  or `extern` throughout when the device does not serve `mobile`.

## Chrome

- Headers, buttons, and status badges appear on hover and fade out; the
  picture is the interface.
- A tile that is reconnecting says so in words with its retry countdown;
  detections show as small type badges (people, vehicle, animal) with the
  source (camera AI or this Mac). A smoke or CO alarm heard in a tile's
  audio shows as "Smoke alarm · sound" or "CO alarm · sound" in a calm slate,
  on every lens tile of the channel (a microphone belongs to no lens), and
  in the sidebar's dot. It goes away when the core reports the stop; the
  absence of a sound badge is never shown as "no alarm", since a camera
  microphone is not an alarm sensor.
- Measured against the budgets in ARCHITECTURE.md: the interface must never
  cost frames. Overlays are lightweight layers above the Metal surface.

## Tile header

> **Built on both apps** (the header, its "…" menu, the right-click menu and
> the keys). Save picture is built on macOS (see "Save picture") and planned
> on Windows; recording and PTZ are not built yet: their buttons and menu
> items are in place and say so (below).

Every tile's hover header carries its actions as buttons, in an order that
survives narrow tiles: what does not fit moves into a "…" menu, never off
the edge. Both apps compute it with the same model and test vectors
(`TileHeaderLayout`).

| Priority | Item | Shown for |
| --- | --- | --- |
| 0 | Recording pill ("● REC 1:23") | a recorded camera; an indicator, never in the overflow, visible without hover |
| 0 | Alert strip and Dismiss | an active alert (see "Alerts"); never in the overflow |
| 1 | Title (camera name) | every tile; truncated before it is dropped |
| 2 | Speaker (listen or mute) | a camera with audio |
| 3 | Record or Stop | every camera |
| 4 | Save picture | a tile with a picture |
| 5 | Start or End Conversation | a doorbell, or a camera with talk-back (`ipcAudioTalk` 1); for other cameras it is in the menu only |
| 6 | PTZ pad | a pan-and-tilt camera (`ptzType` 3) |
| 7 | Presets | a camera with presets (`ptzPreset` bit 0) |
| 8 | Lens zoom and focus | `ptzType` 1 or 3, when the camera reports ranges |
| 9 | Reset zoom | a tile zoomed past 1x (or a moved virtual camera) |
| 10 | Quality | every camera (the menu of "Stream quality per tile") |
| 11 | Fisheye view | a fisheye camera |
| | "…" | whenever anything is hidden; also holds the long tail: Reset Position (a filled picture's crop moved at 1x), Fill Tile (checked while the tile fills; not in the call window), Pop Out or Pop In, Allow talk-back, Entrance, Calls open in, Save Picture As…, Save Zoomed Area, Save Original Fisheye Picture, Record Main Stream, Record Sub-stream |

- Items are placed by priority until the width runs out; once one is
  hidden, room for "…" is kept. A change of under 8 pt does not reflow it,
  so dragging a divider does not make buttons flicker. Below 120 pt only the
  recording pill, the title and "…" show.
- Under a no-picture message ("Connecting", "Reconnecting") the header
  shows only the title and "…"; Retry stays in the message (see "Tiles
  without a picture").
- The right-click menu is the full list, in the same order, so nothing is
  only in the header.
- Corner indicators are unchanged and do not compete for header space.

Details settled while building it (macOS; Windows does the same except where
"Windows" is named):

- The buttons sit together at the header's right end in priority order;
  the title takes what is left from the left edge. Quality, Fisheye View
  and "…" open their menus under the button; the Speaker button still
  listens to several cameras with Option-click.
- **Not built yet:** Record, the PTZ pad, Presets, Lens, and
  the long tail's record items are shown in their final places
  (so the layout and the overflow are the final ones), dimmed, with "coming
  soon" in the tooltip. A click, the key (R) or the menu item
  shows "Start Recording is coming soon" (and so on) over the stage for
  2.5 s and does nothing else. Hiding them behind a flag was rejected: the
  narrow-tile behavior would change again when they land.
- The alert's Dismiss stays in the alert strip under the title, beside
  the alert's lines; the right-click and "…" menus list "Dismiss Alert"
  first.
- Fisheye View's submenu also holds the camera's mount (Ceiling, Wall,
  Desk).
- In a viewer window the right-click menu goes on, after a separator, with
  the layout's own items: Focus, Split Side by Side, Split Stacked, Remove
  Tile, Clear Tile, Show Camera.
- A grid tile whose camera's call is in its own window offers "Pop Out
  Call", which brings that window forward; "Pop In Call" is in the call
  window's own tile.
- Every camera tile has the Speaker button (as before; the apps learn
  whether a camera sends audio only once it plays).
- Start Conversation and the PTZ buttons follow the `talk`, `doorbell` and
  `ptz` fields of the `channels` event (docs/FFI.md), per lens for a
  multi-lens camera.
- **Windows:** the header's buttons are real buttons (Tab reaches them while
  the header shows; a faded header takes no clicks), the Speaker button
  listens to several cameras with Ctrl-click, and the message over the
  stage ("Save Picture is coming soon") is a small pill at the bottom,
  announced to screen readers. Entrance is in its place in the menus but
  unavailable, because Windows has no entrance insets yet. The Menu key and
  Shift+F10 open the selected tile's right-click menu at its header. The
  right-click menu is the model's list followed, after a separator, by the
  layout's own items with the same wording as on macOS (Focus, Split Side
  by Side, Split Stacked, Remove Tile, Clear Tile, Show Camera, Layout
  Preset). The per-camera "Unmute on ring" choice is in Settings > Cameras
  only, no longer also in the tile menu.

## Tiles without a picture

A tile that has no picture says why in words over the tile: "Connecting",
"Reconnecting" with its countdown, "Waiting for video", "Recovering". The
core works on its own to bring the picture back (a silent stream is
restarted, a refused one is tried again every minute; docs/ARCHITECTURE.md,
"Previews that deliver nothing"). When the same kind of message has been up
for about 30 seconds, a small bordered **Retry** button appears under it.

- It asks the core to restart that camera's stream now (`nv_tile_retry`,
  docs/FFI.md "Retrying a tile"); when the device is waiting to reconnect, it
  connects now instead. The core keeps its own floor of 2 s, so clicking
  again and again does not hammer the device; after a click the button goes
  away and returns only after another 30 seconds without a picture.
- It is never offered for "Login failed" (that state is final, to protect the
  account), "Disconnected", "Device removed", "Camera offline", or a stream
  the app cannot decode, and never over a tile that shows a picture.
- It is reachable with the keyboard like any button, and its accessibility
  label is "Retry". The delay is counted on the viewer's clock, so the
  self-test and unit tests step it by hand (`TileRetry` on macOS,
  `TileRetryRule` in `NvrView.Tiles` on Windows).

## Keyboard

Single-key shortcuts act on the window with the keyboard focus and never fire
while a text field is being edited (Settings, a layout's name). Letters work
in either case.

| Key | Action |
| --- | --- |
| F | Toggle full screen for the window (the system's own transition). |
| S | Show or hide the sidebar. |
| Arrow keys | Move the selection to the neighboring tile in that direction, by position in the layout (nearest tile whose center lies in that direction), not by list order. In focus, switch the focused camera to the neighbor in that direction. |
| Space | Toggle the selected tile between focus (it fills the stage) and the grid it came from, as a continuous animation (see "Full screen and focus"). Combined with F, Space gives full-screen video of one camera and back. |
| 0 | Reset digital zoom on the selected tile, and bring a filled picture's crop back to the middle. |
| M | Listen to the selected tile, or mute it if it is the one playing (⌘L / Ctrl+L does the same). |

Selection is visible as a thin outline that fades with the other chrome, and
follows a click on a tile. The outline and the header belong to the tile: they
cover its whole area (not just a fitted picture inside it), whatever the zoom
or Fill, and the whole stage while the tile is focused or in full screen.
Shortcuts are listed in the menus (macOS) or a help
overlay (Windows) so they can be discovered.

### Capture and camera keys

> **macOS:** P, Shift+P, R, T, ⇧⌘P, ⇧⌘R and the Camera menu exist; T works,
> the others say "… is coming soon" until capture lands, and the Option
> keys come with PTZ. **Windows:** P, Shift+P, R, T, Ctrl+Shift+P and
> Ctrl+Shift+R exist (`TileKeyMap`); T works, the others say "… is coming
> soon"; there is no Camera menu (the window's header has no menu bar) and
> the Alt keys come with PTZ. They act on the selected tile,
> never while text is edited, and toggles ignore key repeat.

| Key | Action |
| --- | --- |
| P | Save picture (see "Save picture"). |
| Shift+P | Save Picture As… (choose the place and format). |
| R | Start or stop recording the selected tile's camera (from the "Record from" setting). |
| T | Start or End Conversation. |
| Option+arrows (macOS), Alt+arrows (Windows) | Move a PTZ camera while held; it stops when the key is released. |
| Option or Alt with = and - | Lens zoom in and out. |
| Option or Alt with 1 to 9 | Recall preset 1 to 9 (the order of the camera's list). |
| ⇧⌘P / Ctrl+Shift+P | Save picture in a call window, where P may be typed into the reply (also accepted in viewer windows). |
| ⇧⌘R / Ctrl+Shift+R | Start or stop the call recording in a call window (also accepted in viewer windows, for the selected tile). |

The keys are read by key position, not by the character typed, so Option
with = or a digit works on every keyboard layout that has those keys.

- **macOS:** a new **Camera** menu lists them: Save Picture, Save Picture
  As…, Start Recording (Stop Recording while recording), Start
  Conversation, Pan and Tilt…, Presets (a submenu), Recordings (a submenu
  of the running recordings, each with Stop), Stop All Recordings, Open
  Pictures Folder, Open Recordings Folder. Until recording and PTZ land,
  Presets and Recordings hold a disabled placeholder, Stop All Recordings
  is disabled, and the other items say "coming soon". Pan and Tilt… is
  enabled only for a pan-and-tilt camera. ⇧⌘P and ⇧⌘R are hidden items,
  so they also work in a call window.
- **Windows:** the F1 help overlay lists them, the keys are mapped in
  one place (`TileKeyMap`) as the other tile keys are, and the tile's
  menus show them as key hints. In a call window only the Ctrl+Shift forms
  (and M, T, 0 while the reply field is not being edited) reach the tile,
  so P and R can be typed into the reply.

### Start Conversation (no press)

Captions, Talk, and the typed reply must not wait for a ring: a visitor may
be at the door without pressing anything, or the user may want to speak to
someone through a camera's speaker.

- **Where.** A camera tile's context menu has "Start Conversation" (and "End
  Conversation" while its call is open), next to "Allow talk-back" (see below). It is offered on every camera tile,
  not only doorbells: the core learns whether a camera has a speaker only when
  talk-back starts (`not_supported`, docs/FFI.md "Talk-back"), so the call
  says "this camera has no talk-back" when Talk is pressed, and the captions
  still work without a speaker.
- **What opens.** The same call as a press: the camera's inset (or the
  controls over the stage when the camera fills it) with captions, Talk (hold
  or "Keep talking") and the typed reply, the same lease, audio tap and
  transcription. It differs only in what a press decides: the camera is made
  audible at once (the user asked for it; whatever else was playing is muted
  for the call and plays again afterward), so no Unmute control is needed, and
  the log says "Conversation" instead of counting presses. A press during a
  conversation joins it.
- **Nothing is sent by itself.** Starting a conversation opens no talk-back.
  Talk-back still starts only when the user presses Talk or sends a reply,
  needs the camera's "Allow talk-back" setting (when it is off, that press
  asks "Allow talk-back for <camera>?" first; Cancel sends nothing), and is
  rate-limited and logged like any other (camera motion control, CLAUDE.md).
- **Ending.** The inset's close control or Esc, "End Conversation", or about
  60 s after the last activity (speech, Talk, a reply, using the inset), as for
  a press. When the camera fills the stage there is no inset to close, so a
  header with a close control sits over the stage. Every path ends the call
  the same way: talk-back stops, the tap and transcription end, the lease is
  released, and the call is logged.

Details settled while building it (macOS):

- The window takes these keys before the focused view and the menus, except
  while text is edited. When a sidebar list has the keyboard focus it keeps
  its own arrows, Space and Return; F, S and 0 still act on the window.
  Command, Control or Option make a key a menu shortcut instead (⌃⌘F and
  ⌃⌘S keep working, unlisted). Toggles ignore key repeat; arrows repeat.
- An arrow moves only to a tile whose center lies beyond the selected tile's
  edge in that direction. Tiles touching that edge come first (the nearest
  edge, then the one across from the selected tile's center); otherwise the
  nearest center, counting the offset across the direction twice. From a tile
  spanning the whole height, up and down do nothing.
- Return (or Enter) on the selected tile dismisses its alert if it has one
  (see "Alerts"), and otherwise toggles focus, as Space does.
- The menus show F (View > Enter Full Screen), S (View > Hide Sidebar),
  0 (View > Reset Zoom) and Space (Layout > Focus Tile) as their keys. The
  arrows are listed by name under Layout > Select Tile without being key
  equivalents: AppKit lets a menu's arrow key equivalent swallow the key even
  while its item is disabled, which would take the arrows from lists.

## Full screen chrome

In full screen, every header, toolbar, sidebar edge, and hover control moves
out of the way about 3 seconds after the last interaction (pointer movement,
a click, a key, a scroll) and comes back on the next one. The video is never
resized or redrawn when the chrome comes and goes: overlays slide or fade
above it. Corner indicators (below) stay.

- The pointer hides with the chrome and returns when it moves.
- The header of a tile with an active alert stays (see "Alerts").
- The toolbar and menu bar use the system's full-screen auto-hide: hidden,
  and sliding in over the picture when the pointer reaches the top edge.
- The sidebar closes when the window enters full screen (the transition
  resizes the stage anyway) and reopens on the way out if it was open. S
  still shows it in full screen; it then stays until S again, since hiding it
  on a timer would resize the video.
- Out of full screen the headers keep their hover timing and the pointer
  stays; the selection outline and the zoom map fade after the same idle
  delay (the outline stays while editing the layout).

## Corner indicators

Each tile shows small, subtle symbols for what is happening now: motion,
person, vehicle, animal, face, package, visitor (doorbell), and sound alarms
(smoke, CO), one symbol per active kind, from device detections and the
computer's own (vision or sound). They are always visible, including in full
screen and on idle tiles, unlike the hover chrome, so activity can be seen at
a glance across a wall of cameras.

- Placed in one corner of the tile, the same corner on every tile; which of
  the four corners is a setting (default top right), so they can avoid
  timestamps or logos the camera burns into the picture.
- Small (about the size of the picture's own overlay text at the tile's
  size), monochrome with a soft shadow for legibility on any picture, a
  restrained accent for sound alarms; they appear and disappear with short
  fades as detections start and stop (holding about 2 seconds after a stop
  so brief gaps do not flicker).
- Never an "all clear": no symbol means no current detection reported, not
  that nothing is there.
- A tooltip or accessibility label names each symbol and its source.
- **The same symbols appear on each camera's row in the sidebar,** after its
  name, so activity shows for cameras that are not on screen. Device
  detections arrive for every channel whether or not it is being watched
  (the device pushes its alarm state for all channels after the session's
  subscription; protocol.md "Detection events"); sound alarms exist only
  for cameras whose audio is being decoded (open, or watched for sound). A
  device row shows a compact summary of its cameras' activity when
  collapsed.

## Digital zoom

Every tile and the focused or full-screen video can be zoomed digitally to
look at a detail.

- Mouse wheel or trackpad pinch zooms around the pointer; trackpad
  two-finger scroll, or dragging, pans while zoomed. Zoom ranges from 1x to
  about 8x, smoothly, at display rate.
- The zoom belongs to the tile: two tiles of the same camera are
  independent views (one wide, one zoomed on a detail). It survives layout
  changes and focus, and when focus switches the camera to the main stream
  the same region is shown, now with the main stream's detail (the region
  is kept in picture coordinates, independent of resolution). It is saved
  with the window (see "Fill Tiles").
- Double-click keeps its meaning (focus); 0 or a small reset control in the
  hover header returns to 1x (and a filled picture's crop to the middle);
  zoom never changes what is recorded or streamed.
- Zooming in uses the whole tile: the picture grows over the letterbox or
  pillarbox bars, and as soon as it is larger than the tile on an axis it
  fills that axis (cropping), so the bars shrink and then disappear. The
  same holds for the focused tile and in full screen. At 1x the tile looks
  exactly as unzoomed (fitted, or filled with Fill Tiles).
- Panning stops at the picture's edges (nothing beyond the picture is ever
  shown; with Fill Tiles it can reach the parts the 1x crop hides). The
  picture point under the pointer or pinch stays put while zooming; on an
  axis the picture does not fill yet, it stays centered, so there the point
  moves until the picture fills that axis.
- A small inset map of the whole picture, with the visible region outlined,
  shows while zoomed and fades with the chrome.
- How: zoom only changes the rectangles the renderer draws and samples from
  the decoded picture (no extra decoding or copies), so it costs nothing per
  frame beyond the drawing that already happens. The rule, per axis, with
  `b` the picture's 1x size as a fraction of the tile, `s` the scale and
  `c` the picture point at the tile's center: the picture is `L = s * b`
  tiles long and starts at `u0 = (1 - L) / 2` while `L <= 1`, else at
  `0.5 - c * L` clamped to `1 - L ... 0`; it is drawn over its span
  intersected with the tile, sampling the matching part of the picture.
  The same pure geometry on both apps (`TileZoom`).

Details settled while building it (macOS):

- A plain drag on the picture of a tile zoomed past 1x pans its picture, as
  does one on a filled picture with part hidden (see "Fill Tiles"). On other
  tiles, on a tile's shown hover header, and always while editing the
  layout, a drag drags the tile to swap. A pan follows the pointer 1:1 from
  press to release, also when the hover header appears under the pointer
  (Windows: a lost pointer capture or a mouse drag starting a manipulation no
  longer ends it; only a touch pinch replaces it).
- A mouse wheel (line-based scrolling) zooms, each notch 1.25x, eased over
  the next refreshes. Precise scrolling (trackpad, Magic Mouse) pans a zoomed
  tile and does nothing at 1x; with Option held it zooms instead, for devices
  without a pinch. A pinch zooms continuously.
- The zoom is kept per tile: per window, per layout, and per tile of the
  layout (with the camera the tile shows). Each tile of a layout has a
  stable identity, its `tile` member in the stored layout, which stays put
  when other tiles are split, removed or swapped (a split keeps the old
  tile's identity and gives the new tile one of its own; a layout stored
  before tiles had identities gets the same ones on every launch, `t<n>` by
  reading order). A swap moves each tile's zoom, crop and Fill choice with
  its camera (the view travels with what was dragged); focus, layout
  switches and the switch between sub-stream and main stream keep it; Show
  Camera, a camera dropped on the tile or any other change of the tile's
  camera starts that tile at 1x, centered, following the window's Fill
  Tiles. Removing a tile forgets its view.
- The inset map is an outline of the whole picture with the visible region
  marked, drawn as overlay layers (no second picture, so no extra drawing or
  texture work), in the bottom left corner of the picture as drawn.

## Fill Tiles

A tile normally fits its picture (letterbox or pillarbox bars where the
shapes differ). Filled, the picture covers the whole tile and the part that
does not fit is cropped.

- **Fill Tiles** (macOS: the Layout menu; Windows: the header's "Fill tiles"
  toggle) sets every tile of the window at once: turning it on or off
  clears the tiles' own choices, so all tiles follow it.
- **Fill Tile** in a tile's menu (macOS: the right-click menu and the
  header's "…"; Windows: the right-click menu, "Fill Tile") fills or fits
  that tile only (another tile of the same camera keeps its own choice),
  checked while it fills. While
  some tiles differ from the window's switch, the macOS menu item shows a
  mixed state; the Windows toggle shows the window's own setting.
- **Choosing the part shown**: on a filled tile whose picture is partly
  hidden, a plain drag on the picture moves the crop, stopping at the
  picture's edges (nothing beyond the picture shows). It is the digital
  zoom's own rule at 1x (`TileZoom`): the crop's center is the zoom's
  center, so zooming in starts from the part shown and zooming back out to
  1x leaves the crop where the zoom ended.
- **Swapping moves to the header**: on such a tile a drag on the picture
  pans, so a drag on the tile's hover header swaps it with the tile it is
  dropped on. A drag on the shown header swaps on every tile, so the
  gesture is the same everywhere; tiles with nothing hidden still swap when
  dragged by the picture, and while editing the layout a drag anywhere
  swaps. A zoomed tile pans, as before.
- **Pointer**: over a picture a drag pans (below the header strip), the
  pointer is an open hand (macOS; a closed hand while dragging) or the hand
  pointer (Windows, which has no closed-hand system pointer).
- **Reset**: "Reset Position" in the tile's
  menu, shown while a filled crop is moved at 1x, brings it back to the
  middle; 0 and the header's reset control (shown while zoomed) also
  recenter it.
- **Remembered**: the window's switch, and per tile its zoom (scale and
  center), crop position and own Fill choice, are saved with the window
  and restored on relaunch (windows.json: `fill`, and `tiles`, entries of
  `layout` (`layout:<id>`, `group:<id>` or `scratch`), `tile` (the tile's
  identity), `camera`, `scale`, `x`, `y`, `fill`; written only when not
  the default). They are per window, per layout and per tile (see "Digital
  zoom" for what keeps or resets them); the same layout in another window
  has its own. Positions are fractions of the picture, so the sub-stream
  and the main stream show the same part. A window keeps at most 256
  tiles across its layouts (the one changed longest ago is dropped first);
  saved values are clamped when read, and entries that cannot be read are
  skipped. A file from before views were per tile (`tileViews`, per
  camera) is read once: each camera's view goes to every tile of the
  window's layout showing it, and the window is saved in the new form. The
  rule and its vectors (`TileViews`, `fixtures/synthetic/tile-views.json`)
  are shared by both apps.
- **Fisheye and multi-lens cameras**: each lens of a multi-lens camera is a
  camera of its own here (its tiles have their own zoom, crop and Fill
  choice). A fisheye
  tile in Original fills, pans and zooms like any tile; a dewarped view
  (Panorama, Double panorama, Quad, Virtual PTZ) always covers its tile and
  a drag turns the view instead (see "Fisheye cameras"), so its Fill choice
  and crop apply again only once it is back in Original.

## Fisheye cameras

A fisheye (360°) camera sends one circular picture of everything around
it. Each tile showing one can turn that circle into easier views, drawn by
this computer while it draws the tile (docs/RENDERING.md, "Fisheye
dewarping"). Nothing is sent to the camera: no motion commands, no
settings; the camera keeps streaming the same picture.

- **Fisheye view** in the tile's context menu (right-click), shown only for
  a fisheye camera (the core reports `fisheye`), chooses the view:
  - **Original**: the circle as the camera sends it, with digital zoom as
    on any tile.
  - **Panorama**: the whole circle unwrapped into one 360° strip, the
    horizon along the top for a ceiling camera.
  - **Double panorama**: two 180° strips stacked, which fits an ordinary
    tile shape better than one long strip.
  - **Quad**: four virtual cameras, 90° each, looking in four directions
    (front, right, back, left in reading order).
  - **Virtual PTZ**: one virtual camera that the user aims and zooms with
    the pointer, with straight lines kept straight.
- **The default is Original**, for existing tiles and for a fisheye camera
  newly put in a tile alike: a tile without a saved choice shows the
  camera's own picture, so nothing looks different until the user picks a
  view, and the choice means the same in every app sharing the layouts.
- The view belongs to the tile and is saved with the layout (the leaf's
  `fisheye` member: the view, and the pan, tilt and field of view of the
  virtual camera), like Quality. Several tiles of the same camera can show
  different views; the camera is still streamed once.
- **Mount** (ceiling, wall, desk) is a per-camera setting in the same menu
  and in Settings > Cameras, default ceiling. The panorama and the virtual
  views depend on it (a ceiling camera's horizon is the rim of the circle,
  a wall camera's runs across the middle).
- The image circle (its center and size) is found automatically from the
  first pictures and kept for the run; a manual adjustment may come later.
  The camera's text overlay outside the circle does not appear in the
  dewarped views.
- A tile in a dewarped view names it in its hover header ("Panorama",
  "Virtual PTZ"), as it names a quality that is not Auto.

Pointer and keys in a dewarped view (they replace digital zoom there):

| Input | Panorama, Double panorama, Quad | Virtual PTZ |
| --- | --- | --- |
| Drag | turns the view left and right; the picture follows the pointer | aims the virtual camera (pan and tilt); the picture follows the pointer |
| Mouse wheel, pinch, Option-scroll | nothing | zooms (field of view 120° to 20°), eased smoothly like digital zoom |
| Trackpad two-finger scroll | turns the view | aims the virtual camera |
| 0, or the header's reset control | back to the default direction | back to the default direction and field of view |

While editing the layout a drag still moves the tile, as on any tile.
Double-click keeps its meaning (focus), and focus, full screen and Quality
work as on any tile: focusing a dewarped tile keeps its view and switches it
to the main stream, which dewarps with more detail.

Details settled while building it (macOS):

- The view's pan, tilt and field of view are saved with the layout when a
  drag or zoom ends, not on every refresh.
- Settings > Cameras lists Mount for fisheye cameras only.

## Stream quality per tile

A tile's context menu (right-click) has **Quality**, as in the official app,
for seeing more detail in a tile without focusing it (useful with digital
zoom):

- **Auto** (default): the low stream in the grid, the main stream when
  focused. Auto also switches a tile to the main stream while it is zoomed
  past about 2x, and back when zoomed out again, so zooming in shows real
  detail.
- **Clear**: the main stream, also in the grid.
- **Fluent**: the low stream, also when focused (saves bandwidth).
- **Balanced**: the middle stream, offered only for cameras that serve one
  (a single-lens camera's `extern` tier; never for a second lens, whose
  tiers are its own).

The choice belongs to the tile and is saved with the layout (a member of the
layout's leaf), so it survives relaunches. Because a camera is streamed
once, a tile asking for a higher quality raises it for every tile showing
that camera while it asks, as focus does. Changing quality uses the aligned
tier switch, so the picture sharpens in place without a gap. A tile not on
Auto shows its quality in its hover header, and the menu notes that the main
stream on many tiles costs the device and this computer much more.

## Save picture

> **Built on macOS** (CAPTURE.md, "Save picture"); planned on Windows
> (CAP-P2). Playback tiles come later.

- **Where:** the header's camera button, the right-click menu, P (and
  Shift+P for Save Picture As…), the call window's tile, and later playback
  tiles.
- **What:** the full frame of the stream the tile shows, at its own size
  (the menu says the pixel size), whatever the zoom. While zoomed, "Save
  Zoomed Area" saves only the visible region, at the source's resolution,
  never enlarged. A fisheye tile saves its current view (dewarped), and
  "Save Original Fisheye Picture" the circle as the camera sends it.
- **Files:** PNG by default, or JPEG (Settings); to `~/Pictures/NVRView` or
  `Pictures\NvrView` ("Save pictures to"), named
  `2026-10-11 17.02.03.250 Camera 1.png`; with "Share-safe names" the
  camera is `Camera A`. No metadata; an optional time and name label burned
  in, off by default.
- **Settings (macOS):** Settings > Capture has "Save pictures to" (Choose…
  and Reset), Format (PNG or JPEG) and "Share-safe names". Save Picture As…
  (Shift+P) opens a save panel in that folder with a Format menu; Open
  Pictures Folder is in the Camera menu.
- **Feedback:** a brief shutter flash on the tile (not under Reduce Motion)
  and a "Picture saved"
  toast with Show in Finder or Show in Explorer. A failure names the file,
  not the folder path.

## Recording

> **Planned** (CAPTURE.md, "Live recording"; not built yet).

Recording saves a camera's live stream on this computer as an MP4 (video
as received, the camera's audio).

- **Start and stop:** the header's Record button, R, the right-click menu,
  or the Camera menu (macOS). The recording starts at the next keyframe
  (usually within a second or two) and records the camera, not the tile: it
  keeps going if the tile closes, the layout changes, or the tile's quality
  changes. Stop with the same button (it becomes Stop), R, the window's
  recording list, or Stop All.
- **Stream:** Settings, **"Record from"**: Main stream (the default) or
  Sub-stream. The Record menu and the overflow also offer "Record Main
  Stream" and "Record Sub-stream" for one recording. A device records from
  its main stream at most twice at a time; a third is refused with an offer
  to record the sub-stream instead.
- **Indicators:** while a camera is recorded, every tile showing it (every
  window, full screen too) shows a "● REC 1:23" pill, always visible, in the
  corner opposite the corner indicators. On Windows the window's header
  shows "● N recording" with the list, Stop and Stop All; on macOS a
  toolbar item and the Camera menu's Recordings submenu.
- **Limits:** a recording stops by itself after 60 minutes or 4 GB (both in
  Settings), when free disk space runs low (it does not start with under
  2 GB free), when the device is removed, or at quit. At most four live
  recordings at a time. A short reconnect keeps the same file (the last
  picture holds through the gap); a long one, or a change of resolution,
  continues in `… (part 2).mp4`.
- **Files:** `~/Movies/NVRView` or `Videos\NvrView` ("Save recordings to"),
  named `2026-10-11 17.02.03 Camera 1.mp4` (`Camera A` with share-safe
  names). A file is always playable, even after a crash; an unfinished one
  is completed at the next launch.
- These are this computer's recordings of what it received, gaps included,
  not the NVR's own recordings (PLAYBACK.md, "Local captures and the NVR's
  recordings").

## Pan, tilt, and zoom

> **Planned** (CAPTURE.md, "Pan, tilt, and zoom"). The core's side is
> built (Z1: commands, limits, the stop on every path, grants); the apps'
> controls are not.

Steering a camera that can move is the user's own action and needs no
enable step (camera motion control, docs/ARCHITECTURE.md "Device
commands"); every command is still rate-limited (far above what hands can
do) and logged.

- **Pan and tilt** (`ptzType` 3): the header's PTZ button opens a popover
  with a pad of four directions (diagonals only once observed on a camera).
  Press and hold to move; letting go stops it. A speed slider runs 1 to 64,
  default 20. When the camera reports lens ranges, zoom − and + and focus −
  and + sit below the pad.
- **Presets** (`ptzPreset` bit 0): a popover listing the camera's presets
  by name, with the 1 to 9 key hint; a click recalls one. Saving or
  deleting presets is a device setting and is not offered.
- **Lens only** (`ptzType` 1): a popover with zoom and focus sliders. A
  drag sends a position at most five times a second and once when it ends,
  within the range the camera reported.
- The positions are read again about 6 s after a zoom or a preset recall
  (when the camera has settled), as the official client does.
- Errors and refusals show in the popover, with the camera's name. There is
  no permission prompt; automation (the local API, automatic correction)
  needs its own per-camera grant.

## Alerts

Some detections matter enough that the tile must not let them pass unseen:
today the sound alarms (`smoke_alarm`, `co_alarm` from `local_audio`); the
list is one setting so device alarms can be added later. When one starts:

- **The tile raises an alert:** a red border around the camera's tile that
  blinks about once a second (well below photosensitivity limits); with the
  system's Reduce Motion setting it is a steady red border instead. Every
  tile showing that camera, in every window and on every lens of a
  multi-lens camera, shows it.
- **The tile's header stays visible while the alert is active,** also in full
  screen where other chrome hides, and states plainly:
  - the reason ("Smoke alarm (sound)", "CO alarm (sound)");
  - when it was first detected (local time, with the date if not today);
  - how many times it has been detected since the alert was raised (each
    detection start counts once), and when the last one was;
  - whether it is still sounding now.
- **The alert stays until the user dismisses it** with the Dismiss control in
  that header (a click, or Return when the tile is selected). Detections
  stopping do not clear it. Dismissing clears it for that camera everywhere
  and resets the count; a new detection afterwards raises a new alert.
- Alerts are kept for the running session and listed in the event log with
  their times and counts; they are an aid for noticing and finding events,
  never a replacement for alarm systems or monitoring (see "Corner
  indicators": no alert never means "all clear").

## Alert insets

When something important happens on a camera that is not on screen (another
camera is focused or full screen, or the camera is not in the current
layout), its live video appears in an inset drawn above the current video,
not in a separate system window.

- **Triggers (defaults):** a doorbell press (the device's `visitor` alarm),
  smoke and CO sound alarms, and person detections on cameras marked as
  entrances. Other kinds (person or vehicle on any camera) can be turned on
  per camera. A camera already visible gets its tile alert instead.
- **The inset:** about 30% of the screen width, in a corner (opposite the
  corner indicators by default; configurable), with a border colored by
  severity (red for smoke and CO, a calmer accent for the doorbell and
  detections) and a header with the reason, time, and count like the alert
  header. It shows the sub-stream, so it appears quickly; it uses the same
  shared tiles as everything else (no extra device login).
- **Several at once** stack, newest on top, up to three; beyond that a
  "+N more" chip. A repeat on the same camera updates its inset instead of
  adding one.
- **Swap:** clicking the inset (or Tab) swaps places: the inset's camera
  goes full screen and the camera you were watching becomes the inset, so one
  more click swaps back. Esc or the inset's close control dismisses it.
- **Closing:** doorbell and detection insets close by themselves about 60 s
  after the event ends unless hovered or used; smoke and CO insets stay until
  dismissed, and dismissing one also dismisses its alert (see "Alerts").
- Insets never take the keyboard focus or change what the user is listening
  to, except for the doorbell rules below.
- **Moving them:** the insets can be dragged anywhere on the stage by their
  header (or by the picture once the pointer moves past the 6 pt click
  threshold; a click still swaps). All of a window's insets move together as
  one column, which grows away from the nearer horizontal edge: in the top
  half the newest inset is at the column's top and the others stack
  downward; in the bottom half the newest is at the bottom and they stack
  upward. Within 24 pt of an edge (inside the 12 pt margin), each axis snaps
  to that edge. The position is kept as a fraction of the free space,
  `(left - margin) / (stage width - 2 margin - column width)` and the same
  for the height, each between 0 and 1, so resizing the window or going
  full screen keeps a column in its corner or in the same relative place.
  The position belongs to each viewer window and is saved with it; until
  the user moves the column it sits in the default corner (the Settings
  corner, else opposite the corner indicators), and "Reset Position" in an
  inset's menu returns it there. A column never sits over the sidebar's
  edge-reveal zone or the full-screen toolbar's reveal area. On Windows the
  same rules move the call cards (DIPs for points; the saved key is
  `insetAnchor`, as on the Mac).

Details settled while building it (macOS):

- An inset is one more tile of the window's Metal grid, drawn last (on top)
  from the camera's shared tile: no second decode, no extra login. Its
  header, border and controls are lightweight views above the picture.
- "Visible" means on this window's stage now: the swapped-in camera, else the
  focused tile, else any tile of the layout. A sound alarm counts as visible
  when any lens of its channel is. A doorbell press opens the call inset
  unless the doorbell fills the stage (focused, swapped in, or the only
  tile); its call controls then show over the stage instead.
- Entrance is a per-camera option (the tile's context menu, Settings >
  Cameras), off by default, saved in the app's data directory.
- The insets stack from their corner, newest nearest it; those that do not
  fit the stage's height count in "+N more". The corner is a setting
  (Settings > Alerts), opposite the corner indicators by default.
- Moving the column: a press on the header (its labels included) becomes a
  drag after 2 pt, one on the picture after the 6 pt click threshold;
  releasing after a drag never swaps. The column follows the pointer with
  the snap applied as it moves, and the position is stored when the button
  is released, in the window's record in `windows.json` (`insetAnchor`, left
  out while the column is in its default corner). "Reset Position" is in
  the menu of every inset over the stage. Moving the column counts as using
  its insets (their minute starts again). The 12 pt margin keeps the column
  clear of the sidebar's 6 pt edge-reveal strip and of the top edge that
  reveals the toolbar in full screen.
- Swapping with a layout that is not focused puts the selected (or first)
  tile's camera into the return inset ("Back to the view"); clicking it, its
  close control, or Tab swaps back. Clicking another inset while swapped puts
  that camera on the stage and keeps the way back. The swapped-in camera gets
  the main stream (the same aligned switch as focus) and a header along the
  top of the stage.
- Esc dismisses the swapped-in camera (the stage returns to its view), else
  the top inset; only when there is no inset does Esc leave focus as before.
  Tab and Esc go to the insets only while the stage has the keyboard.
- A detection whose stop never arrives counts as ended 120 s after its last
  start, so its inset still closes. Using an inset (a click, hovering, its
  call's activity) restarts the minute.

## Doorbell calls

A doorbell press opens the doorbell's inset as a call, and the user can open
the same call themselves without a press ("Start Conversation", below):

- **Audio:** the doorbell becomes audible if the user was already listening
  to a camera (that camera is muted meanwhile); a per-doorbell setting makes
  it audible even when nothing was playing. Otherwise a large Unmute control
  covers the whole inset (and the full-screen video when the doorbell is
  swapped in), so one quick click anywhere on it unmutes.
- **Talk back:** an always-visible Talk control in the call (push to talk,
  or a toggle) sends the computer's microphone through the doorbell's
  speaker. This is camera motion control (CLAUDE.md, docs/ARCHITECTURE.md
  "Device commands"): available only on a doorbell where the user enabled
  it, rate-limited, every use logged; the talk-back protocol facts go into
  docs/protocol.md before any code sends them. The limit is 20 talk starts
  per camera per minute outside a call; while a call (a press or Start
  Conversation) is open on the camera there is none, so pressing Talk again
  and again in a conversation is never refused: each app tells the core
  when a call opens and ends (FFI.md, "Talk-back", item 5).
- **Live transcript:** from the press, both sides are transcribed on this
  computer (local speech recognition on both macOS and Windows, a different
  engine on each; nothing leaves the house): the doorbell's audio and, while
  talking back, the microphone, labeled by side, so the visitor can be read
  when the user cannot unmute (in a meeting, for example). Engines: Apple's
  SpeechAnalyzer on macOS (about 4% word errors on clean speech, measured
  2026-10-10 on doorbell-quality speech: 16 kHz, outdoor noise, low-bitrate
  AAC; about 1% of a core, no model to ship). Windows today uses the desktop
  recognizer of Windows (`System.Speech`, SAPI, dictation grammar), which is
  older and less accurate, and gives no captions where Windows has no
  recognizer for the language; Moonshine streaming models (MIT) are the
  planned replacement (ARCHITECTURE.md, "Live transcription"). Both sit
  behind one engine interface in each app. One session per
  source (doorbell, microphone), so the speaker label is the source. While
  the user talks, the operating system's echo cancellation removes the
  doorbell's audio playing through the speakers from the microphone, so the
  visitor does not hear themselves sent back and both sides can talk at once;
  the call says "Echo cancellation on". Where the system cannot confirm echo
  cancellation, the call says "No echo cancellation" and the microphone's
  captions pause while the doorbell audibly plays (and 0.4 s after) so the
  visitor is not transcribed twice (AUDIO.md, "Echo cancellation"). Shown live in the call; saved with the event only if the user
  turned saving on (recording a conversation may need the other person's
  consent in some places).
- **Typed reply:** a text box in the call speaks what the user types through
  the doorbell's speaker with the system's text-to-speech (the same Talk
  permission and limits). Text comes only from what the user types or picks
  from their own templates, never from the transcript, detected text, or any
  other untrusted source (see docs/ARCHITECTURE.md "Doorbell text-to-speech").
- The call ends when the user closes it or, while it shows in the insets
  (not in a call window of its own, see below), about 60 s after the last activity
  (press, speech, talk-back, typing a reply, hovering or clicking its inset),
  and is listed in the event log with its times and, if saved, its
  transcript.

Details settled while building it (macOS):

- The Unmute control covers the inset's picture (or the stage's, when the
  doorbell fills it); the Talk control and the reply box stay reachable below
  it. Cameras muted for the call play again when it ends, unless the user
  changed what plays meanwhile.
- Talk: hold the button, or turn on "Keep talking". Both, and Send, ask the
  core for talk-back, but the app decides first from the camera's own
  "Allow talk-back" option (Settings > Cameras, off by default): when it is
  off, Talk or Send shows **"Allow talk-back for <camera name>?"** ("Talk-back
  sends sound through this camera's speaker. You can turn it off in
  Settings.", buttons Allow and Cancel) before anything reaches the core. A
  sheet on the window that holds the call (a dialog on Windows). Allow saves
  the option exactly as the toggle does (which also tells the core) and then
  goes on with what the user started, with no second press: the microphone
  opens (it stays open like "Keep talking" until Talk is pressed and released
  or the toggle is used), or the typed reply is spoken. Cancel sends nothing,
  leaves the option off and the call open, and a typed reply stays in the
  field (it is cleared only once the reply is on its way). The prompt appears
  only for the user's own Talk or Send, never for a ring or Start
  Conversation. If the core still refuses with `not_permitted` (the options
  out of sync), the same prompt appears, and Allow repeats the action. Every
  other refusal is logged and shown with the camera's name. The permission is
  passed to the core after every connection and whenever it changes. The microphone is asked for (the system prompt) only
  when Talk is first pressed; a typed reply needs no microphone.
- A typed reply is rendered by the system voice into buffers (never played
  locally), converted to 16 kHz mono, held until talk-back is active, pushed
  in one go, and ended with a draining stop unless Talk is held. Letting go
  of Talk is a draining stop too. After a draining stop the core keeps the
  talk open for 1.5 s of silence before closing it, because the doorbell
  plays about a second behind and drops what it has not played at the close
  (AUDIO.md, "Talk-back (sending)"); talking or sending again meanwhile
  continues the same talk. Both apps do this.
- The visitor's transcript comes from the doorbell tile's audio tap (16 kHz
  mono), enabled for the call; the user's from the microphone while talking.
  The microphone runs on the playback engine with the system's voice
  processing (echo cancellation), so it is captioned while the doorbell
  speaks, except for 0.15 s after the doorbell is very loud (a peak over
  0.5). Without voice processing it is not captioned while the doorbell's
  audio is audible and carrying sound (and 0.4 s after). Voice processing
  is prepared when a call opens and stays on after Talk until the call ends, so pressing Talk again does not interrupt the
  doorbell's audio; meanwhile the system shows the microphone in use,
  though nothing is taken from it or sent. Switching to
  it and back each interrupts the doorbell's audio briefly (AUDIO.md, "Echo
  cancellation"). Apple's SpeechTranscriber (volatile and fast results) runs on the
  Mac; while it installs its English assets the call says "Preparing speech
  recognition", and without them, or without the speech recognition
  permission, the call goes on without a transcript.
- The app logs every talk-back start, refusal and stop, typed replies (their
  length, not their text) and the call's end in the session's event log; the
  transcript goes there only with "Save call transcripts" (Settings >
  Alerts), never to the system log.
- **Transcript files** (both apps, with the same setting, off by default):
  when a call that transcribed something ends, its transcript is also
  written as a plain-text file (UTF-8), atomically, to
  `~/Library/Application Support/NVRView/Transcripts` on macOS and
  `%LOCALAPPDATA%\NvrView\Transcripts` on Windows (the data directory's
  `Transcripts` folder when one is given). The name is the call's start and
  the camera's name, `2026-10-11 17.02.03 Front door.txt` (path separators,
  reserved and control characters become `-`, at most 60 characters of the
  name, " 2", " 3", ... when taken). The text: a header (`NVRView call
  transcript`, `Camera:`, `Call:` a doorbell press count or a conversation,
  `Started:` date, time and UTC offset, `Ended:` with the length), a blank
  line, then each finished caption line as `17:02:10 Visitor: ...` or
  `17:02:15 You: ...`. The folder is bounded like the logs: after each file
  the newest 500 files and at most 50 MB in all are kept, the oldest deleted
  first (only `.txt` files). An "Open Transcripts Folder" button sits next to
  the setting. Nothing is written with the setting off, or for a call with
  no finished caption.

**Captions and echo.** The doorbell plays our talk-back about a second late and
its own microphone hears its speaker, so the "Visitor" captions would repeat
what the user just said or typed. A caption echo filter (`CaptionEcho`, the
same rules on both apps) treats a visitor caption as echo while talk-back is
asked for, starting, or playing (the core's `stopped` event, after its silent
tail, ends it) and for 3 s after, when it closely matches text we recently
sent: the "You" captions of the microphone (volatile ones too) and typed
replies, remembered for 30 s. Matching: lowercase, no punctuation, digits 0-99
as number words; words in order (longest common subsequence, a word one letter
off counts for words of four or more letters); echo when at least 60% of the
longer phrase matches, or when the visitor line is a fragment of what we
sent (at least two words, 80% of its own words matching), since volatile
captions arrive incrementally. Phrases sent in a row can come back as one
caption and are also compared joined. Visitor text that does not resemble
what we sent is never filtered, and outside the window nothing is. Echo is
hidden: it is not shown on screen, not saved in the transcript file or the
event log, and is not visitor activity (it does not keep the call open or
reset its minute). Hiding, not dimming, so the call shows what was said to
the user. A single shared word (such as "hello") of a longer sentence is not
echo.

Details settled while building it (Windows):

- The same call, a press or Start conversation: a card per call in every
  window, the Windows form of the call inset: a header with a close control
  that ends the call, the doorbell's live picture (the Unmute control covers
  it when the call opened silent), then the live captions, Talk (hold the
  button, or "Keep talking") and the typed reply box with Send. The picture
  is one more tile of the window's own grid, drawn last from the doorbell's
  shared tile, as on the Mac; the card holds its own lease while it shows it.
  The cards stack from the inset corner, newest nearest it, sized like the
  Mac's insets but at least 340 wide for the controls: the corner is a
  setting (Settings, "Doorbell call cards"), opposite the corner indicators
  by default, as on the Mac. When the doorbell fills the stage (focused, or
  the only tile) the same controls show along the bottom of the stage
  instead of a card, beside the other calls' cards. Calls are the only
  insets on Windows: smoke, CO and entrance insets, and the swap, are not
  there yet. The cards move as one column by their header or picture, as
  above ("Moving them"; Reset Position is in a card's right-click menu).
- Talk-back needs the camera's permission, which on Windows is the tile
  menu's "Allow talk-back" (a checked item, off by default, kept in
  `view-settings.json` and passed to the core before every start and when it
  changes; withdrawing it stops a running talk). The call says where to turn
  it on when the core refuses with `not_permitted`.
- The microphone is WASAPI capture (no permission prompt for desktop apps; if
  Windows blocks it the call says so). A typed reply is rendered by the
  system voice (`Windows.Media.SpeechSynthesis`) into memory, never played on
  the computer, and pushed to the camera at the voice's own rate; the core
  resamples it. The microphone is captured in communications mode, with
  Windows' echo canceller pointed at the device that plays the camera where
  Windows allows it (AUDIO.md, "Echo cancellation"); the call's status says
  "Echo cancellation on" or "No echo cancellation", and the caption gate
  follows the Mac's rules for each.
- Captions use the desktop speech recognizer of Windows (`System.Speech`,
  local, no model to ship), one session per source, fed the same 16 kHz audio
  as on the Mac. It is a stand-in for the engine docs/ARCHITECTURE.md plans
  (Moonshine, measured on doorbell-quality speech before it replaces this):
  where Windows has no recognizer for the language, the call says "No
  transcript" and goes on. Partial results show with an ellipsis until the
  recognizer settles on a phrase, and a phrase it is not sure of stays a
  partial.
- "Save call transcripts" is in Settings with an "Open transcripts folder"
  button, kept in `view-settings.json`: the transcript goes into the app's
  log and into a transcript file, as on the Mac.
- Not on Windows yet: a per-doorbell "unmute on ring" (a press unmutes only
  if the user was already listening, else Unmute shows).

### Recording calls

> **Planned** (CAPTURE.md, "Recording calls"; not built yet).

- **Setting:** Settings > Alerts > Doorbell calls, **"Record calls"**, off
  by default, and per doorbell Default, Always, or Never. Turning it on the
  first time shows: "Recording a conversation may need the other person's
  consent where you live. The visitor is not told that the call is
  recorded." Nothing is announced to the visitor (that would be talk-back
  the user did not ask for).
- **When:** with the setting on for the doorbell, recording starts when the
  call opens (a press or Start Conversation). The call's Record button (and
  ⇧⌘R or Ctrl+Shift+R) starts or stops it at any time, with or without the
  setting; starting again in the same call continues in `(part 2)`.
- **What:** the doorbell's video and three audio tracks: "Conversation"
  (both sides mixed, the one players play), "Visitor", and "You" (exactly
  what was sent through the doorbell's speaker, where it played). The
  visitor's track may hold an echo of the user's words from the doorbell's
  speaker (AUDIO.md, "Recording calls"). The stream follows "Record from".
- **Where:** beside the call transcripts, with the transcript's name
  (`2026-10-11 17.02.03 Camera 1.mp4` next to `... Camera 1.txt`). The
  folder keeps the newest 100 recordings and at most 10 GB of them; "Open
  Call Recordings Folder" sits next to the setting. Share-safe names do not
  apply here.
- **Shown:** the call bar's "● Recording 0:42" pill, and the "● REC" pill on
  the doorbell's tiles everywhere.

### A call in its own window

A call can leave the viewer windows and live in a small call window of its
own (the doorbell's live picture, the header, and the call's controls), for
example to keep it beside other work. Both apps follow these rules.

- **One presentation per call, for the whole app:** in the insets, or in a
  window. "Pop Out Call" (a header button, and an item in the inset's and the
  doorbell tile's menus) removes the call's inset from every viewer window,
  hides the call controls over the stage when the doorbell fills it, ends a
  swap that shows the doorbell, and opens one call window. "Pop In Call" (a
  header button in the call window, and the same menus) closes the window,
  and every viewer window shows the inset again, on top, with a fresh
  minute.
- **The call is never interrupted by a pop:** the same call, lease (the new
  holder acquires the doorbell before the old one releases it), audio tap,
  transcript, running talk-back, "Keep talking", and unsent reply text.
  "Keep talking" and the reply text belong to the call, not to a view, so
  any view built for it shows them.
- **Ending:** closing the call window (its close button, ⌘W, Alt+F4) ends the
  call, as closing its inset does; so do Esc while the reply field is not
  being edited, the device going away, and quitting the app. A call in its
  own window **never ends by inactivity**: the user put it there to keep
  it, so the 60 s rule applies only in the insets. Pop In restarts that
  rule with a fresh minute. (Both apps apply the same rule.)
- **Focus:** a call window opened by a ring never takes the keyboard focus
  from what the user is doing; an explicit Pop Out activates it, and so does
  Start Conversation opening one (the user asked for the call). Inside it,
  Tab moves between the controls and Return sends the reply.
- **On top:** the window floats above other windows by default, also over
  full-screen apps, with a pin toggle in its header whose state is
  remembered.
- **Several calls** each get a window; extra windows cascade by 24 pt.
- Call windows are not viewer windows: they are not restored at launch, not
  counted as viewer windows, and close with the app's other calls at quit.
  The picture's stream follows its size (Auto: the main stream when large).
- **Default:** a setting "Open calls in their own window" (Settings > Alerts >
  Doorbell calls, off by default) and a per-doorbell choice (Default, Window,
  Inset) that overrides it. It is decided when the call opens, for a ring and
  for Start Conversation; a press that joins an open call keeps the call
  where it is. A call opened in a window still records its inset in each
  viewer window, detached: not drawn, not counted in "+N more", not leased,
  not swapped or closed by Tab, Esc or the clock, until it pops in or the
  call ends.
- **Every viewer window has the call:** a viewer window opened while a call
  is open records it too (detached while the call is in its own window, else
  as its inset, or the controls over the stage when the doorbell fills it),
  and so does a window that had no item when the call pops in, so Pop In
  shows the inset in every viewer window. A conversation started in one
  window therefore shows in the others too, as a press does.
- **Settings (macOS):** Settings > Alerts > Doorbell calls has "Open calls
  in their own window" and "Open call windows"; Settings > Cameras has "Calls
  open in" per camera (Default, which names what the setting gives now;
  Window; Inset), saved with the camera's other options and left out of the
  file while it is Default.
- **Where it opens:** a setting "Open call windows" with "Where I left it"
  (the default) and "Top right of the active screen". Where I left it: the
  frame the user last left a call window at (one frame for the app, its size
  kept), as long as some screen's visible area still holds the middle of its
  top edge; each further window moves 24 pt right and down, kept on that
  screen. Otherwise, and always with the other choice: the top right corner
  of the visible area of the screen holding the active viewer window, 12 pt
  in, 480 pt wide by default (the remembered size when there is one, limited
  to the screen), each further window 24 pt left and down.
- **Layout (built on both apps; CAPTURE.md "The call
  window is a regular tile"):**
  the picture becomes the viewer's own tile in a single-tile mode (no
  layout editing, split, swap, focus, Show Camera, Remove or Clear; the
  window keeps its own lease), so zoom, the zoom map, fisheye views,
  Quality, Save picture and Record behave as in the grid. Top to bottom: a
  call bar (camera name, a press or a conversation, elapsed time, the
  "● Recording" pill, pin, Pop In, close); the tile with its hover header
  (Save picture, Record, Speaker or Unmute, PTZ, Quality, "…") and the
  Unmute cover as a veil inside it; the captions; the Talk row (Talk, Keep
  talking, the echo status); the reply field and Send. Record there is the
  call recording. Insets and call cards keep their drawing for now, with
  the same header buttons.
  - macOS: the tile is the viewer's `StageView` in a single fixed tile
    mode, under the call bar ("Doorbell · 14:02:11 · 0:42", the pin, Pop
    In, close) and above the captions, the Talk row and the reply. Wheel
    and pinch zoom, the zoom map, Reset Zoom (the header button or the
    menu), fisheye views and Quality work as in the grid; Quality and views
    chosen there stay with that window. Its right-click menu is the header
    model's list for a call window (Pop In Call, End Conversation, Record
    Call, Save Picture with ⇧⌘R and ⇧⌘P), with no layout items; focus,
    split and dropping a camera do nothing. The tile asks for the main
    stream while its picture, zoom included, is drawn wider than the
    sub-stream's (`CallWindowQuality`). Save Picture and Record Call say
    "coming soon" until capture lands; no recording pill yet.

- **Windows:** the call window is a WinUI window of its own with its own
  swap chain and renderer (a swap chain is never moved between windows).
  The picture is a `StageView` in single fixed tile mode in the middle of
  the call card, drawing the doorbell's shared tile with the same hover
  header, "…" and right-click menus (the model's call list, no layout
  items), digital zoom and zoom map, Quality and fisheye views as any
  tile, and recovering from a lost GPU device on its own; the window
  keeps one renderer and one lease. Above it the call bar has Pop In and
  a pin toggle; below it, the captions, Talk, and the reply field. The
  Unmute cover is a veil inside the tile. Quality and views chosen in the
  call window are not kept. Pop Out is the button in a card's header and an item in the
  card's menu (right-click) and the doorbell tile's menu. A ring opens the
  window without activating it (the user keeps typing where they are),
  Pop Out and Start Conversation activate it. The pin is "always on top"
  (remembered; whether it also floats over a full-screen app is not
  measured). Closing the window, Alt+F4, Ctrl+W, and Esc (while the reply
  field is not focused; Esc there only leaves the field) end the call.
  Settings has "Open calls in their own window", "Open call windows"
  (Where I left it, Top right of the active screen), and, under "Calls open
  in, per camera", each channel's Default, Window, or Inset; the tile menu
  has "Calls open in" too. The frame (physical pixels, a virtual-screen
  origin), the placement, the pin, and the choices are kept in
  `view-settings.json`; call windows are not in `windows.json`.

## Recordings

The NVR's own recordings, found by what happened and played back
([PLAYBACK.md](PLAYBACK.md), "Finding" and "Review mode"; the calls are
[FFI-RECORDINGS.md](FFI-RECORDINGS.md) sections 1 to 6). Built on macOS
(milestone P, M1 and M2) and on Windows (W1, W2); sound (M3: loudness
strips, sound events, listening for sounds) on both; the follow cut and
grids on both (M3 and W3). Exports (M4 and W4) come
later.

- **Where:** Window > Recordings (⌥⌘R) opens the Recordings window, or
  brings it forward. It is a window of its own, not a sidebar section:
  live view stays as it is while recordings are searched and played, and
  the window can sit on another screen. The menu item is disabled with a
  core that has no recordings. On Windows it is **Ctrl+Alt+R** or
  **Recordings** in the header's "…" menu (the tile menu holds no app-wide
  items); the window is one instance, opening it again brings it forward,
  and it hosts its own render thread and swap chain for the player (a
  panel never moves between windows), so a GPU device loss recovers it with
  the other windows. On Windows the pane buttons are the
  hamburger and the details icons, Ctrl-click adds an incident to the
  selection, Show in Explorer and the Recycle Bin (through `IFileOperation`)
  stand for Finder and the Trash, and the timeline's wheel zoom is not
  eased and has no pinch (the scroll bar and a touchpad pan it).
- **Layout:** seeing the clip comes first, so the **player** is the middle
  of the window, with the **transport** under it and the day's **timeline
  lanes** under that. On the left, collapsible, the month's calendar, the
  search state, the incident filters, the incident list, and the cameras to
  search (folded away once some are chosen). On the right, collapsible, the
  selected incident (the **inspector**). A header across the top has the
  two pane buttons, the shown day ("Today, Sunday, October 11, 2026" style,
  with previous and next day), and Player or Downloads (with their count).
  The side panes have fixed widths (280 and 290 points) and the player
  keeps at least 440: in a window too narrow for all three the inspector
  gives way first, then the sidebar, and turning a pane on in a narrow
  window widens the window instead; nothing is squeezed under or over
  another, and nothing reaches into the title bar. A message line at the
  bottom says what a request answered when it was refused or needs the
  user (for example that a locked incident's video is not being kept).
- **Cameras to search:** nothing is searched until the user chooses
  cameras, per device (one checkbox for all its cameras) or per camera (each
  lens of a multi-lens camera on its own). The choice is kept between
  launches (`recordings-cameras.json` in the app's data folder) and given to
  the core again at each launch; removing a device stops searching it.
- **Camera names** everywhere in the window (rows, inspector, player,
  lanes) are the camera list's: the user's name for the camera, else the
  name the device reports with the lens for a multi-lens camera ("Front ·
  Lens 2"), else "Channel N". The core names cameras the same way in its
  answers and download folders.
- **Days:** times are the device's own wall-clock time, as burned into the
  picture (the computer's time zone stands in for the device's until the
  core reads it). Showing a day asks the device for it; the window holds
  today (its own hold) only while it shows today, and lets go when it shows
  another day or closes. The calendar marks the days with recordings that
  are known: the shown day's month arrives with the day, and paging the
  calendar to another month asks the device for that month's calendar
  alone (once per window and set of cameras; minor 7). The refresh button
  searches the shown day again. A day the device could not search shows a
  warning on its lane and is not asked again by itself.
- **Incidents:** newest first, filtered by kind (People, Vehicles,
  Animals, Motion, Other as icon buttons; none chosen shows all; Animals
  matches every animal kind a device reports, `dog_cat` today and finer
  kinds later), by cameras (all, a camera group, or one camera), and
  Locked. Each row shows a strip of covers of a fixed width (two
  thumbnails apart from each other, "+N" for more cameras), so every row's
  text starts at the same place; then its name (or its time and kinds), its
  time span, the kinds as the live detection badges, its cameras, and marks
  for locked and downloaded. A click selects one, ⌘-click adds or removes
  one, and with several selected, Merge joins them. A row's menu has Play,
  Split Here (when it plays and the playhead is inside it), Remove from
  Incident (one of its cameras), Lock or Unlock, Download, and Revert Edits.
- **Playing:** selecting one incident **plays it at once** from its start
  less its margin (the clip is what the user came for; Space or the button
  pauses). The player shows the incident's first four cameras (the device
  plays at most four at once beside live view, PLAYBACK.md "Review mode")
  over the whole day, so the playhead can move past the incident; another
  incident of the same cameras only seeks, and an edit that adds or removes
  a camera adds or removes it without moving the playhead. Several cameras
  share the player in a grid (side by side, or two by two); a click on a
  camera makes it large with the others beside it, another click goes back,
  and a double click (or the one-camera button) shows it alone. The large
  (or first) camera's sound plays, through the core's tile audio like a
  live tile's. Each camera's label says when it is loading, catching up, at
  the end of its recording, or playing in another window, and a camera with
  nothing at the time says "No recording at this time". A camera's menu has
  Show Alone, Make Large, Show All Cameras, and Remove from Incident. Live
  view keeps running. The playback closes when another day is shown, the
  window closes, or the app quits.
- **Transport:** back and forward 10 s, play and pause, the device time at
  the playhead, the speed (0.25×, 0.5×, 1×, 2×, 4×, 8×, 16×; from 2× up
  "Keyframes only, no sound", below 1× "No sound below 1×"), Split Here,
  mute and volume, and the one-camera button. The scrubber spans the
  incident with its margins (or the whole day, with the Incident and Day
  switch) with the events marked by kind; dragging shows keyframes, the
  release the exact picture.
- **Follow and Grid** (macOS M3, Windows W3): the view menu at the right of the
  transport switches the player between **Cameras** (the device playback
  above) and three views made of the incident's **downloaded** files:
  **Follow Cut** (one picture that cuts to the camera with the action, the
  core's edit list, FFI-RECORDINGS.md section 7), **2-Up**, and **4-Up**
  (the cameras composed on a 1920 by 1080 canvas, letterboxed into the
  player; a cell is black where its camera has no video). The three are
  disabled, with "Download the incident for a follow cut or a grid", until
  the incident has a finished download. They start at the playhead; the
  device playback pauses meanwhile (nothing is closed), and going back to
  Cameras continues it from the edit's position. Selecting another
  downloaded incident shows its own edit; one not downloaded goes back to
  Cameras. The pictures are the files decoded by VideoToolbox and drawn by
  the same Metal grid renderer as live view, nothing re-encoded; seeking is
  frame accurate (decoding from the keyframe before the position), and while
  dragging the scrubber and from 2× up only keyframes are decoded, as the
  device does. The same transport drives it: play and pause (Space), back
  and forward 10 s, the speeds, mute and volume, and the scrubber, which
  spans the edit's range with the incident's events and, in the follow cut,
  each cut as a white mark. A lane click inside the edit's range on one of
  its cameras seeks the edit; elsewhere the cameras come back. The follow
  cut's label is the shown camera's name and the device time; each grid
  cell has its camera's name. **Sound** follows the picture: in the follow
  cut the shown camera's, cutting with it; in a grid the first cell's, or
  the cell clicked; at 1× only, as in Cameras. A file that cannot be opened
  draws black and the player says so. The same drawing serves exports (M4):
  `EditSession` renders any time of the edit offscreen at any size.
  **On Windows** the view menu is a drop-down button in the second row of
  the transport (Cameras, Follow Cut, 2-Up, 4-Up; the three disabled with
  the same words until the incident has a finished download). The files are
  decoded by Media Foundation (an `IMFSourceReader` per file on the shared
  D3D11 device's DXGI manager, so on the GPU where it can, else on the CPU)
  into the same kind of picture the live tiles use and drawn by the window's
  own `StageRenderer` in place of its tiles, so the look matches the live
  grid. The keyframes the seeks start from are read from the MP4's sample
  tables (`Mp4Index`: sync samples, composition offsets, and the edit list,
  the timeline the plan's `source_in_ms` uses); the decoding is the macOS
  one (frame accurate from the keyframe at or before, keyframes only while
  dragging and from 2×, the newest request winning on a pool thread per
  file, at most the picture chosen and the one read past it held). The
  sound is a Windows `MediaPlayer` per file, the audible one kept within
  250 ms of the picture. Space plays and pauses what the player shows after
  a click on it; clicking a grid's cell picks the cell whose sound plays.
  Cuts are white ticks on the scrubber, labels are the shown camera's name
  (and the time in the follow cut) with a speaker mark on the audible one,
  and a cell without video says "No recording at this time" or "This file
  could not be opened". The same drawing serves exports (W4):
  `EditFiles.Render` draws any time of the edit into a `GridRenderer`'s
  offscreen frame of any size.
- **Timeline:** one lane per camera for the day under the player, recording
  spans in gray, events as bars colored by kind with the kind's symbol when
  there is room, an hour ruler, zoom from the whole day to 48×, and the
  playhead. The lanes zoom as a tile's digital zoom does ("Digital zoom"):
  a mouse wheel zooms around the pointer (the time under it stays put),
  each notch 1.25×, eased; Shift and the wheel pans; a horizontal
  two-finger scroll pans and a pinch (or Option and a two-finger scroll)
  zooms; a vertical two-finger scroll scrolls the lanes when there are more
  than fit. The zoom buttons zoom around the middle. The lanes draw only the
  visible part of the day, and the ruler's ticks get finer as it zooms (down
  to 5 minutes). A click on a lane of a camera that plays seeks there; a click on
  an event of another camera selects its incident; anywhere else that
  camera plays alone from there.
- **Sound** ([PLAYBACK.md](PLAYBACK.md) "Sound"; FFI-RECORDINGS.md
  section 2a). Under each lane a **loudness strip** (10 points) shows the
  camera's loudness for the day from `nv_recordings_loudness`: per bucket
  the loudest second light and the average dark, from -80 dBFS (nothing)
  to 0 dBFS (full height), blank where no audio was analyzed, which is not
  silence. A legend under the lanes says so. The bucket follows the zoom,
  about one value per two points (60 to 300 s for the whole day, down to a
  few seconds at 48×); the strip of a camera's second lens shows the first
  lens's microphone. Today's strips are read again every 10 s (live seconds
  come with no event; the call answers from memory). **Sound events**
  (`source` `local_audio`: smoke and CO alarms from the core's detector,
  other sounds from labels an app classifier puts) are bars in the lanes
  like other events, alarms in their kind's color and other sounds in their
  own (with a waveform symbol on macOS); they join incidents, the
  incident rows show a sound badge with the sounds' names on hover (alarms
  keep their own badges), and a **Sounds** kind filter shows only incidents
  with a sound (the sound kinds, or any event from sound). **Listen for
  sounds** is a per-camera switch (an ear on macOS, a speaker on Windows)
  beside each camera in "Cameras to search": `nv_sound_watch` keeps that
  camera's sub-stream open with no tile, so its sound is analyzed while it
  is not on screen. It costs the device a sub-stream (about 110 kbit/s)
  that counts toward its streams like a tile, so at most four cameras of a
  device listen (a device carried six sub-streams on one session, and the
  player takes up to four); a fifth is refused with a message. The switch's
  tooltip says what the core answered (listening, no sound from this
  camera, the device is not connected). The list is kept with the cameras
  to search (`listen` in `recordings-cameras.json`) and given to the core
  at each launch. Not built yet: analyzing a recorded day
  (`nv_sound_analyze_day` answers `no_source` until the core has a source
  for recorded audio), and the app classifiers that put labels (SoundAnalysis
  on macOS, a model on Windows).
- **The selected incident (inspector):** its name (Rename; an empty name
  goes back to time and kinds), time span and date, badges, and camera
  group; Locked (locking keeps it: its video is downloaded and nothing about
  it expires; the message line says when its video is not being kept
  because there is no downloads folder or too many downloads; unlocking
  never touches files); Download; Revert Edits when edited; **Split Here**
  at the playhead, with what it does in words (the incident becomes two at
  the playhead: what happened before stays, what follows becomes a new
  incident; Revert Edits undoes it); and each camera with its thumbnail and
  **Remove from Incident** (not the last camera), for a camera that only
  happened to record at the same time; Add Camera from the searched
  cameras. Removing a camera is the finest edit there is: the core's edit
  log has no way to leave out one event (FFI-RECORDINGS.md section 3).
- **Thumbnails:** asked for as rows appear; a spinner until the device's
  keyframe arrives, which the app decodes (VideoToolbox), shrinks to 320
  points, and gives back to the core's cache as a JPEG, so the next time it
  is read as is. A crossed-out camera means no recording in the range. The
  window keeps at most 512 in memory; queued fetches are dropped when it
  shows another day or closes.
- **Downloads** (the header's Downloads): every download with its folder's
  name, state (Queued, Waiting for the device, Downloading, Paused,
  Finishing, Done, Failed with the reason), progress, and each camera's
  chunks; Pause or Resume, Retry (failed), Show in Finder (the files, or
  the folder until they exist), Forget (not while running; files stay), and
  Move to Trash (the core stops writing first, then the folder goes to the
  Trash; nothing is deleted permanently). Progress redraws at most four
  times a second. Downloads go to `~/Movies/NVRView/Incidents` (`Incidents`
  in `--data-dir`).
- **Camera groups:** "Camera Groups…" (with the cameras to search) edits
  the groups whose cameras cover the same area, so their events join one
  incident; it says that changing a group changes its incidents.
- **Never in the way:** every query reads the core's memory; device work
  (searches, thumbnails, downloads, plays) reports by events; the player's
  position is read ten times a second from memory; decoding runs off the
  main thread. Self-test steps `recordings-*`, `playback-*`, and `edit-*`
  (the fake core) and `recordings-real-core` and `playback-real-core`
  (`scripts/macos-real-selftest.sh`, against the mock).

## Finding devices

Adding a device starts from a list of what answers on the network, so the
user types a password instead of an address. The core does the searching
(docs/FFI.md, "Device discovery"); the apps only ask and show.

- **Where.** The add-device screen (Settings > Devices, the new-device
  form) opens with "Find Devices on This Network" above the form. A search
  starts when the screen is shown (also when its window is shown again after
  a close) and stops when the user picks an existing device, closes the
  window, or leaves the form. Nothing searches in the background; the app
  never searches on its own.
- **The list.** One row per device, in the order they answered: a kind icon
  (recorder, camera, unknown), the name (else "NVR", "Camera" or "Device"),
  the model when known, the address (and port), and the channel count or a
  battery percentage when the answer carried one. A device that answers
  again with more updates its row. While the search runs a spinner and
  "Searching..." show; afterwards the count. **Search Again** restarts the
  search and empties the list. At most 1024 rows.
- **Already added.** A device whose address and port match one the user
  added shows "Added" and cannot be chosen. A found device that offers no
  protocol the viewer speaks (an ONVIF-only camera) shows "Not supported
  yet" and cannot be chosen.
- **Choosing a row** fills the form's address and port (9000 when the answer
  had no port), and the name while it still has the default one; the user
  types the user name and password. Nothing is added or logged in to until
  the user presses Add Device.
- **Nothing found.** The list says so and points at the usual cause on a Mac,
  Local Network access still waiting for the user (macOS drops the
  broadcasts without an error): allow the app in System Settings > Privacy &
  Security > Local Network, then Search Again. On Windows the first search may
  raise a Windows Firewall prompt for the app (it listens on UDP port 3000
  for the answers); allowing it on private networks is what makes answers
  arrive, and the same hint applies. Typing the address stays possible
  throughout. Interfaces that could not be searched (a busy port) show as a
  line with the details in a tooltip.
- **Privacy.** Device IDs and MAC addresses are never shown or logged;
  addresses appear on this screen only.

## Sidebar

- Whether a window's sidebar is open or collapsed is saved with the window
  and restored on relaunch (window restore).
- **Edge reveal:** while it is collapsed, resting the pointer at the window's
  far left edge for a moment (about 150 ms, so passing over does not count)
  slides the sidebar in over the video as an overlay: the stage is not
  resized or redrawn. It slides away about half a second after the pointer
  leaves it; clicking or dragging cameras from it works as usual. S, or a pin
  control in the overlay, docks it again (which resizes the stage as before).
  The same works in full screen. On macOS the system's own peek of a
  collapsed sidebar (macOS 26 opens it into the window when the pointer rests
  near the edge, pushing the stage aside) is turned off; the edge reveal
  takes its place.

## Windows header

On Windows the window's title bar is a header the app draws
(`ExtendsContentIntoTitleBar`; the system's minimize, maximize, and close
buttons stay, and so do the snap layouts on the maximize button). It takes the
system's title bar height and display scale, and tells the system which of its
controls take the pointer; everything else drags the window. macOS keeps its
menu bar (docs/PARITY.md): this is a Windows-only part.

- **Left.** The app name and the window's current layout with a picker: the
  layouts, All Cameras, and the groups to switch to, then New layout, New
  layout from template, and Save as layout (an unsaved arrangement only).
- **Middle.** The app-wide quick actions: Find devices (opens Settings on
  Devices and searches the network), Edit layout (Ctrl+E, a toggle), Fill
  tiles (a toggle, per window, not kept), and Full screen (F).
- **Right.** A spot reserved for a future "N recording" indicator (hidden until
  recording is reported), Settings (Ctrl+,), and a "…" menu with the rest:
  New window (Ctrl+N), Recordings (Ctrl+Alt+R), Show or Hide sidebar (S), Indicator corner, Show or
  Hide diagnostics (Ctrl+D), Keyboard shortcuts (F1), and the update entry.
- **Full screen.** The header is hidden and slides over the stage from the top
  edge like the sidebar does from the left: resting the pointer in a thin strip
  at the top for about 150 ms reveals it as an overlay (the stage is not
  resized); it slides away about half a second after the pointer leaves it,
  unless a menu of the header is open. The timing is the sidebar's
  (`SidebarReveal`; `HeaderChrome` adds the full screen switch).
- **The tile menu** (right-click) holds only what concerns the tile under the
  pointer: Focus, Listen, Start or End Conversation, Pop Out Call, Calls open
  in, Allow talk-back, Unmute on ring, Reset zoom, Quality, Fisheye view and
  mount, Dismiss alert, and, outside focus, the layout actions for that tile
  (split, remove, clear, Show camera, Layout preset). Nothing under the pointer:
  no menu. Every app-wide item it once held is in the header.

## Settings window

Settings (Windows) is a resizable window of its own, not a dialog, so a tall
section scrolls. It opens from the header, Ctrl+, , a device row's menu, or
Find devices; there is one instance, brought to the front (on the section
asked for) if it is already open. It remembers its size and position, closes
with the app, and stops the device search when it closes or when the Devices
section is left. A pane lists the sections, each in a scrolling page:

- **Devices.** The device list and the add or edit form, with Find Devices on
  This Network ("Finding devices").
- **Cameras.** Per camera: where its calls open, unmute on ring, allow
  talk-back (off until allowed; every use is logged), and for fisheye cameras
  the mount.
- **Alerts & calls.** The corner of the detection symbols and of the call cards,
  calls in their own window and where those open, and saved transcripts.
- **Updates.** The update source, trusted keys, automatic checks, Check now.
- **About.** The version and license.

### Update prompt

"NVRView X is available" shows the title, the "You have ..." line, and the
release notes (plain text, up to 2000 characters) in a text area of bounded
height (about 220 points) that wraps, is selectable, and scrolls with the
mouse wheel and, once focused, the arrow, Page, Home, and End keys. Short notes
get a short area. The title, the line, and the buttons (Download / Not Now)
never scroll away. macOS: an alert sheet with the notes as its accessory
view; Windows: a dialog with the same area.

## Windows notes

The Windows app (`apps/windows/NvrView.App`) follows these requirements
with Windows conventions:

- Ctrl takes the place of ⌘ (Ctrl+L listens, Ctrl+, opens Settings), and
  Ctrl+click on a speaker listens to several tiles.
- Shortcuts are listed in a help overlay (F1), opened from the header's "…"
  menu, which also has diagnostics (Ctrl+D). Settings and full screen are
  buttons in the header ("Windows header").
- Full screen is the window's own full-screen presenter (F, and Esc after
  leaving focus).
- A touchpad pinch arrives as Ctrl+mouse wheel and zooms like the wheel
  (digital zoom, or the field of view of a Virtual PTZ tile). A touch
  screen's two-finger pinch zooms a Virtual PTZ tile by the pinch's scale
  (the field of view is divided by it, 120° to 20°, each step bounded); on
  other tiles it zooms the digital zoom around the point between the
  fingers, with the same bounded steps.
- Fill Tiles is the header's "Fill tiles" toggle; each tile's menu has
  "Fill tile", "Reset position" and "Reset zoom" (see "Fill Tiles").
  The zoom map sits in the tile's chrome and the reset control is a
  magnifier button at the right of the hover header, shown while the tile
  is zoomed (or a virtual camera was moved).
- Settings > Cameras lists "Unmute on ring even when not listening" with a
  check box per camera (the macOS Settings > Cameras option), kept with the
  other view settings; the tile menu has the same toggle.
- Settings > Cameras lists each fisheye camera with a Mount picker (Ceiling,
  Wall, Desk; default Ceiling), kept with the other view settings like the
  right-click menu's Fisheye mount, and tiles in every window follow it at
  once.
- Corner indicators use Segoe Fluent Icons (a vector paw for animals); the
  corner is chosen in Settings > Alerts & calls or the header's "…" menu. A sound alarm also
  shows as a slate badge at the bottom left. Alerts, the Quality submenu, M,
  and the full screen idle hiding of the chrome and the pointer follow the
  sections above; the red border is steady when Windows' "Show animations" is
  off, as Reduce Motion is on the Mac.
- Layouts, groups, and windows follow the sections above. Edit Layout is
  Ctrl+E or the header's Edit layout button (the edit bar holds the name, split, remove,
  presets, and Save as layout); Ctrl+N opens a new window. Layouts are the
  same documents the macOS app stores. The edge reveal, its timing, and its
  pin are as described under "Sidebar"; the sidebar's width is remembered
  with the window.
