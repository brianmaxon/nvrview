#!/bin/sh
# Runs the workspace tests and the C interface smoke test on Linux, in a
# Docker container, so Linux-only breaks show up before a push. The build
# output and the cargo registry live in Docker volumes (nvrview-linux-target,
# nvrview-linux-cargo), so the Mac's target/ is never touched and later runs
# are incremental. Remove them with
#   docker volume rm nvrview-linux-target nvrview-linux-cargo
set -eu
cd "$(git rev-parse --show-toplevel)"

image="${NVRVIEW_LINUX_IMAGE:-rust:1-bookworm}"

docker run --rm \
  -v "$PWD":/src \
  -v nvrview-linux-target:/src/target \
  -v nvrview-linux-cargo:/usr/local/cargo/registry \
  -w /src \
  -e CARGO_TERM_COLOR=never \
  "$image" \
  sh -c 'git config --global --add safe.directory /src &&
    cargo test --workspace --locked --quiet &&
    scripts/test-c-ffi.sh'
