# Fork CI

<!-- herdr-fork: fork-only file. Unofficial stopgap; see ../FORK.md. -->

`.github/workflows/fork-ci.yml` is **fork-only**. Every job carries
`if: github.repository == 'Exploded-views/herdr'`, so it doesn't run in upstream's repository
or in forks of this fork. Upstream's workflows are untouched. Upstream's `ci.yml` runs on
pushes to `master` and `windows` **and on every pull request**, including pull requests into
`integ/next`, so it also runs in the fork once Actions is on (useful Windows coverage, at the
cost of duplicate minutes).

Actions is enabled on the fork, with SHA-pinned actions required and the upstream workflows
below disabled. `fork-ci` and CodeQL run.

Fork CI runs on pushes to `integ/**` and `feat/**`, on pull requests into `integ/**`, and
on manual dispatch. It uses GitHub-hosted runners only, pinned actions (full commit SHAs),
`permissions: {}` at the top with `contents: read` per job, and `persist-credentials: false`.

## Jobs

| Job | Runner | What it runs |
|---|---|---|
| `linux` | `ubuntu-latest` | `scripts/fork/linux-test.sh --fetch`, then `scripts/fork/linux-test.sh just ci`, inside `fork-ci.Dockerfile` |
| `mutants` | `ubuntu-latest`, 4 shards | `scripts/fork/linux-test.sh scripts/fork/run-mutants.sh <shard>/4` in the same image (see "Mutants in CI") |
| `macos` | `macos-latest` | `just ci 'not binary(live_handoff)'` natively, with upstream's macOS filter |
| `windows` | `windows-latest` | `cargo nextest run --locked --no-fail-fast` (tests only), set up as upstream's Windows leg is |

The `windows` job exists because upstream's `check (windows-latest)` stops at the first failing test; this one shows every Windows failure in a single run.

## One image for local and CI Linux runs

`.github/workflows/fork-ci.Dockerfile` mirrors upstream's Linux CI job: Ubuntu 24.04
(pinned by digest), Rust 1.96.1 with rustfmt and clippy, Zig 0.16.0, `just` 1.58.0,
`cargo-nextest` 0.9.148 and Bun 1.3.14. Downloads are checked against pinned sha256 sums.
CI and local runs build and use the same file, so a version bump touches one file.

`scripts/fork/linux-test.sh` runs it:

- **`--fetch`** (network on) builds the image, creates the `herdr-fork-cargo`,
  `herdr-fork-target` and `herdr-fork-zig` volumes and hands them to your uid, fetches
  crates, and runs one build to warm Zig's package cache.
- **Default** (`--network none`, `CARGO_NET_OFFLINE=true`) runs `just ci`, or the command
  you pass. The repo is mounted read-only. Cargo's home, the target dir and both Zig caches
  live in the volumes. Zig 0.16 unpacks dependencies into `vendor/libghostty-vt/zig-pkg` in
  the project, so that directory (gitignored upstream) is a volume too.
- The container runs with `--init`, `--cap-drop ALL` and `no-new-privileges`. If the script
  is killed (for example by a timeout), it stops the container rather than leaving the build
  running.

macOS builds and tests natively on the Mac.

## Mutants in CI

A mutant is a small patch that breaks one security guard (the access gate, caller classification,
the remote stamp, and so on). A named test must fail when the patch is applied; if it doesn't, the
guard is not really tested. The `mutants` job runs them as a matrix of four shards, each on its own
runner, inside the same image as the `linux` job. Nothing needs a secret.

- `scripts/fork/mutants/manifest.tsv` has one line per mutant: `<id>`, TAB, `<patch file>`, TAB,
  `<nextest filter expression>`. The filter names the test or tests that must fail. Lines starting
  with `#` are comments.
- `scripts/fork/mutants/<id>.patch` is a unified diff that applies with `git apply` at the branch
  head. Text before the first `diff --git` line is ignored, so the patch starts with a one-line note.
