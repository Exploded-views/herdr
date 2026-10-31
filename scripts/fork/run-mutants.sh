#!/usr/bin/env bash
# herdr-fork: fork-only. Runs a shard of the security-guard mutants: each is a small patch that
# breaks one guard, and a named test must fail with it applied. Unofficial stopgap; see FORK.md
# and docs/ci.md.
#
# Usage: scripts/fork/run-mutants.sh <shard>/<total>      (shard counts from 1)
#
# Reads scripts/fork/mutants/manifest.tsv: <id> TAB <patch file> TAB <nextest filter expression>.
# Takes every <total>-th manifest line starting at line <shard> and, for each mutant:
#   1. git apply --check        fails -> STALE    (the patch no longer applies)
#   2. apply, build the tests   fails -> BROKEN   (the mutant does not compile)
#   3. nextest run -E <filter>  passes -> SURVIVED (no test noticed the broken guard)
#                               fails  -> KILLED  (what we want)
#                               any other outcome (no tests matched, a crash) -> ERROR
#   (a test killed by the `mutants` nextest profile's slow-timeout counts as failed -> KILLED)
#   4. git apply -R, then check the tree is back to the base.
# Before any mutant it builds the clean copy and runs this shard's filters once; both must succeed
# (baseline). Exits 1 if any mutant is not KILLED, 2 on a usage error, an empty manifest, a failing
# baseline or a tree that did not restore.
#
# The repo is mounted read-only in CI's image, so the tracked tree is copied to a writable
# directory first and every mutant runs there. Never touches the source tree.
# Test hooks: MUTANTS_ROOT (repo root), MUTANTS_DIR (manifest and patches), MUTANTS_WORK (keep
# the working copy there; it must not exist or be empty).
set -uo pipefail

usage() {
  echo "usage: run-mutants.sh <shard>/<total>   (e.g. 1/4)" >&2
  exit 2
}
[[ ${1:-} =~ ^([1-9][0-9]*)/([1-9][0-9]*)$ ]] || usage
shard=${BASH_REMATCH[1]}
total=${BASH_REMATCH[2]}
((shard <= total)) || usage

root=${MUTANTS_ROOT:-$(git rev-parse --show-toplevel)} || exit 2
dir=${MUTANTS_DIR:-$root/scripts/fork/mutants}
manifest=$dir/manifest.tsv
[[ -f $manifest ]] || { echo "run-mutants.sh: no manifest at $manifest" >&2; exit 2; }

