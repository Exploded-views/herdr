#!/usr/bin/env python3
# herdr-fork: fork-only file. Asserts the security conditions of .github/workflows/fork-release.yml
# from its YAML, pinning each guard to its exact command. Stdlib only (PyYAML is used when
# installed, otherwise a minimal parser that covers the subset this workflow uses; every parsed
# scalar is normalised to a string, so the result does not depend on which parser ran). The
# SelfTest class applies a list of mutations to the workflow text in memory and fails if the
# checks accept any of them. Usage: test-fork-release-workflow.py [workflow.yml]
import io
import os
import re
import shlex
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


def normalise(node):
    """Strings for every scalar (a YAML bool and the text "false" compare equal)."""
    if isinstance(node, dict):
        out = {}
        for k, v in node.items():
            if k is True:  # YAML 1.1 parses the bare key `on` as boolean True
                k = "on"
            out[normalise(k) if not isinstance(k, str) else k] = normalise(v)
        return out
    if isinstance(node, list):
        return [normalise(v) for v in node]
    if node is None or isinstance(node, str):
        return node
    if isinstance(node, bool):
        return "true" if node else "false"
    return str(node)


def parse_workflow(text, use_yaml=True):
    data = None
    if use_yaml:
        try:
            import yaml  # type: ignore

            data = yaml.safe_load(text)
        except ImportError:
            data = None
    if data is None:
        data = MiniYaml(text).parse()
    return normalise(data)


def load_workflow(path):
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    return text, parse_workflow(text)


# Set by SelfTest to run the checks against a mutated copy of the workflow text.
TEXT_OVERRIDE = None


def strip_comment_lines(text):
    return "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))


def all_steps(job):
    return job.get("steps") or []


def step_runs(job):
    return [str(st.get("run", "")) for st in all_steps(job)]


FORK_CI_PATH = os.path.join(os.path.dirname(DEFAULT_PATH), "fork-ci.yml")


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


def logical_lines(run):
    """Script lines with backslash continuations joined, blanks and comments dropped."""
    joined = re.sub(r"\s*\\\n\s*", " ", run)
    return [l.strip() for l in joined.splitlines() if l.strip() and not l.strip().startswith("#")]


def step_named(job, name):
    for st in all_steps(job):
        if st.get("name") == name:
            return st
    raise AssertionError("no step named %r" % name)


def block_after(lines, header):
    """Body of the `if` block whose header line equals `header`; also asserts that it has no
    else/elif branch and that its last command is `exit 1`."""
    assert lines.count(header) == 1, "expected exactly one %r, found %d" % (header, lines.count(header))
    depth, body = 1, []
    for line in lines[lines.index(header) + 1:]:
        if line.startswith("if "):
            depth += 1
        elif line == "fi":
            depth -= 1
            if depth == 0:
                break
        elif depth == 1 and (line.startswith("else") or line.startswith("elif")):
            raise AssertionError("%r has an else/elif branch" % header)
        body.append(line)
    else:
        raise AssertionError("%r is not closed" % header)
    if not body or body[-1] != "exit 1":
        raise AssertionError("%r failure branch does not end in `exit 1`: %r" % (header, body))
    return body


def strict_shell(lines, what):
    if not lines or lines[0] != "set -euo pipefail":
        raise AssertionError("%s must start with `set -euo pipefail`" % what)


