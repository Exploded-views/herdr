#!/usr/bin/env python3
# herdr-fork: fork-only file. Asserts the security conditions of .github/workflows/fork-release.yml
# from its YAML. Stdlib only (PyYAML is used when installed; otherwise a minimal parser that
# covers the subset this workflow uses). Usage: test-fork-release-workflow.py [workflow.yml]
import os
import re
import sys
import unittest

REPO_GUARD = "github.repository == 'Exploded-views/herdr'"
DEFAULT_PATH = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "..", ".github", "workflows", "fork-release.yml"
)
WORKFLOW_PATH = DEFAULT_PATH


def _scalar(text):
    text = text.strip()
    if text == "{}":
        return {}
    if text.startswith("[") and text.endswith("]"):
        inner = text[1:-1].strip()
        return [_scalar(p) for p in inner.split(",")] if inner else []
    if len(text) >= 2 and text[0] == text[-1] and text[0] in "'\"":
        return text[1:-1]
    if text in ("~", "null"):
        return None
    return text


def _strip_comment(text):
    quote = None
    for i, ch in enumerate(text):
        if quote:
            if ch == quote:
                quote = None
        elif ch in "'\"":
            quote = ch
        elif ch == "#" and (i == 0 or text[i - 1] in " \t"):
            return text[:i].rstrip()
    return text.rstrip()


class MiniYaml:
    """Block mappings, block lists, scalars, flow [] and {}, and | / > block scalars."""

    def __init__(self, text):
        self.raw = text.splitlines()
        self.pos = 0
        self.lines = [(len(l) - len(l.lstrip(" ")), l.strip()) for l in self.raw]

    def _skip(self):
        while self.pos < len(self.lines):
            ind, txt = self.lines[self.pos]
            if txt and not txt.startswith("#"):
                return
            self.pos += 1

    def parse(self):
        self._skip()
        return self._node(0)

    def _node(self, min_indent):
        self._skip()
        ind, txt = self.lines[self.pos]
        return self._list(ind) if txt.startswith("- ") or txt == "-" else self._map(ind)

    def _list(self, ind):
        out = []
        while True:
            self._skip()
            if self.pos >= len(self.lines):
                break
            i, txt = self.lines[self.pos]
            if i != ind or not (txt.startswith("- ") or txt == "-"):
                break
            rest = txt[1:].lstrip()
            if not rest:
                self.pos += 1
                out.append(self._node(ind + 1))
                continue
            offset = len(txt) - len(rest)
            self.lines[self.pos] = (ind + offset, rest)
            if re.match(r"^[^\s'\"\[{][^:]*:(\s|$)", rest):
                out.append(self._map(ind + offset))
            else:
                self.pos += 1
                out.append(self._value(_strip_comment(rest), ind))
        return out

    def _map(self, ind):
        out = {}
        while True:
            self._skip()
            if self.pos >= len(self.lines):
                break
            i, txt = self.lines[self.pos]
            if i != ind or txt.startswith("- "):
                break
            m = re.match(r"^(\"[^\"]*\"|'[^']*'|[^:]+?):(?:\s+(.*))?$", txt)
            if not m:
                raise ValueError("unparsed line %d: %r" % (self.pos + 1, txt))
            key = _scalar(m.group(1))
            rest = _strip_comment(m.group(2) or "")
            self.pos += 1
            if rest == "":
                self._skip()
                if self.pos < len(self.lines):
                    ni, nt = self.lines[self.pos]
                    if ni > ind or (ni == ind and nt.startswith("- ")):
                        out[key] = self._node(ind + 1)
                        continue
                out[key] = None
            else:
                out[key] = self._value(rest, ind)
        return out

    def _value(self, rest, ind):
        if re.match(r"^[|>][+-]?$", rest):
            chunks = []
            base = None
            while self.pos < len(self.raw):
                raw = self.raw[self.pos]
                if raw.strip() == "":
                    chunks.append("")
                    self.pos += 1
                    continue
                cur = len(raw) - len(raw.lstrip(" "))
                if cur <= ind:
                    break
                base = cur if base is None else base
                chunks.append(raw[base:])
                self.pos += 1
            return "\n".join(chunks).rstrip("\n") + "\n"
        return _scalar(rest)


