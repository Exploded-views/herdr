// herdr-fork: tests for binding access at `agent start`, the named input mode and the bridge guard. Unofficial stopgap; see FORK.md.
use std::time::Duration;

use bytes::Bytes;

use crate::api::schema::{
    AgentPromptParams, AgentRenameParams, AgentSendKeysParams, AgentStartParams, AgentTarget,
    PaneReadResult, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

const AGENT_PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);

// Codex's Windows input reader does not surface bracketed paste. It detects the prompt as a
// "paste burst" and, while that burst is buffered, rewrites a following Enter into a newline
// instead of submitting. The burst only flushes after an idle timeout, so any size-based delay is
// a timing guess that fails when ConPTY delivery lags it. Codex flushes a buffered burst
// synchronously when it receives a non-character key, so appending one after the paste gives the
// submission a deterministic paste boundary regardless of prompt size or delivery speed.
#[cfg(windows)]
fn append_codex_paste_boundary(runtime: &crate::terminal::TerminalRuntime, text: &mut Vec<u8>) {
    let keys = match crate::app::api_helpers::encode_api_keys(runtime, &["right".to_string()]) {
        Ok(keys) => keys,
        Err(key) => {
            tracing::warn!(key = %key, "failed to encode Codex paste boundary key");
            return;
        }
    };
    if let Some(key) = keys.into_iter().find(|bytes| !bytes.is_empty()) {
        text.extend_from_slice(&key);
    }
}

