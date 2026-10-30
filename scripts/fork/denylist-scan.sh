#!/usr/bin/env bash
# herdr-fork: fork-only. Scans a commit's tree against the private denylist. Unofficial stopgap;
# see FORK.md.
#
# Usage: scripts/fork/denylist-scan.sh [repo] [allowlist] [ref]
#   repo       defaults to this checkout
#   allowlist  defaults to scripts/fork/denylist-allow.sha256 ("<sha256>  <path>" per line)
#   ref        defaults to HEAD
#
# Scans the bytes git would publish (the blobs in <ref>'s tree, not the working tree): every
# file's content, every symlink's target and every path name. Prints paths and counts only,
# never the matched text or the denylist. A pinned path is skipped only when it is a regular
# file whose blob hashes to its pin, so any edit to an exempted file is scanned again.
# Exit 0 = clean, 1 = hits, 2 = usage or scan error (an error is never reported as clean).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
repo=${1:-$root}
allow=${2:-$root/scripts/fork/denylist-allow.sha256}
ref=${3:-HEAD}
denylist=${SCRUB_DENYLIST:-$HOME/.config/crewkit/denylist.txt}

die() { echo "denylist-scan: $*" >&2; exit 2; }
[[ -r $denylist ]] || die "no readable denylist"
[[ -r $allow ]] || die "no readable allowlist at $allow"
git -C "$repo" rev-parse --verify --quiet "$ref^{tree}" >/dev/null || die "$repo has no tree at $ref"

tmp=$(mktemp -d)
trap 'rm -r "$tmp"' EXIT
grep -Ev '^[[:space:]]*(#|$)' "$denylist" >"$tmp/patterns" || true
[[ -s $tmp/patterns ]] || die "denylist has no patterns"
# grep exits 2 on an invalid pattern; one bad line must not turn the whole scan clean.
rc=0; grep -Ec -f "$tmp/patterns" /dev/null >/dev/null 2>&1 || rc=$?
[[ $rc == 1 ]] || die "denylist has an invalid pattern"

locales=(C)
utf8=$(locale -a 2>/dev/null | grep -Eix 'en_US\.utf-?8|C\.utf-?8' | head -1 || true)
[[ -n $utf8 ]] || die "no UTF-8 locale available"
locales+=("$utf8")

declare -A pinned=()
while IFS= read -r line || [[ -n $line ]]; do
  [[ $line =~ ^[[:space:]]*(#|$) ]] && continue
  [[ $line =~ ^([0-9a-f]{64})\ \ ([^[:space:]].*)$ ]] || die "malformed allowlist line in $allow"
  pinned[${BASH_REMATCH[2]}]=${BASH_REMATCH[1]}
done <"$allow"

# Write each blob to blobs/<n>, and record n -> path. Path names go to names.txt.
mkdir "$tmp/blobs"
n=0
: >"$tmp/names.txt"
: >"$tmp/index"
while IFS= read -r -d '' entry; do
  meta=${entry%%$'\t'*}; path=${entry#*$'\t'}
  read -r mode type sha <<<"$meta"
  printf '%s\n' "$path" >>"$tmp/names.txt"
  [[ $type == blob ]] || continue
  git -C "$repo" cat-file blob "$sha" >"$tmp/blobs/$n" || die "cannot read blob for $path"
  if [[ -n ${pinned[$path]:-} ]]; then
    sum=$(shasum -a 256 <"$tmp/blobs/$n" | cut -c1-64)
    if [[ $mode == 100644 || $mode == 100755 ]] && [[ $sum == "${pinned[$path]}" ]]; then
      echo "exempt (pinned hash matches): $path"; rm "$tmp/blobs/$n"; n=$((n + 1)); continue
    fi
    echo "scanned (pinned hash or mode changed): $path"
  fi
  printf '%s\t%s\n' "$n" "$path" >>"$tmp/index"
  n=$((n + 1))
done < <(git -C "$repo" ls-tree -r -z --full-tree "$ref")
(( n > 0 )) || die "no files at $ref"

hits=0
for locale in "${locales[@]}"; do
  rc=0
  out=$(cd "$tmp/blobs" && LC_ALL=$locale grep -raEicH -f "$tmp/patterns" . 2>"$tmp/err") || rc=$?
  [[ $rc -le 1 && ! -s $tmp/err ]] || die "grep failed ($locale)"
  names=0; names=$(LC_ALL=$locale grep -aEic -f "$tmp/patterns" "$tmp/names.txt") || [[ $? == 1 ]] || die "grep failed on names ($locale)"
  found=$(printf '%s\n' "$out" | awk -F: '$NF > 0 { sub("^\\./", "", $1); print $1 "\t" $NF }')
  if [[ -n $found || $names != 0 ]]; then
    echo "hits ($locale):"
    [[ $names == 0 ]] || echo "  path names: $names"
    while IFS=$'\t' read -r idx count; do
      [[ -n $idx ]] || continue
      echo "  $(awk -F'\t' -v k="$idx" '$1 == k { sub("^[^\t]*\t", ""); print; exit }' "$tmp/index"):$count"
    done <<<"$found"
    hits=1
  fi
done
exit $hits
