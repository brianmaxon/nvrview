#!/bin/sh
# Builds the C smoke test (crates/viewer-ffi/tests/c/smoke.c) against the
# generated header and the static library, then runs it against a spawned
# mock-nvr with two days of recordings. With --asan, the C program is built
# with AddressSanitizer.
set -eu
cd "$(git rev-parse --show-toplevel)"
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

asan=""
if [ "${1:-}" = "--asan" ]; then
  asan="-fsanitize=address -fno-omit-frame-pointer"
fi

cargo build --quiet -p viewer-ffi -p mock-nvr
out=target/c-ffi
mkdir -p "$out"

case "$(uname)" in
  Darwin) libs="-liconv -lm" ;;
  *) libs="-lgcc_s -lutil -lrt -lpthread -lm -ldl -lc" ;;
esac
# shellcheck disable=SC2086 # word splitting of the flag lists is intended
${CC:-cc} -std=c11 -Wall -Wextra -Werror -g $asan \
  -I crates/viewer-ffi/include crates/viewer-ffi/tests/c/smoke.c \
  target/debug/libnvrview.a $libs -o "$out/smoke"

log="$out/mock-nvr.log"
MOCK_NVR_PASSWORD=example-password target/debug/mock-nvr --port 0 --channels 2 \
  --recordings 2 --ptz 0 --fixture fixtures/synthetic/h264-basic.h264 >"$log" 2>&1 &
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
NVRVIEW_TEST_PASSWORD=example-password "$out/smoke" "$host" "$port" "$work"