impl App {
    pub(super) fn handle_agent_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        self.reconcile_managed_agent_target(&target.target);
        let agent = match self.agent_info_for_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_focus(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.focus_agent_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_rename(&mut self, id: String, params: AgentRenameParams) -> String {
        let agent = match self.rename_agent_target(&params.target, params.name) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_rename_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_start(&mut self, id: String, params: AgentStartParams) -> String {
        let (agent, argv) = match self.start_agent(params) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentStarted { agent, argv })
    }

    pub(crate) fn handle_deferred_agent_api_request(
        &mut self,
        request: crate::api::schema::Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        let crate::api::schema::Method::AgentPrompt(params) = request.method else {
            return false;
        };
        let validated = match self.validate_agent_prompt(request.id, &params) {
            Ok(validated) => validated,
            Err(response) => {
                let _ = respond_to.send(response);
                return true;
            }
        };
        // Start the completion waiter before submitting, so a refused thread
        // fails the request while nothing has been sent yet.
        let (handoff_tx, handoff_rx) = std::sync::mpsc::channel::<(
            String,
            crate::api::schema::AgentInfo,
            std::sync::mpsc::Receiver<std::io::Result<()>>,
        )>();
        let waiter_respond_to = respond_to.clone();
        let spawned = crate::thread_spawn::spawn_named("herdr-agent-prompt", move || {
            let Ok((id, agent, completion)) = handoff_rx.recv() else {
                return;
            };
            let response = match completion.recv() {
                Ok(Ok(())) => encode_success(id, ResponseResult::AgentPrompted { agent }),
                Ok(Err(err)) if err.kind() == std::io::ErrorKind::TimedOut => {
                    encode_error(id, "timeout", err.to_string())
                }
                Ok(Err(err)) => encode_error(id, "agent_prompt_failed", err.to_string()),
                Err(_) => encode_error(id, "agent_prompt_failed", "pty actor closed"),
            };
            let _ = waiter_respond_to.send(response);
        });
        if let Err(err) = spawned {
            tracing::warn!(err = %err, "failed to spawn agent prompt thread");
            let _ = respond_to.send(encode_error(
                validated.id,
                "agent_prompt_failed",
                format!("could not start prompt completion waiter: {err}"),
            ));
            return true;
        }
        match self.submit_agent_prompt(validated, &params) {
            Ok(submission) => {
                let _ = handoff_tx.send(submission);
            }
            Err(response) => {
                let _ = respond_to.send(response);
            }
        }
        true
    }

    /// Runs every check that can reject a prompt without touching the pane.
    fn validate_agent_prompt(
        &self,
        id: String,
        params: &AgentPromptParams,
    ) -> Result<ValidatedAgentPrompt, String> {
        if params.text.is_empty() {
            return Err(encode_error(
                id,
                "empty_agent_prompt",
                "agent prompt must not be empty",
            ));
        }
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return Err(encode_error_body(id, self.agent_target_error_body(err))),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return Err(agent_not_found(id, &params.target));
        };
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        if terminal.state == crate::detect::AgentState::Blocked {
            return Err(encode_error(
                id,
                "agent_blocked",
                format!(
                    "agent {} is blocked and requires interactive input",
                    params.target
                ),
            ));
        }
        let Some(expected_agent) = terminal.effective_known_agent() else {
            return Err(agent_not_ready(id, &params.target));
        };
        if terminal.managed_agent_launch_pending() {
            return Err(agent_not_ready(id, &params.target));
        }
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return Err(encode_error(
                id,
                "agent_not_ready",
                format!(
                    "agent {} is no longer the pane foreground process",
                    params.target
                ),
            ));
        }
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        Ok(ValidatedAgentPrompt {
            id,
            ws_idx: resolved.ws_idx,
            pane_id: resolved.pane_id,
            expected_agent,
            agent,
        })
    }

    fn submit_agent_prompt(
        &mut self,
        validated: ValidatedAgentPrompt,
        params: &AgentPromptParams,
    ) -> Result<
        (
            String,
            crate::api::schema::AgentInfo,
            std::sync::mpsc::Receiver<std::io::Result<()>>,
        ),
        String,
    > {
        let ValidatedAgentPrompt {
            id,
            ws_idx,
            pane_id,
            expected_agent,
            agent,
        } = validated;
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        #[cfg(windows)]
        let submit_deadline = params
            .wait
            .as_ref()
            .and_then(|wait| wait.submission_deadline);
        #[cfg(not(windows))]
        let submit_deadline = None;
        if expected_agent == crate::detect::Agent::GithubCopilot {
            // Copilot ignores synthetic Enter after focus loss until it receives focus gained.
            let focus = match crate::ghostty::encode_focus(crate::ghostty::FocusEvent::Gained) {
                Ok(focus) => focus,
                Err(err) => {
                    return Err(encode_error(id, "agent_prompt_failed", err.to_string()));
                }
            };
            if let Err(err) = runtime.try_send_bytes(Bytes::from(focus)) {
                return Err(encode_error(id, "agent_prompt_failed", err.to_string()));
            }
        }
        let (text, enter) =
            crate::app::api_helpers::encode_api_submission_parts(runtime, &params.text);
        #[cfg(windows)]
        let text = if expected_agent == crate::detect::Agent::Codex {
            let mut text = text;
            append_codex_paste_boundary(runtime, &mut text);
            text
        } else {
            text
        };
        let completion = runtime
            .queue_user_input_submission(
                Bytes::from(text),
                Bytes::from(enter),
                AGENT_PROMPT_SUBMIT_DELAY,
                submit_deadline,
            )
            .map_err(|err| encode_error(id.clone(), "agent_prompt_failed", err.to_string()))?;
        Ok((id, agent, completion))
    }

    pub(super) fn handle_agent_read(
        &mut self,
        id: String,
        params: crate::api::schema::AgentReadParams,
    ) -> String {
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &params.target);
        };
        let snapshot = crate::app::api_helpers::read_terminal_snapshot(
            pane,
            params.source,
            params.format,
            params.lines,
        );

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: self
                        .public_pane_id(resolved.ws_idx, resolved.pane_id)
                        .unwrap_or_else(|| params.target.clone()),
                    workspace_id,
                    tab_id: self
                        .public_tab_id(resolved.ws_idx, resolved.tab_idx)
                        .unwrap(),
                    source: params.source,
                    format: params.format,
                    text: snapshot.text,
                    revision: 0,
                    truncated: snapshot.truncated,
                },
            },
        )
    }

    pub(super) fn handle_agent_explain(&mut self, id: String, target: AgentTarget) -> String {
        let resolved = match self.resolve_agent_target(&target.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, _workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return agent_not_found(id, &target.target);
        };
        if terminal.full_lifecycle_hook_authority_active() {
            let explain = serde_json::json!({
                "agent": terminal.effective_agent_label().unwrap_or("unknown"),
                "state": crate::detect::manifest::agent_state_label(terminal.state),
                "manifest_source": null,
                "manifest_version": null,
                "cached_remote_version": null,
                "local_override_shadowing_remote": false,
                "remote_update_status": null,
                "remote_update_error": null,
                "matched_rule": null,
                "visible_idle": false,
                "visible_blocker": false,
                "visible_working": false,
                "screen_detection_skipped": true,
                "screen_detection_skip_reason": "full_lifecycle_hook_authority",
                "skip_state_update": false,
                "skipped_update_reason": null,
                "fallback_reason": null,
                "warning": null,
                "evaluated_rules": [],
            });
            return encode_success(id, ResponseResult::AgentExplain { explain });
        }
        let Some(agent) = terminal.effective_known_agent().or(terminal.detected_agent) else {
            return encode_error(
                id,
                "agent_explain_unavailable",
                format!(
                    "agent target {} does not have a detected agent label",
                    target.target
                ),
            );
        };

        let screen = pane.detection_text();
        let osc_title = pane.agent_osc_title();
        let osc_progress = pane.agent_osc_progress();
        let explain = crate::detect::manifest::explain_with_input(
            agent,
            crate::detect::manifest::DetectionInput {
                screen: &screen,
                osc_title: &osc_title,
                osc_progress: &osc_progress,
            },
        );
        let value = crate::detect::manifest::explain_to_json_value(&explain);

        encode_success(id, ResponseResult::AgentExplain { explain: value })
    }

    pub(super) fn handle_agent_send_keys(
        &mut self,
        id: String,
        params: AgentSendKeysParams,
    ) -> String {
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &params.target);
        };
        let Some(expected_agent) = self
            .state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.effective_known_agent())
        else {
            return agent_not_ready(id, &params.target);
        };
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return agent_not_ready(id, &params.target);
        }
        let encoded = match super::super::api_helpers::encode_api_keys(runtime, &params.keys) {
            Ok(encoded) => encoded,
            Err(key) => {
                return encode_error(id, "invalid_key", format!("unsupported key {key}"));
            }
        };
        let bytes: Vec<u8> = encoded.into_iter().flatten().collect();
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            return encode_error(id, "agent_send_keys_failed", err.to_string());
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

struct ValidatedAgentPrompt {
    id: String,
    ws_idx: usize,
    pane_id: crate::layout::PaneId,
    expected_agent: crate::detect::Agent,
    agent: crate::api::schema::AgentInfo,
}

