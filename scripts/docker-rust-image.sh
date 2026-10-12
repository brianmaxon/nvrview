#!/bin/sh
# Sourced by scripts/linux-check.sh and scripts/csharp-check.sh. Sets
# `rust_image` to the Rust build image with sccache installed, building it
# locally on first use (a few seconds; later runs find it), and `rust_env` to
# the options every Rust container needs.
#
# sccache is a pinned release whose SHA-256 is checked during the image build.
# To update: change the version and both checksums (published beside each
# release asset as <asset>.sha256).
#
# Shared caches (the only named volumes the checks use):
#   nvrview-linux-cargo   cargo registry; cargo locks it, so concurrent
#                         containers are safe (one may wait for another's lock)
#   nvrview-sccache       sccache's local disk cache; entries are written
#                         atomically and keyed by content, so concurrent
#                         containers are safe; capped by SCCACHE_CACHE_SIZE
#   nvrview-csharp-nuget  NuGet packages; content-addressed, written by rename
#                         (a cold run of two containers may download twice)
# Build output is never in a named volume: it goes to an anonymous volume that
# `docker run --rm` deletes with the container.

sccache_version=0.18.0
sccache_sha_x86_64=45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89
sccache_sha_aarch64=2b3284d5da3b46a47dc4229e75bb7b88ac4aa99c8d754fb7d2f84997e5a4354a

base_image="${NVRVIEW_LINUX_IMAGE:-rust:1-bookworm}"
rust_image="nvrview-rust-sccache:$sccache_version-$(printf '%s' "$base_image" | tr '/:' '__')"
if ! docker image inspect "$rust_image" >/dev/null 2>&1; then
  echo "Building $rust_image (sccache $sccache_version on $base_image)..." >&2
  docker build --quiet -t "$rust_image" \
    --build-arg "BASE=$base_image" \
    --build-arg "VERSION=$sccache_version" \
    --build-arg "SHA_X86_64=$sccache_sha_x86_64" \
    --build-arg "SHA_AARCH64=$sccache_sha_aarch64" - >/dev/null <<'DOCKERFILE'
ARG BASE
FROM ${BASE}
ARG VERSION
ARG SHA_X86_64
ARG SHA_AARCH64
RUN set -eu; \
    arch=$(uname -m); \
    case "$arch" in x86_64) sha=$SHA_X86_64 ;; aarch64) sha=$SHA_AARCH64 ;; *) echo "unsupported $arch" >&2; exit 1 ;; esac; \
    name="sccache-v${VERSION}-${arch}-unknown-linux-musl"; \
    curl -fsSL -o /tmp/s.tgz "https://github.com/mozilla/sccache/releases/download/v${VERSION}/${name}.tar.gz"; \
    echo "${sha}  /tmp/s.tgz" | sha256sum -c -; \
    tar -xzf /tmp/s.tgz -C /tmp; \
    install -m 0755 "/tmp/${name}/sccache" /usr/local/bin/sccache; \
    rm -rf /tmp/s.tgz "/tmp/${name}"; \
    sccache --version
DOCKERFILE
fi

# Environment for every Rust container: sccache wraps rustc; incremental
# compilation is off because sccache cannot cache it.
rust_env="-e RUSTC_WRAPPER=sccache -e SCCACHE_DIR=/sccache -e SCCACHE_CACHE_SIZE=${NVRVIEW_SCCACHE_SIZE:-10G} -e CARGO_INCREMENTAL=0"
