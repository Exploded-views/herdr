// herdr-fork: bind, unbind and default-bind per-pane access policies. Unofficial stopgap; see FORK.md.
//! Policy state only. Nothing here gates a request; the server gate reads
//! `TerminalState::access` in a later task.

use std::io;

use super::state::AppState;
use crate::access::policy::{AccessBindingId, AccessPolicy, Principal};
use crate::terminal::TerminalId;

// bind_access and unbind_access are called by the access.* methods in a later task.
#[allow(dead_code)]
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
        }
        self.mark_session_dirty();
        Ok(binding)
    }

    /// Open a pane again.
    pub(crate) fn unbind_access(&mut self, terminal: &TerminalId) {
        if let Some(state) = self.terminals.get_mut(terminal) {
            state.access = None;
            self.mark_session_dirty();
        }
    }
}

impl AppState {
    /// Sticky default: bind `[access.default]` to a pane the first time an
    /// agent is seen in it. A no-op, and free, when no default is configured.
    pub(crate) fn bind_default_access(&mut self, terminal: &TerminalId) {
        let Some(default) = self.access_defaults.default.as_ref() else {
            return;
        };
        let Some(state) = self.terminals.get_mut(terminal) else {
            return;
        };
        if state.access.is_some() || !state.is_agent_terminal() {
            return;
        }
        let binding = match AccessBindingId::mint() {
            Ok(binding) => binding,
            Err(err) => {
                tracing::error!(%err, "could not mint an access binding; pane left open");
                return;
            }
        };
        state.access = Some(AccessPolicy {
            binding,
            accept_from: default.clone(),
        });
        self.mark_session_dirty();
    }

    /// After a restore: panes that already hold agents get the default too.
    pub(crate) fn bind_default_access_all(&mut self) {
        if self.access_defaults.default.is_none() {
            return;
        }
        let ids: Vec<TerminalId> = self.terminals.keys().cloned().collect();
        for id in ids {
            self.bind_default_access(&id);
        }
    }
}
