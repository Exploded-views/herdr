#!/usr/bin/env bash
# herdr-fork: fork-only. Tests scripts/fork/denylist-scan.sh. Unofficial stopgap; see FORK.md.
#
# Builds a throwaway repo with a synthetic denylist term (never a real one) and checks that a
# hash-pinned allowlist entry exempts exactly that path at exactly those committed bytes, and
# that every failure mode is an error, never a clean result.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
scan=$here/denylist-scan.sh
tmp=$(mktemp -d)
trap 'rm -r "$tmp"' EXIT
fail=0
term=canaryterm-q7x
good=$'header\ncredit: canaryterm-q7x\nfooter\n'

repo=$tmp/repo
mkdir -p "$repo/vendor/lib"
git -C "$repo" init -q
git -C "$repo" config user.email t@example.com
git -C "$repo" config user.name t
commit() { git -C "$repo" add -A && git -C "$repo" commit -qm x --allow-empty; }
printf '%s' "$good" >"$repo/vendor/lib/third.h"
printf 'clean text\n' >"$repo/ours.rs"
commit
base=$(git -C "$repo" rev-parse HEAD)
reset() { git -C "$repo" reset -q --hard "$base"; git -C "$repo" clean -qfd; }
printf '%s\n' "$term" >"$tmp/denylist"
allow=$tmp/allow.sha256
printf '%s  vendor/lib/third.h\n' "$(printf '%s' "$good" | shasum -a 256 | cut -c1-64)" >"$allow"

check() { # name expected-exit [denylist] [repo] [allowlist]
  local name=$1 want=$2 dl=${3:-$tmp/denylist} r=${4:-$repo} al=${5:-$allow} got=0
  SCRUB_DENYLIST=$dl "$scan" "$r" "$al" >"$tmp/out" 2>&1 || got=$?
  if [[ $got == "$want" ]] && ! grep -q "$term" "$tmp/out"; then
    echo "  ok    $name"
  else
    echo "  FAIL  $name (exit $got, want $want)"; sed 's/^/        /' "$tmp/out"; fail=1
  fi
}

check "pinned file at its pinned hash is exempt" 0

printf 'header\ncredit: canaryterm-q7x\nfooteR\n' >"$repo/vendor/lib/third.h"; commit
check "one changed committed byte makes the pinned file scanned again" 1
reset

printf 'extra %s\n' "$term" >"$repo/vendor/lib/third.h"; commit
printf '%s' "$good" >"$repo/vendor/lib/third.h"
check "committed bytes are scanned, not a restored working tree" 1
reset

printf 'x %s\n' "$term" >"$repo/ours.rs"; git -C "$repo" add ours.rs; printf 'clean text\n' >"$repo/ours.rs"
check "an uncommitted index change is not what is published (HEAD is scanned)" 0
reset

rm "$repo/ours.rs"
printf 'x %s\n' "$term" >>"$repo/vendor/lib/other.h"; commit; rm "$repo/vendor/lib/other.h"
check "a committed file deleted from the working tree is still scanned" 1
reset

cp "$repo/vendor/lib/third.h" "$repo/copy.h"; commit
check "the same content at another path is not exempt" 1
reset

mv "$repo/vendor/lib/third.h" "$repo/real.h"; ln -s ../../real.h "$repo/vendor/lib/third.h"; commit
check "a symlink at the pinned path is not exempt" 1
reset

printf 'see %s\n' "$term" >"$repo/link-target.txt"; rm "$repo/link-target.txt"
ln -s "$term" "$repo/link"; commit
check "a symlink target is scanned" 1
reset

printf 'clean\n' >"$repo/$term.txt"; commit
check "a path name is scanned" 1
reset

printf 'x %s\n' "$term" >>"$repo/ours.rs"; commit
check "a hit outside the allowlist still fails" 1
reset

printf '%s\n(\n' "$term" >"$tmp/badre"
check "an invalid denylist pattern is an error, not clean" 2 "$tmp/badre"

check "a missing denylist is an error" 2 "$tmp/missing"

mkdir "$tmp/notrepo"
check "a directory that is not a git repo is an error" 2 "$tmp/denylist" "$tmp/notrepo"

printf 'not-a-hash  vendor/lib/third.h\n' >"$tmp/bad.sha256"
check "a malformed allowlist line is an error" 2 "$tmp/denylist" "$repo" "$tmp/bad.sha256"

exit $fail
