# Interface requirements

What the macOS app must feel like, written from the official app's pain
points. Each item is a requirement for milestone M3 and later, not a
suggestion; [ARCHITECTURE.md](ARCHITECTURE.md) has the goals and
[PLAYBACK.md](PLAYBACK.md) the playback screens.

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
| 0 | Reset digital zoom on the selected tile. |
| M | Listen to the selected tile, or mute it if it is the one playing (⌘L / Ctrl+L does the same). |

Selection is visible as a thin outline that fades with the other chrome, and
follows a click on a tile. Shortcuts are listed in the menus (macOS) or a help
overlay (Windows) so they can be discovered.

### Start Conversation (no press)

Captions, Talk, and the typed reply must not wait for a ring: a visitor may
be at the door without pressing anything, or the user may want to speak to
someone through a camera's speaker.

- **Where.** A camera tile's context menu has "Start Conversation" (and "End
  Conversation" while its call is open); on Windows, "Start conversation" and
  "End conversation", next to "Allow talk-back" (see below). It is offered on every camera tile,
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
  needs the camera's "Allow talk-back" setting, and is rate-limited and
  logged like any other (camera motion control, CLAUDE.md).
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
- The zoom belongs to the tile: it survives layout changes and focus, and
  when focus switches the camera to the main stream the same region is
  shown, now with the main stream's detail (the region is kept in picture
  coordinates, independent of resolution).
- Double-click keeps its meaning (focus); 0 or a small reset control in the
  hover header returns to 1x; zoom never changes what is recorded or
  streamed.
- A small inset map of the whole picture, with the visible region outlined,
  shows while zoomed and fades with the chrome.
- How: zoom only changes the source rectangle the renderer samples from the
  decoded picture (no extra decoding or copies), so it costs nothing per
  frame beyond the drawing that already happens.

Details settled while building it (macOS):

- A plain drag in a tile zoomed past 1x pans its picture. At 1x, and always
  while editing the layout, a drag drags the tile to swap, as before.
- A mouse wheel (line-based scrolling) zooms, each notch 1.25x, eased over
  the next refreshes. Precise scrolling (trackpad, Magic Mouse) pans a zoomed
  tile and does nothing at 1x; with Option held it zooms instead, for devices
  without a pinch. A pinch zooms continuously.
- The zoom is kept per camera in each window, so it follows the picture
  through swaps, layout changes, focus and stream switches; two tiles of the
  same camera in one window share it.
- The inset map is an outline of the whole picture with the visible region
  marked, drawn as overlay layers (no second picture, so no extra drawing or
  texture work), in the shown picture's bottom left corner.

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
  docs/protocol.md before any code sends them.
- **Live transcript:** from the press, both sides are transcribed on this
  computer (a local speech recognition model on both macOS and Windows,
  possibly a different engine on each; nothing leaves the house): the
  doorbell's audio and, while talking back, the microphone, labeled by side,
  so the visitor can be read when the user cannot unmute (in a meeting, for
  example). Engines (measured 2026-10-10 on doorbell-quality speech: 16 kHz,
  outdoor noise, low-bitrate AAC): Apple's SpeechAnalyzer on macOS (about 4%
  word errors on clean speech, about 1% of a core, no model to ship) and
  Moonshine's streaming models (MIT) on Windows (about 4-5% clean; the tiny
  and base models are tried first on modest CPUs, with sherpa-onnx as a
  lighter fallback), behind one engine interface in each app. One session per
  source (doorbell, microphone), so the speaker label is the source; while
  doorbell audio plays through the speakers the microphone session is muted
  or echo-cancelled so it does not transcribe the visitor twice. Shown live in the call; saved with the event only if the user
  turned saving on (recording a conversation may need the other person's
  consent in some places).
- **Typed reply:** a text box in the call speaks what the user types through
  the doorbell's speaker with the system's text-to-speech (the same Talk
  permission and limits). Text comes only from what the user types or picks
  from their own templates, never from the transcript, detected text, or any
  other untrusted source (see docs/ARCHITECTURE.md "Doorbell text-to-speech").
- The call ends when the user closes it or about 60 s after the last activity
  (press, speech, talk-back), and is listed in the event log with its times
  and, if saved, its transcript.

Details settled while building it (macOS):

- The Unmute control covers the inset's picture (or the stage's, when the
  doorbell fills it); the Talk control and the reply box stay reachable below
  it. Cameras muted for the call play again when it ends, unless the user
  changed what plays meanwhile.
