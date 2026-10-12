#!/bin/sh
# Runs the workspace tests and the C interface smoke test on Linux, in a
# Docker container, so Linux-only breaks show up before a push.
#
# The Mac's target/ is never touched. Build output lives in an anonymous
# volume that `docker run --rm` deletes with the container, so no number of
# checkouts or agents leaves anything behind in Docker. Speed comes from the
# shared caches only (see scripts/docker-rust-image.sh): the cargo registry
# and sccache. NVRVIEW_LINUX_CACHE=0 uses throwaway cache volumes too (every
# run builds from scratch). scripts/clean-docker.sh removes the shared caches.
set -eu
cd "$(git rev-parse --show-toplevel)"
. scripts/docker-rust-image.sh

if [ "${NVRVIEW_LINUX_CACHE:-1}" = "0" ]; then
  cargo_vol=/usr/local/cargo/registry
  sccache_vol=/sccache
else
  cargo_vol=nvrview-linux-cargo:/usr/local/cargo/registry
  sccache_vol=nvrview-sccache:/sccache
fi

# shellcheck disable=SC2086 # $rust_env is a list of -e options
docker run --rm \
  -v "$PWD":/src \
  -v /src/target \
  -v "$cargo_vol" \
  -v "$sccache_vol" \
  -w /src \
  -e CARGO_TERM_COLOR=never \
  $rust_env \
  "$rust_image" \
  sh -c 'git config --global --add safe.directory /src
    cargo test --workspace --locked --quiet && scripts/test-c-ffi.sh'
