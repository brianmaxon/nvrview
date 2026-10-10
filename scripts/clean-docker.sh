#!/bin/sh
# Removes what the Linux check (scripts/linux-check.sh) leaves in Docker: its
# build and cargo-registry volumes, and with --image also the Rust image it
# pulled. Touches nothing else in Docker. The next Linux check rebuilds from
# scratch (a few minutes).
#
#   scripts/clean-docker.sh            remove the nvrview-linux-* volumes
#   scripts/clean-docker.sh --image    also remove the Rust image
#   scripts/clean-docker.sh --dry-run  show what would be removed
set -eu

image="${NVRVIEW_LINUX_IMAGE:-rust:1-bookworm}"
remove_image=0
dry_run=0
for arg in "$@"; do
  case "$arg" in
    --image) remove_image=1 ;;
    --dry-run) dry_run=1 ;;
    *) echo "usage: $0 [--image] [--dry-run]" >&2; exit 2 ;;
  esac
done

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  echo "Docker is not running; nothing to clean."
  exit 0
fi

run() {
  if [ "$dry_run" = 1 ]; then echo "would run: $*"; else "$@"; fi
}

# Containers from the check are started with --rm; remove any left by an
# interrupted run that still hold the volumes.
for c in $(docker ps -aq --filter volume=nvrview-linux-target --filter volume=nvrview-linux-cargo); do
  run docker rm -f "$c"
done

for v in $(docker volume ls -q --filter name='^nvrview-linux-'); do
  run docker volume rm "$v"
done

if [ "$remove_image" = 1 ] && docker image inspect "$image" >/dev/null 2>&1; then
  # Fails, and leaves the image, if a container of another project uses it.
  run docker image rm "$image" || echo "kept $image: it is still in use"
fi

[ "$dry_run" = 1 ] || echo "Docker cleaned for nvrview."