- Talk: hold the button, or turn on "Keep talking". Both, and Send, ask the
  core for talk-back; without the doorbell's "Allow talk-back" (Settings >
  Cameras, off by default) the core refuses with `not_permitted` and the call
  says so. The permission is passed to the core after every connection and
  whenever it changes. The microphone is asked for (the system prompt) only
  when Talk is first pressed; a typed reply needs no microphone.
- A typed reply is rendered by the system voice into buffers (never played
  locally), converted to 16 kHz mono, held until talk-back is active, pushed
  in one go, and ended with a draining stop unless Talk is held.
- The visitor's transcript comes from the doorbell tile's audio tap (16 kHz
  mono), enabled for the call; the user's from the microphone while talking,
  not while the doorbell's audio is audible and carrying sound (and 0.4 s
  after). Apple's SpeechTranscriber (volatile and fast results) runs on the
  Mac; while it installs its English assets the call says "Preparing speech
  recognition", and without them, or without the speech recognition
  permission, the call goes on without a transcript.
- The app logs every talk-back start, refusal and stop, typed replies (their
  length, not their text) and the call's end in the session's event log; the
  transcript goes there only with "Save call transcripts" (Settings >
  Alerts), never to the system log.

Details settled while building it (Windows):

- The same call, a press or Start conversation: a card per call in the
  stage's bottom right corner of every window, with the live captions, an
  Unmute control when the call opened silent, Talk (hold the button, or
  "Keep talking"), the typed reply box with Send, and a close control that
  ends the call. Windows has no alert insets yet, so a press does not bring
  the doorbell's video onto the screen: the card opens, and the picture shows
  only if a tile shows the doorbell.
- Talk-back needs the camera's permission, which on Windows is the tile
  menu's "Allow talk-back" (a checked item, off by default, kept in
  `view-settings.json` and passed to the core before every start and when it
  changes; withdrawing it stops a running talk). The call says where to turn
  it on when the core refuses with `not_permitted`.
- The microphone is WASAPI capture (no permission prompt for desktop apps; if
  Windows blocks it the call says so). A typed reply is rendered by the
  system voice (`Windows.Media.SpeechSynthesis`) into memory, never played on
  the computer, and pushed to the camera at the voice's own rate; the core
  resamples it. The echo gate is the Mac's: the microphone is not
  transcribed while the camera plays through the speakers, and 0.4 s after.
- Captions use the desktop speech recognizer of Windows (`System.Speech`,
  local, no model to ship), one session per source, fed the same 16 kHz audio
  as on the Mac. It is a stand-in for the engine docs/ARCHITECTURE.md plans
  (Moonshine, measured on doorbell-quality speech before it replaces this):
  where Windows has no recognizer for the language, the call says "No
  transcript" and goes on. Partial results show with an ellipsis until the
  recognizer settles on a phrase, and a phrase it is not sure of stays a
  partial.
- Not on Windows yet: the setting that saves call transcripts into the event
  log (a call is logged without its text) and a per-doorbell "unmute on ring"
  (a press unmutes only if the user was already listening, else Unmute shows).

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
  The same works in full screen.

## Windows notes

The Windows app (`apps/windows/NvrView.App`) follows these requirements
with Windows conventions:

- Ctrl takes the place of ⌘ (Ctrl+L listens, Ctrl+, opens Settings), and
  Ctrl+click on a speaker listens to several tiles.
- Shortcuts are listed in a help overlay (F1) and in the right-click menu,
  which also carries Settings, full screen, and diagnostics (Ctrl+D).
- Full screen is the window's own full-screen presenter (F, and Esc after
  leaving focus).
- A touchpad pinch arrives as Ctrl+mouse wheel and zooms like the wheel;
  touch-screen pinch is not handled yet.
- Corner indicators use Segoe Fluent Icons (a vector paw for animals); the
  corner is chosen in Settings or the right-click menu. A sound alarm also
  shows as a slate badge at the bottom left. Alerts, the Quality submenu, M,
  and the full screen idle hiding of the chrome and the pointer follow the
  sections above; the red border is steady when Windows' "Show animations" is
  off, as Reduce Motion is on the Mac.
- Layouts, groups, and windows follow the sections above. Edit Layout is
  Ctrl+E or the right-click menu (the edit bar holds the name, split, remove,
  presets, and Save as layout); Ctrl+N opens a new window. Layouts are the
  same documents the macOS app stores. The edge reveal, its timing, and its
  pin are as described under "Sidebar"; the sidebar's width is remembered
  with the window.
