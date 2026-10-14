# nvrview

A lightweight, native viewer for **Reolink** NVRs and cameras, for macOS and
Windows. It talks to the devices directly over their own protocol (Baichuan,
TCP port 9000) with hardware video decoding and no browser engine: a fast,
low-power alternative to the official desktop client. Both apps share one
Rust core.

nvrview is an independent project. It is not affiliated with, endorsed by, or
sponsored by Reolink. "Reolink" is a trademark of its owner and is used here
only to describe compatibility.

## Features

- **Live view:** split-pane layouts and camera groups, several windows, a
  full-screen tile on the main stream, digital zoom and pan, fisheye views,
  multi-lens cameras, listening, and the cameras' own person, vehicle and
  animal detections.
- **Doorbells:** calls with live captions and two-way talk (by voice or a
  typed reply), started by a ring or by you, and recorded if you choose.
- **Pan, tilt and zoom:** a pad, presets, and lens zoom and focus, with
  moves that always stop when you let go.
- **Recordings:** a calendar and per-camera timeline with events and sound,
  synchronized playback of up to four cameras at 0.25x to 16x, quality per
  camera, and zoom and fisheye views on recorded video.
- **Export:** trims, a follow cut or 2-up and 4-up videos with sound, time
  ranges, and stills, with a share-safe option that leaves out names, times
  and metadata.
- **Recording live video** to MP4 on your computer.
- **Smart alerts** (macOS): a motion sensor or other MQTT trigger wakes the
  matching cameras, an on-device detector confirms a person, vehicle or
  animal, and only then does it alert you (and, if you like, Home Assistant).
  An Events window shows every decision with its pictures.
- **Self-updates** from a folder or web server you host, signed with your
  own key.

Both apps have these features except smart alerts and the alert insets
(doorbell, smoke and entrance pictures over the layout), which are macOS-only
for now; Windows shows doorbell calls as call cards. The details, feature by
feature, are in [docs/PARITY.md](docs/PARITY.md).

**Status:** young and moving fast. It is used daily with real NVRs and
cameras, but expect rough edges. There are no prebuilt downloads here yet;
build from source as below.

## Building and running

### macOS

Needs an Apple silicon Mac with macOS 26, Xcode 26, and Rust (through
[rustup](https://rustup.rs)).

```sh
git clone https://github.com/brianmaxon/nvrview.git
cd nvrview
scripts/package-macos.sh        # builds build/NVRView.app with the Rust core
open build/NVRView.app
scripts/restart-macos-app.sh --build   # later: rebuild, then quit and reopen it
```

The app is signed with your Apple Development identity when you have one
(ad hoc otherwise) and is not notarized.

### Windows

Needs Windows 11 (x64). `scripts/setup-windows.ps1` installs the toolchain;
then, in Git Bash from the repository root:

```sh
scripts/windows-app.sh build
target/dotnet/bin/NvrView.App/release_win-x64/NvrView.App.exe
```

The result is a self-contained folder that runs from wherever you copy it.
More in [apps/windows/README.md](apps/windows/README.md).

### First run

Add your NVR or camera in **Settings**: the form lists devices found on the
network (or type the address, port 9000, user and password). Passwords are
kept in the macOS Keychain or Windows Credential Manager.

### Without a camera

`tools/mock-nvr` serves synthetic streams, so you can try the app (and run
the tests) without hardware:

```sh
cargo run -p mock-nvr -- --port 9100 --channels 4
NVRVIEW_PASSWORD=example-password \
  build/NVRView.app/Contents/MacOS/NVRView --host 127.0.0.1 --port 9100 --user example-user
```

## Repository

| Path | What |
| --- | --- |
| `crates/` | The shared Rust core: protocol, sessions, live and recorded media, audio, alerts, the C interface |
| `apps/macos/` | The macOS app (Swift, AppKit and SwiftUI, Metal, VideoToolbox) |
| `apps/windows/` | The Windows app (C#, WinUI 3, Direct3D 11, Media Foundation) |
| `tools/` | `mock-nvr` (a fake device), `bc-cli` (a command-line client), `soak` |
| `docs/` | Design and contracts |
| `scripts/` | Checks, packaging, setup |

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): the design
- [docs/UX.md](docs/UX.md): how the apps behave
- [docs/protocol.md](docs/protocol.md): what is known about the device protocol, and how
- [docs/UPDATES.md](docs/UPDATES.md): self-hosted updates and releases
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): checks, test computers, and the test result cache

## Contributing

This repository is public and the software talks to private security
equipment. Before your first commit, read
[docs/SANITIZATION.md](docs/SANITIZATION.md) (what must never be committed)
and [docs/CLEAN-ROOM.md](docs/CLEAN-ROOM.md) (how protocol knowledge enters
the code), run `scripts/setup-dev.sh`, and run `scripts/check.sh` before
committing ([docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
