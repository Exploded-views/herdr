#!/usr/bin/env -S python3 -I
# herdr-fork: fork-only. Scans a commit's tree against the private denylist. Unofficial stopgap;
# see FORK.md.
"""Scan the bytes git would publish against the private denylist.

Usage: scripts/fork/denylist-scan.py [repo] [allowlist] [ref]
  repo       defaults to this checkout
  allowlist  defaults to scripts/fork/denylist-allow.sha256 ("<sha256>  <path>" per line)
  ref        defaults to HEAD

Every blob in <ref>'s tree is scanned (file contents and symlink targets), and so is every path
name. A pinned path is skipped only when it is a regular file whose blob hashes to its pin, so
any edit to an exempted file is scanned again. Output is paths and counts only, never matched
text or the denylist; a path whose own name matches is withheld.

Denylist: $SCRUB_DENYLIST or ~/.config/crewkit/denylist.txt, one case-insensitive extended
regex per line. CR line endings are ignored. Blank lines and lines starting with "#" are
comments, so a pattern that must start with "#" is written "[#]...". A line may end in a label,
exactly one space then "#class=<word>" (lowercase letters, digits or "-"); the pattern is what
comes before it. Anything else on a pattern line that looks like a tag ("#", "class" and "=" in
any case, with or without spaces between) is an error naming only its line number, and so is any "#" that follows whitespace or a
non-ASCII byte, and a fullwidth "\uff03": a pattern line carries no free-form notes.

Run it directly (its shebang is python3 -I) or as python3 -I; run any other way it exits 2. Only
an isolated interpreter ignores modules planted next to it or on PYTHONPATH (sitecustomize runs
before the first line, so the script can't defend against a non-isolated start).

Matching uses grep -E, in the C locale and in a UTF-8 locale, so a term next to a byte that is
not valid UTF-8 is still found. Exit 0 = clean, 1 = hits, 2 = usage or scan error. An error is
never reported as clean.
"""
import sys

if not sys.flags.isolated:
    print("denylist-scan: run isolated (directly, or as python3 -I)", file=sys.stderr)
    sys.exit(2)

import hashlib
import os
import re
import subprocess
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


class ScanError(Exception):
    pass


def run(args, **kw):
    proc = subprocess.run(args, capture_output=True, **kw)
    return proc.returncode, proc.stdout, proc.stderr


def grep_count(patterns, locale, path, *extra):
    """grep -Eic over a file or directory; any error is a ScanError."""
    env = dict(os.environ, LC_ALL=locale)
    rc, out, err = run(["grep", "-aEic", *extra, "-f", patterns, "--", path], env=env)
    if rc > 1 or err:
        raise ScanError(f"grep failed ({locale})")
    return out


def utf8_locale():
    rc, out, _ = run(["locale", "-a"])
    names = out.decode("ascii", "replace").split() if rc == 0 else []
    for want in ("en_US.UTF-8", "en_US.utf8", "C.UTF-8", "C.utf8"):
        if want in names:
            return want
    raise ScanError("no UTF-8 locale available")


CLASS_TAG = re.compile(rb"(.*\S) #class=[a-z0-9-]+")
TAG_LIKE = re.compile(rb"#\s*class|(?:\s|[\x80-\xff])#|\xef\xbc\x83", re.IGNORECASE)


def strip_class_tag(line, lineno):
    # Read whole, a tagged line is a regex that never matches, which silently blinds the scan.
    if not TAG_LIKE.search(line):
        return line
    m = CLASS_TAG.fullmatch(line)
    if not m or TAG_LIKE.search(m.group(1)):
        raise ScanError(f"denylist line {lineno} has a malformed class tag")
    return m.group(1)


def load_patterns(denylist, tmp):
    try:
        with open(denylist, "rb") as f:
            raw = f.read()
    except OSError:
        raise ScanError("no readable denylist")
    lines = [l.rstrip(b"\r") for l in raw.split(b"\n")]
    pats = [
        strip_class_tag(l, n)
        for n, l in enumerate(lines, 1)
        if l.strip() and not l.lstrip().startswith(b"#")
    ]
    if not pats:
        raise ScanError("denylist has no patterns")
    path = os.path.join(tmp, "patterns")
    with open(path, "wb") as f:
        f.write(b"\n".join(pats) + b"\n")
    # grep exits 2 on an invalid pattern; one bad line must not turn the scan clean.
    rc, _, _ = run(["grep", "-Ec", "-f", path, os.devnull])
    if rc != 1:
        raise ScanError("denylist has an invalid pattern")
    return path


def load_allowlist(allow):
    try:
        with open(allow, "rb") as f:
            lines = f.read().split(b"\n")
    except OSError:
        raise ScanError(f"no readable allowlist at {allow}")
    pinned = {}
    for line in lines:
        line = line.rstrip(b"\r")
        if not line.strip() or line.lstrip().startswith(b"#"):
            continue
        sha, sep, path = line.partition(b"  ")
        if not sep or len(sha) != 64 or any(c not in b"0123456789abcdef" for c in sha) or not path or path[:1].isspace():
            raise ScanError(f"malformed allowlist line in {allow}")
        pinned[path] = sha.decode()
    return pinned


