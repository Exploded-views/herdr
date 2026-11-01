// herdr-fork: bind, unbind and default-bind per-pane access policies and input modes; the bridge keeps mode `any`. Unofficial stopgap; see FORK.md.
//! Policy state only. Nothing here gates a request; the server gate reads
//! `TerminalState::access` in a later task.

use std::io;

use super::state::AppState;
use crate::access::policy::AccessDefaults;
use crate::access::policy::{AcceptFrom, AccessBindingId, AccessPolicy, Principal};
use crate::terminal::{TerminalId, TerminalState};

impl AppState {
    /// Set a pane's policy. An existing binding id is kept (a rebind changes
    /// who is accepted, not which binding the pane is).
    pub(crate) fn bind_access(
        &mut self,
        terminal: &TerminalId,
        accept_from: Vec<Principal>,
    ) -> io::Result<AccessBindingId> {
        let existing = self
            .terminals
            .get(terminal)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such terminal"))?
            .access
            .as_ref()
            .map(|policy| policy.binding.clone());
        let binding = match existing {
            Some(binding) => binding,
            None => AccessBindingId::mint()?,
        };
        if let Some(state) = self.terminals.get_mut(terminal) {
            state.access = Some(AccessPolicy {
                binding: binding.clone(),
                accept_from,
            });
            state.access_opened_explicitly = false;
        }
        self.mark_session_dirty();
        Ok(binding)
    }

    /// Open a pane on purpose. The pane is remembered as opened, so the
    /// sticky default does not bind it again on the next detection or restart.
    pub(crate) fn unbind_access(&mut self, terminal: &TerminalId) {
        if let Some(state) = self.terminals.get_mut(terminal) {
            state.access = None;
            state.access_opened_explicitly = true;
            self.mark_session_dirty();
        }
    }
}

impl AppState {
    /// The `accept_from` a start into `terminal` is measured against: the
    /// pane's bound policy; for an unbound pane that was not opened on
    /// purpose, what its name's section or the default would bind; for a pane
    /// opened on purpose, `None` (it is open).
    pub(crate) fn start_baseline_access(
        &self,
        terminal: &TerminalId,
        name: &str,
    ) -> Option<&[Principal]> {
        let state = self.terminals.get(terminal)?;
        if let Some(policy) = state.access.as_ref() {
            return Some(&policy.accept_from);
        }
        if state.access_opened_explicitly {
            return None;
        }
        self.access_defaults
            .by_name
            .get(name)
            .or(self.access_defaults.default.as_ref())
            .map(Vec::as_slice)
    }

    /// Settle a pane's policy for `agent start`. An explicit `accept_from`
    /// binds (or, for `any`, opens the pane on purpose). Without one, a pane
    /// that holds a policy keeps it, a pane opened on purpose stays open, and
    /// any other pane gets its name's section or the default.
    pub(crate) fn bind_access_for_start(
        &mut self,
        terminal: &TerminalId,
        name: &str,
        explicit: Option<AcceptFrom>,
    ) -> io::Result<()> {
        match explicit {
            Some(AcceptFrom::Any) => self.unbind_access(terminal),
            Some(AcceptFrom::Only(list)) => {
                self.bind_access(terminal, list)?;
            }
            None => {
                let Some(state) = self.terminals.get(terminal) else {
                    return Ok(());
                };
                // Only the policy step is skipped for a pane that has settled its policy;
                // the named input mode below is independent of it.
                if state.access.is_none() && !state.access_opened_explicitly {
                    if let Some(list) = self.start_baseline_access(terminal, name) {
                        let list = list.to_vec();
                        self.bind_access(terminal, list)?;
                    }
                }
            }
        }
        // `[access.<name>] input_mode` is bound at the start, unless the pane has a mode
        // already. The bridge never gets one (its config entry was dropped at load).
        if let Some(mode) = self.access_defaults.mode_by_name.get(name).copied() {
            if !self.is_bridge_name(name) {
                if let Some(state) = self.terminals.get_mut(terminal) {
                    if state.input_mode.is_any() && !mode.is_any() {
                        state.input_mode = mode;
                        self.mark_session_dirty();
                    }
                }
            }
        }
        Ok(())
    }

