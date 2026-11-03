#!/usr/bin/env -S python3 -I
# herdr-fork: fork-only. Checks that FORK.md names every upstream file the fork modifies. Unofficial
# stopgap; see FORK.md.
"""Fail when FORK.md's "Changed files" table misses an upstream file the fork modifies.

Usage: python3 -I scripts/fork/check-fork-md-coverage.py [repo]
  repo  defaults to the checkout this script is in

The upstream base is the one line in scripts/fork/upstream-base (a full 40-hex sha). Update that file
whenever upstream is merged in, to the upstream commit merged.

Every path `git diff --name-status -M <base> HEAD` reports with a status other than A (modified,
deleted, renamed, type-changed) must match a backticked entry in the first column of a "Changed
files" row. A rename needs both its old and new path. An entry containing `*` is a glob in which
`*` does not cross `/`. Added files are out of scope.

Exit 0: complete. Exit 1: paths missing. Exit 2: setup or usage error.
"""
import os
import re
import subprocess
import sys

HEX40 = re.compile(r"[0-9a-f]{40}")


def die(msg):
    print(f"check-fork-md-coverage: error: {msg}", file=sys.stderr)
    sys.exit(2)


def git(repo, *args):
    try:
        return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True)
    except (OSError, UnicodeDecodeError) as exc:
        # No git binary, or a path that is not valid UTF-8: a setup error, never "paths missing".
        die(f"cannot run git: {exc}")


def table_entries(text):
    """Backticked paths in the first column of every row from '## Changed files' to the next '## '."""
    entries = []
    inside = False
    for line in text.splitlines():
        if line.startswith("## "):
            inside = line.strip() == "## Changed files"
            continue
        if not inside or not line.startswith("|"):
            continue
        cells = line.split("|")
        if len(cells) < 3:
            continue
        entries.extend(re.findall(r"`([^`]+)`", cells[1]))
    return entries


def glob_regex(pattern):
    out = []
    for ch in pattern:
        out.append("[^/]*" if ch == "*" else re.escape(ch))
    return re.compile("".join(out) + r"\Z")


def main():
    repo = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    base_file = os.path.join(repo, "scripts", "fork", "upstream-base")
    try:
        with open(base_file, encoding="utf-8") as fh:
            base = fh.read().strip()
    except OSError as exc:
        die(f"cannot read {base_file}: {exc}")
    if not HEX40.fullmatch(base):
        die(f"{base_file} must hold one 40-hex sha, got {base!r}")
    if git(repo, "merge-base", "--is-ancestor", base, "HEAD").returncode != 0:
        die(f"base {base} is not an ancestor of HEAD (shallow clone? use fetch-depth: 0)")
    diff = git(repo, "diff", "--name-status", "-M", "-z", base, "HEAD")
    if diff.returncode != 0:
        die(f"git diff failed: {diff.stderr.strip()}")
    fields = diff.stdout.split("\0")
    paths = []
    i = 0
    while i < len(fields) and fields[i]:
        status = fields[i][0]
        n = 2 if status in "RC" else 1
        if status != "A":
            paths.extend(fields[i + 1 : i + 1 + n])
        i += 1 + n
    try:
        with open(os.path.join(repo, "FORK.md"), encoding="utf-8") as fh:
            entries = table_entries(fh.read())
    except OSError as exc:
        die(f"cannot read FORK.md: {exc}")
    exact = {e for e in entries if "*" not in e}
    globs = [glob_regex(e) for e in entries if "*" in e]
    missing = sorted(p for p in set(paths) if p not in exact and not any(g.match(p) for g in globs))
    if missing:
        print("FORK.md does not name these upstream files the fork modifies:")
        for p in missing:
            print(f"  {p}")
        print(f"FAIL: {len(missing)} missing of {len(set(paths))} checked")
        sys.exit(1)
    print(f"ok: FORK.md names all {len(set(paths))} modified upstream paths")


main()
