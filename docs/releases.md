# Fork releases

<!-- herdr-fork: fork-only file. Unofficial stopgap; see ../FORK.md. -->

- Releases are tagged from `integ/next` as `vX.Y.Z-ev.N`. `X.Y.Z` is the upstream version
  the fork is based on (it must equal `Cargo.toml`'s version), and `N` counts fork releases on
  that base, starting at 1 (`v0.9.5-ev.1`). A release candidate adds `-rcM`
  (`v0.9.5-ev.1-rc1`) and is published as a prerelease. The suffix keeps fork tags apart from
  upstream's.
- Fork builds have self-update disabled. To move to a newer fork build, install it in place
  of the old one, then restart the server.
- Each release's user-visible changes are in [../FORK-CHANGELOG.md](../FORK-CHANGELOG.md). The
  release notes are that file's `## <tag>` section, or "See FORK-CHANGELOG.md" if it has none.

## Cutting a release

`.github/workflows/fork-release.yml` builds and publishes. Nothing else publishes the fork:
upstream's `release.yml`, `preview.yml` and `distribution/` stay untouched.

1. On `integ/next`, move the *Unreleased* entries of `FORK-CHANGELOG.md` under a
   `## <tag>` heading (for example `## v0.9.5-ev.1`). That section becomes the release notes;
   without it the notes say "See FORK-CHANGELOG.md".
2. The admin tags that commit on `integ/next` and pushes the tag. Only tags matching
   `v*-ev.*` start the workflow; mirrored upstream tags (`v0.9.5`) never do.
3. `prepare` checks that the actor (and the triggering actor) have admin permission, rejects any
   tag that is not `vX.Y.Z-ev.N` or `vX.Y.Z-ev.N-rcM`, and resolves the tag to one commit that
   must be an ancestor of `origin/integ/next`. Every later job builds exactly that commit.
4. Each build job checks that the tag's base version matches `Cargo.toml`, runs
   `cargo build --locked --release` from a clean tree (no shared build cache), checks the
   binary's architecture, and runs `herdr --version` (natively, under Rosetta, or under qemu)
   expecting the base version.
5. The `release` job, the only one with write access, runs in the `release` environment (see
   [ci.md](ci.md#repository-settings)), so it waits there for approval once the admin has set a
   required reviewer. It re-checks the admin, re-verifies every `.sha256`, confirms the tag
   still points at the built commit, stops if a release or draft already exists for the tag,
   creates the release as a **draft** with one API call (`target_commitish` set to the built
   commit) and takes the draft's id from that response, uploads everything to that id,
   confirms the tag again, and only then publishes that draft by its id. A failed run leaves at most a draft.

To re-run a release, use *Run workflow* on `fork-release`, choose the **tag itself** under
*Use workflow from* (not `integ/next`), and enter the same tag as the `tag` input. `prepare`
fails at once when the run's ref is not `refs/tags/<tag>`, and the `release` environment's
deployment-ref policy (tags `v*-ev.*` only) would reject any other ref after the full build
anyway. Delete the leftover release or draft first if there is one: the run refuses to start
the release while any release or draft exists for the tag.

### Upstream's workflows

A fork tag also triggers upstream's `release.yml` (its `v*` trigger), but every one of its jobs
is guarded to `herdrdev/herdr` and skips at fork commits. `preview.yml` triggers only on
`preview-*` tags, which fork tags never match. Both are still disabled as defence in depth
(`gh workflow disable`) once they show up in `gh workflow list --all`, which happens after the
first trigger at the latest, and re-checked after each upstream merge in case upstream drops or
changes a guard. Only mirrored old upstream tags (`v0.8.x`, whose `release.yml` is unguarded)
need the Actions-off window; see [fork.md](fork.md).

## What gets published

For each of `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl`:

- `herdr-<tag>-<target>.tar.gz`, containing the `herdr` binary
- `herdr-<tag>-<target>.tar.gz.sha256`, which `shasum -a 256 -c` accepts next to the tarball

Windows is not published. The `x86_64-apple-darwin` and `aarch64-unknown-linux-musl` binaries
are cross-built and not run in CI.

## Signing and macOS quarantine

Binaries are neither signed nor notarized. A macOS download carries a quarantine attribute and
Gatekeeper refuses to run it until it is cleared:

```sh
xattr -d com.apple.quarantine herdr
```

Check the tarball first (`shasum -a 256 -c herdr-<tag>-<target>.tar.gz.sha256`), extract it,
then clear the attribute. A brew *formula* or an install script that downloads with `curl` does
not set the attribute, so it does not need this step. A *cask* does apply quarantine, so the
planned tap should be a formula.