- `scripts/fork/run-mutants.sh <shard>/<total>` takes every `<total>`-th manifest line starting at
  line `<shard>`. The repo is mounted read-only in the image, so it first copies the tracked tree to a
  writable directory (plus Zig's gitignored `zig-pkg`, which the build needs) and works only there.
  It never touches the checkout.

Before any mutant it builds the clean copy and runs every filter of the shard once; if that fails it
prints `baseline failed` and exits 2, so a test that is already broken cannot read as a kill. A
manifest with no mutants at all also exits 2. Runs use the `mutants` nextest profile
(`.config/nextest.toml`), which terminates a hung test after two minutes; a terminated test counts as
failed, so KILLED.

For each mutant it prints one verdict line, and exits non-zero unless every verdict is `KILLED`:

| Verdict | Meaning | What to do |
|---|---|---|
| `KILLED` | The patch applied, compiled, and a test in the filter failed. | Nothing. |
| `SURVIVED` | It applied and compiled, but every test in the filter passed: no test notices the broken guard. | Fix the test (or the filter), or, if the mutation is equivalent because another guard shadows it, change the mutation so it isn't. |
| `STALE` | `git apply --check` failed: the code the patch edits has moved. | Re-derive the same mutation by hand against the new code and regenerate the patch. |
| `BROKEN` | The patch applied but the tests don't compile with it. | Fix the mutation so it compiles. |
| `ERROR` | The test run failed without a test failing. The runner passes `--no-tests=fail`, so a filter that matches no test (renamed or removed) ends here, as does a crash. | Fix the filter. |

### Add a mutant

1. Make the one-line change that breaks the guard in your working tree, so the named test should
   fail. Keep it to the guard: a mutation that also breaks unrelated tests proves little.
2. Save it as `scripts/fork/mutants/<id>.patch` with `git diff > ...` and add a note line before the
   first `diff --git`: `# herdr-fork: mutant <id>: <what it breaks>. Unofficial stopgap; see FORK.md.`
   Then revert your working tree.
3. Add a line to `manifest.tsv` with the id, the patch file name and a filter such as
   `test(name_of_the_test)`. Join several with ` | ` if any one of several tests should catch it: the mutant counts as KILLED when any of them fails, and every joined test must pass on the unmutated tree (the baseline).
4. Check that it applies with `git apply --check scripts/fork/mutants/<id>.patch`. Shards take lines
   by position, so a new line shifts which shard later mutants run in; that is fine.
5. A mutation that only changes behaviour on one OS needs a test that runs on Linux, because the
   image is Linux. macOS-only code is not covered here.

### Run one shard locally

Like every Linux run, from a full clone and not a worktree, with the image already fetched:

    scripts/fork/linux-test.sh --fetch                                   # once, network on
    scripts/fork/linux-test.sh scripts/fork/run-mutants.sh 1/4           # shard 1 of 4

A shard builds the crate once per mutant, so it takes a while. Take the heavy lock first if you run
this by hand, as for any Linux run. `scripts/fork/test-run-mutants.sh` tests the runner itself with a
stub `cargo` and runs in a second.

## Denylist scan before every push

CI has no copy of the private denylist, so the scan runs locally before each push:

    scripts/fork/denylist-scan.py            # scans HEAD; exit 0 clean, 1 hits, 2 error
    scripts/fork/test-denylist-scan.sh       # the scanner's own tests (synthetic terms only)

It scans the committed tree (file contents, symlink targets and path names), not the working
tree, in the C locale and a UTF-8 locale. It prints paths and counts, never matched text, and
withholds a path whose own name matches. `scripts/fork/denylist-allow.sha256` exempts unmodified
vendored upstream files by sha256: an entry applies only while the file is a regular file with
exactly the pinned bytes, so an upstream sync that changes one is scanned again. Adding an entry
needs the owner's go; never widen the denylist's patterns to make a scan pass.

## Repository settings

These are settings, not file edits: upstream's workflow files stay untouched. In the same
sitting as enabling Actions, and before any push to `master`:

- `gh workflow disable` these upstream workflows, which have no repository guard and would
  act or fail in the fork:
  - `label-next-release-issues` (issue-writing, needs an upstream secret)
  - `website-deploy` (deploy hook)
  - `build-artifacts-manual` (`actions: write`)
  - `windows-arm64` (fires on pushes to `master`)
- Also disable the guarded set, as defence in depth: `pr-gate`, `preview`, `release` and
  `stale-close`.
- Keep or disable `ci.yml` (Windows coverage against minutes), and `nix` and `distribution`
  (path-filtered, read-only). That is the owner's call; record the choice
  here.
- Require SHA-pinned actions, and allow only the pinned set.
- Disabling is keyed by the workflow path, so it survives merges. New upstream workflows
  arrive enabled, and register only on their first trigger, so pushes to `master` follow the
  Actions-off procedure in [fork.md](fork.md).