def load_workflow(path):
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    try:
        import yaml  # type: ignore

        data = yaml.safe_load(text)
    except ImportError:
        data = MiniYaml(text).parse()
    if True in data:  # YAML 1.1 parses the bare key `on` as boolean True
        data["on"] = data.pop(True)
    return text, data


def walk_uses(node):
    if isinstance(node, dict):
        for k, v in node.items():
            if k == "uses" and isinstance(v, str):
                yield v
            else:
                yield from walk_uses(v)
    elif isinstance(node, list):
        for v in node:
            yield from walk_uses(v)


class ForkReleaseWorkflow(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text, cls.wf = load_workflow(WORKFLOW_PATH)
        cls.jobs = cls.wf.get("jobs") or {}

    def test_has_jobs(self):
        self.assertIn("release", self.jobs)
        self.assertGreaterEqual(len(self.jobs), 3)

    def test_triggers_are_fork_tag_push_and_dispatch_only(self):
        on = self.wf["on"]
        self.assertEqual(set(on), {"push", "workflow_dispatch"})
        self.assertEqual(on["push"], {"tags": ["v*-ev.*"]})
        tag = (on["workflow_dispatch"] or {}).get("inputs", {}).get("tag")
        self.assertIsNotNone(tag, "workflow_dispatch needs a `tag` input")
        self.assertIn(str(tag.get("required")).lower(), ("true",))

    def test_no_pull_request_triggers_anywhere(self):
        self.assertNotIn("pull_request_target", self.text)
        self.assertNotRegex(self.text, r"(?m)^\s*pull_request\s*:")

    def test_top_level_permissions_empty(self):
        self.assertEqual(self.wf.get("permissions"), {})

    def test_only_release_job_writes_contents(self):
        writers = []
        for name, job in self.jobs.items():
            perms = job.get("permissions")
            self.assertIsInstance(perms, dict, "job %s must set permissions" % name)
            for scope, level in perms.items():
                if level == "write":
                    writers.append((name, scope))
                if name != "release":
                    self.assertIn((scope, level), [("contents", "read")], "%s: %s" % (name, scope))
        self.assertEqual(writers, [("release", "contents")])

    def test_every_uses_pinned_to_full_sha(self):
        uses = list(walk_uses(self.jobs))
        self.assertTrue(uses)
        for u in uses:
            self.assertRegex(u, r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$", u)

    def test_checkouts_do_not_persist_credentials(self):
        n = 0
        for job in self.jobs.values():
            for step in job.get("steps", []):
                if str(step.get("uses", "")).startswith("actions/checkout@"):
                    n += 1
                    self.assertEqual(str((step.get("with") or {}).get("persist-credentials")), "false")
        self.assertGreater(n, 0)

    def test_only_github_token_secret(self):
        found = set(re.findall(r"secrets\.([A-Za-z0-9_]+)", self.text))
        self.assertLessEqual(found, {"GITHUB_TOKEN"})
        self.assertNotRegex(self.text, r"secrets\s*\[|secrets:\s*inherit")

    def test_every_job_has_repo_guard(self):
        for name, job in self.jobs.items():
            self.assertIn(REPO_GUARD, str(job.get("if", "")), "job %s lacks the repo guard" % name)

    def test_release_job_needs_every_other_job(self):
        needs = self.jobs["release"].get("needs")
        needs = [needs] if isinstance(needs, str) else list(needs or [])
        self.assertEqual(set(needs), set(self.jobs) - {"release"})

    def test_tag_validated_with_fork_tag_regex(self):
        self.assertIn(r"^v[0-9]+\.[0-9]+\.[0-9]+-ev\.[0-9]+(-rc[0-9]+)?$", self.text)
        self.assertIn("--verify-tag", self.text)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1].endswith(".yml"):
        WORKFLOW_PATH = sys.argv.pop(1)
    unittest.main()
