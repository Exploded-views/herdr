// herdr-fork: handlers for the `access.pane.*` methods, including the pane's input mode. Unofficial stopgap; see FORK.md.
//! `access.pane.set` and `access.pane.get`. The gate has already decided who
//! may ask: `set` is an `AccessAdmin` request (only a caller outside every
//! pane), `get` is open. Nothing here handles a token.

use super::responses;
use crate::access::policy::{parse_accept_from, parse_input_mode, AcceptFrom};
use crate::api::schema::{AccessPaneGetParams, AccessPaneSetParams, ResponseResult};
use crate::app::App;
use crate::terminal::TerminalId;

impl App {
    /// The pane's public id and terminal.
    fn access_pane(&self, pane_id: &str) -> Result<(String, TerminalId), String> {
        let not_found = |id: &str| format!("pane {id} not found");
        let (ws_idx, pane) = self
            .parse_current_public_pane_id(pane_id)
            .ok_or_else(|| not_found(pane_id))?;
        let terminal = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane))
            .cloned()
            .ok_or_else(|| not_found(pane_id))?;
        let public = self
            .public_pane_id(ws_idx, pane)
            .ok_or_else(|| not_found(pane_id))?;
        Ok((public, terminal))
    }

    /// The pane's policy and input mode as the wire shows them: no binding and `["any"]`
    /// when the pane holds no policy.
    fn access_policy_response(&self, id: String, pane_id: String, terminal: &TerminalId) -> String {
        let policy = self
            .state
            .terminals
            .get(terminal)
            .and_then(|state| state.access.as_ref());
        let input_mode = self
            .state
            .terminals
            .get(terminal)
            .map_or("any", |state| state.input_mode.as_str())
            .to_string();
        let (binding, accept_from) = match policy {
            Some(policy) => (
                Some(policy.binding.to_string()),
                policy
                    .accept_from
                    .iter()
                    .cloned()
                    .map(String::from)
                    .collect(),
            ),
            None => (None, vec!["any".to_string()]),
        };
        responses::encode_success(
            id,
            ResponseResult::AccessPolicy {
                pane_id,
                binding,
                accept_from,
                input_mode,
            },
        )
    }

    pub(super) fn handle_access_pane_set(
        &mut self,
        id: String,
        params: AccessPaneSetParams,
    ) -> String {
        let (pane_id, terminal) = match self.access_pane(&params.pane_id) {
            Ok(found) => found,
            Err(message) => return responses::encode_error(id, "pane_not_found", message),
        };
        // Validate everything before changing anything.
        let accept = match params.accept_from.as_deref().map(parse_accept_from) {
            Some(Ok(accept)) => Some(accept),
            Some(Err(message)) => {
                return responses::encode_error(id, "invalid_accept_from", message)
            }
            None => None,
        };
        let mode = match params.input_mode.as_deref().map(parse_input_mode) {
            Some(Ok(mode)) => Some(mode),
            Some(Err(message)) => {
                return responses::encode_error(id, "invalid_input_mode", message)
            }
            None => None,
        };
        // The bridge receives by API by design: its pane stays `any`.
        if mode.is_some_and(|mode| !mode.is_any()) {
            let bridge = self
                .state
                .terminals
                .get(&terminal)
                .and_then(|state| state.agent_name.as_deref())
                .filter(|name| self.state.is_bridge_name(name));
            if let Some(bridge) = bridge {
                return responses::encode_error(
                    id,
                    "access_bridge_input_mode",
                    crate::app::access::bridge_mode_message(bridge),
                );
            }
        }
        if accept.is_none() && mode.is_none() {
            return responses::encode_error(
                id,
                "invalid_accept_from",
                "set needs accept_from, input_mode or both",
            );
        }
        match accept {
            None => {}
            Some(AcceptFrom::Any) => self.state.unbind_access(&terminal),
            Some(AcceptFrom::Only(list)) => {
                if let Err(err) = self.state.bind_access(&terminal, list) {
                    tracing::error!(%err, pane = %pane_id, "could not bind a pane's access policy");
                    return responses::encode_error(
                        id,
                        "access_bind_failed",
                        format!("could not bind the pane's access policy: {err}"),
                    );
                }
            }
        }
        if let Some(mode) = mode {
            if let Some(state) = self.state.terminals.get_mut(&terminal) {
                if state.input_mode != mode {
                    state.input_mode = mode;
                    self.state.mark_session_dirty();
                }
            }
        }
        self.schedule_session_save();
        tracing::info!(pane = %pane_id, "access policy set");
        self.access_policy_response(id, pane_id, &terminal)
    }

    pub(super) fn handle_access_pane_get(&self, id: String, params: AccessPaneGetParams) -> String {
        match self.access_pane(&params.pane_id) {
            Ok((pane_id, terminal)) => self.access_policy_response(id, pane_id, &terminal),
            Err(message) => responses::encode_error(id, "pane_not_found", message),
        }
    }
}