ADMIN_HEADER = 'if [ "$permission" != "admin" ]; then'
ADMIN_LOOP = 'for actor in "$GITHUB_ACTOR" "$GITHUB_TRIGGERING_ACTOR"; do'
ADMIN_QUERY = 'permission="$(gh api "repos/$GITHUB_REPOSITORY/collaborators/$actor/permission" --jq .permission)"'
TAG_REGEX = r"^v[0-9]+\.[0-9]+\.[0-9]+-ev\.[0-9]+(-rc[0-9]+)?$"
TAG_HEADER = "if ! printf '%s' \"$tag\" | grep -Eq '" + TAG_REGEX + "'; then"
ANCESTOR_HEADER = 'if ! git merge-base --is-ancestor "$sha" refs/remotes/origin/integ/next; then'
LS_REMOTE = (
    'current="$(git ls-remote origin "refs/tags/${TAG}" "refs/tags/${TAG}^{}" | awk -v ref="refs/tags/${TAG}" '
    "'$2 == (ref \"^{}\") { peeled = $1 } $2 == ref { plain = $1 } END { print (peeled != \"\" ? peeled : plain) }')\""
)
RECHECK_HEADER = 'if [ "$current" != "$SHA" ]; then'
EXISTING_QUERY = (
    'existing="$(gh api "repos/$GITHUB_REPOSITORY/releases" --paginate '
    "--jq '.[] | select(.tag_name == env.TAG) | .id')\""
)
EXISTING_HEADER = 'if [ -n "$existing" ]; then'
CREATE = (
    'release_id="$(gh api -X POST "repos/$GITHUB_REPOSITORY/releases" -f tag_name="$TAG" '
    '-f target_commitish="$SHA" -f name="$TAG" -F body=@"$RUNNER_TEMP/notes.md" -F draft=true '
    '-F prerelease="$PRERELEASE" --jq .id)"'
)
ID_HEADER = 'if [[ ! "$release_id" =~ ^[0-9]+$ ]]; then'
UPLOAD_LOOP = 'for f in dist/*.tar.gz dist/*.sha256; do'
UPLOAD_IF = (
    'if ! out="$(printf \'Authorization: Bearer %s\\n\' "$GH_TOKEN" | curl --fail-with-body -sS -X POST -H @- '
    '-H "Content-Type: application/octet-stream" --data-binary "@$f" '
    '"https://uploads.github.com/repos/$GITHUB_REPOSITORY/releases/$release_id/assets?name=$(basename "$f")")"; then'
)
UPLOAD_ERR = 'echo "error: uploading $f failed: $out" >&2'
REF_HEADER = 'if [ "$GITHUB_REF" != "refs/tags/$tag" ]; then'
GUARD_STEPS = {
    "prepare": ["Require repository admins", "Validate the tag", "Resolve the tag to a commit on integ/next"],
    "build": ["Verify the tag's base version matches Cargo.toml", "Check the binary's architecture and that it runs"],
    "release": [
        "Require repository admins",
        "Verify the artifacts",
        "Create the release as a draft, then publish it",
    ],
}
PUBLISH = 'gh api -X PATCH "repos/$GITHUB_REPOSITORY/releases/$release_id" -F draft=false'


