#!/bin/sh
# Runs the workspace tests and the C interface smoke test on Linux, in a
# Docker container, so Linux-only breaks show up before a push.
#
# By default the build output and the cargo registry live in Docker volumes
# (nvrview-linux-target, nvrview-linux-cargo), so the Mac's target/ is never
# touched and later runs are incremental. The build volume is emptied after a
# run once it grows past NVRVIEW_LINUX_TARGET_MAX_MB (default 6000), so it
# cannot grow without bound. NVRVIEW_LINUX_CACHE=0 keeps nothing: the run uses
# throwaway volumes that are deleted with the container (every run builds
# from scratch). scripts/clean-docker.sh removes the volumes.
set -eu
cd "$(git rev-parse --show-toplevel)"

image="${NVRVIEW_LINUX_IMAGE:-rust:1-bookworm}"
max_mb="${NVRVIEW_LINUX_TARGET_MAX_MB:-6000}"

if [ "${NVRVIEW_LINUX_CACHE:-1}" = "0" ]; then
  # Anonymous volumes: `docker run --rm` deletes them with the container.
  target_vol=/src/target
  cargo_vol=/usr/local/cargo/registry
else
  target_vol=nvrview-linux-target:/src/target
  cargo_vol=nvrview-linux-cargo:/usr/local/cargo/registry
fi

docker run --rm \
  -v "$PWD":/src \
  -v "$target_vol" \
  -v "$cargo_vol" \
  -w /src \
  -e CARGO_TERM_COLOR=never \
  -e MAX_MB="$max_mb" \
  "$image" \
  sh -c 'git config --global --add safe.directory /src
    status=0
    { cargo test --workspace --locked --quiet && scripts/test-c-ffi.sh; } || status=$?
    used=$(du -sm target | cut -f1)
    if [ "$used" -gt "$MAX_MB" ]; then
      echo "Linux build volume at ${used} MB (cap ${MAX_MB} MB): emptying it"
      find target -mindepth 1 -maxdepth 1 -exec rm -rf {} +
    fi
    exit $status'
