#!/bin/sh
# Regenerates crates/viewer-ffi/include/nvrview.h with cbindgen
# (cargo install cbindgen). With --check, fails if the committed header is
# out of date instead of writing it.
set -eu
cd "$(git rev-parse --show-toplevel)"
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi
PATH="$HOME/.cargo/bin:$PATH"

out=crates/viewer-ffi/include/nvrview.h
if ! command -v cbindgen >/dev/null 2>&1; then
  echo "gen-header: cbindgen not found (cargo install cbindgen --locked)" >&2
  exit 1
fi
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
(cd crates/viewer-ffi && cbindgen --quiet --config cbindgen.toml --output "$tmp" .)
if [ "${1:-}" = "--check" ]; then
  if ! cmp -s "$tmp" "$out"; then
    echo "gen-header: $out is out of date; run scripts/gen-header.sh" >&2
    diff -u "$out" "$tmp" >&2 || true
    exit 1
  fi
  echo "gen-header: $out is up to date"
else
  cp "$tmp" "$out"
  echo "$out"
fi