class ForkReleaseWorkflow(unittest.TestCase):
    """Known limits: a keyword checker cannot stop deliberate sabotage by someone who can edit the
    workflow, for example a dead `if false; then ... fi` around a guard, an override after a
    guarded value (`permission=admin`, `current="$SHA"`, `existing=""`), or `tag_still_at_sha`
    redefined as a no-op. The owner-side ruleset and environment policy (docs/ci.md) are the real
    control; this test catches accidents and plausible edits."""

    @classmethod
    def setUpClass(cls):
        if TEXT_OVERRIDE is not None:
            cls.text, cls.wf = TEXT_OVERRIDE, parse_workflow(TEXT_OVERRIDE)
        else:
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
        self.assertEqual(tag.get("required"), "true")

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
                    self.assertEqual((step.get("with") or {}).get("persist-credentials"), "false")
        self.assertGreater(n, 0)

    def test_only_github_token_secret(self):
        found = set(re.findall(r"secrets\.([A-Za-z0-9_]+)", self.text))
        self.assertLessEqual(found, {"GITHUB_TOKEN"})
        self.assertNotRegex(self.text, r"secrets\s*\[|secrets:\s*inherit")
        body = strip_comment_lines(self.text)
        self.assertEqual(re.findall(r"\bsecrets\b(?!\.GITHUB_TOKEN\b)", body), [])

    def test_every_job_has_repo_guard(self):
        for name, job in self.jobs.items():
            cond = str(job.get("if", "")).strip()
            self.assertEqual(cond, REPO_GUARD, "job %s: guard must be exactly the repo guard" % name)

    def test_release_job_needs_every_other_job(self):
        needs = self.jobs["release"].get("needs")
        needs = [needs] if isinstance(needs, str) else list(needs or [])
        self.assertEqual(set(needs), set(self.jobs) - {"release"})

    def test_no_expression_interpolation_in_run_blocks(self):
        for name, job in self.jobs.items():
            for run in step_runs(job):
                self.assertNotIn("${{", run, "job %s interpolates into a shell script" % name)

    def test_no_shared_cache_in_release_builds(self):
        for u in walk_uses(self.jobs):
            self.assertFalse(u.startswith("Swatinem/rust-cache@"), u)
            self.assertFalse(u.startswith("actions/cache"), u)

    def test_no_anchors_or_aliases(self):
        body = strip_comment_lines(self.text)
        self.assertNotRegex(body, r"(?m)^\s*(-\s+)?[\w-]+:\s+[&*]\w|^\s*-\s+[&*]\w|<<:")

    def test_secrets_only_in_release_job(self):
        for name, job in self.jobs.items():
            if name != "release":
                self.assertNotIn("secrets", repr(job), "job %s must not touch secrets" % name)

    def test_job_chain(self):
        self.assertIsNone(self.jobs["prepare"].get("needs"))
        needs = self.jobs["build"].get("needs")
        self.assertEqual([needs] if isinstance(needs, str) else needs, ["prepare"])

    def test_tag_validation_guard(self):
        step = step_named(self.jobs["prepare"], "Validate the tag")
        self.assertEqual(step.get("id"), "tag")
        lines = logical_lines(step["run"])
        strict_shell(lines, "Validate the tag")
        block_after(lines, TAG_HEADER)
        block_after(lines, REF_HEADER)
        self.assertLess(lines.index(TAG_HEADER), lines.index(REF_HEADER))
        self.assertLess(lines.index(REF_HEADER), lines.index('echo "tag=$tag" >> "$GITHUB_OUTPUT"'))
        self.assertLess(lines.index(TAG_HEADER), lines.index('echo "tag=$tag" >> "$GITHUB_OUTPUT"'))
        env = step.get("env") or {}
        self.assertEqual(env.get("INPUT_TAG"), "${{ github.event.inputs.tag }}")

    def _check_admin(self, job_name, step):
        lines = logical_lines(step["run"])
        strict_shell(lines, "%s admin check" % job_name)
        self.assertEqual(lines.count(ADMIN_LOOP), 1, job_name)
        self.assertEqual(lines.count(ADMIN_QUERY), 1, job_name)
        self.assertLess(lines.index(ADMIN_LOOP), lines.index(ADMIN_QUERY))
        self.assertLess(lines.index(ADMIN_QUERY), lines.index(ADMIN_HEADER))
        block_after(lines, ADMIN_HEADER)
        self.assertEqual(lines[-1], "done", job_name)

    def test_admin_check_in_prepare_and_first_in_release(self):
        prep = step_named(self.jobs["prepare"], "Require repository admins")
        self.assertIs(all_steps(self.jobs["prepare"])[0], prep)
        self._check_admin("prepare", prep)
        first = all_steps(self.jobs["release"])[0]
        self.assertEqual(first.get("name"), "Require repository admins")
        self._check_admin("release", first)

    def test_tag_resolved_once_to_sha_on_integ_next(self):
        prep = self.jobs["prepare"]
        self.assertEqual(prep.get("outputs", {}).get("sha"), "${{ steps.resolve.outputs.sha }}")
        co = [st for st in all_steps(prep) if str(st.get("uses", "")).startswith("actions/checkout@")]
        self.assertEqual(len(co), 1)
        self.assertEqual((co[0].get("with") or {}).get("fetch-depth"), "0")
        step = step_named(prep, "Resolve the tag to a commit on integ/next")
        self.assertEqual(step.get("id"), "resolve")
        self.assertEqual((step.get("env") or {}).get("TAG"), "${{ steps.tag.outputs.tag }}")
        lines = logical_lines(step["run"])
        strict_shell(lines, "resolve")
        rev = 'sha="$(git rev-parse "refs/tags/${TAG}^{commit}")"'
        self.assertEqual(lines.count(rev), 1)
        block_after(lines, ANCESTOR_HEADER)
        self.assertLess(lines.index(rev), lines.index(ANCESTOR_HEADER))
        out = [l for l in lines if "GITHUB_OUTPUT" in l]
        self.assertEqual(out, ['echo "sha=$sha" >> "$GITHUB_OUTPUT"'])
        self.assertLess(lines.index(ANCESTOR_HEADER), lines.index(out[0]))
        fetch = [l for l in lines if l.startswith("git fetch ")]
        self.assertEqual(
            fetch,
            ['git fetch --force origin "refs/heads/integ/next:refs/remotes/origin/integ/next" '
             '"refs/tags/${TAG}:refs/tags/${TAG}"'],
        )
        for name in ("build", "release"):
            cos = [st for st in all_steps(self.jobs[name]) if str(st.get("uses", "")).startswith("actions/checkout@")]
            self.assertEqual(len(cos), 1, name)
            self.assertEqual((cos[0].get("with") or {}).get("ref"), "${{ needs.prepare.outputs.sha }}", name)

    def test_build_checks_binary(self):
        lines = logical_lines(step_named(self.jobs["build"], "Check the binary's architecture and that it runs")["run"])
        strict_shell(lines, "binary check")
        self.assertEqual(lines.count('file "$bin" | grep -Eq "$FILE_EXPECT"'), 1)
        self.assertEqual(lines.count('printf \'%s\' "$out" | grep -qF "$base"'), 1)
        self.assertEqual([l for l in lines if "||" in l], [])

    def test_artifacts_verified_before_release(self):
        lines = logical_lines(step_named(self.jobs["release"], "Verify the artifacts")["run"])
        strict_shell(lines, "verify")
        for want in (
            'test "$(find dist -name \'*.tar.gz\' | wc -l)" -eq 4',
            'test "$(find dist -name \'*.sha256\' | wc -l)" -eq 4',
            "(cd dist && sha256sum -c ./*.sha256)",
        ):
            self.assertEqual(lines.count(want), 1, want)
        self.assertEqual([l for l in lines if "||" in l or "&&  true" in l or "; true" in l], [])
        self.assertEqual([l for l in lines if "sha256sum" in l], ["(cd dist && sha256sum -c ./*.sha256)"])

    def test_release_step_guards_and_order(self):
        rel = self.jobs["release"]
        names = [st.get("name") for st in all_steps(rel)]
        self.assertLess(names.index("Verify the artifacts"), names.index("Extract release notes"))
        self.assertLess(names.index("Extract release notes"), names.index("Create the release as a draft, then publish it"))
        step = step_named(rel, "Create the release as a draft, then publish it")
        lines = logical_lines(step["run"])
        strict_shell(lines, "publish step")
        env = rel.get("env") or {}
        self.assertEqual(env.get("SHA"), "${{ needs.prepare.outputs.sha }}")
        self.assertEqual(env.get("TAG"), "${{ needs.prepare.outputs.tag }}")

        # The re-check function.
        start = lines.index("tag_still_at_sha() {")
        end = lines.index("}", start)
        func = lines[start + 1:end]
        self.assertEqual(func.count(LS_REMOTE), 1, "ls-remote must match the exact ref")
        block_after(func, RECHECK_HEADER)
        self.assertLess(func.index(LS_REMOTE), func.index(RECHECK_HEADER))
        top = lines[:start] + lines[end + 1:]

        # No release or draft may exist for the tag already.
        self.assertEqual(top.count(EXISTING_QUERY), 1)
        block_after(top, EXISTING_HEADER)
        # create: one API call whose response carries the id.
        self.assertEqual(top.count(CREATE), 1)
        ci = top.index(CREATE)
        self.assertFalse([l for l in top if l.startswith("gh release")], "no gh release subcommands")
        self.assertEqual([l for l in top if "draft=" in l and l not in (CREATE, PUBLISH)], [])
        self.assertNotIn("--draft", "\n".join(top))
        self.assertNotIn("||", top[ci])
        block_after(top, ID_HEADER)
        self.assertEqual(top.index(ID_HEADER), ci + 1)
        # the first re-check sits after the existence guard and right before create.
        calls = [i for i, l in enumerate(top) if l == "tag_still_at_sha"]
        self.assertEqual(len(calls), 2, "tag_still_at_sha must be called twice")
        first, second = calls
        self.assertLess(top.index(EXISTING_HEADER), first)
        self.assertLess(first, ci)
        self.assertEqual([l for l in top[first:ci] if l.startswith("gh ")], [])
        # uploads go by the captured id, then the re-check, then publish by id.
        self.assertEqual(top.count(UPLOAD_LOOP), 1)
        self.assertEqual(top.count(UPLOAD_IF), 1)
        ul = top.index(UPLOAD_LOOP)
        self.assertEqual(top[ul + 1 : ul + 6], [UPLOAD_IF, UPLOAD_ERR, "exit 1", "fi", "done"])
        self.assertLess(top.index(ID_HEADER), ul)
        self.assertEqual(second, ul + 6, "re-check must directly follow the uploads")
        self.assertEqual(top.count(PUBLISH), 1)
        self.assertEqual(top.index(PUBLISH), second + 1, "publish must follow the re-check directly")
        self.assertEqual(top[-1], PUBLISH)
        self.assertNotIn("release edit", "\n".join(top))
        self.assertNotIn("--draft=false", "\n".join(top))

    def test_guard_steps_cannot_be_switched_off(self):
        def walk(node):
            if isinstance(node, dict):
                for k, v in node.items():
                    yield k
                    yield from walk(v)
            elif isinstance(node, list):
                for v in node:
                    yield from walk(v)

        self.assertNotIn("continue-on-error", list(walk(self.jobs)), "continue-on-error is not allowed")
        for job_name, names in GUARD_STEPS.items():
            for name in names:
                step = step_named(self.jobs[job_name], name)
                self.assertNotIn("if", step, "guard step %r must not have an `if:`" % name)
        for name, job in self.jobs.items():
            for run in step_runs(job):
                for line in logical_lines(run):
                    self.assertNotRegex(line, r"^set\s+\+|^set\s+-o\b|\|\|\s*(true|:)\s*$|\|\|\s*(true|:)\s*[;)]",
                                        "job %s: errexit dropped or masked: %r" % (name, line))
                    if line.startswith("set "):
                        self.assertEqual(line, "set -euo pipefail", name)

    def test_token_never_in_command_arguments(self):
        feed = re.compile(r"printf 'Authorization: Bearer %s\\n' \"\$GH_TOKEN\" \| curl [^|]* -H @- ")
        for name, job in self.jobs.items():
            for run in step_runs(job):
                for line in logical_lines(run):
                    refs = re.findall(r"GH_TOKEN|GITHUB_TOKEN|github\.token", line)
                    if not refs:
                        continue
                    self.assertEqual(len(refs), 1, "job %s: %r" % (name, line))
                    self.assertTrue(feed.search(line), "job %s: token outside `printf | curl -H @-`: %r" % (name, line))
                    self.assertNotRegex(line, r'-H\s+"[^"]*(TOKEN|token)')

    def test_run_shell_is_bash_and_no_trap(self):
        default = ((self.wf.get("defaults") or {}).get("run") or {}).get("shell")
        for name, job in self.jobs.items():
            jdefault = (((job.get("defaults") or {}).get("run")) or {}).get("shell", default)
            for st in all_steps(job):
                if "run" in st:
                    self.assertEqual(st.get("shell", jdefault), "bash", "job %s step %r" % (name, st.get("name")))
                    for line in logical_lines(st["run"]):
                        self.assertNotRegex(line, r"\btrap\b", "job %s: trap is not allowed" % name)

    def test_build_has_no_publish_rights(self):
        for name in ("prepare", "build"):
            self.assertNotIn("gh release", "\n".join(step_runs(self.jobs[name])))

    def test_only_release_job_uses_release_environment(self):
        for name, job in self.jobs.items():
            env = job.get("environment")
            if name == "release":
                self.assertEqual(env, "release", "release job must use environment: release")
            else:
                self.assertIsNone(env, "job %s must not use an environment" % name)

    def test_fork_ci_runs_this_policy_test(self):
        _, ci = load_workflow(FORK_CI_PATH)
        hits = [
            n for n, j in (ci.get("jobs") or {}).items()
            if any("test-fork-release-workflow.py" in r for r in step_runs(j))
        ]
        self.assertTrue(hits, "fork-ci.yml does not run the release policy test")
        for n in hits:
            self.assertEqual(ci["jobs"][n].get("if"), REPO_GUARD)
            self.assertEqual(ci["jobs"][n].get("permissions"), {"contents": "read"})


