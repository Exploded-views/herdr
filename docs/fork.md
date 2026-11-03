# This fork: a per-pane input allow-list for herdr

<!-- herdr-fork: fork-only file. Unofficial stopgap; see ../FORK.md. -->

**Exploded-views/herdr** is an unofficial stopgap fork of
[herdrdev/herdr](https://github.com/herdrdev/herdr). It adds one feature: a pane can refuse
input (`agent prompt`, `agent send-keys`, `pane send-text`, `pane send-keys`, `pane run`,
`agent start`, and typing through a TUI client running inside another pane; attach is
excepted, see the known bypasses) unless it comes from a sender
on its allow-list. The allow-list is off by default. A pane can also be set to an input mode,
`human-only`, which refuses input sent through the herdr API and takes client input only from a
person at a terminal outside every pane (see "The input mode" below). The mode is `any`, which
changes nothing, by default.

The official path is upstream discussion
[#5638](https://github.com/herdrdev/herdr/discussions/5638), where the core idea, letting a pane
choose who may send it input, was offered as an idea. herdr does not accept unsolicited pull
requests, and this fork opens none: nothing has been proposed as a pull request. The fork is
dropped the moment upstream ships an equivalent.

Every changed file is listed in [../FORK.md](../FORK.md). Fork CI is in [ci.md](ci.md),
releases in [releases.md](releases.md), and user-visible changes in
[../FORK-CHANGELOG.md](../FORK-CHANGELOG.md).

## Relationship to upstream

The fork is under the same **Apache License 2.0** as upstream; `LICENSE` is unchanged.
Upstream ships no `NOTICE` file, and the fork adds none. Files the fork changes say so in a
`herdr-fork:` note at the top, as the licence's section 4(b) asks.

- `master` mirrors upstream's `master` and carries no fork changes. (Upstream's default
  branch is `master`, so the mirror keeps that name.)
- `integ/next` holds the fork's work. It is the repository's default branch.
- Upstream changes come in by **merge** into `integ/next`, never by rebase or force-push.
- Nothing is ever pushed upstream. The local `upstream` remote's push URL is
  `DISABLED-never-push-upstream`.
- Releases are tagged from `integ/next` as `vX.Y.Z-ev.N` (see [releases.md](releases.md)).
- Upstream's own workflows stay as they are. The fork's checks are in
  `.github/workflows/fork-ci.yml`, whose jobs skip in upstream's repository (see
  [ci.md](ci.md)).
