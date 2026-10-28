# FORK.md: what this fork changes

<!-- herdr-fork: fork-only file. Unofficial stopgap; see docs/fork.md. -->

This is **Exploded-views/herdr**, an unofficial stopgap fork of
[herdrdev/herdr](https://github.com/herdrdev/herdr). It exists to add a per-pane input
allow-list until upstream ships an equivalent. The official path is upstream discussion
[#5638](https://github.com/herdrdev/herdr/discussions/5638). The fork is dropped the moment
upstream has an equivalent.

How the fork relates to upstream (licence, branches, syncing, CI, releases) is in
[docs/fork.md](docs/fork.md). Each user-visible change is in
[FORK-CHANGELOG.md](FORK-CHANGELOG.md).

## Authorship

This fork is built by AI coding agents (Claude) under its owner's supervision. Its commits carry
a `Co-Authored-By` trailer. herdr's AGENTS.md rule against AI co-author lines governs
contributions to upstream; this fork sends none.

## Changed files

Every file the fork adds or changes, with the reason. Files the fork changes carry a
`herdr-fork:` note at the top. Generated files can't carry one and are marked *(generated)*.

| Path | Kind | Why |
|---|---|---|
| `README.md` | changed | Banner: unofficial stopgap fork, link to #5638 |
| `.gitignore` | changed | Lets `docs/fork.md`, `docs/ci.md` and `docs/releases.md` be tracked (upstream ignores `/docs/*`) |
| `FORK.md` | new | This list |
| `FORK-CHANGELOG.md` | new | Fork changes, labelled *upstream candidate* or *fork-only* |
| `docs/fork.md`, `docs/ci.md`, `docs/releases.md` | new | Relationship to upstream, fork CI, fork releases |
| `.github/workflows/fork-ci.yml` | new | Fork CI; every job skips in upstream's repository |
| `.github/workflows/fork-ci.Dockerfile` | new | Linux build and test image, shared by fork CI and local runs |
| `scripts/fork/linux-test.sh` | new | Runs the Linux image locally: repo read-only, offline after `--fetch` |

## Limits

At one uid, the allow-list is a guardrail against **misrouted input from cooperative
agents**, not a security boundary. [docs/fork.md](docs/fork.md#limits) lists what it can't
stop.
