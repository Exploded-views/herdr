// herdr-fork: handlers for the `access.pane.*` methods. Unofficial stopgap; see FORK.md.
//! `access.pane.set` and `access.pane.get`. The gate has already decided who
//! may ask: `set` is an `AccessAdmin` request (only a caller outside every
//! pane), `get` is open. Nothing here handles a token.

use super::responses;
use crate::access::policy::{parse_accept_from, AcceptFrom};
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

    /// The pane's policy as the wire shows it: no binding and `["any"]` when
    /// the pane holds none.
    fn access_policy_response(&self, id: String, pane_id: String, terminal: &TerminalId) -> String {
        let policy = self
            .state
            .terminals
            .get(terminal)
            .and_then(|state| state.access.as_ref());
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
        let accept = match parse_accept_from(&params.accept_from) {
            Ok(accept) => accept,
            Err(message) => return responses::encode_error(id, "invalid_accept_from", message),
        };
        match accept {
            AcceptFrom::Any => self.state.unbind_access(&terminal),
            AcceptFrom::Only(list) => {
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
