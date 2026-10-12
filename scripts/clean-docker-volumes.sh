#!/bin/sh
# Removes leftover per-checkout volumes from older versions of the Docker
# checks: nvrview-linux-target-<hash> and nvrview-csharp-build-<hash>, where
# <hash> is the first 8 hex digits of the SHA-1 of the checkout's absolute
# path. A volume is removed only when no existing checkout of this repository
# (the main one or any worktree, per `git worktree list`) hashes to it.
#
# Touches nothing else: not the shared caches (nvrview-linux-cargo,
# nvrview-sccache, nvrview-csharp-nuget), not other projects' volumes, images,
# or containers, and it never prunes. A volume still attached to a container
# is skipped. See scripts/clean-docker.sh to remove the shared caches.
#
#   scripts/clean-docker-volumes.sh            remove orphaned volumes
#   scripts/clean-docker-volumes.sh --dry-run  show what would be removed
set -eu
cd "$(git rev-parse --show-toplevel)"

dry_run=0
case "${1:-}" in
  "") ;;
  --dry-run) dry_run=1 ;;
  *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
esac

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  echo "Docker is not running; nothing to clean."
  exit 0
fi

# Hashes of every checkout that still exists.
live=" "
for path in $(git worktree list --porcelain | sed -n 's/^worktree //p' | tr ' ' '?'); do
  path=$(printf '%s' "$path" | tr '?' ' ')
  [ -d "$path" ] || continue
  live="$live$(printf '%s' "$path" | shasum | cut -c1-8) "
done

removed=0
kept=0
for v in $(docker volume ls -q --filter name='^nvrview-linux-target-' --filter name='^nvrview-csharp-build-'); do
  hash=${v##*-}
  case "$live" in *" $hash "*) kept=$((kept + 1)); continue ;; esac
  if [ -n "$(docker ps -aq --filter volume="$v")" ]; then
    echo "skipped $v: a container still uses it"
    continue
  fi
  if [ "$dry_run" = 1 ]; then
    echo "would remove $v"
  else
    docker volume rm "$v" >/dev/null && echo "removed $v"
  fi
  removed=$((removed + 1))
done
echo "orphaned per-checkout volumes: $removed ($kept belong to existing checkouts)"
