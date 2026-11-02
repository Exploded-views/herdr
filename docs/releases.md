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

1. **Disable upstream's `release` and `preview` workflows** (`gh workflow disable`), in the
   admin's Actions-off window, before the first fork tag. A tag push also triggers both of
   them, and they are written for upstream's repository. Leave them disabled; see
   [ci.md](ci.md#repository-settings).
2. The admin tags the commit on `integ/next` and pushes the tag. Only tags matching
   `v*-ev.*` start the workflow; mirrored upstream tags (`v0.9.5`) never do.
3. The `prepare` job rejects any tag that is not `vX.Y.Z-ev.N` or `vX.Y.Z-ev.N-rcM`. Each
   build job then checks out the tag, checks that its base version matches `Cargo.toml`, runs
   `cargo build --locked --release`, and runs `herdr --version` where the runner can run
   the binary.
4. The `release` job, the only one with write access, creates the GitHub release with `gh`
   and the automatic `GITHUB_TOKEN`, with `--verify-tag`. It uses no other secret.

To re-run a release, use *Run workflow* on `fork-release` with the existing tag as the `tag`
input. Delete the failed release first if it was created.

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

A brew formula or an install script that downloads with `curl` does not set the attribute, so
it does not need this step.
