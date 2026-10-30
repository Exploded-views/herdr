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

## Build notes

Native macOS builds use the same pinned tools as upstream's CI. Each one sits outside the
system paths and goes on `PATH` only for herdr build and test commands:

- Zig `0.16.0`: the official tarball, sha256-checked, unpacked into its own directory
  (`crates/ghostty-vt/build.rs` runs `zig build`).
- Bun `1.3.14`: the official release zip, sha256-checked against the published
  `SHASUMS256.txt`, kept in its own directory beside any other Bun install (`just ci` runs Bun
  suites).
- `just` `1.58.0` and `cargo-nextest` `0.9.148`, via `cargo install --locked`.
- Rust `1.96.1` from `rust-toolchain.toml`.

Linux builds and tests use `.github/workflows/fork-ci.Dockerfile` (see
[docs/ci.md](docs/ci.md)), the same image fork CI uses.

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
| `scripts/fork/denylist-scan.py` | new | Scans a commit's tree (contents, symlink targets, path names) against the private denylist; prints paths and counts only |
| `scripts/fork/denylist-allow.sha256` | new | Denylist exemptions for unmodified vendored upstream files, each pinned by sha256 |
| `docs/next/website/src/data/config-reference.json` | changed | Documents the fork-only `[access]` keys (Access section) |
| `scripts/config_reference_check.py` | changed | Skips the user-defined `[access.<name>]` map, as it skips `[[keys.command]]` |
| `scripts/fork/test-denylist-scan.sh` | new | Tests the scanner: pinned exemptions lapse on any change, and every error fails rather than passing clean |
| `src/update.rs` | changed | Self-update and background update checks disabled in fork builds (`FORK_BUILD`) |
| `src/api/schema/operations.rs` | changed | `AccessClass` enum and `Operation.access`; the operation macro requires a class on every row |
| `src/api/schema.rs` | changed | Access class on every row of the operation table; re-exports `AccessClass` |
| `src/platform/mod.rs` | changed | `ProcessIdentity`, `PeerLineage`, `LinkInfo` and the ssh-server name set; lineage entry points |
| `src/platform/unix_common.rs` | changed | The peer lineage walk, `random_secret` |
| `src/platform/macos.rs` | changed | `p_uniqueid` link info (proc_pidinfo flavors 13 and 17, readable on root-owned processes) and link check |
| `src/platform/linux.rs` | changed | `/proc` ppid and start-time link info and link check |
| `src/platform/fallback.rs` | changed | Lineage stubs |
| `src/access/mod.rs`, `src/access/policy.rs`, `src/access/caller.rs` | new | Pure access model: policy types, `decide`, caller classification (no wiring yet) |
| `src/main.rs` | changed | Declares `mod access` |
| `src/access/caller.rs` | changed | Adds `Caller` (peer lineage, optional token, remote flag) that rides on every api request |
| `src/api/mod.rs`, `src/api/server.rs`, `src/api/wait.rs`, `src/api/subscriptions.rs` | changed | Every `ApiRequestMessage` carries a `Caller`; the connection builds it from the socket peer and the request envelope (`caller_token`, `caller_origin`), and sub-requests inherit it |
| `src/remote.rs` | changed | The remote api bridge stamps `caller_origin: "remote"` on the first request line and refuses (fails closed) when that line is not a JSON object |
| `src/cli/target.rs`, `src/server/headless/tests/*.rs` | changed | Tests only: `--machine` refuses access commands; test constructors carry a `Caller` |
| `src/terminal/state.rs`, `src/persist/snapshot.rs`, `src/persist/restore.rs` | changed | Per-pane `access` policy on terminal state, saved in snapshots (`PaneSnapshot.access`) and restored in every restore branch |
| `src/config.rs`, `src/config/io.rs`, `src/config/model.rs` | changed | `[access]` config: `default` and named `accept_from` sections plus `remote_input`, `bridge_agent`, `remote_owner_tui`; read once at startup |
| `src/app/access.rs`, `src/app/access_tests.rs` | new | `bind_access`, `unbind_access`, sticky `bind_default_access`; their tests; `start_baseline_access` and `bind_access_for_start` for `agent start` |
| `src/app/mod.rs`, `src/app/state.rs`, `src/app/actions.rs`, `src/access/policy.rs` | changed | Startup-resolved `access_defaults` and remote settings on `AppState`; default bound on first agent detection and after restore; serde for policy types |
| `src/app/agents.rs`, `src/app/api/agents.rs`, `src/api/schema/agents.rs`, `src/api/schema/tests.rs`, `src/cli/agent.rs`, `src/cli/agent/bindings.rs`, `src/platform/macos/input_tests.rs`, `docs/next/website/src/content/docs/cli-reference.mdx` | changed | `agent start --accept-from` / `AgentStartParams.accept_from`: binds, keeps or opens the pane's policy as the agent starts (restored if the send fails); `invalid_accept_from`; tests; CLI reference |
| `docs/next/api/herdr-api.schema.json` | changed *(generated)* | Schema gains `accept_from` on `agent.start` |
| `src/app/access_gate.rs` | new | `check_access`: classifies the caller and calls `decide` for every Input-class method; exhaustive `gate_target`; an `agent.start` whose `accept_from` is looser than the pane's baseline is `AccessAdmin`; the `access_gate_profile` budget benchmark |
| `src/server/headless.rs`, `src/server/alt_screen_read.rs`, `src/server/headless/endpoint_requests.rs`, `src/server/clients.rs`, `src/server/client_transport.rs` | changed | The gate runs right after the shutdown check and again right before dispatch (after queued events are applied); a refusal is `access_denied` and writes no bytes; reads skip the alt-screen wheel harvest when Input would be refused; each client connection keeps the caller captured from its socket peer at accept, and shell-endpoint requests use it |
| `src/server/headless/tests/access_gate.rs`, `src/server/headless/tests/mod.rs`, `src/server/headless/tests/surface_interest.rs` | new / changed | Gate tests for every Input method, legacy and moved pane ids, bridge-only, restore, alt-screen reads and client callers |
| `src/platform/mod.rs`, `src/platform/unix_common.rs`, `src/pane.rs`, `src/terminal/runtime.rs`, `src/terminal/runtime_registry.rs` | changed | `process_identity` (a pane shell's identity with the same token the lineage walk records); runtime registry iteration on every platform; test-only child pid setter |
| `src/access/mod.rs`, `src/api/mod.rs`, `src/api/schema.rs`, `src/api/schema/operations.rs`, `src/config/model.rs`, `src/app/mod.rs`, `src/app/access_tests.rs`, `justfile` | changed | Temporary dead-code allows removed now that the gate uses them; `[access] input_log` key parsed (default true, not used yet); `just bench-access` |

## Limits

At one uid, the allow-list is a guardrail against **misrouted input from cooperative
agents**, not a security boundary. [docs/fork.md](docs/fork.md#limits) lists what it can't
stop.

An orphaned caller (parent exited, re-parented to init) gets a short lineage that still reports `complete`; see [docs/fork.md](docs/fork.md#limits).
