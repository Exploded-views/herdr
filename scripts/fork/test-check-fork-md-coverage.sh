#!/usr/bin/env bash
# herdr-fork: fork-only. Tests scripts/fork/check-fork-md-coverage.py. Unofficial stopgap; see FORK.md.
#
# Builds a throwaway git repo (no network) with an "upstream" commit, then a fork commit, and checks
# the pass and fail cases: a complete table, a missing modified file, a glob, a glob that must not
# cross a slash, a rename's old path, a missing base file and a base that is not an ancestor.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# CHECK_SCRIPT points the test at another copy of the check (used to prove mutants fail).
check=${CHECK_SCRIPT:-$here/check-fork-md-coverage.py}
py=$(command -v python3)
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

# rawtable: write FORK.md from stdin.
rawtable() { cat > "$repo/FORK.md"; }

# expect <name> <want-exit> <want-text-or-empty>
expect() {
  local name=$1 want=$2 text=$3 out rc=0
  out=$("$py" -I "$check" "$repo" 2>&1) || rc=$?
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

new_repo whycol
modify a.rs
rawtable <<'EOT'
## Changed files

| Path | Kind | Why |
|---|---|---|
| `other.rs` | changed | mentions `a.rs` in the why column |

## Limits
EOT
expect "a path in the Why column does not count" 1 "a.rs"

new_repo afterheading
modify a.rs d/b.rs
rawtable <<'EOT'
## Changed files

| Path | Kind | Why |
|---|---|---|
| `a.rs` | changed | x |

## Limits

| `d/b.rs` | changed | below the next heading |
EOT
expect "a path after the next heading does not count" 1 "d/b.rs"

new_repo deleted
g rm -q d/b.rs; g commit -q -m delete
table '`a.rs`'
expect "a deleted upstream file is required" 1 "d/b.rs"
table '`d/b.rs`'
expect "a deleted file named in the table passes" 0 ""

new_repo typechange
rm "$repo/a.rs"; ln -s old.rs "$repo/a.rs"; g add -A; g commit -q -m symlink
if g diff --name-status "$base" HEAD | grep -q '^T'; then
  table '`old.rs`'
  expect "a type-changed file is required" 1 "a.rs"
  table '`a.rs`'
  expect "a type-changed file named in the table passes" 0 ""
else
  echo "FAIL: could not make a type change in the throwaway repo"; fail=1
fi

new_repo renameold
g mv old.rs new.rs; g commit -q -m rename
table '`old.rs`'
expect "a rename needs its new path" 1 "new.rs"

new_repo renamenew
g mv old.rs new.rs; g commit -q -m rename
table '`new.rs`'
expect "a rename needs its old path (new only listed)" 1 "old.rs"

new_repo renamereport
g mv old.rs new.rs; g commit -q -m rename
table '`a.rs`'
out=$("$py" -I "$check" "$repo" 2>&1) || true
if grep -qF "old.rs" <<<"$out" && grep -qF "new.rs" <<<"$out"; then
  echo "ok: a rename is reported under both names"
else
  echo "FAIL: a rename is reported under both names"; echo "$out"; fail=1
fi

new_repo nogit
modify a.rs
table '`a.rs`'
mkdir "$tmp/nobin"
rc=0
out=$(PATH=$tmp/nobin "$py" -I "$check" "$repo" 2>&1) || rc=$?
if [ "$rc" = 2 ] && grep -qF "cannot run git" <<<"$out"; then
  echo "ok: a missing git binary exits 2"
else
  echo "FAIL: a missing git binary exits 2 (exit $rc)"; echo "$out"; fail=1
fi

exit "$fail"
