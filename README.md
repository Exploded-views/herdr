> **Unofficial stopgap fork of herdr** (Exploded-views/herdr). It is adding a per-pane
> input allow-list, so a pane can refuse prompts and keystrokes from other panes. The
> official path is upstream discussion
> [herdrdev/herdr#5638](https://github.com/herdrdev/herdr/discussions/5638); this fork is
> dropped the moment upstream ships an equivalent. It is not affiliated with or endorsed
> by herdr's maintainers. Please don't report fork issues upstream. See
> [docs/fork.md](docs/fork.md) and [FORK.md](FORK.md).

## Divergence from upstream

This is an unofficial fork of [herdr](https://github.com/herdrdev/herdr), and is not affiliated with or endorsed by its maintainers.

**Upstream base:** herdr's `master` branch at `bc8c2898` (2026-10-28), crate version `0.9.5`. Upstream cut its `v0.9.5` release from a separate release branch, so this fork is not "the `v0.9.5` release plus changes". The `master` history already carries equivalents of the six fixes that `v0.9.5` added, and 19 commits after the release-metadata commit `2f3432c6`. Upstream's code is kept except where the fork changes it, and [FORK.md](FORK.md) lists every changed file. That includes upstream's macOS change that starts the server as a launchd job, so quitting the terminal app no longer stops the server. That change is upstream's work, not the fork's.

**What this fork adds:**
- **Per-pane input access control.** Each pane can carry an allow-list of who may send it input (`--accept-from`, `herdr access pane set|get`). Every request that sends input to a pane is checked against the sender's process lineage: `pane send-text`, `send-keys` and `run`, and `agent prompt`, `send-keys` and `start`. The `access.*` admin methods are checked too. Reads and other methods are not gated. Named callers present a token, and a binding id (`acc_…`) targets a pane. Typing from a TUI client is checked too. No input is gated until a policy is set, on a pane or in the config (the `access.*` admin methods are always checked); see [what changes on day one](docs/fork.md#what-changes-on-day-one). At one uid, this is a guardrail against **misrouted input from cooperative agents**, not a security boundary.
- **A human-only input mode.** `herdr access pane set <pane> --input-mode human-only` refuses all API input to that pane and takes typing only from a client outside every pane.
- **An input-origin log.** It records who sent each input to a pane, never what was typed. It is on by default, local and owner-readable only; `herdr access log` prints it, and `[access] input_log = false` turns it off.
- **Self-update and update checks are disabled,** so a fork install never replaces itself with upstream.
- **Reserved agent names.** Agent names starting with `acc_`, and the `[access]` key names `remote_input`, `bridge_agent`, `remote_owner_tui`, `input_log` and `node_name`, are no longer accepted.
- **An end-to-end test suite** for the access controls, mutation tests in CI, a release workflow, and a Linux build image. These change nothing at run time.

The full list, with each change marked *upstream candidate* or *fork-only*, is in [FORK-CHANGELOG.md](FORK-CHANGELOG.md). The known limits and bypasses are in [docs/fork.md](docs/fork.md).

**Versions:** fork releases are tagged `vX.Y.Z-ev.N`. `X.Y.Z` is the upstream version the fork is based on, and `N` counts fork releases on that base, starting at 1. The suffix keeps fork tags apart from upstream's. The first published release will be `v0.9.5-ev.2`, because the `v0.9.5-ev.1` build failed and was never published. Fork binaries are unsigned, and no Windows binary is published.

**Upstream:** herdr does not accept unsolicited pull requests ([CONTRIBUTING.md](https://github.com/herdrdev/herdr/blob/master/CONTRIBUTING.md)). This fork offers ideas upstream only in Discussions, never as pull requests. The core idea, letting a pane choose who may send it input, was offered upstream as an idea in [discussion #5638](https://github.com/herdrdev/herdr/discussions/5638). Nothing has been proposed as a pull request. The fork itself is dropped once upstream ships an equivalent.

# herdr


<p align="center">
  <img src="assets/logo.png" alt="herdr" width="100" />
</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#install">install</a> · <a href="https://herdr.dev/docs/quick-start/">quick start</a> · <a href="https://herdr.dev/docs/">docs</a>
</p>

<p align="center">
  English · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/herdrdev/herdr/releases"><img src="https://img.shields.io/github/downloads/herdrdev/herdr/total?labelColor=333333&color=666666" alt="total GitHub release downloads" /></a>
  <a href="https://github.com/herdrdev/herdr/stargazers"><img src="https://img.shields.io/github/stars/herdrdev/herdr?labelColor=333333&color=666666&logo=github" alt="GitHub stars" /></a>
  <a href="https://github.com/herdrdev/herdr/releases/latest"><img src="https://img.shields.io/github/v/release/herdrdev/herdr?label=release&labelColor=333333&color=666666" alt="latest stable release" /></a>
  <a href="https://formulae.brew.sh/formula/herdr"><img src="https://img.shields.io/homebrew/v/herdr?label=homebrew&labelColor=333333&color=666666" alt="Homebrew version" /></a>
  <a href="https://x.com/herdrdev"><img src="https://img.shields.io/badge/follow-%40herdrdev-000000?logo=x&logoColor=white" alt="follow @herdrdev on X" /></a>
</p>

---

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

**the runtime your coding agents live on.**

- **detach without stopping work** — herdr keeps terminals running in a background server when you close the client or lose your SSH connection. after a server or machine restart, herdr restores the saved layout and can resume supported agent sessions; the original processes do not survive. [session state →](https://herdr.dev/docs/session-state/)
- **several machines, one window** — keep local work and saved ssh machines together, with a combined agent list and independent reconnects. [remote machines →](https://herdr.dev/docs/connecting-machines/)
- **never hunt for the stuck one** — every pane is marked working, blocked, or idle. when an agent stops and needs an answer, herdr says so.
- **agent-native** — agents drive herdr through the cli and socket api: they can spawn panes, prompt each other, and wait until another agent is genuinely blocked. [agent skill →](https://herdr.dev/docs/agent-skill/)
- **runs what you already run** — claude code, codex, cursor, opencode, grok and the rest. herdr doesn't wrap or replace them; it owns their terminals. building an agent? [add herdr support →](https://herdr.dev/docs/add-herdr-support/)
- **keyboard and mouse, both first-class** — tmux-style prefix keys *and* click, drag, split. pick per moment, not per tool.
- **plugins** — extend panes and workflows. [browse the marketplace →](https://herdr.dev/plugins/)
- **one rust binary, no electron** — runs in whatever terminal you already use.

---

## install

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

or `brew install herdr` · `mise use -g herdr` · windows: `powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"` · [endpoint-protected Windows](https://herdr.dev/docs/windows-beta/) · [binaries](https://github.com/herdrdev/herdr/releases)

then start it where the work lives:

```bash
herdr
```

run your agents, split panes, walk away. `ctrl+b q` detaches, `herdr` reattaches. [quick start →](https://herdr.dev/docs/quick-start/)

## docs

everything lives at [herdr.dev/docs](https://herdr.dev/docs/): [quick start](https://herdr.dev/docs/quick-start/) · [concepts](https://herdr.dev/docs/concepts/) · [supported agents](https://herdr.dev/docs/agents/) · [keyboard](https://herdr.dev/docs/keyboard/) · [configuration](https://herdr.dev/docs/configuration/) · [session state](https://herdr.dev/docs/session-state/) · [connecting machines](https://herdr.dev/docs/connecting-machines/) · [remote](https://herdr.dev/docs/persistence-remote/) · [integrations](https://herdr.dev/docs/integrations/) · [add herdr support to your agent](https://herdr.dev/docs/add-herdr-support/) · [plugins](https://herdr.dev/docs/plugins/) · [socket api](https://herdr.dev/docs/socket-api/)

## thanks

every past sponsor and backer is listed in [SPONSORS.md](./SPONSORS.md) — thank you 🐑

enterprise / partnership: hey@herdr.dev

## agent instructions

if you are an ai agent helping with this repository, read [`AGENTS.md`](./AGENTS.md) before making changes and read [`CONTRIBUTING.md`](./CONTRIBUTING.md) before opening issues or PRs.

## development

```bash
git clone https://github.com/herdrdev/herdr
cd herdr
cargo build --release

just test        # unit tests
just check       # formatting, tests, and maintenance checks
```

## license

Herdr is licensed under the [Apache License 2.0](LICENSE).
