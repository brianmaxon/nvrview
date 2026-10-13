# shellcheck shell=sh
# Sourced by scripts/csharp-check.sh and scripts/linux-check.sh: the Docker
# safeguards shared by every checkout and worktree on this machine.
#
#   docker_slot_acquire   waits (in this one command) until one of the
#                         NVRVIEW_DOCKER_SLOTS (default 2) host-wide slots is
#                         free and takes it. Heavy Docker steps from many
#                         worktrees at once filled the Docker VM's /run and
#                         disk; two at a time do not.
#   docker_slot_release   gives it back (call it from the caller's exit trap)
#   docker_run ARGS...    `docker run` with its output kept, printed, and a
#                         clear message when Docker reports "no space left on
#                         device" (Docker Desktop needs a restart)
#
# A slot is a directory in <git common dir>/docker-slots holding an owner file
# (pid). A slot whose owner process is gone (killed, crashed) is broken at the
# next acquire, so a killed run never holds a slot; a slot directory still
# without an owner file after a minute is broken too.

ds_held=""

docker_slot_acquire() {
  ds_root=$(cd "$(git rev-parse --git-common-dir)" && pwd)/docker-slots
  mkdir -p "$ds_root"
  ds_n="${NVRVIEW_DOCKER_SLOTS:-2}"
  ds_noted=0
  while :; do
    ds_i=1
    while [ "$ds_i" -le "$ds_n" ]; do
      ds_s="$ds_root/$ds_i"
      if mkdir "$ds_s" 2>/dev/null; then
        echo "$$" >"$ds_s/owner"
        ds_held="$ds_s"
        return 0
      fi
      ds_pid=$(cat "$ds_s/owner" 2>/dev/null || true)
      ds_stale=0
      if [ -n "$ds_pid" ]; then
        kill -0 "$ds_pid" 2>/dev/null || ds_stale=1
      elif [ -d "$ds_s" ] && [ -n "$(find "$ds_s" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
        ds_stale=1
      fi
      if [ "$ds_stale" = 1 ] && mv "$ds_s" "$ds_s.stale.$$" 2>/dev/null; then
        # Another waiter may have replaced it between the check and the move:
        # put a live owner's slot back.
        ds_pid2=$(cat "$ds_s.stale.$$/owner" 2>/dev/null || true)
        if [ -n "$ds_pid2" ] && kill -0 "$ds_pid2" 2>/dev/null; then
          mv "$ds_s.stale.$$" "$ds_s" 2>/dev/null || rm -rf "$ds_s.stale.$$"
        else
          rm -rf "$ds_s.stale.$$"
          echo "docker-slot: broke slot $ds_i (its owner process is gone)" >&2
        fi
        continue 2
      fi
      ds_i=$((ds_i + 1))
    done
    if [ "$ds_noted" = 0 ]; then
      echo "docker-slot: waiting for a free Docker slot ($ds_n at a time on this machine)" >&2
      ds_noted=1
    fi
    sleep 2 # a lock poll, not a test wait
  done
}

docker_slot_release() {
  if [ -n "$ds_held" ]; then
    rm -rf "$ds_held"
    ds_held=""
  fi
}

docker_run() {
  dr_log=$(mktemp)
  dr_rc=0
  docker run "$@" >"$dr_log" 2>&1 || dr_rc=$?
  cat "$dr_log"
  if [ "$dr_rc" -ne 0 ] && grep -qi 'no space left on device' "$dr_log"; then
    {
      echo "docker: \"no space left on device\" from Docker itself, not from this machine's disk."
      echo "Docker Desktop's VM is full (its small /run fills after many concurrent or killed runs)."
      echo "Restart Docker Desktop (menu bar icon, Restart), then run the check again."
      echo "scripts/clean-docker.sh also frees the shared caches."
    } >&2
  fi
  rm -f "$dr_log"
  return "$dr_rc"
}