- **Upstream merge checklist.** Upstream workflows arrive *enabled*. After every
  `git merge upstream/master`, run
  `git diff --name-status <previous upstream>..upstream/master -- .github/workflows`. Disable
  (`gh workflow disable`) any new workflow that would act in the fork: one with write
  permissions, a schedule, a `pull_request_target` trigger, or secrets, unless its jobs are
  guarded to herdrdev/herdr. Record it in [ci.md](ci.md#repository-settings). Never edit the
  upstream workflow file.
  Upstream's `release.yml` fires on a fork tag but all its jobs are guarded to herdrdev/herdr
  and skip, and `preview.yml` fires only on `preview-*` tags. Disable both as defence in depth
  once registered, and re-check after each merge that their guards still hold (see
  [releases.md](releases.md#upstreams-workflows)).
- **Pushing `master` (the upstream mirror merge).** Upstream workflows register with GitHub only
  on their first trigger, and some unguarded ones fire on pushes to `master`
  (`label-next-release-issues`, `website-deploy`, `windows-arm64`). So, before any push to
  `master`:
  1. Ask the fork's admin in writing, with the planned `master` SHA.
  2. The admin turns Actions off.
  3. Push `master`.
  4. The admin lists the workflows, disables any newly registered one that would act in the
     fork, and turns Actions back on.
- **The one default-on addition: the input-origin log.**
  Every node records who sent each pane input (the path, the sender's class, the client, the
  host kind and the byte count; never the content) to a local file, mode 0600, capped at about
  2.5 MiB. Every other fork feature is off by default. The owner's decision, verbatim: "keep it
  on" (2026-10-28). `[access] input_log = false` turns it off. `herdr access log` prints it
  (newest first; `--file` reads the files when the server is down). The `access.log` API
  is for a caller outside every pane only; the file itself is not protected from a pane
  agent (see Limits). The file's header names the node by `[access] node_name`
  (default `node`), never the hostname. A burst of typing is one entry per 2 seconds; a
  refusal is always its own entry (written within 50 ms after its handler returns). The server flushes the log before
  it stops or hands off.
- **The input mode (`human-only`).** `herdr access pane set <pane> --input-mode human-only`
  (or `input_mode = "human-only"` in `[access.default]` or `[access.<name>]`) keeps a pane to
  typed input. The `api` path (`agent prompt`, `agent send-keys`, `agent start`,
  `pane send-text`, `pane send-keys`, `pane run`) is refused for every sender, and the `client`
  path (a TUI's typing, mouse reports and image paste, attached or not) is taken only from a
  client classified outside every pane (local, or over SSH under `remote_owner_tui = "allow"`).
  A client in a pane, or one not shown to be outside, gets the same refusal. It refuses rather
  than holds: a held input needs an approval UI, a queue and a rule for who releases it, which
  are new attack surface, and it leaves the sender waiting and tempted to try another route.
  The mode runs after the allow-list, so a request the allow-list refuses keeps the
  allow-list's text. It is independent of the allow-list. The bridge (`bridge_agent` under
  `remote_input = "bridge-only"`) receives by API by design, so its pane stays `any`: the config
  entry is a diagnostic and is ignored, and `access pane set`, `agent start <bridge>` and
  `agent rename` to the bridge's name are refused when they would break that. `flag` and an
  alert file are not in v1.
- Self-update is disabled in fork builds, because `herdr update` would hand the running
  server over to an upstream binary without the allow-list.

### One deviation from the fork standard: FORK-CHANGELOG.md

The fork standard keeps fork changes in `CHANGELOG.md`. herdr's maintainers curate and
rewrite `CHANGELOG.md` at every release, so fork entries there would conflict on every
upstream sync. Fork changes therefore live in [../FORK-CHANGELOG.md](../FORK-CHANGELOG.md),
labelled *upstream candidate* or *fork-only*, and upstream's `CHANGELOG.md` is untouched.
The label describes the change; it is not a plan to send it upstream.

## Getting started

Until a release is published, there is nothing to download.

### What changes on day one

With no `[access]` section in the config, the fork behaves like upstream, with three differences:

- No pane has a policy, so nothing is gated. `remote_input = "any"` and `remote_owner_tui = "allow"`.
- **Update checks and `herdr update` are off.** Fork builds make no background update check and show no in-app update notice. `herdr update` prints that self-update is disabled.
- **The input-origin log is on.** It records who sent each pane input, never the input itself. It goes to `access-input.log` (and `access-input.log.1`) in the herdr data directory, mode 0600, capped at about 2.5 MiB. `herdr access log` prints it. `[access] input_log = false` turns it off.

Access policies are opt-in. Nothing is gated until you set one, either on a pane or in the config:

- `herdr access pane set <pane> --accept-from outside` protects one pane. `herdr access pane get <pane>` shows its policy.
- `herdr access pane set <pane> --input-mode human-only` lets only a person at a TUI outside every pane type into that pane.
- `[access.default]` and `[access.<name>]` tables with `accept_from` bind a default policy when an agent is first detected, so they cover more than one pane at a time.
- `herdr access caller add <name> --target-pane ID --token-file PATH` gives a named sender a token bound to one pane. The pane must already have a policy that lists `caller:<name>`, and only a caller outside every pane may run it. See "Access callers" in the [CLI reference](next/website/src/content/docs/cli-reference.mdx).

Changes to `[access]` are read when the server starts. `herdr server reload-config` does not apply them, so restart the server after any `[access]` edit (and drain first, as below).

Don't set `remote_input = "bridge-only"` until `bridge_agent` names this node's bridge. If it's unset, all API input from another host is refused, and the message names "this node's bridge". Even then, an SSH TUI client still types like an outside client unless you also set `remote_owner_tui = "deny"` (see the Limits).

### Install

Fork releases are GitHub release tarballs on the repository's release page, built for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` (see [releases.md](releases.md#what-gets-published)). There is no Windows build. Each tarball comes with a `.sha256` file.

The fork installs beside upstream and never replaces it. Your upstream `herdr` (`~/.local/bin/herdr`, Homebrew, mise or Nix) is not touched, and a running upstream server keeps running from its own file. Don't copy over a binary a server is running from: on macOS that can kill the server.

1. Download the tarball and its `.sha256` from the release page, over HTTPS, into one directory. Check the tarball:
   - Linux: `sha256sum -c herdr-<tag>-<target>.tar.gz.sha256`
   - macOS: `shasum -a 256 -c herdr-<tag>-<target>.tar.gz.sha256`

   The checksum proves the download is intact. It doesn't prove who built it, because both files come from the same page and the builds are unsigned. Authenticity comes from downloading from this repository's own release page.
2. Install it in its own versioned directory, and point `current` at it:

   ```sh
   (
     set -e   # a failed step stops here and leaves `current` alone
     V=<version>   # the tag without the leading v, e.g. 0.9.5-ev.1
     mkdir -p ~/.local/opt/herdr-fork
     D=~/.local/opt/herdr-fork/$V
     mkdir "$D"   # no -p: refuses if $D exists
     tar -xzf herdr-<tag>-<target>.tar.gz -C "$D"
     # macOS only, and only if a browser download set the flag
     if [ "$(uname)" = Darwin ] && xattr "$D/herdr" 2>/dev/null | grep -q com.apple.quarantine; then
       xattr -d com.apple.quarantine "$D/herdr"
     fi
     cd ~/.local/opt/herdr-fork
     ln -sfn $V current.new
     if [ "$(uname)" = Darwin ]; then mv -h current.new current; else mv -T current.new current; fi
   )
   ```

   If your `mv` lacks `-T`, as with some BusyBox builds, use GNU coreutils. `mkdir` fails if that version is already installed, and the subshell then stops before extracting or repointing `current`. Install each version once, and never extract over a version a server is running.

   On macOS, the block clears the quarantine flag when a browser download set it. A `curl` download doesn't carry the flag, so the block skips that step.
3. Put `~/.local/opt/herdr-fork/current` on `PATH` ahead of the upstream install, in your shell profile, then open a new shell (or run `hash -r`).
4. Check that you're running the fork, before any restart:

   ```sh
   command -v herdr             # a path under ~/.local/opt/herdr-fork
   herdr status client          # the binary: line is also under ~/.local/opt/herdr-fork (Linux shows the resolved version directory)
   herdr access                 # runs locally; see below
   ```

   `herdr access` with no arguments never reaches the server. On the fork it prints the access usage to stderr and exits 2, which is the expected result. On upstream it prints `unknown command: access`, which means `herdr` is still upstream.

   Don't use the protocol number or the version: upstream and the fork print the same ones, and the fork doesn't mark `--version` yet.

To move to a later fork version, install it into its own `~/.local/opt/herdr-fork/<version>` and repoint `current` the same way (the `ln -sfn` and `mv` lines of the block above): write the new symlink under a new name, then `mv` it into place. Never write over a binary in place.

### Switch the running server

A running server keeps the build it started with. The fork's checks apply only after the server restarts, and restarting ends every pane's process:

- Herdr resumes detected agent sessions on restore, unless `[session] resume_agents_on_restore = false`.
- Shells and running commands are lost.

So drain first, then stop every session, not just the default one:

1. `herdr server stop` stops the default session. List the others with `herdr session list` and stop each with `herdr session stop <name>`.
2. Start herdr again with plain `herdr`. (`herdr server` runs a headless server in the foreground, for supervised setups.)
3. Check the server: `herdr status server` shows `status: running`, and `herdr access pane get <pane>` works, because an upstream server rejects it. (`herdr access log` also needs the fork server, but it is refused from inside a pane; run it from outside every pane.) If you can't tell, stop every session again before starting.

A saved SSH machine runs its own herdr server. Install and switch the fork on that machine separately; this install doesn't change it.

Don't use `herdr update` (self-update is disabled in fork builds). Don't use live handoff for the switch: handoff between this upstream and fork pair is untested, and `herdr server live-handoff --import-exe` can swap in a build without the allow-list (see the known bypasses).

### Roll back

Upstream is untouched, so rolling back only changes which `herdr` runs:

1. Drain, then stop every session as above.
2. Remove `~/.local/opt/herdr-fork/current` from `PATH` (or repoint the symlink at another fork version, as above), then open a new shell.
3. Check that `command -v herdr` no longer resolves under `~/.local/opt/herdr-fork` and that `herdr access pane get <pane>` now says unknown command, then start herdr again.

Rollback ends every pane again, like the switch did. Upstream ignores the fork's `[access]` keys, with a warning. The fork's `access-input.log` and `access-callers.json` stay in the data directory. Once upstream saves the session, the per-pane policies are gone, so switching back to the fork starts with no pane policies, except the defaults from the config.

## Limits

At one uid, this is a guardrail against **misrouted input from cooperative agents**, not a
security boundary. Every agent runs as the same user as the herdr server, so the fork can't
stop a process that sets out to get around it:

- Any process can read a caller's token file and send input as that caller, but only to the
  one pane that token is bound to.
- Any process can edit herdr's config and restart the server, or edit the protected agent's
  own files (its settings, hooks and instructions).
- A process inside a pane can escape its pane's ancestry (`setsid`, `nohup`, a double fork,
  `tmux new -d`, any SSH login to the host) and then counts as being outside every pane.
  Callers under an SSH login, TUI clients included, count as another host under `bridge-only`;
  only herdr's `--machine` route is stamped `remote`.
- **Remote client bridge.** The server can't tell at accept that a TUI came in through the
  remote client bridge, so a TUI's remote origin comes from `via_ssh` only. A bridge launched under
  something other than an SSH server looks local. This matches the decision that remote TUIs
  keep typing in v1.
  A TUI client's typing and image paste into a pane are checked against that pane's policy
  with the client's own class. A client that came in over SSH types like an outside client,
  unless `remote_input = "bridge-only"` and `remote_owner_tui = "deny"`, which refuses it as
  another host, on every pane and in the popup. Under bridge-only, an invalid
  `remote_owner_tui` counts as `"deny"`. Focus reports and the key releases sent when a client
  disconnects are not gated.
- **Reused pane pid.** If a pane's shell dies and its pid is reused before herdr processes the
  exit, a descendant of the new process can briefly classify as that pane.
- **Windows.** The caller lineage is unavailable there, so once any policy is bound every
  caller classifies as Unknown and is refused (fail closed). The Windows build is unverified.
- **Orphaned callers look complete.** A process whose parent has exited is re-parented to
  init, and the lineage walk then reports a short, complete chain (observed on macOS and on
  Linux: `complete=true`, two links). Reaching init proves the chain is intact, not that the
  caller was never inside a pane.
- **What the input log can tell.** It tells input sent through the herdr API apart from
  keystrokes from an attached client. It does not tell the owner from an agent driving a TUI
  from outside every pane, local or remote (`expect`, `script`, or a remote `herdr
  --remote`): keystrokes from an outside client count as human. A process that escapes its
  pane's ancestry (`setsid`, `tmux`, and so on) and then attaches counts as an outside client.
- **What the input log does not cover.** Input that reaches a PTY outside herdr (`TIOCSTI`,
  `ptrace`) is not seen or logged. Popup input is logged under `shell.popup_input` and is
  never bound by a pane policy or an input mode. The command herdr itself types to resume an
  agent after a restore is server-made, and is not logged. `agent.start` records the command
  line before shell quoting.
- **Who can read the log.** Only the `access.log` API (and `herdr access log` through it) is
  limited to a caller outside every pane. The file is mode 0600 under the same uid as the
  panes, so any process at that uid, a pane agent included, can read, edit or delete it, and
  `herdr access log --file` reads it without asking the server. Everything it prints is
  escaped, so a planted entry can't send terminal control codes. On Windows the file has no
  0600 or no-follow protection.
- **What the input log is.** A local guardrail record, not an audit trail. Any process at the
  same uid can edit or delete it, and it is not synced to disk, so it survives a crash only up
  to the writer's last write. The writer sleeps with no timer and is woken by the first entry
  after it went idle (the server wakes it once the request or client event has been handled and answered, so the input path pays no system call), then writes: an entry is written within 50 ms after its request handler returns, and the first entry after idle within 100 ms of the input unless the handler itself runs longer; no entry is skipped in either case. So a crash loses at most the entries from the
  last 50 ms (and no more than the 1024 the channel holds) plus the latest burst, which stays open to repeats (up to 2 seconds of typing) and is written when
  the next input arrives, a read or a stop closes it: on a quiet pane it may stay out of the file
  indefinitely, and a crash loses it however old it is. A read with `access log` writes everything first. When the writer falls behind, entries are dropped and a `dropped` marker is written;
  `access log` shows the count.
- **What the input mode can tell.** It tells input sent through the herdr API apart from
  keystrokes at an attached or shell client. It does not tell the owner from an agent driving a
  TUI from outside every pane, local or remote (`expect`, `script`, or a remote `herdr
  --remote`): keystrokes from an outside client count as human. A process that escapes its
  pane's ancestry (`setsid`, `tmux`) and then attaches counts as an outside client, and so as
  human. Input that reaches a PTY outside herdr (`TIOCSTI`, `ptrace`) is not seen, logged or
  moded, and popup input is logged but never moded. A scroll by an attached
  pane client is refused on a `human-only` pane only when it would write into the pane (a wheel
  the child asked for, or a page key it owns). The default `input_mode` in `[access.default]`
  applies to every agent pane whose name has no `input_mode` of its own, whatever its policy. It
  is applied when an agent is detected in the pane, so an admin who sets a pane's mode on
  purpose with `access pane set --input-mode` (even to `any`) is not overridden: `access pane
  get` shows that mark. The mark belongs to the agent that was in the pane when it was set. It is dropped when that agent is released or exits, when a different agent (or the same one under another name) is detected, when the agent is renamed, and when a new agent is started in the pane. A pane marked while empty drops it when the first agent arrives. A restored pane keeps it only if the agent that comes back is the one it was set for, in the same session (the agent process, plus the hook or saved session reference when there is one). A new run of the same kind, for instance after the last one exited unseen, is a different session and drops the mark. An unknown session, such as a mark saved by an older version or a process herdr could not read, drops it too: this fails closed. A detection that blinks, with the same process still running, changes nothing. A client that came in over
  SSH counts as outside only under `remote_owner_tui = "allow"`.
- "The human" can't be identified. A client outside every pane is treated as the human, and
  a daemonized process looks the same.

**Known bypasses in v1.** These are scheduled for v1.1 or left to upstream:

- **Attach.** `herdr agent attach` from inside a pane can still type into a protected pane.
  Terminal attach and the attach client's raw input are not gated by the allow-list. They are
  gated by the input mode: a pane set to `human-only` takes attach input only from an outside
  client.
- **Live handoff.** `herdr server live-handoff --import-exe` can swap in a build without the
  allow-list.
- **Control requests** (close, rename, report-agent on another pane, `server stop`,
  `server reload-config`, `server ssh-agent register`, plugin and custom commands) aren't
  gated. `server stop` is a denial of service, not misrouted input.
- **Reading** another pane's output isn't gated. A read that would send wheel events into a
  full-screen agent (to collect its scrollback) skips that and returns the screen as it is when
  the caller may not send that pane input, and a harvest already running stops the same way if
  a policy is bound mid-read (it still sends the wheel events that undo its own scrolling).
- **Focus.** `pane focus` and `agent focus` can move the TUI's focus to a protected pane
  while you type.

The full design, including what a real boundary would take (the bridge under its own uid),
is kept privately.
