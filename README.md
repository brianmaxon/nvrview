# nvrview

A lightweight, native live viewer for **Reolink** NVRs and cameras. It speaks
the devices' own protocol (Baichuan, TCP 9000) directly, with hardware video
decoding and no browser engine: a low-power alternative to the official
Reolink desktop client. macOS (Apple Silicon) first; Windows planned.

Status: planning. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

nvrview is an independent project. It is not affiliated with, endorsed by, or
sponsored by Reolink. "Reolink" is a trademark of its owner and is used here
only to describe compatibility.

## Contributing

This repository is public and the software talks to private security
equipment. Before your first commit, read:

- [docs/SANITIZATION.md](docs/SANITIZATION.md): what must never be committed
- [docs/CLEAN-ROOM.md](docs/CLEAN-ROOM.md): how protocol knowledge enters the code

and run `scripts/setup-dev.sh`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