class ParserParity(unittest.TestCase):
    def test_minimal_parser_matches_pyyaml(self):
        try:
            import yaml  # type: ignore  # noqa: F401
        except ImportError:
            self.skipTest("PyYAML is not installed")
        for path in (WORKFLOW_PATH, FORK_CI_PATH):
            with open(path, encoding="utf-8") as fh:
                text = fh.read()
            self.assertEqual(parse_workflow(text, use_yaml=False), parse_workflow(text, use_yaml=True), path)


def _nth(old, new, n):
    """Replace only the nth (1-based) occurrence of old."""

    def apply(text):
        parts = text.split(old)
        assert len(parts) > n, "mutation anchor %r has fewer than %d occurrences" % (old, n)
        return old.join(parts[:n]) + new + old.join(parts[n:])

    return apply


def _all(old, new, count=None):
    def apply(text):
        assert old in text, "mutation anchor not found: %r" % old
        assert count is None or text.count(old) == count, "%r occurs %d times" % (old, text.count(old))
        return text.replace(old, new)

    return apply


ADMIN_DENY = 'echo "error: $actor must have repository admin permission to publish" >&2\n              exit 1'
REPO_GUARD_LINE = "    needs: prepare\n    if: " + REPO_GUARD + "\n"

MUTATIONS = [
    # r1 mutations plus the cache step.
    ("repo guard OR-ed with true", _nth(REPO_GUARD_LINE, REPO_GUARD_LINE.replace(REPO_GUARD, REPO_GUARD + " || true"), 1)),
    ("toJSON(secrets) in build env", _all("      TAG: ${{ needs.prepare.outputs.tag }}\n      TARGET:", "      ALL: ${{ toJSON(secrets) }}\n      TAG: ${{ needs.prepare.outputs.tag }}\n      TARGET:", 1)),
    ("expression interpolated into run", _nth('BASE="${TAG#v}"', 'BASE="${{ github.event.inputs.tag }}"', 1)),
    ("rust-cache step added", _all("      - name: Install Zig\n", "      - name: Cache\n        uses: Swatinem/rust-cache@23bce251a8cd2ffc3c1075eaa2367cf899916d84 # v2\n\n      - name: Install Zig\n", 1)),
    # r2 review list.
    ("both tag re-check calls deleted", _all("          tag_still_at_sha\n", "")),
    ("pre-create re-check deleted", _nth("          tag_still_at_sha\n", "", 1)),
    ("pre-publish re-check deleted", _nth("          tag_still_at_sha\n", "", 2)),
    ("re-check without exit 1", _all('during the run" >&2\n              exit 1', 'during the run" >&2')),
    ("re-check mismatch ignored", _all('if [ "$current" != "$SHA" ]; then', 'if [ "$current" != "$SHA" ] && false; then')),
    ("admin check without exit 1 (both jobs)", _all(ADMIN_DENY, 'echo "error: $actor must have repository admin permission to publish" >&2', 2)),
    ("admin check without exit 1 (prepare)", _nth(ADMIN_DENY, 'echo "error: $actor must have repository admin permission to publish" >&2', 1)),
    ("admin check without exit 1 (release)", _nth(ADMIN_DENY, 'echo "error: $actor must have repository admin permission to publish" >&2', 2)),
    ("admin loop checks only GITHUB_ACTOR", _all('for actor in "$GITHUB_ACTOR" "$GITHUB_TRIGGERING_ACTOR"; do', 'for actor in "$GITHUB_ACTOR"; do', 2)),
    ("admin test weakened", _all('if [ "$permission" != "admin" ]; then', 'if [ "$permission" = "none" ]; then', 2)),
    ("ancestor guard without exit 1", _all('on origin/integ/next" >&2\n            exit 1', 'on origin/integ/next" >&2')),
    ("ancestor guard && false", _all('refs/remotes/origin/integ/next; then', 'refs/remotes/origin/integ/next && false; then', 1)),
    ("tag regex guard without exit 1", _all("or vX.Y.Z-ev.N-rcM)\" >&2\n            exit 1", "or vX.Y.Z-ev.N-rcM)\" >&2")),
    ("tag regex loosened", _all("-ev\\.[0-9]+(-rc[0-9]+)?$'; then", "-ev\\.[0-9]+(-rc[0-9]+)?'; then")),
    ("sha256sum -c || true", _all("sha256sum -c ./*.sha256)", "sha256sum -c ./*.sha256) || true")),
    ("sha256sum -c || :", _all("sha256sum -c ./*.sha256)", "sha256sum -c ./*.sha256) || :")),
    ("sha256sum check removed", _all("          (cd dist && sha256sum -c ./*.sha256)\n", "")),
    ("sha output points at the tag", _all('echo "sha=$sha" >> "$GITHUB_OUTPUT"', 'echo "sha=$TAG" >> "$GITHUB_OUTPUT"')),
    ("prepare sha output wired to the tag", _all("sha: ${{ steps.resolve.outputs.sha }}", "sha: ${{ steps.tag.outputs.tag }}")),
    ("sha resolved from the tag object, not the commit", _all('"refs/tags/${TAG}^{commit}"', '"refs/tags/${TAG}"')),
    ("build checks out the tag name", _nth("ref: ${{ needs.prepare.outputs.sha }}", "ref: ${{ needs.prepare.outputs.tag }}", 1)),
    ("release checks out the tag name", _nth("ref: ${{ needs.prepare.outputs.sha }}", "ref: ${{ needs.prepare.outputs.tag }}", 2)),
    ("architecture check removed", _all('          file "$bin" | grep -Eq "$FILE_EXPECT"\n', "")),
    ("tarball count check removed", _all("          test \"$(find dist -name '*.tar.gz' | wc -l)\" -eq 4\n", "")),
    ("checksum count check removed", _all("          test \"$(find dist -name '*.sha256' | wc -l)\" -eq 4\n", "")),
    ("release environment removed", _all("    environment: release\n", "")),
    ("build loses needs: prepare", _all("  build:\n    needs: prepare\n", "  build:\n")),
    ("secret in build env", _all("      TAG: ${{ needs.prepare.outputs.tag }}\n      TARGET:", "      GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n      TAG: ${{ needs.prepare.outputs.tag }}\n      TARGET:", 1)),
    ("persist-credentials true", _nth("persist-credentials: false", "persist-credentials: true", 2)),
    # N3 and the nit.
    ("publish by tag name", _all(PUBLISH, 'gh release edit "$TAG" --repo Exploded-views/herdr --draft=false')),
    ("existing-release guard removed", _all('          if [ -n "$existing" ]; then\n            echo "error: a release or draft already exists for $TAG; delete it first" >&2\n            exit 1\n          fi\n', "")),
    ("existing-release guard without exit 1", _all('delete it first" >&2\n            exit 1', 'delete it first" >&2')),
    ("existing-release query removed", _all("select(.tag_name == env.TAG) | .id", "select(.tag_name == \"none\") | .id")),
    ("draft not created as a draft", _all("-F draft=true", "-F draft=false", 1)),
    ("draft flag removed from create", _all("            -F draft=true \\\n", "", 1)),
    ("create without target_commitish", _all('            -f target_commitish="$SHA" \\\n', "", 1)),
    ("target_commitish points at the tag", _all('target_commitish="$SHA"', 'target_commitish="$TAG"')),
    ("create response id not read", _all("--jq .id)\"\n          if [[ ! \"$release_id\"", "--jq .name)\"\n          if [[ ! \"$release_id\"")),
    ("release id not validated", _all("the create call returned no release id for $TAG: '$release_id'\" >&2\n            exit 1", "the create call returned no release id for $TAG: '$release_id'\" >&2")),
    ("release id lookup back to the list", _all('release_id="$(gh api -X POST "repos/$GITHUB_REPOSITORY/releases" \\', 'release_id="$(gh api "repos/$GITHUB_REPOSITORY/releases" \\')),
    ("create by gh release create", _all('release_id="$(gh api -X POST', 'gh release create "$TAG" --draft; release_id="$(gh api -X POST')),
    ("uploads removed", _all("          for f in dist/*.tar.gz dist/*.sha256; do\n", "          for f in; do\n")),
    ("upload failures ignored", _all('              exit 1\n            fi\n          done', '              true\n            fi\n          done')),
    ("token back in curl argv", _all('printf \'Authorization: Bearer %s\\n\' "$GH_TOKEN" | curl --fail-with-body -sS -X POST -H @-', 'curl --fail-with-body -sS -X POST -H "Authorization: Bearer $GH_TOKEN"')),
    ("token passed in argv of another command", _all('echo "draft release $release_id created', 'echo "token $GH_TOKEN" >/dev/null; echo "draft release $release_id created')),
    ("upload error body discarded", _all('echo "error: uploading $f failed: $out" >&2', 'echo "error: uploading $f failed" >&2')),
    ("shell: sh on the publish step", _all("      - name: Create the release as a draft, then publish it\n        shell: bash\n", "      - name: Create the release as a draft, then publish it\n        shell: sh\n")),
    ("default shell removed", _all("defaults:\n  run:\n    shell: bash\n\n", "")),
    ("default shell sh", _all("    shell: bash\n\nconcurrency", "    shell: sh\n\nconcurrency")),
    ("trap exit 0 in the publish step", _all("          tag_still_at_sha() {\n", "          trap 'exit 0' EXIT\n          tag_still_at_sha() {\n")),
    ("trap exit 0 in Verify the artifacts", _nth("          set -euo pipefail\n          find dist", "          set -euo pipefail\n          trap 'exit 0' ERR EXIT\n          find dist", 1)),
    ("uploads go to the tag name", _all("releases/$release_id/assets", "releases/tags/$TAG/assets")),
    ("tag/ref early check removed", _all('          if [ "$GITHUB_REF" != "refs/tags/$tag" ]; then\n            echo "error: run from the tag itself (\'Use workflow from: $tag\'), not from $GITHUB_REF" >&2\n            exit 1\n          fi\n', "")),
    ("tag/ref early check without exit 1", _all("not from $GITHUB_REF\" >&2\n            exit 1", "not from $GITHUB_REF\" >&2")),
    ("tag/ref early check weakened", _all('if [ "$GITHUB_REF" != "refs/tags/$tag" ]; then', 'if [ "$GITHUB_REF" = "refs/heads/none" ]; then')),
    # N7: guard steps switched off by YAML keys or shell options.
    ("continue-on-error on Verify the artifacts", _all("      - name: Verify the artifacts\n", "      - name: Verify the artifacts\n        continue-on-error: true\n", 1)),
    ("if: false on Verify the artifacts", _all("      - name: Verify the artifacts\n", "      - name: Verify the artifacts\n        if: false\n", 1)),
    ("continue-on-error on release admin step", _nth("      - name: Require repository admins\n", "      - name: Require repository admins\n        continue-on-error: true\n", 2)),
    ("if: false on release admin step", _nth("      - name: Require repository admins\n", "      - name: Require repository admins\n        if: false\n", 2)),
    ("continue-on-error on prepare admin step", _nth("      - name: Require repository admins\n", "      - name: Require repository admins\n        continue-on-error: true\n", 1)),
    ("if: false on prepare admin step", _nth("      - name: Require repository admins\n", "      - name: Require repository admins\n        if: false\n", 1)),
    ("if: false on Validate the tag", _all("      - name: Validate the tag\n", "      - name: Validate the tag\n        if: false\n", 1)),
    ("if: false on the publish step", _all("      - name: Create the release as a draft, then publish it\n", "      - name: Create the release as a draft, then publish it\n        if: false\n", 1)),
    ("continue-on-error on the release job", _all("    environment: release\n", "    environment: release\n    continue-on-error: true\n", 1)),
    ("continue-on-error on the build job", _all("    timeout-minutes: 90\n", "    timeout-minutes: 90\n    continue-on-error: true\n", 1)),
    ("release job if: guard && always()", _nth("    if: " + REPO_GUARD + "\n", "    if: " + REPO_GUARD + " && always()\n", 3)),
    ("build job if: guard && always()", _nth("    if: " + REPO_GUARD + "\n", "    if: " + REPO_GUARD + " && always()\n", 2)),
    ("set +e before sha256sum -c", _all("          (cd dist && sha256sum -c", "          set +e\n          (cd dist && sha256sum -c")),
    ("set +o errexit in the publish step", _all("          tag_still_at_sha() {\n", "          set +o errexit\n          tag_still_at_sha() {\n")),
    ("errexit dropped from Verify the artifacts", _nth("          set -euo pipefail\n          find dist", "          set -o pipefail\n          find dist", 1)),
    ("|| true on the admin query", _all('--jq .permission)"', '--jq .permission)" || true', 2)),
    ("ls-remote tail-matches the tag", _all('git ls-remote origin "refs/tags/${TAG}" "refs/tags/${TAG}^{}"', 'git ls-remote origin "${TAG}" "${TAG}^{}"')),
    ("ls-remote takes the last line, not the exact ref", _all("awk -v ref=\"refs/tags/${TAG}\" '$2 == (ref \"^{}\") { peeled = $1 } $2 == ref { plain = $1 } END { print (peeled != \"\" ? peeled : plain) }'", "awk 'END { print $1 }'")),
]


