# Fork CI

<!-- herdr-fork: fork-only file. Unofficial stopgap; see ../FORK.md. -->

`.github/workflows/fork-ci.yml` is **fork-only**. Every job carries
`if: github.repository_owner != 'herdrdev'`, so it wouldn't run in upstream's repository
even if merged there. Upstream's workflows are untouched. Upstream's `ci.yml` runs only on
pushes to `master` and `windows` and on pull requests, so on the fork it covers the
`master` mirror.

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
  live in the volumes.

macOS builds and tests natively on the Mac.