# Select this shard's lines.
ids=()
patches=()
filters=()
index=0
tab=$'\t'
while IFS= read -r line || [[ -n $line ]]; do
  [[ -z $line || $line == \#* ]] && continue
  # Exactly two single tabs, and no empty field (read would collapse repeated tabs).
  tabs=${line//[!$tab]/}
  id=${line%%"$tab"*}
  rest=${line#*"$tab"}
  patch=${rest%%"$tab"*}
  filter=${rest#*"$tab"}
  if [[ ${#tabs} -ne 2 || -z $id || -z $patch || -z $filter ]]; then
    echo "run-mutants.sh: malformed manifest line '$line' (want: id TAB patch TAB filter, one tab each, none empty)" >&2
    exit 2
  fi
  if (((index % total) == shard - 1)); then
    ids+=("$id")
    patches+=("$patch")
    filters+=("$filter")
  fi
  index=$((index + 1))
done <"$manifest"
if [[ $index -eq 0 ]]; then
  echo "run-mutants.sh: the manifest holds no mutants" >&2
  exit 2
fi
echo "shard $shard/$total: ${#ids[@]} of $index mutants"
if [[ ${#ids[@]} -eq 0 ]]; then
  exit 0
fi

# A writable copy of the tracked tree, as its own throwaway git repo so a revert can be checked.
work=${MUTANTS_WORK:-}
if [[ -z $work ]]; then
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
else
  mkdir -p "$work"
fi
log=$(mktemp)
trap 'rm -f "$log"; [[ -n ${MUTANTS_WORK:-} ]] || rm -rf "$work"' EXIT
git -C "$root" ls-files -z | tar -C "$root" --null -T - -cf - | tar -C "$work" -xf - || exit 2
# Zig unpacks its packages into the project (gitignored, so not tracked); the copy needs them.
zigpkg=vendor/libghostty-vt/zig-pkg
if [[ -d $root/$zigpkg ]]; then
  mkdir -p "$work/$zigpkg"
  cp -R "$root/$zigpkg/." "$work/$zigpkg/" || exit 2
fi
g() { git -C "$work" -c user.name=mutants -c user.email=mutants@localhost -c commit.gpgsign=false "$@"; }
g init -q && g add -A && g commit -qm base || { echo "run-mutants.sh: cannot snapshot the copy" >&2; exit 2; }

# Baseline: the unmutated copy must build and every filter of this shard must pass, or a test
# that fails for an unrelated reason would read as KILLED for every mutant aimed at it.
baseline_filter=
for f in "${filters[@]}"; do
  baseline_filter=${baseline_filter:+$baseline_filter | }"($f)"
done
echo "baseline: building and running this shard's tests on the clean copy"
if ! (cd "$work" && cargo nextest run --locked --profile mutants --no-run) >"$log" 2>&1; then
  tail -n 40 "$log" | sed -e 's/^/    | /'
  echo "run-mutants.sh: baseline failed: the unmutated copy does not build" >&2
  exit 2
fi
if ! (cd "$work" && cargo nextest run --locked --profile mutants --no-tests=fail -E "$baseline_filter" \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output final \
  --success-output never) >"$log" 2>&1; then
  tail -n 40 "$log" | sed -e 's/^/    | /'
  echo "run-mutants.sh: baseline failed: the shard's tests do not pass on the unmutated copy" >&2
  exit 2
fi

verdicts=()
failed=0
n=${#ids[@]}
for ((i = 0; i < n; i++)); do
  id=${ids[i]}
  patch=$dir/${patches[i]}
  filter=${filters[i]}
  started=$SECONDS
  verdict=
  : >"$log"
  if [[ ! -f $patch ]]; then
    echo "patch file missing: $patch" >"$log"
    verdict=STALE
  elif ! g apply --check "$patch" >"$log" 2>&1; then
    verdict=STALE
  elif ! g apply "$patch" >"$log" 2>&1; then
    verdict=STALE
  else
    if ! (cd "$work" && cargo nextest run --locked --profile mutants --no-run) >"$log" 2>&1; then
      verdict=BROKEN
    else
      (cd "$work" && cargo nextest run --locked --profile mutants --no-tests=fail -E "$filter" --no-fail-fast \
        --status-level fail --final-status-level fail --failure-output final \
        --success-output never) >"$log" 2>&1
      case $? in
        0) verdict=SURVIVED ;;
        100) verdict=KILLED ;;
        *) verdict=ERROR ;;
      esac
    fi
    if ! g apply -R "$patch" >>"$log" 2>&1 || ! g diff --quiet || [[ -n $(g status --porcelain) ]]; then
      echo "run-mutants.sh: $id did not revert cleanly; the working copy is no longer the base" >&2
      exit 2
    fi
  fi
  printf '%-14s %s (%ss)\n' "$id" "$verdict" "$((SECONDS - started))"
  if [[ $verdict == KILLED ]]; then
    grep -E '^[[:space:]]*FAIL ' "$log" | sed -e 's/^[[:space:]]*/    /' | sort -u | head -5
  else
    failed=1
    tail -n 40 "$log" | sed -e 's/^/    | /'
  fi
  verdicts+=("$(printf '%-14s %s' "$id" "$verdict")")
done

echo "== shard $shard/$total summary =="
printf '%s\n' "${verdicts[@]}"
if ((failed)); then
  echo "shard $shard/$total: some mutants were not KILLED"
  exit 1
fi
echo "shard $shard/$total: all $n mutants KILLED"