def tree_entries(repo, ref):
    rc, _, _ = run(["git", "-C", repo, "rev-parse", "--verify", "--quiet", f"{ref}^{{tree}}"])
    if rc != 0:
        raise ScanError(f"{repo} has no tree at {ref}")
    rc, out, err = run(["git", "-C", repo, "ls-tree", "-r", "-z", "--full-tree", ref])
    if rc != 0 or err:
        raise ScanError(f"git ls-tree failed at {ref}")
    entries = []
    for rec in out.split(b"\0"):
        if not rec:
            continue
        meta, _, path = rec.partition(b"\t")
        mode, kind, sha = meta.split(b" ")
        entries.append((mode.decode(), kind.decode(), sha.decode(), path))
    if not entries:
        raise ScanError(f"no files at {ref}")
    return entries


def read_blobs(repo, shas):
    """Yield (sha, bytes) through one git cat-file --batch process."""
    proc = subprocess.Popen(
        ["git", "-C", repo, "cat-file", "--batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    try:
        for sha in shas:
            proc.stdin.write(sha.encode() + b"\n")
            proc.stdin.flush()
            header = proc.stdout.readline().split()
            if len(header) != 3 or header[1] != b"blob":
                raise ScanError(f"cannot read blob {sha}")
            size = int(header[2])
            data = proc.stdout.read(size)
            if len(data) != size or proc.stdout.read(1) != b"\n":
                raise ScanError(f"short read for blob {sha}")
            yield sha, data
    finally:
        proc.stdin.close()
        proc.wait()
    if proc.returncode != 0:
        raise ScanError("git cat-file failed")


def show(path, patterns, locales):
    """A path for output, withheld when the path name itself matches."""
    for locale in locales:
        env = dict(os.environ, LC_ALL=locale)
        rc, _, _ = run(["grep", "-aEiq", "-f", patterns], input=path, env=env)
        if rc == 0:
            return "<path withheld: its name matches the denylist>"
        if rc > 1:
            raise ScanError(f"grep failed ({locale})")
    text = path.decode("utf-8", "replace")
    # Escape control characters so a crafted name can't rewrite the terminal line.
    return "".join(c if c.isprintable() else f"\\x{ord(c):02x}" for c in text)


def scan(repo, allow, ref, denylist):
    with tempfile.TemporaryDirectory() as tmp:
        patterns = load_patterns(denylist, tmp)
        locales = ["C", utf8_locale()]
        pinned = load_allowlist(allow)
        entries = tree_entries(repo, ref)

        names = os.path.join(tmp, "names")
        with open(names, "wb") as f:
            f.write(b"\0".join(path for _, _, _, path in entries) + b"\0")

        blobs_dir = os.path.join(tmp, "blobs")
        os.mkdir(blobs_dir)
        index = {}
        blob_entries = [(m, s, p) for m, k, s, p in entries if k == "blob"]
        data_by_sha = dict(read_blobs(repo, sorted({s for _, s, _ in blob_entries})))
        for n, (mode, sha, path) in enumerate(blob_entries):
            data = data_by_sha[sha]
            pin = pinned.get(path)
            if pin is not None:
                regular = mode in ("100644", "100755")
                if regular and hashlib.sha256(data).hexdigest() == pin:
                    print(f"exempt (pinned hash matches): {path.decode('utf-8', 'replace')}")
                    continue
                print(f"scanned (pinned hash or mode changed): {path.decode('utf-8', 'replace')}")
            with open(os.path.join(blobs_dir, str(n)), "wb") as f:
                f.write(data)
            index[str(n)] = path

        hits = False
        for locale in locales:
            found = []
            name_hits = int(grep_count(patterns, locale, names, "-z").strip() or b"0")
            if index:
                out = grep_count(patterns, locale, blobs_dir, "-rH")
                for line in out.decode().splitlines():
                    file, _, count = line.rpartition(":")
                    if int(count) > 0:
                        found.append((index[os.path.basename(file)], int(count)))
            if found or name_hits:
                hits = True
                print(f"hits ({locale}):")
                if name_hits:
                    print(f"  path names: {name_hits}")
                for path, count in sorted(found):
                    print(f"  {show(path, patterns, locales)}:{count}")
        return 1 if hits else 0


def main(argv):
    repo = argv[1] if len(argv) > 1 else ROOT
    allow = argv[2] if len(argv) > 2 else os.path.join(ROOT, "scripts/fork/denylist-allow.sha256")
    ref = argv[3] if len(argv) > 3 else "HEAD"
    denylist = os.environ.get("SCRUB_DENYLIST") or os.path.expanduser("~/.config/crewkit/denylist.txt")
    try:
        return scan(repo, allow, ref, denylist)
    except ScanError as e:
        print(f"denylist-scan: {e}", file=sys.stderr)
        return 2
    except Exception as e:  # any other failure is an error too, never "hits" or clean
        print(f"denylist-scan: unexpected error: {type(e).__name__}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
