#!/usr/bin/env bash
# herdr-fork: fork-only. Tests scripts/fork/check-fork-md-coverage.py. Unofficial stopgap; see FORK.md.
#
# Builds a throwaway git repo (no network) with an "upstream" commit, then a fork commit, and checks
# the pass and fail cases: a complete table, a missing modified file, a glob, a glob that must not
# cross a slash, a rename's old path, a missing base file and a base that is not an ancestor.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
check=$here/check-fork-md-coverage.py
tmp=$(mktemp -d)
trap 'rm -r "$tmp"' EXIT
fail=0

g() { git -C "$repo" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false "$@"; }

# new_repo <name>: an upstream commit with a.rs, d/b.rs, d/e/c.rs and old.rs; sets $repo and $base.
new_repo() {
  repo=$tmp/$1
  mkdir -p "$repo/d/e" "$repo/scripts/fork"
  git init -q "$repo"
  for f in a.rs d/b.rs d/e/c.rs old.rs; do printf 'one\nline\nof\ntext\n%s\n' "$f" > "$repo/$f"; done
  printf '# fork\n' > "$repo/FORK.md"
  g add -A
  g commit -q -m upstream
  base=$(g rev-parse HEAD)
  printf '%s\n' "$base" > "$repo/scripts/fork/upstream-base"
}

# table <rows...>: write FORK.md with the given first-column cells.
table() {
  { printf '# fork\n\n## Changed files\n\n| Path | Kind | Why |\n|---|---|---|\n'
    for c in "$@"; do printf '| %s | changed | x |\n' "$c"; done
    printf '\n## Limits\n\n| `zzz.rs` | x |\n'; } > "$repo/FORK.md"
}

# expect <name> <want-exit> <want-text-or-empty>
expect() {
  local name=$1 want=$2 text=$3 out rc=0
  out=$(python3 -I "$check" "$repo" 2>&1) || rc=$?
  if [ "$rc" != "$want" ] || { [ -n "$text" ] && ! grep -qF -- "$text" <<<"$out"; }; then
    echo "FAIL: $name (exit $rc, want $want, text '$text')"; echo "$out"; fail=1
  else
    echo "ok: $name"
  fi
}

modify() { for f in "$@"; do printf 'changed\n' >> "$repo/$f"; done; g add -A; g commit -q -m fork; }

new_repo complete
modify a.rs d/b.rs
table '`a.rs`' '`d/b.rs`'
expect "a complete table passes" 0 "all 2"

new_repo missing
modify a.rs d/b.rs
table '`a.rs`'
expect "a missing modified file fails and is named" 1 "d/b.rs"

new_repo added
modify a.rs
printf 'new\n' > "$repo/added.rs"; g add -A; g commit -q -m add
table '`a.rs`'
expect "an added file is out of scope" 0 ""

new_repo glob
modify d/b.rs d/e/c.rs
table '`d/*.rs`' '`d/e/c.rs`'
expect "a glob covers a file" 0 ""

new_repo globslash
modify d/e/c.rs
table '`d/*.rs`'
expect "a glob does not cross a slash" 1 "d/e/c.rs"

new_repo rename
g mv old.rs new.rs; g commit -q -m rename
table '`new.rs`'
expect "a rename's old path is required" 1 "old.rs"
table '`new.rs`' '`old.rs`'
expect "a rename with both paths passes" 0 ""

new_repo nobase
modify a.rs
table '`a.rs`'
rm "$repo/scripts/fork/upstream-base"
expect "a missing base file exits 2" 2 ""

new_repo badsha
modify a.rs
table '`a.rs`'
printf 'nothex\n' > "$repo/scripts/fork/upstream-base"
expect "a malformed base exits 2" 2 ""

new_repo notanc
modify a.rs
table '`a.rs`'
git -C "$repo" checkout -q --orphan other
g commit -q --allow-empty -m other
expect "a base that is not an ancestor exits 2" 2 "not an ancestor"

exit "$fail"
