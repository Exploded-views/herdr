# Fork changelog

<!-- herdr-fork: fork-only file. Unofficial stopgap; see docs/fork.md. -->

This lists user-visible changes in Exploded-views/herdr that upstream herdr doesn't have.
Upstream's own changes are in [CHANGELOG.md](CHANGELOG.md), which the fork leaves untouched
(see [docs/fork.md](docs/fork.md#one-deviation-from-the-fork-standard-fork-changelogmd)).

Each entry is labelled *upstream candidate* (could make sense in herdr itself) or
*fork-only* (exists only because this is a fork). The label describes the change; it is not
a plan to send it upstream.

## Divergence from upstream

This is an unofficial fork of [herdr](https://github.com/herdrdev/herdr), and is not affiliated with or endorsed by its maintainers.

**Upstream base:** herdr's `master` branch at `bc8c2898` (2026-10-28), crate version `0.9.5`. Upstream cut its `v0.9.5` release from a separate release branch, so this fork is not "the `v0.9.5` release plus changes". The `master` history already carries equivalents of the six fixes that `v0.9.5` added, and 19 commits after the release-metadata commit `2f3432c6`. Upstream's code is kept except where the fork changes it, and [FORK.md](FORK.md) lists every changed file, and CI checks that against the upstream base. That includes upstream's macOS change that starts the server as a launchd job, so quitting the terminal app no longer stops the server. That change is upstream's work, not the fork's.

**What this fork adds:**
- **Per-pane input access control.** Each pane can carry an allow-list of who may send it input (`--accept-from`, `herdr access pane set|get`). Every request that sends input to a pane is checked against the sender's process lineage: `pane send-text`, `send-keys` and `run`, and `agent prompt`, `send-keys` and `start`. The `access.*` admin methods are checked too. Reads and other methods are not gated. Named callers present a token, and a binding id (`acc_…`) targets a pane. Typing from a TUI client is checked too. No input is gated until a policy is set, on a pane or in the config (the `access.*` admin methods are always checked); see [what changes on day one](docs/fork.md#what-changes-on-day-one). At one uid, this is a guardrail against **misrouted input from cooperative agents**, not a security boundary.
- **A human-only input mode.** `herdr access pane set <pane> --input-mode human-only` refuses all API input to that pane and takes typing only from a client outside every pane.
- **An input-origin log.** It records who sent each input to a pane, never what was typed. It is on by default, local and owner-readable only; `herdr access log` prints it, and `[access] input_log = false` turns it off.
- **Self-update and update checks are disabled,** so a fork install never replaces itself with upstream.
- **Reserved agent names.** Agent names starting with `acc_`, and the `[access]` key names `remote_input`, `bridge_agent`, `remote_owner_tui`, `input_log` and `node_name`, are no longer accepted.
- **An end-to-end test suite** for the access controls, mutation tests in CI, a release workflow, and a Linux build image. These change nothing at run time.

The full list, with each change marked *upstream candidate* or *fork-only*, is below. The known limits and bypasses are in [docs/fork.md](docs/fork.md).

**Versions:** fork releases are tagged `vX.Y.Z-ev.N`. `X.Y.Z` is the upstream version the fork is based on, and `N` counts fork releases on that base, starting at 1. The suffix keeps fork tags apart from upstream's. The first published release will be `v0.9.5-ev.2`, because the `v0.9.5-ev.1` build failed and was never published. Fork binaries are unsigned, and no Windows binary is published.

**Upstream:** herdr does not accept unsolicited pull requests ([CONTRIBUTING.md](https://github.com/herdrdev/herdr/blob/master/CONTRIBUTING.md)). This fork offers ideas upstream only in Discussions, never as pull requests. The core idea, letting a pane choose who may send it input, was offered upstream as an idea in [discussion #5638](https://github.com/herdrdev/herdr/discussions/5638). Nothing has been proposed as a pull request. The fork itself is dropped once upstream ships an equivalent.

## Unreleased

- Fork CI checks that FORK.md names every upstream file the fork modifies, against the pinned upstream
  base. *fork-only*
- Fork documentation, fork CI, and a Linux build and test image shared by CI and local
  runs. *fork-only*
- Self-update is disabled. *fork-only*
- A release workflow builds and publishes fork binaries (macOS and Linux, with checksums,
  unsigned) when the admin pushes a `vX.Y.Z-ev.N` tag. *fork-only*
- Every API request now carries who sent it (the socket peer's process lineage, an optional
  caller token, and a remote-origin flag) through to the app. Nothing is gated on it yet. The
  remote API bridge marks its requests as remote, and refuses a first line that is not a JSON
  request object instead of forwarding it. *fork-only*
- New `[access]` config section: `[access.default]` and `[access.<name>]` tables with
  `accept_from`, plus `remote_input`, `bridge_agent` and `remote_owner_tui`. A pane's access
  policy is saved with the session, and the default binds to a pane when an agent is first
  detected in it and stays after the agent exits. Read once at startup; `server.reload_config`
  does not change it. Nothing is enforced yet. *fork-only*
- Per-pane access is now enforced. Every method that types into a pane (`pane send-text`,
  `pane send-keys`, `pane send-input`, `agent prompt`, `agent send-keys`, `agent start`) is
  checked against the target pane's policy. A refused request fails with the error code
  `access_denied` and a message that names the pane and who was refused, and nothing is
  written to the pane. With no policy, no caller token and `remote_input = "any"`, the check
  does no work and behaviour is as before; `remote_input = "bridge-only"` turns it on for
  every pane. A read that would send wheel events into a full-screen agent returns the screen
  as it is when the caller may not send that pane input, and a harvest already running stops
  if a policy is bound mid-read. The check is made again right before a request runs, after
  queued events (such as agent detection) have been applied. The `[access]` key `input_log` is
  accepted (it has no effect yet). *fork-only*
- `agent start` takes `--accept-from` (`accept_from` in the API): `any`, or a list of `none`,
  `outside` and `caller:<name>`. It binds the pane's policy when the agent starts. Without it, a
  pane that has a policy keeps it (same binding), a pane opened on purpose stays open, and any
  other pane gets the `[access.<name>]` section for the agent's name, else `[access.default]`.
  `--accept-from any` opens the pane on purpose. A start that would let in more than the pane
  accepts now (or would get from its section or default) is an admin request: only a caller
  outside every pane may make it. A malformed value fails with `invalid_accept_from` and starts
  nothing. *fork-only*
- Named access callers. `herdr access caller add <name> --target-pane ID --token-file PATH`
  registers a caller bound to a pane that already has an access policy, and writes its secret
  token to the file (mode 0600, never printed; an existing file is refused). A request that
  presents the token may send input only to that pane, and only if the pane's policy lists
  `caller:<name>`. `list`, `rotate` (a new token; the old one stops working), `retarget` and
  `remove` complete the set, and the API has matching `access.caller.*` methods. Only a caller
  outside every pane may use them. Only a sha256 of each token is stored, in `access-callers.json`
  (mode 0600) in the herdr data directory. The agent names `remote_input`, `bridge_agent` and
  `remote_owner_tui` are no longer accepted. *fork-only*
- `herdr access pane set <pane> --accept-from LIST` and `herdr access pane get <pane>` (API
  `access.pane.set` and `access.pane.get`) change and read a pane's access policy. `set` is for
  a caller outside every pane only, and `any` removes the policy and opens the pane on purpose.
  *fork-only*
- A caller presents its token with `HERDR_CALLER_TOKEN_FILE` or a leading
  `herdr --caller-token-file PATH`. The file is read for the local server only: with
  `--machine` the flag is a usage error and the variable is ignored. An unreadable token file
  stops the command with exit status 2. *fork-only*
- An agent target of the form `acc_<32 hex>` is a pane's binding id: it reaches the pane that
  holds that binding, and a stale or malformed one is `agent_not_found` instead of falling back
  to a name or position. Agent names starting with `acc_` are no longer accepted. *fork-only*
- The caller registry file must not be writable by group or others; otherwise it is refused at
  startup (callers are then empty and changes are refused) and the log says to `chmod 600` it.
  The registry's directory is synced after each save. *fork-only*
- Caller token files stay out of child environments: panes, the server daemon and the handoff child no longer inherit `HERDR_CALLER_TOKEN_FILE`; an `acc_` target never matches a name in the terminal resolver and resolves only to an agent holder; a caller registry owned by another user is refused; an unreadable token file exits 2 through the normal CLI exit path. *fork-only*
- Binding ids and caller tokens can be minted on Windows (`BCryptGenRandom`); the Japanese and Chinese CLI references document the access commands and `agent start --accept-from`; a test checks that no caller token or token hash reaches the log, and `AccessBindingId` no longer prints its id through `Debug`. *fork-only*
- A TUI client's typing and image paste into a pane are now checked against the pane's policy,
  using the client's own caller class. Under `remote_input = "bridge-only"`, `remote_owner_tui =
  "deny"` refuses a client that came in over SSH, an invalid `remote_owner_tui` counts as `"deny"`,
  and a remote refusal names the bridge's binding id. Terminal attach stays ungated. A refusal shows once as "Input refused". *fork-only*
- Input-origin log, on by default. Every pane input is recorded with its origin: the path
  (`api` or `client`), the route, the sender's class (pane, outside, outside over SSH, remote,
  caller or unknown) with the pane or caller name, the client, the host kind (`local`, `ssh`
  or `remote-stamp`), the byte count and whether it was allowed. It never records the input
  itself. The file is `access-input.log` in the herdr data directory (mode 0600, rotated at
  1 MiB with one old file, so at most 2 MiB), headed by the `[access] node_name` (default
  `node`), never the hostname. A burst of typing is one entry per 2 seconds; a refusal is
  always its own entry. An entry is written within 50 ms after its request handler returns, and the first entry after idle within 100 ms of the input unless the handler itself runs longer; no entry is skipped in either case. The latest burst of typing when the next input, a read or a stop closes it. The server writes what it has before it stops
  or hands off. `herdr access log [--limit N] [--json]` (API `access.log`) prints the newest
  entries first, with the number dropped if the writer fell behind; `--file` reads the files
  when the server is down. The `access.log` API is for a caller outside every pane only; the file itself can be read by any process at the same uid. `[access] input_log =
  false` turns it off and restores the old behaviour exactly: nothing is classified for Input
  requests. The names `input_log` and `node_name` are no longer accepted for agents.
  *fork-only*
- A pane can be set to an input mode, `human-only`, with `herdr access pane set <pane>
  --input-mode human-only` (or `input_mode = "human-only"` in `[access.default]` or
  `[access.<name>]`). Input sent through the herdr API (`agent prompt`, `agent send-keys`,
  `agent start`, `pane send-text`, `pane send-keys`, `pane run`) is then refused for every
  sender, and client input (typing, mouse reports and image paste, in a TUI or an attached
  terminal) is taken only from a client outside every pane. The refusal is immediate and the
  text says the pane accepts only typed input. The mode is checked after the pane's allow-list,
  is saved with the session, and shows in `access pane get`. The default is `any`, which
  changes nothing. The bridge's pane stays `any`. `flag` and an alert file are not in v1.
  An admin who sets a pane's mode with `--input-mode` (even to `any`) marks it as chosen on
  purpose: `access pane get` shows `explicit`, the sticky default leaves it alone, and the mark
  belongs to the agent it was set for: it is dropped when that agent exits, is renamed or is
  replaced by another, and when a new `agent start` takes the pane. A pane marked while empty
  drops it when the first agent arrives; a restored pane keeps it only for the same agent session (a new run of the same kind, or an unknown session, drops it). The `[access.default]` mode applies to every agent pane
  whatever its policy; an `[access.<name>]` section with its own `input_mode` (even `any`)
  overrides it, and is applied when the pane is started, renamed or detected under that name.
  A mode in the config that cannot be read counts as `human-only`, with a diagnostic that names
  the key and the line. A start under the bridge's name into a pane with a mode is refused with
  the code `access_bridge_input_mode`. *upstream candidate*