class SelfTest(unittest.TestCase):
    """Every mutation of the workflow text must make ForkReleaseWorkflow fail."""

    def _run(self, text):
        global TEXT_OVERRIDE
        TEXT_OVERRIDE = text
        try:
            suite = unittest.defaultTestLoader.loadTestsFromTestCase(ForkReleaseWorkflow)
            return unittest.TextTestRunner(stream=io.StringIO(), verbosity=0).run(suite)
        finally:
            TEXT_OVERRIDE = None

    def test_unmutated_workflow_passes(self):
        with open(WORKFLOW_PATH, encoding="utf-8") as fh:
            result = self._run(fh.read())
        self.assertTrue(result.wasSuccessful(), result.failures + result.errors)

    def test_every_mutation_is_caught(self):
        with open(WORKFLOW_PATH, encoding="utf-8") as fh:
            base = fh.read()
        survivors = []
        for name, mutate in MUTATIONS:
            mutated = mutate(base)
            self.assertNotEqual(mutated, base, "mutation %r changed nothing" % name)
            parse_workflow(mutated)  # a mutation must still be valid YAML, so the checks decide
            if self._run(mutated).wasSuccessful():
                survivors.append(name)
        self.assertEqual(survivors, [], "mutations the checks accept")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1].endswith(".yml"):
        WORKFLOW_PATH = sys.argv.pop(1)
    unittest.main()
