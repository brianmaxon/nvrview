#!/bin/sh
# Tests scripts/docker-slot.sh in a throwaway repository: slots are limited,
# a slot with a dead owner is broken, a live owner's is kept, and the "no
# space left on device" message appears. No Docker is needed (a fake `docker`
# on PATH).
set -u
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fails=$((fails + 1)); }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', wanted '$3')"; fi; }

cd "$tmp" && git init -q . || exit 1
mkdir bin
cat >bin/docker <<'E'
#!/bin/sh
echo "write /run/x: no space left on device" >&2
exit 125
E
chmod +x bin/docker

export NVRVIEW_DOCKER_SLOTS=2
# shellcheck disable=SC1091
. "$here/docker-slot.sh"

docker_slot_acquire
first=$ds_held
check "the first acquire takes slot 1" "$(basename "$first")" 1
# A second process takes slot 2 while this one holds slot 1.
sh -c ". '$here/docker-slot.sh'; docker_slot_acquire; basename \"\$ds_held\"; sleep 3" >second.out 2>&1 &
second=$!
i=0
while [ ! -s second.out ] && [ $i -lt 100 ]; do i=$((i + 1)); sleep 0.1; done
check "the second acquire takes slot 2" "$(head -n 1 second.out)" 2

# A third waits: with both slots held it prints the waiting note and does not
# get one until the second process exits.
sh -c ". '$here/docker-slot.sh'; docker_slot_acquire; echo got" >third.out 2>&1 &
third=$!
i=0
while ! grep -q waiting third.out 2>/dev/null && [ $i -lt 100 ]; do i=$((i + 1)); sleep 0.1; done
check "a third acquire waits" "$(grep -c waiting third.out)" 1
check "...and has not got a slot" "$(grep -c '^got' third.out)" 0
kill "$second" 2>/dev/null
wait "$second" 2>/dev/null
# The second shell's sleep child may outlive it, but its owner (the shell) is
# gone, so the slot is stale and the third takes it.
wait "$third"
check "...and gets the slot of the process that ended" "$(grep -c '^got' third.out)" 1

docker_slot_release
check "release frees the slot" "$([ -d "$first" ] && echo held || echo free)" free

# A dead owner's slot is broken; a live owner's is kept.
mkdir -p "$(dirname "$first")/1"
echo 999999 >"$(dirname "$first")/1/owner"
mkdir -p "$(dirname "$first")/2"
echo $$ >"$(dirname "$first")/2/owner"
docker_slot_acquire
check "a slot with a dead owner is broken and taken" "$(basename "$ds_held")" 1
check "a live owner's slot is kept" "$(cat "$(dirname "$first")/2/owner")" "$$"
docker_slot_release

# Docker's "no space left on device" gets the restart message.
PATH="$tmp/bin:$PATH" docker_run --rm x >run.out 2>&1
rc=$?
check "docker_run keeps the exit status" "$rc" 125
check "docker_run says Docker Desktop needs a restart" "$(grep -c 'Restart Docker Desktop' run.out)" 1

if [ "$fails" != 0 ]; then
  echo "$fails docker-slot test(s) failed" >&2
  exit 1
fi
echo "all docker-slot tests passed"
