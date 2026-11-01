// herdr-fork: records the origin of pane input in the input log: what an Input request or a client's keystrokes carried (path, sender class, client, host kind, byte count; never the content), the `access.log` handler and the flush points. Unofficial stopgap; see FORK.md.
//! The input-origin log, seen from the app thread. `access_gate` calls
//! [`App::log_api_input`] for the `api` path; the server calls
//! [`App::log_client_input`] for the `client` path. Both build one
//! [`InputEvent`] and hand it to [`InputLog::record`], which never blocks.

use std::sync::PoisonError;

use crate::access::caller::Caller;
use crate::access::input_log::{
    now_ms, InputEvent, InputLog, InputPath, LoggedClass, DECISION_ALLOW, DECISION_DENY,
};
use crate::access::policy::CallerClass;
use crate::api::schema::{AccessInputEvent, AccessLogParams, Method, ResponseResult};
use crate::terminal::TerminalId;

use super::api::responses;
use super::App;

/// Entries `access.log` returns when the request names no limit.
const DEFAULT_LOG_LIMIT: u32 = 100;

/// One client input to record. The server fills it in where it applies the input.
pub(crate) struct ClientInput<'a> {
    pub terminal: &'a TerminalId,
    /// Public pane id, when the caller already has it.
    pub pane: Option<&'a str>,
    pub route: &'static str,
    /// `shell:<id>` or `attach:<id>`.
    pub client: String,
    pub caller: &'a Caller,
    pub bytes: u64,
    pub allowed: bool,
}

/// The wire form of a class, plus who the entry names and where the caller came from.
struct Origin {
    class: LoggedClass,
    principal: Option<String>,
    host: &'static str,
}

impl App {
    fn with_input_log<R>(&self, work: impl FnOnce(&mut InputLog) -> R) -> R {
        let mut log = self
            .input_log
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        work(&mut log)
    }

    /// Send the open entry to the writer and stop it. Server stop and live handoff.
    pub(crate) fn flush_input_log(&self) {
        self.with_input_log(InputLog::flush_and_stop);
    }

    /// Write everything recorded so far to the file, then return. Reads of the log call it.
    pub(crate) fn sync_input_log(&self) {
        self.with_input_log(InputLog::sync);
    }

    /// Start the writer again after a handoff that did not happen.
    #[cfg(unix)]
    pub(crate) fn resume_input_log(&self) {
        self.with_input_log(InputLog::resume);
    }

    pub(crate) fn input_log_snapshot(&self, limit: usize) -> (Vec<InputEvent>, u64) {
        self.with_input_log(|log| (log.snapshot(limit), log.dropped()))
    }

