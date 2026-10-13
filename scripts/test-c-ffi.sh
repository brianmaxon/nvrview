#!/bin/sh
# Builds the C smoke test (crates/viewer-ffi/tests/c/smoke.c) against the
# generated header and the static library, then runs it against a spawned
# mock-nvr with two days of recordings. With --asan, the C program is built
# with AddressSanitizer.
#
#   scripts/test-c-ffi.sh [--asan]     build, stage, and run
#   scripts/test-c-ffi.sh --prepare    only build and stage (scripts/check.sh
#                                      does this once, then runs the plain and
#                                      the --asan smoke test side by side with
#                                      NVRVIEW_C_FFI_PREPARED=1)
#
# The library and mock-nvr are copied to target/c-ffi/stage first, so a cargo
# build running at the same time (the workspace tests) cannot replace them
# under a running smoke test. The mock streams live video faster than real
# time (--live-speed, picture times unchanged; NVRVIEW_C_SMOKE_LIVE_SPEED=1
# for real time): the assertions are the same, the waits on media time are
# shorter. The core runs with NVRVIEW_MOCK_SPEEDUP=16 (docs/FFI.md,
# "Self-tests"): a play group starts without a real device's pause.
# NVRVIEW_SMOKE_TIMING=1 prints how long each part of the test took.
set -eu
cd "$(git rev-parse --show-toplevel)"
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

asan=""
out=target/c-ffi
prepare_only=0
case "${1:-}" in
  --asan)
    asan="-fsanitize=address -fno-omit-frame-pointer"
    out=target/c-ffi-asan
    ;;
  --prepare) prepare_only=1 ;;
esac
stage=target/c-ffi/stage

if [ "${NVRVIEW_C_FFI_PREPARED:-0}" != 1 ] || [ "$prepare_only" = 1 ]; then
  cargo build --quiet -p viewer-ffi -p mock-nvr
  mkdir -p "$stage"
  cp target/debug/libnvrview.a target/debug/mock-nvr "$stage/"
  if [ "$prepare_only" = 1 ]; then exit 0; fi
fi
mkdir -p "$out"

case "$(uname)" in
  Darwin) libs="-liconv -lm" ;;
  *) libs="-lgcc_s -lutil -lrt -lpthread -lm -ldl -lc" ;;
esac
# shellcheck disable=SC2086 # word splitting of the flag lists is intended
${CC:-cc} -std=c11 -Wall -Wextra -Werror -g $asan \
  -I crates/viewer-ffi/include crates/viewer-ffi/tests/c/smoke.c \
  "$stage/libnvrview.a" $libs -o "$out/smoke"

log="$out/mock-nvr.log"
MOCK_NVR_PASSWORD=example-password "$stage/mock-nvr" --port 0 --channels 2 \
  --recordings 2 --ptz 0 --live-speed "${NVRVIEW_C_SMOKE_LIVE_SPEED:-8}" \
  --fixture fixtures/synthetic/h264-basic.h264 >"$log" 2>&1 &
mock=$!
trap 'kill "$mock" 2>/dev/null || true' EXIT INT TERM

addr=""
for _ in $(seq 1 100); do
  addr=$(sed -n 's/^mock-nvr listening on //p' "$log")
  [ -n "$addr" ] && break
  sleep 0.05
done
if [ -z "$addr" ]; then
  echo "test-c-ffi: mock-nvr did not start" >&2
  cat "$log" >&2
  exit 1
fi
# A fresh directory for the core's data and the downloads.
work="$PWD/$out/work"
rm -rf "$work"
mkdir -p "$work"
host=${addr%:*}
port=${addr##*:}
NVRVIEW_MOCK_SPEEDUP="${NVRVIEW_MOCK_SPEEDUP:-16}" NVRVIEW_TEST_PASSWORD=example-password "$out/smoke" "$host" "$port" "$work"
