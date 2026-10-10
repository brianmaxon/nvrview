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

## Chrome

- Headers, buttons, and status badges appear on hover and fade out; the
  picture is the interface.
- A tile that is reconnecting says so in words with its retry countdown;
  detections show as small type badges (people, vehicle, animal) with the
  source (camera AI or this Mac).
- Measured against the budgets in ARCHITECTURE.md: the interface must never
  cost frames. Overlays are lightweight layers above the Metal surface.
