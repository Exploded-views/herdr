// herdr-fork: records client input in the input log from the server's event handlers: shell pane input and pasted images, popup input, and attached terminals' keystrokes and mouse reports; an attached client's input is checked against the pane's input mode, and a refusal is recorded as a deny. Unofficial stopgap; see FORK.md.
//! Client-path entries for the input-origin log. The handlers measure the bytes a
//! runtime wrote (`user_bytes_sent` before and after) and pass the difference here, so
//! an input that never reached the PTY (a mouse report with tracking off) leaves no
//! entry. A refusal is recorded with an estimate of what it carried.
use super::*;
use crate::app::access_log::ClientInput;

/// A refused input never reached a PTY: count the text it carried, or one byte per key.
pub(super) fn client_events_payload_len(events: &[protocol::ClientPaneInputEvent]) -> u64 {
    events
        .iter()
        .map(|event| match event {
            protocol::ClientPaneInputEvent::TextCommit(text)
            | protocol::ClientPaneInputEvent::Paste(text) => text.len() as u64,
            protocol::ClientPaneInputEvent::Key { generated_text, .. } => {
                generated_text.as_ref().map_or(1, |text| text.len() as u64)
            }
            protocol::ClientPaneInputEvent::Mouse { .. } => 0,
        })
        .sum()
}

impl HeadlessServer {
    /// Shell client input into a pane (`shell.pane_input`, `shell.clipboard_image`).
    pub(super) fn log_shell_pane_input(
        &self,
        client_id: u64,
        pane_id: &str,
        route: &'static str,
        bytes: u64,
        allowed: bool,
    ) {
        if !self.app.state.input_log || (allowed && bytes == 0) {
            return;
        }
        let Some(client) = self.clients.get(&client_id) else {
            return;
        };
        let Some((ws_idx, pane)) = self.app.parse_pane_id(pane_id) else {
            return;
        };
        let Some(terminal) = self
            .app
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane))
        else {
            return;
        };
        let label = self.app.public_pane_id(ws_idx, pane);
        self.app.log_client_input(ClientInput {
            terminal,
            pane: label.as_deref(),
            route,
            client: format!("shell:{client_id}"),
            caller: &client.caller,
            bytes,
            allowed,
        });
    }

    /// Shell client input into the popup terminal (`shell.popup_input`).
    pub(super) fn log_shell_popup_input(
        &self,
        client_id: u64,
        terminal: &crate::terminal::TerminalId,
        bytes: u64,
        allowed: bool,
    ) {
        if !self.app.state.input_log || (allowed && bytes == 0) {
            return;
        }
        let Some(client) = self.clients.get(&client_id) else {
            return;
        };
        self.app.log_client_input(ClientInput {
            terminal,
            pane: Some("popup"),
            route: "shell.popup_input",
            client: format!("shell:{client_id}"),
            caller: &client.caller,
            bytes,
            allowed,
        });
    }

    /// An attached terminal's keystrokes (`attach.input`) or mouse reports (`attach.mouse`).
    /// A refusal (`allowed` false) is recorded even when it carried no countable bytes.
    pub(super) fn log_attach_input(
        &self,
        client_id: u64,
        terminal_id: &str,
        route: &'static str,
        bytes: u64,
        allowed: bool,
    ) {
        if !self.app.state.input_log || (allowed && bytes == 0) {
            return;
        }
        let Some(client) = self.clients.get(&client_id) else {
            return;
        };
        let Some(terminal) = self.terminal_id_by_string(terminal_id) else {
            return;
        };
        self.app.log_client_input(ClientInput {
            terminal: &terminal,
            pane: None,
            route,
            client: format!("attach:{client_id}"),
            caller: &client.caller,
            bytes,
            allowed,
        });
    }

    /// May the client attached to `terminal_id` write input into it? Applies the pane's input
    /// mode (attach input is not gated by the allow-list). On a refusal, tell the client the
    /// first time for this terminal, record it, and return false so the input is dropped.
    /// `bytes` is what the refused input carried, for the log.
    pub(super) fn attach_input_permitted(
        &mut self,
        client_id: u64,
        terminal_id: &str,
        route: &'static str,
        bytes: u64,
    ) -> bool {
        let Some(terminal) = self
            .app
            .state
            .terminals
            .keys()
            .find(|id| id.as_str() == terminal_id)
        else {
            return true;
        };
        let Some(client) = self.clients.get(&client_id) else {
            return true;
        };
        let Err(denied) = self.app.check_attach_input(&client.caller, terminal) else {
            return true;
        };
        let first = self.clients.get_mut(&client_id).is_some_and(|client| {
            let first = client.refused_pane.as_deref() != Some(terminal_id);
            if first {
                client.refused_pane = Some(terminal_id.to_owned());
            }
            first
        });
        self.log_attach_input(client_id, terminal_id, route, bytes, false);
        if first {
            warn!(client_id, terminal_id, "access denied: attach input");
            self.send_to_client(
                client_id,
                ServerMessage::Notify {
                    kind: protocol::NotifyKind::Toast,
                    message: "Input refused".to_owned(),
                    body: Some(denied.message()),
                },
            );
        }
        false
    }
}
