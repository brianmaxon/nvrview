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
  source (camera AI or this Mac).
- Measured against the budgets in ARCHITECTURE.md: the interface must never
  cost frames. Overlays are lightweight layers above the Metal surface.

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
| ⌘L (Ctrl+L) | Listen to the selected tile (as before). |

Selection is visible as a thin outline that fades with the other chrome, and
follows a click on a tile. Shortcuts are listed in the menus (macOS) or a help
overlay (Windows) so they can be discovered.

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
