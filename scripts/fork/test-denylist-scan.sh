#!/usr/bin/env bash
# herdr-fork: fork-only. Tests scripts/fork/denylist-scan.sh. Unofficial stopgap; see FORK.md.
#
# Builds a throwaway repo with a synthetic denylist term (never a real one) and checks that a
# hash-pinned allowlist entry exempts exactly that path at exactly that content.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
scan=$here/denylist-scan.sh
tmp=$(mktemp -d)
trap 'rm -r "$tmp"' EXIT
fail=0

repo=$tmp/repo
mkdir -p "$repo/vendor/lib"
git -C "$repo" init -q
printf 'header\ncredit: canaryterm-q7x\nfooter\n' >"$repo/vendor/lib/third.h"
printf 'clean text\n' >"$repo/ours.rs"
git -C "$repo" add -A
printf 'canaryterm-q7x\n' >"$tmp/denylist"
allow=$tmp/allow.sha256
(cd "$repo" && shasum -a 256 vendor/lib/third.h) >"$allow"

check() { # name expected-exit
  local name=$1 want=$2 got=0
  SCRUB_DENYLIST=$tmp/denylist "$scan" "$repo" "$allow" >"$tmp/out" 2>&1 || got=$?
  if [[ $got == "$want" ]] && ! grep -q canaryterm "$tmp/out"; then
    echo "  ok    $name"
  else
    echo "  FAIL  $name (exit $got, want $want)"; sed 's/^/        /' "$tmp/out"; fail=1
  fi
}

check "pinned file at its pinned hash is exempt" 0

printf 'header\ncredit: canaryterm-q7x\nfooteR\n' >"$repo/vendor/lib/third.h"
check "one changed byte makes the pinned file scanned again" 1
printf 'header\ncredit: canaryterm-q7x\nfooter\n' >"$repo/vendor/lib/third.h"

cp "$repo/vendor/lib/third.h" "$repo/copy.h"
git -C "$repo" add copy.h
check "the same content at another path is not exempt" 1
git -C "$repo" rm -q --cached copy.h
rm "$repo/copy.h"

printf 'canaryterm-q7x\n' >>"$repo/ours.rs"
check "a hit outside the allowlist still fails" 1
printf 'clean text\n' >"$repo/ours.rs"

printf 'not-a-hash  vendor/lib/third.h\n' >"$tmp/bad.sha256"
got=0; SCRUB_DENYLIST=$tmp/denylist "$scan" "$repo" "$tmp/bad.sha256" >/dev/null 2>&1 || got=$?
if [[ $got == 2 ]]; then echo "  ok    a malformed allowlist line is a usage error"; else echo "  FAIL  malformed allowlist (exit $got, want 2)"; fail=1; fi

got=0; SCRUB_DENYLIST=$tmp/missing "$scan" "$repo" "$allow" >/dev/null 2>&1 || got=$?
if [[ $got == 2 ]]; then echo "  ok    a missing denylist is a usage error"; else echo "  FAIL  missing denylist (exit $got, want 2)"; fail=1; fi

exit $fail
