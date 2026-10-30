#!/usr/bin/env bash
# herdr-fork: fork-only. Scans tracked files against the private denylist. Unofficial stopgap;
# see FORK.md.
#
# Usage: scripts/fork/denylist-scan.sh [repo] [allowlist]
#   repo       defaults to this checkout
#   allowlist  defaults to scripts/fork/denylist-allow.sha256 ("<sha256>  <path>" per line)
#
# Prints paths and counts only, never the matched text or the denylist. A path on the
# allowlist is skipped only while its content still hashes to the pinned sha256, so any edit
# to an exempted file is scanned again. Exit 0 = clean, 1 = hits, 2 = usage.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
repo=${1:-$root}
allow=${2:-$root/scripts/fork/denylist-allow.sha256}
denylist=${SCRUB_DENYLIST:-$HOME/.config/crewkit/denylist.txt}

if [[ ! -r $denylist ]]; then echo "denylist-scan: no readable denylist" >&2; exit 2; fi
if [[ ! -r $allow ]]; then echo "denylist-scan: no readable allowlist at $allow" >&2; exit 2; fi

tmp=$(mktemp -d)
trap 'rm -r "$tmp"' EXIT
grep -Ev '^[[:space:]]*(#|$)' "$denylist" >"$tmp/patterns" || true
if [[ ! -s $tmp/patterns ]]; then echo "denylist-scan: denylist has no patterns" >&2; exit 2; fi

declare -A pinned=()
while IFS= read -r line || [[ -n $line ]]; do
  [[ $line =~ ^[[:space:]]*(#|$) ]] && continue
  if [[ ! $line =~ ^([0-9a-f]{64})\ \ ([^[:space:]].*)$ ]]; then
    echo "denylist-scan: malformed allowlist line in $allow" >&2; exit 2
  fi
  pinned[${BASH_REMATCH[2]}]=${BASH_REMATCH[1]}
done <"$allow"

: >"$tmp/files"
while IFS= read -r -d '' path; do
  [[ -f $repo/$path ]] || continue
  if [[ -n ${pinned[$path]:-} ]]; then
    sum=$(shasum -a 256 -- "$repo/$path" | cut -c1-64)
    if [[ $sum == "${pinned[$path]}" ]]; then echo "exempt (pinned hash matches): $path"; continue; fi
    echo "scanned (pinned hash changed): $path"
  fi
  printf '%s\0' "$path" >>"$tmp/files"
done < <(git -C "$repo" ls-files -z)

# Both locales: a term next to a non-UTF-8 byte only matches under C.
hits=0
for locale in C en_US.UTF-8; do
  out=$(cd "$repo" && xargs -0 env LC_ALL=$locale grep -aEicH -f "$tmp/patterns" -- <"$tmp/files" 2>/dev/null | awk -F: '$NF > 0' || true)
  if [[ -n $out ]]; then
    echo "hits ($locale):"; printf '%s\n' "$out" | sed 's/^/  /'
    hits=1
  fi
done
exit $hits
