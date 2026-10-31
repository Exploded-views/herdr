// herdr-fork: records client input in the input log from the server's event handlers: shell pane input and pasted images, popup input, and attached terminals' keystrokes and mouse reports. Unofficial stopgap; see FORK.md.
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
    pub(super) fn log_attach_input(
        &self,
        client_id: u64,
        terminal_id: &str,
        route: &'static str,
        bytes: u64,
    ) {
        if !self.app.state.input_log || bytes == 0 {
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
            allowed: true,
        });
    }
}