fn agent_not_ready(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_ready",
        format!("agent {target} is not an active named agent"),
    )
}

fn agent_not_found(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_found",
        format!("agent target {target} not found"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{AgentStatus, SuccessResponse},
        app::Mode,
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn app_with_agent() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app
    }

    fn start_deferred_agent_prompt(
        app: &mut App,
        id: &str,
        params: AgentPromptParams,
    ) -> std::sync::mpsc::Receiver<String> {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(app.handle_deferred_agent_api_request(
            crate::api::schema::Request {
                id: id.into(),
                method: crate::api::schema::Method::AgentPrompt(params),
            },
            respond_to,
        ));
        response_rx
    }

    fn run_deferred_agent_prompt(app: &mut App, id: &str, params: AgentPromptParams) -> String {
        start_deferred_agent_prompt(app, id, params)
            .recv_timeout(Duration::from_secs(1))
            .expect("agent prompt responds after submission")
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_codex_prompt_flushes_paste_burst_before_enter() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::AgentPrompted { .. }
        ));
        // The non-character key must precede Enter so Codex commits the paste burst first.
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"A != B\x1b[C"));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
    }

    #[tokio::test]
    async fn a_false_process_exit_makes_a_named_live_agent_unreachable_by_name() {
        // Reproduces the registration loss reported on #3225 by rszrszrsz:
        // a live agent pane with an assigned name stops resolving by that name
        // while its process keeps running, and renaming is the only recovery.
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let observed_at = std::time::Instant::now();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("reviewer".into());

        let found = app.handle_agent_get(
            "req:before".into(),
            AgentTarget {
                target: "reviewer".into(),
            },
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&found).is_ok(),
            "the assigned name must resolve while the agent is running: {found}"
        );

        // One process-exit observation, then the same agent is observed alive
        // again on the next probe - the process never actually went away.
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at,
        });
        app.handle_internal_event(crate::events::AppEvent::AgentProcessDetected {
            pane_id,
            agent: Agent::Pi,
            observed_at: observed_at + std::time::Duration::from_secs(1),
        });

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.detected_agent,
            Some(Agent::Pi),
            "the agent process is still there"
        );

        let after = app.handle_agent_get(
            "req:after".into(),
            AgentTarget {
                target: "reviewer".into(),
            },
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&after).is_ok(),
            "a live agent must stay reachable by its assigned name: {after}"
        );
    }

    #[tokio::test]
    async fn agent_prompt_sends_text_then_delays_enter() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 2,
            );
        runtime.test_process_pty_bytes(b"\x1b[?2004h");
        app.state.insert_test_runtime(pane_id, runtime);

        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let bracketed_started = std::time::Instant::now();
        let response_rx = start_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: public_pane_id,
                text: "A != B".into(),
                wait: None,
            },
        );
        assert!(response_rx.try_recv().is_err());
        let response = response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("agent prompt responds after submission");
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentPrompted { agent, .. } = success.result else {
            panic!("expected prompted response");
        };
        assert_eq!(agent.name.as_deref(), Some("reviewer"));
        assert_eq!(
            rx.try_recv().unwrap(),
            Bytes::from_static(b"\x1b[200~A != B\x1b[201~")
        );
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
        assert!(bracketed_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);

        app.lookup_runtime_sender(0, pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[?2004l");
        let raw_started = std::time::Instant::now();
        let raw = run_deferred_agent_prompt(
            &mut app,
            "req-raw",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let raw: SuccessResponse = serde_json::from_str(&raw).unwrap();
        assert!(matches!(raw.result, ResponseResult::AgentPrompted { .. }));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"A != B"));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
        assert!(raw_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);

        let rejected = run_deferred_agent_prompt(
            &mut app,
            "req-label",
            AgentPromptParams {
                target: "opencode".into(),
                text: "wrong target".into(),
                wait: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&rejected).unwrap();
        assert_eq!(error.error.code, "agent_not_found");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_prompt_waiter_spawn_failure_fails_before_submitting() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        crate::thread_spawn::test_hook::fail_next_spawns(1);
        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "hello".into(),
                wait: None,
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_prompt_failed");
        std::thread::sleep(AGENT_PROMPT_SUBMIT_DELAY * 2);
        assert!(rx.try_recv().is_err(), "a failed prompt must not be sent");
    }

    #[tokio::test]
    async fn agent_prompt_validation_errors_win_over_waiter_spawn_failure() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        for (target, text, code) in [
            ("reviewer", "", "empty_agent_prompt"),
            ("missing", "hello", "agent_not_found"),
        ] {
            crate::thread_spawn::test_hook::fail_next_spawns(1);
            let response = run_deferred_agent_prompt(
                &mut app,
                "req",
                AgentPromptParams {
                    target: target.into(),
                    text: text.into(),
                    wait: None,
                },
            );

            let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(error.error.code, code);
            assert!(
                crate::thread_spawn::spawn_named("probe", || {}).is_err(),
                "a rejected prompt must not start a waiter thread"
            );
        }
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_prompt_rejects_blocked_agent_without_writing() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::GithubCopilot), AgentState::Blocked);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "unrelated prompt".into(),
                wait: None,
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_blocked");
        assert!(
            tokio::time::timeout(
                AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "blocked prompt wrote or scheduled terminal input"
        );
    }

    #[tokio::test]
    async fn agent_prompt_focuses_copilot_before_submitting() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::GithubCopilot), AgentState::Idle);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 3,
            );
        runtime.test_process_pty_bytes(b"\x1b[?2004h");
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::AgentPrompted { .. }
        ));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\x1b[I"));
        assert_eq!(
            rx.try_recv().unwrap(),
            Bytes::from_static(b"\x1b[200~A != B\x1b[201~")
        );
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
    }

    #[tokio::test]
    async fn agent_send_keys_validates_every_key_before_writing() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let rejected = app.handle_agent_send_keys(
            "req-invalid".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                keys: vec!["enter".into(), "not-a-key".into()],
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&rejected).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert!(rx.try_recv().is_err());

        let sent = app.handle_agent_send_keys(
            "req-valid".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                keys: vec!["up".into(), "enter".into()],
            },
        );
        let success: SuccessResponse = serde_json::from_str(&sent).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\x1b[A\r"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_prompt_rejects_managed_agent_while_startup_is_pending() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        let now = std::time::Instant::now();
        terminal.begin_managed_agent(
            "reviewer".into(),
            Agent::OpenCode,
            now,
            std::time::Duration::from_secs(3),
            std::time::Duration::from_secs(10),
        );
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req-pending",
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_not_ready");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn agent_focus_marks_already_focused_done_agent_seen() {
        let mut app = app_with_agent();
        app.state.outer_terminal_focus = Some(false);

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);

        let response = app.handle_agent_focus(
            "req".into(),
            AgentTarget {
                target: app.public_pane_id(0, pane_id).unwrap(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Idle);
    }

    #[test]
    fn agent_rename_does_not_replace_the_pane_label() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_manual_label("shell-pane".into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let target = app.public_pane_id(0, pane_id).unwrap();

        for name in [Some("reviewer".to_string()), None] {
            let response = app.handle_agent_rename(
                "req".into(),
                AgentRenameParams {
                    target: target.clone(),
                    name,
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
            assert_eq!(
                app.state.terminals[&terminal_id].manual_label.as_deref(),
                Some("shell-pane")
            );
        }
    }

    mod start_access {
        use super::*;
        use crate::access::caller::Caller;
        use crate::access::policy::{AccessBindingId, AccessPolicy, CallerClass, Principal};
        use crate::api::schema::{AgentStartParams, Method};
        use crate::terminal::TerminalId;

        struct Rig {
            app: App,
            panes: Vec<String>,
            terminals: Vec<TerminalId>,
            _receivers: Vec<Box<dyn std::any::Any>>,
        }

        fn rig() -> Rig {
            let mut app = app_with_agent();
            let mut workspace = Workspace::test_new("start-access");
            workspace.test_split(ratatui::layout::Direction::Horizontal);
            app.state.workspaces = vec![workspace];
            app.state.ensure_test_terminals();
            app.state.active = Some(0);
            let ids = app.state.workspaces[0].tabs[0].layout.pane_ids();
            let mut rig = Rig {
                app,
                panes: Vec::new(),
                terminals: Vec::new(),
                _receivers: Vec::new(),
            };
            for pane in ids {
                let terminal = rig.app.state.workspaces[0]
                    .terminal_id(pane)
                    .unwrap()
                    .clone();
                let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
                rig.app.terminal_runtimes.insert(terminal.clone(), runtime);
                rig.panes.push(rig.app.public_pane_id(0, pane).unwrap());
                rig.terminals.push(terminal);
                rig._receivers.push(Box::new(rx));
            }
            rig
        }

        fn params(pane: &str, accept_from: Option<&[&str]>) -> AgentStartParams {
            AgentStartParams {
                name: "worker".into(),
                kind: "pi".into(),
                pane_id: pane.into(),
                args: Vec::new(),
                timeout_ms: Some(10_000),
                accept_from: accept_from
                    .map(|values| values.iter().map(|value| value.to_string()).collect()),
            }
        }

        fn start(rig: &mut Rig, pane: usize, accept_from: Option<&[&str]>) -> serde_json::Value {
            let response = rig.app.handle_api_request(crate::api::schema::Request {
                id: "req".into(),
                method: Method::AgentStart(params(&rig.panes[pane], accept_from)),
            });
            serde_json::from_str(&response).unwrap()
        }

        fn bind(rig: &mut Rig, pane: usize, list: Vec<Principal>) -> AccessBindingId {
            let binding = AccessBindingId::mint().unwrap();
            rig.app
                .state
                .terminals
                .get_mut(&rig.terminals[pane])
                .unwrap()
                .access = Some(AccessPolicy {
                binding: binding.clone(),
                accept_from: list,
            });
            binding
        }

        fn policy(rig: &Rig, pane: usize) -> Option<AccessPolicy> {
            rig.app.state.terminals[&rig.terminals[pane]].access.clone()
        }

        fn from_pane(rig: &Rig, pane: usize) -> Caller {
            Caller::Classified(CallerClass::Pane(rig.terminals[pane].clone()))
        }

        fn outside() -> Caller {
            Caller::Classified(CallerClass::Outside { via_ssh: false })
        }

        fn method(rig: &Rig, pane: usize, accept_from: Option<&[&str]>) -> Method {
            Method::AgentStart(params(&rig.panes[pane], accept_from))
        }

        #[tokio::test]
        async fn agent_start_binds_named_section_policy() {
            let mut rig = rig();
            rig.app
                .state
                .access_defaults
                .by_name
                .insert("worker".into(), vec![Principal::Outside]);
            rig.app.state.access_defaults.default = Some(vec![Principal::None]);
            let response = start(&mut rig, 0, None);
            assert_eq!(response["result"]["type"], "agent_started");
            assert_eq!(
                policy(&rig, 0).unwrap().accept_from,
                vec![Principal::Outside]
            );
        }

        #[tokio::test]
        async fn agent_start_binds_the_default_when_no_section_names_the_agent() {
            let mut rig = rig();
            rig.app.state.access_defaults.default = Some(vec![Principal::None]);
            start(&mut rig, 0, None);
            assert_eq!(policy(&rig, 0).unwrap().accept_from, vec![Principal::None]);
        }

        #[tokio::test]
        async fn agent_start_binds_an_explicit_accept_from() {
            let mut rig = rig();
            let response = start(&mut rig, 0, Some(&["outside", "caller:ci"]));
            assert_eq!(response["result"]["type"], "agent_started");
            assert_eq!(
                policy(&rig, 0).unwrap().accept_from,
                vec![Principal::Outside, Principal::Caller("ci".into())]
            );
        }

        #[tokio::test]
        async fn agent_start_into_bound_pane_keeps_binding() {
            let mut rig = rig();
            let binding = bind(&mut rig, 0, vec![Principal::Outside]);
            rig.app.state.access_defaults.default = Some(vec![Principal::None]);
            start(&mut rig, 0, None);
            let kept = policy(&rig, 0).unwrap();
            assert_eq!(kept.binding, binding);
            assert_eq!(kept.accept_from, vec![Principal::Outside]);
        }

        #[tokio::test]
        async fn agent_start_with_tighter_accept_from_keeps_the_binding_id() {
            let mut rig = rig();
            let binding = bind(&mut rig, 0, vec![Principal::Outside]);
            let response = start(&mut rig, 0, Some(&["none"]));
            assert_eq!(response["result"]["type"], "agent_started");
            let kept = policy(&rig, 0).unwrap();
            assert_eq!(kept.binding, binding);
            assert_eq!(kept.accept_from, vec![Principal::None]);
        }

        #[tokio::test]
        async fn agent_start_without_accept_from_keeps_explicitly_opened_pane_open() {
            let mut rig = rig();
            rig.app.state.access_defaults.default = Some(vec![Principal::None]);
            rig.app
                .state
                .access_defaults
                .by_name
                .insert("worker".into(), vec![Principal::None]);
            let terminal = rig.terminals[0].clone();
            rig.app.state.unbind_access(&terminal);
            let response = start(&mut rig, 0, None);
            assert_eq!(response["result"]["type"], "agent_started");
            assert!(policy(&rig, 0).is_none());
            assert!(rig.app.state.terminals[&terminal].access_opened_explicitly);
        }

        #[tokio::test]
        async fn agent_start_with_any_opens_the_pane_on_purpose() {
            let mut rig = rig();
            bind(&mut rig, 0, vec![Principal::Outside]);
            let response = start(&mut rig, 0, Some(&["any"]));
            assert_eq!(response["result"]["type"], "agent_started");
            assert!(policy(&rig, 0).is_none());
            assert!(rig.app.state.terminals[&rig.terminals[0]].access_opened_explicitly);
        }

        #[tokio::test]
        async fn invalid_accept_from_is_refused_and_changes_nothing() {
            let mut rig = rig();
            let binding = bind(&mut rig, 0, vec![Principal::Outside]);
            for bad in [&["any", "outside"][..], &["caller:*"], &[]] {
                let response = start(&mut rig, 0, Some(bad));
                assert_eq!(response["error"]["code"], "invalid_accept_from");
            }
            assert_eq!(policy(&rig, 0).unwrap().binding, binding);
            assert_eq!(rig.app.state.terminals[&rig.terminals[0]].agent_name, None);
        }

        #[tokio::test]
        async fn a_failed_send_puts_the_previous_policy_back() {
            let mut rig = rig();
            let terminal = rig.terminals[0].clone();
            let (runtime, _rx) =
                crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 1);
            runtime
                .try_send_bytes(Bytes::from_static(b"occupied"))
                .unwrap();
            rig.app.terminal_runtimes.insert(terminal.clone(), runtime);
            let response = start(&mut rig, 0, Some(&["outside"]));
            assert_eq!(response["error"]["code"], "agent_start_input_failed");
            assert!(policy(&rig, 0).is_none());
            assert!(!rig.app.state.terminals[&terminal].access_opened_explicitly);
        }

        #[tokio::test]
        async fn agent_start_into_protected_pane_from_pane_denied() {
            let mut rig = rig();
            bind(&mut rig, 0, vec![Principal::Outside]);
            let denied = rig
                .app
                .check_access(&method(&rig, 0, None), &from_pane(&rig, 1))
                .unwrap_err();
            assert!(denied
                .message()
                .contains("does not accept control requests from pane:"));
        }

        #[tokio::test]
        async fn agent_start_into_protected_pane_from_outside_allowed() {
            let mut rig = rig();
            bind(&mut rig, 0, vec![Principal::Outside]);
            assert!(rig
                .app
                .check_access(&method(&rig, 0, None), &outside())
                .is_ok());
        }

        #[tokio::test]
        async fn looser_accept_from_from_pane_is_denied() {
            let mut rig = rig();
            rig.app.state.access_defaults.default = Some(vec![Principal::Outside]);
            for loose in [&["any"][..], &["caller:ci"], &["outside", "caller:ci"]] {
                let denied = rig
                    .app
                    .check_access(&method(&rig, 0, Some(loose)), &from_pane(&rig, 1))
                    .unwrap_err();
                assert!(
                    denied
                        .message()
                        .contains("does not accept control requests from pane:"),
                    "{loose:?}"
                );
                assert!(
                    rig.app
                        .check_access(&method(&rig, 0, Some(loose)), &outside())
                        .is_ok(),
                    "{loose:?}"
                );
            }
        }

        fn from_token(rig: &Rig, pane: usize) -> Caller {
            let binding = rig.app.state.terminals[&rig.terminals[pane]]
                .access
                .as_ref()
                .unwrap()
                .binding
                .clone();
            Caller::Classified(CallerClass::Caller {
                name: "ci".into(),
                target: binding,
                via_ssh: false,
            })
        }

        #[tokio::test]
        async fn caller_token_on_its_own_binding_may_tighten_but_not_loosen() {
            let mut rig = rig();
            bind(&mut rig, 0, vec![Principal::Outside]);
            let own = from_token(&rig, 0);
            for ok in [&["none"][..], &["outside"]] {
                assert!(
                    rig.app
                        .check_access(&method(&rig, 0, Some(ok)), &own)
                        .is_ok(),
                    "{ok:?}"
                );
            }
            assert!(rig.app.check_access(&method(&rig, 0, None), &own).is_ok());
            for loose in [&["any"][..], &["caller:ci"]] {
                let denied = rig
                    .app
                    .check_access(&method(&rig, 0, Some(loose)), &own)
                    .unwrap_err();
                assert!(denied
                    .message()
                    .contains("does not accept control requests from caller:ci"));
            }
            // A token that is not this pane's binding is refused outright.
            bind(&mut rig, 1, vec![Principal::Outside]);
            assert!(rig
                .app
                .check_access(&method(&rig, 1, Some(&["none"])), &own)
                .is_err());
        }

        #[tokio::test]
        async fn looser_than_a_bound_policy_needs_admin_beyond_control_of_the_pane() {
            let mut rig = rig();
            bind(&mut rig, 0, vec![Principal::None]);
            assert!(rig
                .app
                .check_access(&method(&rig, 0, Some(&["outside"])), &outside())
                .is_ok());
            let own = from_token(&rig, 0);
            assert!(rig
                .app
                .check_access(&method(&rig, 0, Some(&["outside"])), &own)
                .is_err());
        }

        #[tokio::test]
        async fn loosening_start_must_pass_the_pane_check_as_well_as_admin() {
            use crate::access::policy::{RemoteInput, RemoteSettings};
            let mut rig = rig();
            rig.app.state.remote_settings = RemoteSettings {
                input: RemoteInput::BridgeOnly,
                ..Default::default()
            };
            rig.app.state.access_defaults.default = Some(vec![Principal::Outside]);
            let via_ssh = Caller::Classified(CallerClass::Outside { via_ssh: true });
            // A plain start into the unbound pane is refused by the remote rule.
            assert!(rig
                .app
                .check_access(&method(&rig, 0, None), &via_ssh)
                .is_err());
            for loose in [&["any"][..], &["caller:ci"]] {
                assert!(
                    rig.app
                        .check_access(&method(&rig, 0, Some(loose)), &via_ssh)
                        .is_err(),
                    "{loose:?}"
                );
            }
            // A local outside caller is still admitted.
            assert!(rig
                .app
                .check_access(&method(&rig, 0, Some(&["any"])), &outside())
                .is_ok());
        }

        #[tokio::test]
        async fn any_without_a_default_needs_admin_and_only_admin_sets_the_flag() {
            let mut rig = rig();
            let terminal = rig.terminals[0].clone();
            let denied = rig
                .app
                .check_access(&method(&rig, 0, Some(&["any"])), &from_pane(&rig, 1))
                .unwrap_err();
            assert!(denied
                .message()
                .contains("does not accept control requests from pane:"));
            assert!(!rig.app.state.terminals[&terminal].access_opened_explicitly);
            assert!(rig
                .app
                .check_access(&method(&rig, 0, Some(&["any"])), &outside())
                .is_ok());
            let response = start(&mut rig, 0, Some(&["any"]));
            assert_eq!(response["result"]["type"], "agent_started");
            assert!(rig.app.state.terminals[&terminal].access_opened_explicitly);
            // Already opened on purpose: `any` is no longer a loosening.
            assert!(rig
                .app
                .check_access(&method(&rig, 0, Some(&["any"])), &from_pane(&rig, 1))
                .is_ok());
        }

        #[tokio::test]
        async fn a_failed_any_send_restores_the_binding() {
            let mut rig = rig();
            let binding = bind(&mut rig, 0, vec![Principal::Outside]);
            let terminal = rig.terminals[0].clone();
            let (runtime, _rx) =
                crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 1);
            runtime
                .try_send_bytes(Bytes::from_static(b"occupied"))
                .unwrap();
            rig.app.terminal_runtimes.insert(terminal.clone(), runtime);
            let response = start(&mut rig, 0, Some(&["any"]));
            assert_eq!(response["error"]["code"], "agent_start_input_failed");
            let kept = policy(&rig, 0).unwrap();
            assert_eq!(kept.binding, binding);
            assert_eq!(kept.accept_from, vec![Principal::Outside]);
            assert!(!rig.app.state.terminals[&terminal].access_opened_explicitly);
        }

        #[tokio::test]
        async fn tighter_accept_from_from_pane_is_allowed() {
            let mut rig = rig();
            rig.app.state.access_defaults.default = Some(vec![Principal::Outside]);
            for tight in [&["none"][..], &["outside"]] {
                assert!(
                    rig.app
                        .check_access(&method(&rig, 0, Some(tight)), &from_pane(&rig, 1))
                        .is_ok(),
                    "{tight:?}"
                );
            }
        }

        // The bridge guard and the input mode.

        fn set_mode(rig: &mut Rig, pane: usize, mode: &str) -> serde_json::Value {
            let response = rig.app.handle_api_request(crate::api::schema::Request {
                id: "mode".into(),
                method: Method::AccessPaneSet(crate::api::schema::AccessPaneSetParams {
                    pane_id: rig.panes[pane].clone(),
                    accept_from: None,
                    input_mode: Some(mode.into()),
                }),
            });
            serde_json::from_str(&response).unwrap()
        }

        fn mode_of(rig: &Rig, pane: usize) -> crate::access::policy::InputMode {
            rig.app.state.terminals[&rig.terminals[pane]].input_mode
        }

        const BRIDGE_MODE_MESSAGE: &str =
            "input mode must stay \"any\" on the bridge (relay), which receives by API by design";

        /// Remote input is bridge-only and the bridge is the agent named `relay`.
        fn bridge_only(rig: &mut Rig) {
            rig.app.state.remote_settings.input = crate::access::policy::RemoteInput::BridgeOnly;
            rig.app.state.remote_settings.bridge_agent = Some("relay".into());
        }

        fn agent_in(rig: &mut Rig, pane: usize, name: &str) {
            let terminal = rig
                .app
                .state
                .terminals
                .get_mut(&rig.terminals[pane])
                .unwrap();
            terminal.set_agent_name(name.into());
            terminal.set_detected_state(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Idle,
            );
        }

        fn start_named(rig: &mut Rig, pane: usize, name: &str) -> serde_json::Value {
            let mut params = params(&rig.panes[pane], None);
            params.name = name.into();
            let response = rig.app.handle_api_request(crate::api::schema::Request {
                id: "req".into(),
                method: Method::AgentStart(params),
            });
            serde_json::from_str(&response).unwrap()
        }

        #[tokio::test]
        async fn access_pane_set_refuses_non_any_mode_on_bridge() {
            let mut rig = rig();
            bridge_only(&mut rig);
            agent_in(&mut rig, 0, "relay");
            agent_in(&mut rig, 1, "worker");

            let response = set_mode(&mut rig, 0, "human-only");
            assert_eq!(response["error"]["code"], "access_bridge_input_mode");
            assert_eq!(response["error"]["message"], BRIDGE_MODE_MESSAGE);
            assert_eq!(mode_of(&rig, 0), crate::access::policy::InputMode::Any);
            // `flag` would refuse for its own reason first; either way the bridge stays `any`.
            assert!(set_mode(&mut rig, 0, "flag")["error"].is_object());
            assert_eq!(mode_of(&rig, 0), crate::access::policy::InputMode::Any);
            // A policy on the bridge is fine, and so is `any`; another agent can be moded.
            let response = set_mode(&mut rig, 0, "any");
            assert_eq!(response["result"]["input_mode"], "any");
            let response = set_mode(&mut rig, 1, "human-only");
            assert_eq!(response["result"]["input_mode"], "human-only");
            assert_eq!(
                mode_of(&rig, 1),
                crate::access::policy::InputMode::HumanOnly
            );

            // Without bridge-only the name means nothing.
            rig.app.state.remote_settings.input = crate::access::policy::RemoteInput::Any;
            let response = set_mode(&mut rig, 0, "human-only");
            assert_eq!(response["result"]["input_mode"], "human-only");
        }

        #[tokio::test]
        async fn agent_start_bridge_into_moded_pane_refused() {
            let mut rig = rig();
            bridge_only(&mut rig);
            set_mode(&mut rig, 0, "human-only");

            let response = start_named(&mut rig, 0, "relay");
            assert_eq!(response["error"]["code"], "access_bridge_input_mode");
            assert_eq!(response["error"]["message"], BRIDGE_MODE_MESSAGE);
            // Nothing happened: no name, no bytes, mode untouched.
            let terminal = &rig.app.state.terminals[&rig.terminals[0]];
            assert!(terminal.agent_name.is_none());
            assert_eq!(
                mode_of(&rig, 0),
                crate::access::policy::InputMode::HumanOnly
            );
            let rx = rig._receivers[0]
                .downcast_mut::<tokio::sync::mpsc::Receiver<Bytes>>()
                .unwrap();
            assert!(rx.try_recv().is_err());

            // The same start into a pane with the default mode is fine, as is another name.
            let response = start_named(&mut rig, 1, "relay");
            assert_eq!(response["result"]["type"], "agent_started", "{response}");
            let response = start_named(&mut rig, 0, "worker");
            assert_eq!(response["result"]["type"], "agent_started", "{response}");
        }

        #[tokio::test]
        async fn rename_to_bridge_on_moded_pane_refused() {
            let mut rig = rig();
            bridge_only(&mut rig);
            agent_in(&mut rig, 0, "worker");
            agent_in(&mut rig, 1, "other");
            set_mode(&mut rig, 0, "human-only");

            let rename = |rig: &mut Rig, target: &str, name: &str| {
                let response = rig.app.handle_api_request(crate::api::schema::Request {
                    id: "rename".into(),
                    method: Method::AgentRename(crate::api::schema::AgentRenameParams {
                        target: target.into(),
                        name: Some(name.into()),
                    }),
                });
                serde_json::from_str::<serde_json::Value>(&response).unwrap()
            };
            let response = rename(&mut rig, "worker", "relay");
            assert_eq!(response["error"]["code"], "access_bridge_input_mode");
            assert_eq!(response["error"]["message"], BRIDGE_MODE_MESSAGE);
            assert_eq!(
                rig.app.state.terminals[&rig.terminals[0]]
                    .agent_name
                    .as_deref(),
                Some("worker")
            );
            // Renaming a pane with the default mode to the bridge works, and so does another name.
            let response = rename(&mut rig, "other", "relay");
            assert_eq!(response["result"]["type"], "agent_info", "{response}");
            let response = rename(&mut rig, "worker", "builder");
            assert_eq!(response["result"]["type"], "agent_info", "{response}");
        }

        #[tokio::test]
        async fn agent_start_binds_the_named_sections_input_mode() {
            let mut rig = rig();
            rig.app
                .state
                .access_defaults
                .mode_by_name
                .insert("worker".into(), crate::access::policy::InputMode::HumanOnly);
            let response = start_named(&mut rig, 0, "worker");
            assert_eq!(response["result"]["type"], "agent_started", "{response}");
            assert_eq!(
                mode_of(&rig, 0),
                crate::access::policy::InputMode::HumanOnly
            );
            // Other names, and the other pane, are untouched.
            assert_eq!(mode_of(&rig, 1), crate::access::policy::InputMode::Any);
        }

        #[tokio::test]
        async fn a_failed_start_send_restores_the_mode() {
            let mut rig = rig();
            rig.app
                .state
                .access_defaults
                .mode_by_name
                .insert("worker".into(), crate::access::policy::InputMode::HumanOnly);
            let terminal = rig.terminals[0].clone();
            let (runtime, _rx) =
                crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 1);
            runtime
                .try_send_bytes(Bytes::from_static(b"occupied"))
                .unwrap();
            rig.app.terminal_runtimes.insert(terminal, runtime);
            let response = start_named(&mut rig, 0, "worker");
            assert_eq!(response["error"]["code"], "agent_start_input_failed");
            assert_eq!(mode_of(&rig, 0), crate::access::policy::InputMode::Any);
        }

        #[tokio::test]
        async fn agent_start_binds_the_named_mode_however_the_policy_is_settled() {
            let mut rig = rig();
            for name in ["worker", "builder"] {
                rig.app
                    .state
                    .access_defaults
                    .mode_by_name
                    .insert(name.into(), crate::access::policy::InputMode::HumanOnly);
            }
            // Pane 0 holds a policy already; pane 1 was opened on purpose.
            bind(&mut rig, 0, vec![Principal::Outside]);
            let opened = rig.terminals[1].clone();
            rig.app.state.unbind_access(&opened);
            let response = start_named(&mut rig, 0, "worker");
            assert_eq!(response["result"]["type"], "agent_started", "{response}");
            let response = start_named(&mut rig, 1, "builder");
            assert_eq!(response["result"]["type"], "agent_started", "{response}");
            assert_eq!(
                mode_of(&rig, 0),
                crate::access::policy::InputMode::HumanOnly
            );
            assert_eq!(
                mode_of(&rig, 1),
                crate::access::policy::InputMode::HumanOnly
            );
            // The policy step still keeps what it had.
            assert_eq!(
                policy(&rig, 0).unwrap().accept_from,
                vec![Principal::Outside]
            );
            assert!(policy(&rig, 1).is_none());
        }

        #[tokio::test]
        async fn a_new_agent_clears_the_explicit_mode_mark() {
            let mut rig = rig();
            set_mode(&mut rig, 0, "any");
            assert!(rig.app.state.terminals[&rig.terminals[0]].input_mode_set_explicitly);
            let response = start_named(&mut rig, 0, "worker");
            assert_eq!(response["result"]["type"], "agent_started", "{response}");
            assert!(!rig.app.state.terminals[&rig.terminals[0]].input_mode_set_explicitly);

            // A failed start puts the mark back.
            set_mode(&mut rig, 1, "any");
            let terminal = rig.terminals[1].clone();
            let (runtime, _rx) =
                crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 1);
            runtime
                .try_send_bytes(Bytes::from_static(b"occupied"))
                .unwrap();
            rig.app.terminal_runtimes.insert(terminal.clone(), runtime);
            let response = start_named(&mut rig, 1, "builder");
            assert_eq!(response["error"]["code"], "agent_start_input_failed");
            assert!(rig.app.state.terminals[&terminal].input_mode_set_explicitly);
        }
    }
}
