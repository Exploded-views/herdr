# Fork releases

<!-- herdr-fork: fork-only file. Unofficial stopgap; see ../FORK.md. -->

The fork has no releases yet.

- Releases are tagged from `integ/next` as `vX.Y.Z-ev.N`. `X.Y.Z` is the upstream version
  the fork is based on, and `N` counts fork releases on that base, starting at 1
  (`v0.9.5-ev.1`). The suffix keeps fork tags apart from upstream's.
- Fork builds have self-update disabled. To move to a newer fork build, install it in place
  of the old one, then restart the server.
- Upstream's `release.yml`, `preview.yml` and `distribution/` stay untouched and don't
  publish the fork. How fork binaries will be built and published is decided before the
  first fork release.
- Each release's user-visible changes are in [../FORK-CHANGELOG.md](../FORK-CHANGELOG.md).
