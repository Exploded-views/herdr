# Fork CI

<!-- herdr-fork: fork-only file. Unofficial stopgap; see ../FORK.md. -->

`.github/workflows/fork-ci.yml` is **fork-only**. Every job carries
`if: github.repository == 'Exploded-views/herdr'`, so it doesn't run in upstream's repository
or in forks of this fork. Upstream's workflows are untouched. Upstream's `ci.yml` runs on
pushes to `master` and `windows` **and on every pull request**, including pull requests into
`integ/next`, so it also runs in the fork once Actions is on (useful Windows coverage, at the
cost of duplicate minutes).

**Actions is currently disabled on the fork**, so nothing runs yet. Before enabling it, apply
the repository settings below.

Fork CI runs on pushes to `integ/**` and `feat/**`, on pull requests into `integ/**`, and
on manual dispatch. It uses GitHub-hosted runners only, pinned actions (full commit SHAs),
`permissions: {}` at the top with `contents: read` per job, and `persist-credentials: false`.

## Jobs

| Job | Runner | What it runs |
|---|---|---|
| `linux` | `ubuntu-latest` | `scripts/fork/linux-test.sh --fetch`, then `scripts/fork/linux-test.sh just ci`, inside `fork-ci.Dockerfile` |
| `macos` | `macos-latest` | `just ci 'not binary(live_handoff)'` natively, with upstream's macOS filter |

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

## Repository settings

These are settings, not file edits: upstream's workflow files stay untouched. In the same
sitting as enabling Actions, and before any push to `master`:

- `gh workflow disable` these upstream workflows, which have no repository guard and would
  act or fail in the fork:
  - `label-next-release-issues` (issue-writing, needs an upstream secret)
  - `website-deploy` (deploy hook)
  - `build-artifacts-manual` (`actions: write`)
- Also disable the guarded set, as defence in depth: `pr-gate`, `preview`, `release` and
  `stale-close`.
- Keep or disable `ci.yml` (Windows coverage against minutes), and `nix`, `distribution` and
  `windows-arm64` (path-filtered, read-only). That is the owner's call; record the choice
  here.
- Require SHA-pinned actions, and allow only the pinned set.
- Disabling is keyed by the workflow path, so it survives merges. New upstream workflows
  arrive enabled, so follow the merge checklist in [fork.md](fork.md).