    /// Public id of the pane that shows `terminal`, by a scan that allocates only the answer.
    pub(crate) fn pane_label_of_terminal(&self, terminal: &TerminalId) -> String {
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    if &pane.attached_terminal_id == terminal {
                        return self
                            .public_pane_id(ws_idx, *pane_id)
                            .unwrap_or_else(|| "unknown".to_string());
                    }
                }
            }
        }
        "unknown".to_string()
    }

    fn origin_of(&self, caller: &Caller, class: &CallerClass) -> Origin {
        let (logged, principal) = match class {
            CallerClass::Pane(sender) => {
                (LoggedClass::Pane, Some(self.pane_label_of_terminal(sender)))
            }
            CallerClass::Outside { via_ssh: false } => (LoggedClass::Outside, None),
            CallerClass::Outside { via_ssh: true } => (LoggedClass::OutsideSsh, None),
            CallerClass::Remote => (LoggedClass::Remote, None),
            CallerClass::Caller { name, .. } => (LoggedClass::Caller, Some(name.clone())),
            CallerClass::Unknown { .. } => (LoggedClass::Unknown, None),
        };
        let host = match caller {
            Caller::Peer { remote: true, .. } => "remote-stamp",
            Caller::Peer { lineage, .. } if lineage.via_ssh => "ssh",
            Caller::Peer { .. } => "local",
            #[cfg(test)]
            Caller::Classified(class) => match class {
                CallerClass::Remote => "remote-stamp",
                CallerClass::Outside { via_ssh: true }
                | CallerClass::Caller { via_ssh: true, .. } => "ssh",
                _ => "local",
            },
        };
        Origin {
            class: logged,
            principal,
            host,
        }
    }

    /// Record one `api`-path Input request: `allowed` is the gate's verdict.
    pub(crate) fn log_api_input(
        &self,
        method: &Method,
        caller: &Caller,
        class: &CallerClass,
        pane: String,
        terminal: &TerminalId,
        allowed: bool,
    ) {
        let origin = self.origin_of(caller, class);
        let conn = match caller {
            Caller::Peer { conn, .. } => *conn,
            #[cfg(test)]
            Caller::Classified(_) => 0,
        };
        self.with_input_log(|log| {
            log.record(InputEvent {
                at_ms: now_ms(),
                pane,
                terminal: terminal.as_str().to_owned(),
                path: InputPath::Api,
                route: method.operation().name,
                class: origin.class,
                principal: origin.principal,
                client: format!("api:{conn}"),
                host: origin.host,
                bytes: self.api_input_bytes(method, terminal),
                decision: if allowed {
                    DECISION_ALLOW
                } else {
                    DECISION_DENY
                },
                count: 1,
            });
        });
    }

    /// Record input from a connected client: a shell client's pane input and pasted
    /// image, a popup, or an attached terminal's keystrokes and mouse reports.
    pub(crate) fn log_client_input(&self, input: ClientInput<'_>) {
        if !self.state.input_log {
            return;
        }
        let class = self.classify_caller(input.caller);
        let origin = self.origin_of(input.caller, &class);
        let pane = match input.pane {
            Some(pane) => pane.to_owned(),
            None => self.pane_label_of_terminal(input.terminal),
        };
        self.with_input_log(|log| {
            log.record(InputEvent {
                at_ms: now_ms(),
                pane,
                terminal: input.terminal.as_str().to_owned(),
                path: InputPath::Client,
                route: input.route,
                class: origin.class,
                principal: origin.principal,
                client: input.client,
                host: origin.host,
                bytes: input.bytes,
                decision: if input.allowed {
                    DECISION_ALLOW
                } else {
                    DECISION_DENY
                },
                count: 1,
            });
        });
    }

    /// What the request writes into the pane, by route. Never the content itself.
    fn api_input_bytes(&self, method: &Method, terminal: &TerminalId) -> u64 {
        use super::api_helpers::{encode_api_input, encode_api_keys};
        let runtime = self.terminal_runtimes.get(terminal);
        let encoded_keys = |keys: &[String]| -> u64 {
            runtime
                .and_then(|runtime| encode_api_keys(runtime, keys).ok())
                .map_or(0, |encoded| {
                    encoded.iter().map(|bytes| bytes.len() as u64).sum()
                })
        };
        match method {
            Method::AgentPrompt(params) => params.text.len() as u64,
            Method::PaneSendText(params) => params.text.len() as u64,
            Method::AgentSendKeys(params) => encoded_keys(&params.keys),
            Method::PaneSendKeys(params) => encoded_keys(&params.keys),
            Method::PaneSendInput(params) => runtime
                .and_then(|runtime| encode_api_input(runtime, &params.text, &params.keys).ok())
                .map_or(params.text.len() as u64, |bytes| bytes.len() as u64),
            // The command line the shell is typed: the agent's executable and its arguments,
            // before any quoting.
            Method::AgentStart(params) => {
                let args: usize = params.args.iter().map(|arg| arg.len() + 1).sum();
                (params.kind.len() + args) as u64
            }
            _ => 0,
        }
    }

    /// `access.log`: Outside-only (the gate refuses everyone else). Newest first.
    pub(super) fn handle_access_log(&self, id: String, params: AccessLogParams) -> String {
        let limit = params
            .limit
            .unwrap_or(DEFAULT_LOG_LIMIT)
            .clamp(1, crate::access::input_log::RING_CAP as u32) as usize;
        // What the read shows is in the file by the time it answers.
        self.sync_input_log();
        let (events, dropped) = self.input_log_snapshot(limit);
        let events = events
            .into_iter()
            .map(|event| AccessInputEvent {
                at_ms: event.at_ms,
                pane: event.pane,
                terminal: event.terminal,
                path: wire(&event.path),
                route: event.route.to_owned(),
                class: wire(&event.class),
                principal: event.principal,
                client: event.client,
                host: event.host.to_owned(),
                bytes: event.bytes,
                decision: event.decision.to_owned(),
                count: event.count,
            })
            .collect();
        responses::encode_success(
            id,
            ResponseResult::AccessInputLog {
                node: self.state.node_name.clone(),
                events,
                dropped,
            },
        )
    }
}

/// The serialized name of a plain enum value.
fn wire<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
