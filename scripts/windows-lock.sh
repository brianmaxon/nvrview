#!/bin/sh
# The lock for the shared Windows test computer(s), with priorities.
#
#   scripts/windows-lock.sh acquire [--priority merge|agent] [--owner-pid PID]
#       Waits (in this one command) until a host is free, takes its lock, and
#       prints the host alias on standard output. Messages go to standard
#       error. The default priority is $NVRVIEW_WINDOWS_PRIORITY, else agent.
#   scripts/windows-lock.sh release [--owner-pid PID]
#       Gives back the lock(s) recorded for that owner. It never touches a lock
#       another owner holds.
#   scripts/windows-lock.sh status
#       Prints each host's lock owner and the waiting queue.
#
# Merges (merge-branch, merge-train, release) use priority "merge"; agents'
# scripts/windows-check.sh uses "agent". Priority and order apply per host:
# an agent does not take a free host that a waiting merge could use, so a
# merge gets the next free computer it can run on, and waiters of the same
# priority go first come, first served for the hosts they share. A waiter
# pinned to other hosts (its own NVRVIEW_WINDOWS_HOSTS) never holds a check
# off a host it cannot use. A running check is never interrupted.
#
# OWNER-PID is a long-lived process of the caller (default: the parent shell);
# the lock belongs to it. A lock is a directory holding an owner file (pid,
# machine, start time, priority, host). When the owner process is gone for
# NVRVIEW_LOCK_STALE_SECONDS (default 120) the lock is broken, with a log line.
# A lock directory without an owner file (made by hand) is never broken.
#
# Hosts: NVRVIEW_WINDOWS_HOSTS="host-a host-b" (ssh aliases), else
# NVRVIEW_WINDOWS_HOST (one host). One lock per host, named after its alias,
# not its place in the list: "<NVRVIEW_WINDOWS_LOCK>-<alias>" (the base
# defaults to /private/tmp/claude-<uid>/nvrview-windows.lock; the base itself
# only when no host is set). So a computer listed alone, first, or second
# always shares one lock with every other check that uses it, and two
# computers never share one. A check takes the first free host. Waiting
# is polled every NVRVIEW_LOCK_POLL seconds (default 10).
set -eu

base="${NVRVIEW_WINDOWS_LOCK:-/private/tmp/claude-$(id -u)/nvrview-windows.lock}"
hosts="${NVRVIEW_WINDOWS_HOSTS:-${NVRVIEW_WINDOWS_HOST:-}}"
poll="${NVRVIEW_LOCK_POLL:-10}"
stale="${NVRVIEW_LOCK_STALE_SECONDS:-120}"
waiters="$base.waiters"
me=$(hostname)

log() { echo "windows-lock: $*" >&2; }

# One "lock path|host" line per lock (a single empty host when none is set).
lock_list() {
  for h in $hosts; do echo "$base-$h|$h"; done
  [ -n "$hosts" ] || echo "$base|"
}

field() { # FILE KEY
  sed -n "s/^$2=//p" "$1" 2>/dev/null | head -n 1
}

alive() { kill -0 "$1" 2>/dev/null; }

# Breaks the lock when its owner is gone for $stale seconds.
check_stale() { # LOCK
  l="$1"
  [ -f "$l/owner" ] || return 0
  opid=$(field "$l/owner" pid)
  ohost=$(field "$l/owner" machine)
  { [ "$ohost" = "$me" ] && [ -n "$opid" ]; } || return 0
  if alive "$opid"; then
    rm -f "$l/dead-since"
    return 0
  fi
  now=$(date +%s)
  if [ ! -f "$l/dead-since" ]; then
    echo "$now" >"$l/dead-since" 2>/dev/null || true
    return 0
  fi
  since=$(cat "$l/dead-since" 2>/dev/null || echo "$now")
  if [ $((now - since)) -ge "$stale" ]; then
    log "breaking the stale lock $l (owner pid $opid gone for $((now - since)) s)"
    moved="$l.stale.$$"
    if mv "$l" "$moved" 2>/dev/null; then
      if [ "$(field "$moved/owner" pid)" != "$opid" ] && [ ! -e "$l" ]; then
        mv "$moved" "$l" # it was replaced by a new owner meanwhile
      else
        rm -rf "$moved"
      fi
    fi
  fi
}

try_take() { # LOCK HOST PRIORITY OWNERPID
  mkdir -p "$(dirname "$1")"
  if mkdir "$1" 2>/dev/null; then
    {
      echo "pid=$4"
      echo "machine=$me"
      echo "start=$(date +%s)"
      echo "priority=$3"
      echo "host=$2"
    } >"$1/owner.tmp"
    mv "$1/owner.tmp" "$1/owner"
    return 0
  fi
  check_stale "$1"
  return 1
}