    /// Terminals named like the bridge that hold an input mode: a bridge that only takes
    /// typed input receives nothing from the other host. A config reload or a restore can
    /// leave one; the mode is never changed silently, so this is reported.
    pub(crate) fn bridge_terminals_with_a_mode(&self) -> Vec<TerminalId> {
        let Some(bridge) = self.remote_settings.bridge_if_bridge_only() else {
            return Vec::new();
        };
        self.terminals
            .iter()
            .filter(|(_, terminal)| {
                terminal.agent_name.as_deref() == Some(bridge) && !terminal.input_mode.is_any()
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Is `name` the bridge agent while remote input is bridge-only? The bridge receives
    /// by API by design, so its pane's input mode must stay `any`.
    pub(crate) fn is_bridge_name(&self, name: &str) -> bool {
        self.remote_settings.bridge_if_bridge_only() == Some(name)
    }
}

/// The refusal for a non-`any` mode on the bridge.
pub(crate) fn bridge_mode_message(bridge: &str) -> String {
    crate::access::policy::bridge_mode_text(bridge)
}

/// Bind `[access.default]` to a terminal that is an agent, holds no policy
/// and was not opened on purpose. Returns true when it bound. One `Option`
/// check when no default is configured.
///
/// The default `input_mode` is independent of the policy: it applies to every agent
/// terminal whose mode is `any`, unless an admin set the mode on purpose
/// (`input_mode_set_explicitly`), the agent's name has an `[access.<name>] input_mode` of
/// its own, or the terminal is the bridge, whose mode stays `any`.
pub(crate) fn apply_default_access(
    defaults: &AccessDefaults,
    bridge: Option<&str>,
    terminal: &mut TerminalState,
) -> bool {
    if defaults.default.is_none() && defaults.default_mode.is_any() {
        return false;
    }
    if !terminal.is_agent_terminal() {
        return false;
    }
    let mut bound = false;
    if let Some(default) = defaults.default.as_ref() {
        if terminal.access.is_none() && !terminal.access_opened_explicitly {
            match AccessBindingId::mint() {
                Ok(binding) => {
                    terminal.access = Some(AccessPolicy {
                        binding,
                        accept_from: default.clone(),
                    });
                    bound = true;
                }
                Err(err) => {
                    tracing::error!(%err, "could not mint an access binding; pane left open");
                    return false;
                }
            }
        }
    }
    let name = terminal.agent_name.as_deref();
    let on_bridge = bridge.is_some_and(|bridge| name == Some(bridge));
    let named = name.is_some_and(|name| defaults.mode_by_name.contains_key(name));
    if !defaults.default_mode.is_any()
        && terminal.input_mode.is_any()
        && !terminal.input_mode_set_explicitly
        && !on_bridge
        && !named
    {
        terminal.input_mode = defaults.default_mode;
        bound = true;
    }
    bound
}

impl AppState {
    /// Sticky default: bind `[access.default]` to a pane the first time an
    /// agent is seen in it. A no-op, and free, when no default is configured.
    pub(crate) fn bind_default_access(&mut self, terminal: &TerminalId) {
        let Some(state) = self.terminals.get_mut(terminal) else {
            return;
        };
        if apply_default_access(
            &self.access_defaults,
            self.remote_settings.bridge_if_bridge_only(),
            state,
        ) {
            self.mark_session_dirty();
        }
    }

    /// After a restore: panes that already hold agents get the default too.
    pub(crate) fn bind_default_access_all(&mut self) {
        for terminal in self.bridge_terminals_with_a_mode() {
            tracing::warn!(
                %terminal,
                "the bridge pane has an input mode; it receives by API by design and will refuse its input"
            );
        }
        if self.access_defaults.default.is_none() && self.access_defaults.default_mode.is_any() {
            return;
        }
        let ids: Vec<TerminalId> = self.terminals.keys().cloned().collect();
        for id in ids {
            self.bind_default_access(&id);
        }
    }

    /// True when `resolved` (a freshly read `[access]`) matches what startup
    /// applied. `bridge_binding` is filled at runtime and not compared.
    pub(crate) fn access_matches_startup(&self, resolved: &crate::config::ResolvedAccess) -> bool {
        resolved.defaults == self.access_defaults
            && resolved.remote.input == self.remote_settings.input
            && resolved.remote.bridge_agent == self.remote_settings.bridge_agent
            && resolved.remote_owner_tui == self.remote_owner_tui
    }
}
