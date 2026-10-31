// herdr-fork: handlers for the `access.caller.*` methods. Unofficial stopgap; see FORK.md.
//! Named callers. Every method here is an `AccessAdmin` request: the gate has
//! already refused panes, remote hosts and callers before a handler runs.
//! A token is returned by `add` and `rotate` and never logged or kept.

use std::io;

use super::responses;
use crate::access::policy::AccessBindingId;
use crate::access::registry::CallerRegistry;
use crate::api::schema::{
    AccessCallerAddParams, AccessCallerInfo, AccessCallerNameParams, AccessCallerRetargetParams,
    CallerToken, ResponseResult,
};
use crate::app::App;

/// An error code and message for the wire.
type Failure = (&'static str, String);

fn io_failure(err: &io::Error) -> Failure {
    match err.kind() {
        io::ErrorKind::InvalidInput => ("invalid_caller_name", err.to_string()),
        io::ErrorKind::AlreadyExists => ("caller_exists", err.to_string()),
        io::ErrorKind::NotFound => ("caller_not_found", err.to_string()),
        _ => (
            "internal_error",
            "could not update the caller registry".into(),
        ),
    }
}

impl App {
    /// The binding of the policy on `target_pane`.
    fn binding_of_pane(&self, target_pane: &str) -> Result<AccessBindingId, Failure> {
        let (ws_idx, pane_id) = self
            .parse_current_public_pane_id(target_pane)
            .ok_or_else(|| ("pane_not_found", format!("pane {target_pane} not found")))?;
        let policy = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .and_then(|terminal| self.state.terminals.get(terminal))
            .and_then(|state| state.access.as_ref());
        match policy {
            Some(policy) => Ok(policy.binding.clone()),
            None => Err((
                "no_access_policy",
                format!("pane {target_pane} has no access policy; set one first"),
            )),
        }
    }

    /// Change a copy of the registry, save it, and only then keep it, so a
    /// failed save leaves memory and disk as they were.
    fn change_callers<T>(
        &mut self,
        change: impl FnOnce(&mut CallerRegistry) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        if self.state.access_callers_unreadable {
            return Err((
                "internal_error",
                "the caller registry could not be read at startup; fix or remove it and restart"
                    .into(),
            ));
        }
        let mut updated = self.state.access_callers.clone();
        let value = change(&mut updated)?;
        if let Some(path) = self.state.access_callers_path.as_deref() {
            updated.save(path).map_err(|err| {
                tracing::error!(%err, "could not save the access caller registry");
                io_failure(&err)
            })?;
        }
        self.state.access_callers = updated;
        Ok(value)
    }

    fn binding_of_caller(&self, name: &str) -> String {
        self.state
            .access_callers
            .callers()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, binding)| binding.to_string())
            .unwrap_or_default()
    }

    fn token_response(&self, id: String, name: String, token: String) -> String {
        let binding = self.binding_of_caller(&name);
        responses::encode_success(
            id,
            ResponseResult::AccessCallerToken {
                name,
                binding,
                token: CallerToken::new(token),
            },
        )
    }

    fn failure(id: String, (code, message): Failure) -> String {
        responses::encode_error(id, code, message)
    }

    pub(super) fn handle_access_caller_add(
        &mut self,
        id: String,
        params: AccessCallerAddParams,
    ) -> String {
        let result = self
            .binding_of_pane(&params.target_pane)
            .and_then(|binding| {
                self.change_callers(|registry| {
                    registry
                        .add(&params.name, binding)
                        .map_err(|err| io_failure(&err))
                })
            });
        match result {
            Ok(token) => {
                tracing::info!(name = %params.name, "access caller added");
                self.token_response(id, params.name, token)
            }
            Err(failure) => Self::failure(id, failure),
        }
    }

    pub(super) fn handle_access_caller_list(&self, id: String) -> String {
        let callers = self
            .state
            .access_callers
            .callers()
            .map(|(name, binding)| AccessCallerInfo {
                name: name.to_string(),
                binding: binding.to_string(),
                pane_id: self.public_pane_id_of_binding(binding),
            })
            .collect();
        responses::encode_success(id, ResponseResult::AccessCallers { callers })
    }

    pub(super) fn handle_access_caller_rotate(
        &mut self,
        id: String,
        params: AccessCallerNameParams,
    ) -> String {
        match self.change_callers(|registry| {
            registry
                .rotate(&params.name)
                .map_err(|err| io_failure(&err))
        }) {
            Ok(token) => {
                tracing::info!(name = %params.name, "access caller token rotated");
                self.token_response(id, params.name, token)
            }
            Err(failure) => Self::failure(id, failure),
        }
    }

    pub(super) fn handle_access_caller_retarget(
        &mut self,
        id: String,
        params: AccessCallerRetargetParams,
    ) -> String {
        let result = self
            .binding_of_pane(&params.target_pane)
            .and_then(|binding| {
                self.change_callers(|registry| {
                    registry
                        .retarget(&params.name, binding)
                        .map_err(|message| ("caller_not_found", message))
                })
            });
        match result {
            Ok(()) => {
                tracing::info!(name = %params.name, "access caller retargeted");
                responses::encode_success(id, ResponseResult::Ok {})
            }
            Err(failure) => Self::failure(id, failure),
        }
    }

    pub(super) fn handle_access_caller_remove(
        &mut self,
        id: String,
        params: AccessCallerNameParams,
    ) -> String {
        match self.change_callers(|registry| {
            if registry.remove(&params.name) {
                Ok(())
            } else {
                Err((
                    "caller_not_found",
                    format!("no caller named {}", params.name),
                ))
            }
        }) {
            Ok(()) => {
                tracing::info!(name = %params.name, "access caller removed");
                responses::encode_success(id, ResponseResult::Ok {})
            }
            Err(failure) => Self::failure(id, failure),
        }
    }

    /// Public id of the open pane whose policy holds `binding`.
    fn public_pane_id_of_binding(&self, binding: &AccessBindingId) -> Option<String> {
        let terminal = self.state.terminals.iter().find_map(|(terminal, state)| {
            state
                .access
                .as_ref()
                .is_some_and(|policy| policy.binding == *binding)
                .then_some(terminal)
        })?;
        let target = self.resolve_terminal_target(terminal.as_str()).ok()?;
        self.public_pane_id(target.ws_idx, target.pane_id)
    }
}
