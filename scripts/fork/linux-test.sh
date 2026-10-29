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
# Zig 0.16 unpacks dependencies into <project>/zig-pkg, inside the read-only checkout, so that
# directory (gitignored upstream) gets its own volume. Docker can't create a mountpoint inside a
# read-only bind, so the empty directory is made on the host first.
zigpkg=vendor/libghostty-vt/zig-pkg
mkdir -p "$root/$zigpkg"
vols=(-v herdr-fork-cargo:/cargo -v herdr-fork-target:/target -v herdr-fork-zig:/zig
      -v "herdr-fork-zigpkg:/src/$zigpkg")
envs=(-e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target
      -e ZIG_GLOBAL_CACHE_DIR=/zig/global -e ZIG_LOCAL_CACHE_DIR=/zig/local
      -e CARGO_PROFILE_DEV_DEBUG=1 -e CARGO_PROFILE_TEST_DEBUG=1
      # git refuses a checkout owned by another uid; the bind mount's owner can differ from --user.
      -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0=/src)
user=(--user "$(id -u):$(id -g)")
# ssh refuses to run for a uid with no passwd entry, so the caller's uid gets one (home /tmp).
ids=$(mktemp -d)
printf 'root:x:0:0:root:/root:/bin/bash\ndev:x:%s:%s:dev:/tmp:/bin/sh\n' "$(id -u)" "$(id -g)" >"$ids/passwd"
printf 'root:x:0:\ndev:x:%s:\n' "$(id -g)" >"$ids/group"
user+=(-v "$ids/passwd:/etc/passwd:ro" -v "$ids/group:/etc/group:ro" -e HOME=/tmp)
# --init forwards signals to the build; the trap stops the container when this script is killed
# (for example by the heavy lock's timeout), since killing the docker CLI alone leaves it running.
name="herdr-fork-$$"
trap 'docker kill "$name" >/dev/null 2>&1 || true; rm -r "$ids"' EXIT INT TERM
run() {
  docker run --rm --init --name "$name" --cap-drop ALL --security-opt no-new-privileges \
    "${user[@]}" "${vols[@]}" "${envs[@]}" -v "$root:/src:ro" -w /src "$@"
}

if [[ ${1:-} == --fetch ]]; then
  docker build -t "$image" -f "$dockerfile" "$root/.github/workflows"
  # Fresh named volumes are root-owned; hand them to the caller's uid once.
  docker run --rm --user 0 "${vols[@]}" -v "$root:/src:ro" "$image" \
    chown -R "$(id -u):$(id -g)" /cargo /target /zig "/src/$zigpkg"
  run "$image" cargo fetch --locked
  # A networked build warms the Zig package cache, which cargo fetch does not touch.
  run "$image" cargo build --locked --all-targets
  exit 0
fi
docker image inspect "$image" >/dev/null 2>&1 || { echo "image $image missing; run --fetch first" >&2; exit 1; }
[[ $# -gt 0 ]] || set -- just ci
run --network none -e CARGO_NET_OFFLINE=true "$image" "$@"
