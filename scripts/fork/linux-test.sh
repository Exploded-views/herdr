#!/usr/bin/env bash
# herdr-fork: fork-only. Builds and tests herdr on Linux in the image that fork-ci.yml
# also uses (.github/workflows/fork-ci.Dockerfile). Unofficial stopgap; see FORK.md.
#
# Usage:
#   scripts/fork/linux-test.sh --fetch          # network on: build the image, create the cache
#                                               # volumes, fetch cargo crates and Zig packages
#   scripts/fork/linux-test.sh [cmd ...]        # network off: run cmd (default: just ci) in /src
#
# The repo is mounted read-only. Cargo's home, the target dir and both Zig caches live in
# named volumes, so nothing is written into the checkout. Runs as the caller's uid.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
dockerfile="$root/.github/workflows/fork-ci.Dockerfile"
image="herdr-fork-linux:$(git -C "$root" hash-object "$dockerfile" | cut -c1-12)"
vols=(-v herdr-fork-cargo:/cargo -v herdr-fork-target:/target -v herdr-fork-zig:/zig)
envs=(-e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target
      -e ZIG_GLOBAL_CACHE_DIR=/zig/global -e ZIG_LOCAL_CACHE_DIR=/zig/local
      -e CARGO_PROFILE_DEV_DEBUG=1 -e CARGO_PROFILE_TEST_DEBUG=1)
user=(--user "$(id -u):$(id -g)")
run() { docker run --rm "${user[@]}" "${vols[@]}" "${envs[@]}" -v "$root:/src:ro" -w /src "$@"; }

if [[ ${1:-} == --fetch ]]; then
  docker build -t "$image" -f "$dockerfile" "$root/.github/workflows"
  # Fresh named volumes are root-owned; hand them to the caller's uid once.
  docker run --rm "${vols[@]}" "$image" chown -R "$(id -u):$(id -g)" /cargo /target /zig
  run "$image" cargo fetch --locked
  # A networked build warms the Zig package cache, which cargo fetch does not touch.
  run "$image" cargo build --locked --all-targets
  exit 0
fi
docker image inspect "$image" >/dev/null 2>&1 || { echo "image $image missing; run --fetch first" >&2; exit 1; }
[[ $# -gt 0 ]] || set -- just ci
run --network none -e CARGO_NET_OFFLINE=true "$image" "$@"