prune_waiters() {
  [ -d "$waiters" ] || return 0
  for f in "$waiters"/*; do
    [ -f "$f" ] && [ "${f##*/}" != "${wname:-}" ] || continue
    wpid=$(field "$f" pid)
    wmachine=$(field "$f" machine)
    if [ "$wmachine" = "$me" ] && [ -n "$wpid" ] && ! alive "$wpid"; then rm -f "$f"; fi
  done
}

# Live waiters of a priority, oldest first.
queue() { # PRIORITY
  [ -d "$waiters" ] || return 0
  ls "$waiters" 2>/dev/null | grep "^$1-" | LC_ALL=C sort || true
}

# Whether a waiter can use HOST (its hosts= line; none: any host, as waiters
# written by older copies of this script).
wants() { # WAITER-FILE HOST
  wh=$(field "$1" hosts)
  [ -z "$wh" ] && return 0
  case " $wh " in *" $2 "*) return 0 ;; esac
  return 1
}

# Whether a waiter ahead of us wants HOST: any merge waiter when we are an
# agent, and the older waiters of our own priority. Priority and order apply
# per host, so a merge pinned to one computer never holds agents off another.
blocked() { # HOST
  {
    if [ "$priority" = agent ]; then queue merge; fi
    queue "$priority" | sed "/^$wname\$/,\$d"
  } | while read -r w; do
    if [ -f "$waiters/$w" ] && wants "$waiters/$w" "$1"; then echo yes; break; fi
  done | grep -q yes
}

owner_pid="$PPID"
priority="${NVRVIEW_WINDOWS_PRIORITY:-agent}"
cmd="${1:-}"
[ $# -eq 0 ] || shift
while [ $# -gt 0 ]; do
  case "$1" in
    --priority) priority="${2:?--priority needs merge or agent}"; shift 2 ;;
    --owner-pid) owner_pid="${2:?--owner-pid needs a pid}"; shift 2 ;;
    *) echo "usage: $0 acquire|release|status [--priority merge|agent] [--owner-pid PID]" >&2; exit 2 ;;
  esac
done
case "$priority" in merge | agent) ;; *) echo "priority: merge or agent" >&2; exit 2 ;; esac

case "$cmd" in
  acquire)
    mkdir -p "$waiters"
    wname="$priority-$(date +%s)-$$"
    # shellcheck disable=SC2086 # the host list, one line
    printf 'pid=%s\nmachine=%s\nhosts=%s\n' "$owner_pid" "$me" "$(echo $hosts)" >"$waiters/$wname"
    trap 'rm -f "$waiters/$wname"' EXIT
    trap 'exit 1' INT TERM HUP
    n=0
    while :; do
      prune_waiters
      for entry in $(lock_list); do
        if blocked "${entry#*|}"; then continue; fi
        if try_take "${entry%%|*}" "${entry#*|}" "$priority" "$owner_pid"; then
          printf '%s\n' "${entry#*|}"
          exit 0
        fi
      done
      if [ $((n % 6)) = 0 ]; then
        log "waiting for the Windows computer (priority $priority; $(queue merge | wc -l | tr -d ' ') merge and $(queue agent | wc -l | tr -d ' ') agent waiters)"
      fi
      n=$((n + 1))
      sleep "$poll"
    done
    ;;
  release)
    for entry in $(lock_list); do
      l="${entry%%|*}"
      if [ -f "$l/owner" ] && [ "$(field "$l/owner" pid)" = "$owner_pid" ] && [ "$(field "$l/owner" machine)" = "$me" ]; then
        rm -rf "$l"
      fi
    done
    ;;
  status)
    for entry in $(lock_list); do
      l="${entry%%|*}"
      if [ -f "$l/owner" ]; then
        echo "${entry#*|}: held by pid $(field "$l/owner" pid) on $(field "$l/owner" machine), priority $(field "$l/owner" priority), since $(field "$l/owner" start)"
      elif [ -d "$l" ]; then
        echo "${entry#*|}: held (no owner file)"
      else
        echo "${entry#*|}: free"
      fi
    done
    prune_waiters
    echo "waiting: $(queue merge | wc -l | tr -d ' ') merge, $(queue agent | wc -l | tr -d ' ') agent"
    ;;
  *) echo "usage: $0 acquire|release|status [--priority merge|agent] [--owner-pid PID]" >&2; exit 2 ;;
esac
