# Fork changelog

<!-- herdr-fork: fork-only file. Unofficial stopgap; see docs/fork.md. -->

This lists user-visible changes in Exploded-views/herdr that upstream herdr doesn't have.
Upstream's own changes are in [CHANGELOG.md](CHANGELOG.md), which the fork leaves untouched
(see [docs/fork.md](docs/fork.md#one-deviation-from-the-fork-standard-fork-changelogmd)).

Each entry is labelled *upstream candidate* (could make sense in herdr itself) or
*fork-only* (exists only because this is a fork). The label describes the change; it is not
a plan to send it upstream, and nothing has been proposed upstream.

## Unreleased

- Fork documentation, fork CI, and a Linux build and test image shared by CI and local
  runs. *fork-only*
- Self-update is disabled. *fork-only*
- Every API request now carries who sent it (the socket peer's process lineage, an optional
  caller token, and a remote-origin flag) through to the app. Nothing is gated on it yet. The
  remote API bridge marks its requests as remote, and refuses a first line that is not a JSON
  request object instead of forwarding it. *fork-only*
