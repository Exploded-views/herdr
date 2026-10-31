// herdr-fork: tests for the per-pane access gate in the api request path and for shell-client input. Unofficial stopgap; see FORK.md.
// access gate
use super::*;
use crate::access::caller::Caller;
use crate::access::policy::{AccessBindingId, AccessPolicy, CallerClass, Principal, RemoteInput};
use crate::api::schema::{
    AgentPromptParams, AgentSendKeysParams, AgentStartParams, ErrorResponse, Method,
    PaneAgentState, PaneMoveDestination, PaneMoveParams, PaneReadParams, PaneReportAgentParams,
    PaneSendInputParams, PaneSendKeysParams, PaneSendTextParams,
};
use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::platform::{PeerLineage, ProcessIdentity};
use crate::terminal::TerminalId;

const TAIL: &str = "This is deliberate policy. Do not retry with another herdr command, attach, tmux, or any other route. Use your own messaging channel (e.g. SendMessage) or report to your operator.";

struct Fixture {
    server: HeadlessServer,
    panes: Vec<PaneId>,
    terminals: Vec<TerminalId>,
    inputs: Vec<tokio::sync::mpsc::Receiver<bytes::Bytes>>,
}

/// One workspace of `count` panes. Every pane has a runtime that records the
/// bytes written to it, so a test can prove nothing reached the pane.
fn gate_fixture(count: usize) -> Fixture {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("gate");
    for _ in 1..count {
        workspace.test_split(ratatui::layout::Direction::Horizontal);
    }
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let panes = server.app.state.workspaces[0].tabs[0].layout.pane_ids();
    let mut terminals = Vec::new();
    let mut inputs = Vec::new();
    for pane in &panes {
        let terminal = server.app.state.workspaces[0]
            .terminal_id(*pane)
            .unwrap()
            .clone();
        let (runtime, input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        server
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime);
        terminals.push(terminal);
        inputs.push(input);
    }
    Fixture {
        server,
        panes,
        terminals,
        inputs,
    }
}

impl Fixture {
    fn public(&self, index: usize) -> String {
        self.server
            .app
            .public_pane_id(0, self.panes[index])
            .unwrap()
    }

    fn protect(&mut self, index: usize, accept_from: Vec<Principal>) {
        self.server
            .app
            .state
            .terminals
            .get_mut(&self.terminals[index])
            .unwrap()
            .access = Some(AccessPolicy {
            binding: AccessBindingId::mint().unwrap(),
            accept_from,
        });
    }

    fn make_agent(&mut self, index: usize, name: &str) {
        let terminal = self
            .server
            .app
            .state
            .terminals
            .get_mut(&self.terminals[index])
            .unwrap();
        terminal.set_agent_name(name.into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
    }

    fn pane_caller(&self, index: usize) -> Caller {
        Caller::Classified(CallerClass::Pane(self.terminals[index].clone()))
    }

    fn call(&mut self, caller: Caller, method: Method) -> String {
        send_as(&mut self.server, caller, method)
    }

    fn assert_no_bytes(&mut self, index: usize) {
        assert!(
            self.inputs[index].try_recv().is_err(),
            "bytes reached pane {index}"
        );
    }
}

fn send_as(server: &mut HeadlessServer, caller: Caller, method: Method) -> String {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        caller,
        request: api::schema::Request {
            id: "gate".into(),
            method,
        },
        respond_to,
        response_write_complete: None,
    });
    response_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("api response")
}

fn error_code(response: &str) -> Option<String> {
    serde_json::from_str::<ErrorResponse>(response)
        .ok()
        .map(|error| error.error.code)
}

fn error_message(response: &str) -> String {
    serde_json::from_str::<ErrorResponse>(response)
        .expect("error response")
        .error
        .message
}

fn outside() -> Caller {
    Caller::test_outside()
}

fn send_text(pane_id: &str) -> Method {
    Method::PaneSendText(PaneSendTextParams {
        pane_id: pane_id.into(),
        text: "x".into(),
    })
}

fn send_keys(pane_id: &str) -> Method {
    Method::PaneSendKeys(PaneSendKeysParams {
        pane_id: pane_id.into(),
        keys: vec!["Enter".into()],
    })
}

fn send_input(pane_id: &str) -> Method {
    Method::PaneSendInput(PaneSendInputParams {
        pane_id: pane_id.into(),
        text: "x".into(),
        keys: Vec::new(),
    })
}

fn agent_prompt(target: &str) -> Method {
    Method::AgentPrompt(AgentPromptParams {
        target: target.into(),
        text: "x".into(),
        wait: None,
    })
}

fn agent_send_keys(target: &str) -> Method {
    Method::AgentSendKeys(AgentSendKeysParams {
        target: target.into(),
        keys: vec!["Enter".into()],
    })
}

fn agent_start(pane_id: &str) -> Method {
    Method::AgentStart(AgentStartParams {
        name: "worker".into(),
        kind: "pi".into(),
        pane_id: pane_id.into(),
        args: Vec::new(),
        timeout_ms: None,
        accept_from: None,
    })
}

fn input_denial(pane: &str, from: &str) -> String {
    format!("access_denied: pane {pane} does not accept input from {from}. {TAIL}")
}

fn control_denial(pane: &str, from: &str) -> String {
    format!("access_denied: pane {pane} does not accept control requests from {from}. {TAIL}")
}

/// A peer that walked to pid 1 through processes that are in no pane.
fn peer(via_ssh: bool, remote: bool) -> Caller {
    Caller::Peer {
        lineage: PeerLineage {
            chain: vec![
                ProcessIdentity {
                    pid: 4_000_001,
                    start_token: 11,
                },
                ProcessIdentity {
                    pid: 1,
                    start_token: 1,
                },
            ],
            complete: true,
            via_ssh,
        },
        token: None,
        remote,
    }
}

#[tokio::test]
async fn agent_prompt_rejects_pane_caller_before_sending_input() {
    let mut f = gate_fixture(2);
    f.make_agent(0, "reviewer");
    f.protect(0, vec![Principal::Outside]);
    let caller = f.pane_caller(1);

    let response = f.call(caller, agent_prompt("reviewer"));

    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn every_input_method_denies_uniformly() {
    let mut f = gate_fixture(2);
    f.make_agent(0, "reviewer");
    f.protect(0, vec![Principal::Outside]);
    let (victim, caller_id) = (f.public(0), f.public(1));
    let from = format!("pane:{caller_id}");
    let input_cases = [
        ("agent.prompt", agent_prompt("reviewer")),
        ("agent.send_keys", agent_send_keys("reviewer")),
        ("pane.send_text", send_text(&victim)),
        ("pane.send_keys", send_keys(&victim)),
        ("pane.send_input", send_input(&victim)),
    ];
    for (name, method) in input_cases {
        let caller = f.pane_caller(1);
        let response = f.call(caller, method);
        assert_eq!(
            error_code(&response).as_deref(),
            Some("access_denied"),
            "{name}"
        );
        assert_eq!(
            error_message(&response),
            input_denial(&victim, &from),
            "{name}"
        );
    }
    // Starting an agent in a protected pane is a control request.
    let caller = f.pane_caller(1);
    let response = f.call(caller, agent_start(&victim));
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    assert_eq!(error_message(&response), control_denial(&victim, &from));
    f.assert_no_bytes(0);

    // Every one of these is a method the Input class lists.
    let listed: Vec<_> = Method::OPERATIONS
        .iter()
        .filter(|operation| operation.access == crate::api::schema::AccessClass::Input)
        .map(|operation| operation.name)
        .collect();
    assert_eq!(listed.len(), 6, "{listed:?}");
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn denial_for_a_pane_caller_names_the_public_pane_id() {
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let victim = f.public(0);
    let caller = f.pane_caller(1);

    let response = f.call(caller, send_text(&victim));

    let message = error_message(&response);
    assert!(
        message.contains(&format!("from pane:{}.", f.public(1))),
        "{message}"
    );
    assert!(!message.contains(f.terminals[1].as_str()), "{message}");
    assert!(!message.contains(f.terminals[0].as_str()), "{message}");
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn agent_state_hook_reports_own_pane_when_protected() {
    let mut f = gate_fixture(2);
    f.protect(1, vec![Principal::Outside]);
    let own = f.public(1);
    let caller = f.pane_caller(1);

    let response = f.call(
        caller,
        Method::PaneReportAgent(PaneReportAgentParams {
            pane_id: own,
            source: "prime-agent".into(),
            agent: "prime-agent".into(),
            state: PaneAgentState::Idle,
            message: None,
            seq: None,
            agent_session_id: None,
            agent_session_path: None,
            resume_argv: None,
        }),
    );

    assert_ne!(
        error_code(&response).as_deref(),
        Some("access_denied"),
        "report_agent stays Open in v1: {response}"
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn orchestrator_pane_can_start_worker_with_default_policy() {
    let mut f = gate_fixture(3);
    // The default policy is `outside`: panes holding agents refuse other panes.
    f.server.app.state.access_defaults.default = Some(vec![Principal::Outside]);
    f.make_agent(1, "orchestrator");
    let orchestrator = f.terminals[1].clone();
    f.server.app.state.bind_default_access(&orchestrator);
    assert!(f.server.app.state.terminals[&orchestrator].access.is_some());
    let fresh = f.public(0);
    let caller = f.pane_caller(1);

    // A fresh pane has no agent, so no policy yet: the orchestrator may start into it.
    let response = f.call(caller, agent_start(&fresh));
    assert_ne!(
        error_code(&response).as_deref(),
        Some("access_denied"),
        "{response}"
    );

    // A pane that already holds a policy is control-protected from it.
    f.protect(2, vec![Principal::Outside]);
    let guarded = f.public(2);
    let caller = f.pane_caller(1);
    let response = f.call(caller, agent_start(&guarded));
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn gate_uses_handler_resolver_for_agent_targets() {
    let mut f = gate_fixture(2);
    f.make_agent(0, "reviewer");
    f.protect(0, vec![Principal::Outside]);
    let by_public_id = f.public(0);
    let by_terminal_id = f.terminals[0].to_string();
    // The agent resolver does not accept a terminal id; the handlers answer
    // not-found, so the gate must not resolve it either.
    for (name, build) in [
        ("agent.prompt", agent_prompt as fn(&str) -> Method),
        ("agent.send_keys", agent_send_keys as fn(&str) -> Method),
    ] {
        let caller = f.pane_caller(1);
        let response = f.call(caller, build("reviewer"));
        assert_eq!(
            error_code(&response).as_deref(),
            Some("access_denied"),
            "{name} by name"
        );
        let caller = f.pane_caller(1);
        let response = f.call(caller, build(&by_public_id));
        assert_eq!(
            error_code(&response).as_deref(),
            Some("access_denied"),
            "{name} by public id"
        );
        let caller = f.pane_caller(1);
        let response = f.call(caller, build(&by_terminal_id));
        assert_ne!(
            error_code(&response).as_deref(),
            Some("access_denied"),
            "{name} by terminal id is the handler's not-found: {response}"
        );
    }
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn pane_send_text_via_legacy_p_id_is_gated() {
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let legacy = format!("p_{}", f.panes[0].raw());
    assert_ne!(legacy, f.public(0));
    let caller = f.pane_caller(1);

    let response = f.call(caller, send_text(&legacy));

    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn pane_send_text_via_legacy_ws_p_id_is_gated() {
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let legacy = format!("p_1_{}", f.panes[0].raw());
    assert_ne!(legacy, f.public(0));
    let caller = f.pane_caller(1);

    let response = f.call(caller, send_text(&legacy));

    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn pane_send_input_via_dash_id_is_gated() {
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let number = f.server.app.state.workspaces[0].public_pane_numbers[&f.panes[0]];
    let dashed = format!("1-{number}");
    assert_ne!(dashed, f.public(0));
    let caller = f.pane_caller(1);

    let response = f.call(caller, send_input(&dashed));

    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn pane_send_keys_via_moved_alias_is_gated() {
    let mut f = gate_fixture(2);
    // A second tab keeps the moved pane's source tab alive; a second
    // workspace is where the pane goes.
    f.server.app.state.workspaces[0].test_add_tab(Some("remaining"));
    f.server.app.state.workspaces[0].switch_tab(0);
    f.server
        .app
        .state
        .workspaces
        .push(crate::workspace::Workspace::test_new("destination"));
    f.server.app.state.ensure_test_terminals();
    let old_id = f.public(0);
    let destination = f.server.app.public_workspace_id(1);

    let moved = f.call(
        outside(),
        Method::PaneMove(PaneMoveParams {
            pane_id: old_id.clone(),
            destination: PaneMoveDestination::NewTab {
                workspace_id: Some(destination),
                label: None,
            },
            focus: false,
        }),
    );
    assert!(error_code(&moved).is_none(), "{moved}");
    assert!(
        f.server
            .app
            .state
            .public_pane_id_aliases
            .contains_key(&old_id),
        "the move should leave an alias for {old_id}"
    );
    f.protect(0, vec![Principal::Outside]);
    let caller = f.pane_caller(1);

    let response = f.call(caller, send_keys(&old_id));

    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn defaults_leave_behaviour_unchanged() {
    let mut f = gate_fixture(2);
    let target = f.public(0);
    let before = crate::app::access_gate::classify_calls();

    // Rule 3: no policy, so a pane may type into another pane, as upstream does.
    let caller = f.pane_caller(1);
    let response = f.call(caller, send_text(&target));
    assert!(error_code(&response).is_none(), "{response}");
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"x");

    // R1-R4 (remote input left at `any`): the same request from each remote
    // route is also upstream behaviour.
    for (route, caller) in [
        ("R1 stamped api bridge", peer(false, true)),
        ("R2 client bridge under sshd", peer(true, true)),
        ("R3 forced command under sshd", peer(true, false)),
        ("R4 other ssh login", peer(true, false)),
        ("local cli", peer(false, false)),
    ] {
        let response = f.call(caller, send_text(&target));
        assert!(error_code(&response).is_none(), "{route}: {response}");
        assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"x", "{route}");
    }

    // Rule 4: agent.start into an open pane passes the gate (the handler
    // answers; the pane is a test pane, so it may refuse for its own reasons).
    let caller = f.pane_caller(1);
    let response = f.call(caller, agent_start(&target));
    assert_ne!(
        error_code(&response).as_deref(),
        Some("access_denied"),
        "{response}"
    );

    assert_eq!(
        crate::app::access_gate::classify_calls(),
        before,
        "the off path must not classify"
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn bridge_only_enforced_with_no_bound_policy() {
    let mut f = gate_fixture(2);
    f.server.app.state.remote_settings.input = RemoteInput::BridgeOnly;
    f.server.app.state.remote_settings.bridge_agent = Some("relay".into());
    let target = f.public(0);
    assert!(!f.server.app.any_access_protected());

    for (route, caller) in [
        ("stamped remote", peer(false, true)),
        ("outside over ssh", peer(true, false)),
    ] {
        let response = f.call(caller, send_text(&target));
        assert_eq!(
            error_code(&response).as_deref(),
            Some("access_denied"),
            "{route}"
        );
        let message = error_message(&response);
        assert!(
            message.contains("does not accept input from another host"),
            "{message}"
        );
        assert!(message.contains("relay"), "{message}");
        f.assert_no_bytes(0);
    }

    // agent.start into an open pane is Input, so the rule applies to it too.
    let response = f.call(peer(true, true), agent_start(&target));
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    assert!(error_message(&response).contains("another host"));

    // A local caller is untouched.
    let response = f.call(peer(false, false), send_text(&target));
    assert!(error_code(&response).is_none(), "{response}");
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"x");
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn restored_policy_enforced_on_first_request() {
    let mut f = gate_fixture(2);
    let cwd = std::env::current_dir().unwrap();
    for terminal in &f.terminals {
        f.server.app.state.terminals.get_mut(terminal).unwrap().cwd = cwd.clone();
    }
    f.protect(0, vec![Principal::Outside]);
    let snapshot = crate::persist::capture(
        &f.server.app.state.workspaces,
        &f.server.app.state.terminals,
        &crate::terminal::TerminalRuntimeRegistry::new(),
        f.server.app.state.active,
        0,
    );
    let json = serde_json::to_string(&snapshot).unwrap();
    let snapshot: crate::persist::SessionSnapshot = serde_json::from_str(&json).unwrap();
    let (events, _rx) = tokio::sync::mpsc::channel(4);
    let (workspaces, terminals, runtimes) = crate::persist::restore(
        &snapshot,
        None,
        24,
        80,
        0,
        "/bin/sh",
        crate::config::ShellModeConfig::NonLogin,
        false,
        events,
        std::sync::Arc::new(tokio::sync::Notify::new()),
        std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
    );
    drop(runtimes);

    // A fresh server that only knows what the restore handed it.
    let mut server = test_headless_server();
    server.app.state.workspaces = workspaces;
    server.app.state.terminals = terminals;
    server.app.state.active = Some(0);
    let panes = server.app.state.workspaces[0].tabs[0].layout.pane_ids();
    let mut inputs = Vec::new();
    let mut ids = Vec::new();
    for pane in &panes {
        let terminal = server.app.state.workspaces[0]
            .terminal_id(*pane)
            .unwrap()
            .clone();
        let (runtime, input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        server
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime);
        ids.push((terminal, server.app.public_pane_id(0, *pane).unwrap()));
        inputs.push(input);
    }
    let protected = ids
        .iter()
        .position(|(terminal, _)| server.app.state.terminals[terminal].access.is_some())
        .expect("one restored pane holds the policy");
    let other = 1 - protected;

    let response = send_as(
        &mut server,
        Caller::Classified(CallerClass::Pane(ids[other].0.clone())),
        send_text(&ids[protected].1),
    );

    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    assert!(inputs[protected].try_recv().is_err());
    shutdown_test_runtimes(&mut server);
}

// Reads never inject input past the gate.

fn alt_screen_fixture() -> Fixture {
    let mut f = gate_fixture(2);
    f.make_agent(0, "reviewer");
    // A full-screen agent that asked for mouse reports: the read would
    // harvest its history by sending wheel events into it.
    let terminal = f.terminals[0].clone();
    let (runtime, input) = crate::terminal::TerminalRuntime::test_with_channel(20, 5);
    runtime.test_process_pty_bytes(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[2J\x1b[Hone\r\ntwo");
    f.server.app.terminal_runtimes.insert(terminal, runtime);
    f.inputs[0] = input;
    f
}

fn interactive_read(pane_id: &str) -> Method {
    Method::PaneRead(PaneReadParams {
        pane_id: pane_id.into(),
        source: api::schema::ReadSource::Recent,
        lines: Some(50),
        format: api::schema::ReadFormat::Text,
        strip_ansi: true,
        intent: api::schema::ReadIntent::Interactive,
    })
}

#[tokio::test]
async fn alt_screen_read_starts_a_wheel_harvest_when_input_is_allowed() {
    let mut f = alt_screen_fixture();
    let target = f.public(0);
    let caller = f.pane_caller(1);

    // Control: with no policy, the harvest starts (its response is deferred).
    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    f.server
        .handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            caller,
            request: api::schema::Request {
                id: "read".into(),
                method: interactive_read(&target),
            },
            respond_to,
            response_write_complete: None,
        });
    assert_eq!(f.server.pending_alt_screen_reads.len(), 1);
    f.server
        .poll_pending_alt_screen_reads(Instant::now() + Duration::from_millis(50));
    assert!(
        f.inputs[0].try_recv().is_ok(),
        "the harvest sends wheel bytes"
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn read_of_protected_alt_screen_pane_sends_no_bytes_to_a_denied_caller() {
    let mut f = alt_screen_fixture();
    f.protect(0, vec![Principal::Outside]);
    let target = f.public(0);
    let caller = f.pane_caller(1);

    let response = f.call(caller, interactive_read(&target));

    // The read is answered from the screen as it is: no harvest, no bytes.
    assert!(error_code(&response).is_none(), "{response}");
    assert!(f.server.pending_alt_screen_reads.is_empty());
    f.server
        .poll_pending_alt_screen_reads(Instant::now() + Duration::from_millis(50));
    f.assert_no_bytes(0);

    // An allowed caller on the same pane still gets the harvest.
    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    f.server
        .handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            caller: outside(),
            request: api::schema::Request {
                id: "read".into(),
                method: interactive_read(&target),
            },
            respond_to,
            response_write_complete: None,
        });
    assert_eq!(f.server.pending_alt_screen_reads.len(), 1);
    shutdown_test_runtimes(&mut f.server);
}

// Shell-endpoint requests carry their client's own caller.

fn connect_shell_with_lineage(server: &mut HeadlessServer, client_id: u64, lineage: PeerLineage) {
    let (writer, _control_rx, _render_rx) = test_client_writer();
    server.handle_server_event(ServerEvent::ClientShellConnected {
        client_id,
        surface_cols: 80,
        surface_rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        direct_graphics: false,
        endpoint_keybindings: false,
        mouse_capture: false,
        surface_active: true,
        surface_reuse: false,
        surface_delta: false,
        surface_scroll: false,
        peer_lineage: lineage,
        writer,
    });
}

#[tokio::test]
async fn shell_endpoint_request_carries_its_clients_stored_caller() {
    let mut server = test_headless_server();
    let remote_lineage = PeerLineage {
        chain: vec![ProcessIdentity {
            pid: 4_000_002,
            start_token: 22,
        }],
        complete: true,
        via_ssh: true,
    };
    let local_lineage = PeerLineage {
        chain: vec![ProcessIdentity {
            pid: 4_000_003,
            start_token: 33,
        }],
        complete: true,
        via_ssh: false,
    };
    connect_shell_with_lineage(&mut server, 71, remote_lineage.clone());
    connect_shell_with_lineage(&mut server, 72, local_lineage.clone());

    let request = |id: &str| api::schema::Request {
        id: id.into(),
        method: Method::Ping(api::schema::PingParams {}),
    };
    let (respond_to, _rx) = std::sync::mpsc::channel();
    let remote = server
        .shell_endpoint_message(71, request("a"), respond_to.clone())
        .expect("client 71");
    let local = server
        .shell_endpoint_message(72, request("b"), respond_to)
        .expect("client 72");

    // The `remote` stamp is a separate signal (`--machine`, a bridge's origin); an SSH
    // login travels as `via_ssh` on the lineage and is not stamped. Nothing defaults to local.
    assert_eq!(
        remote.caller,
        Caller::Peer {
            lineage: remote_lineage,
            token: None,
            remote: false
        }
    );
    assert_eq!(
        local.caller,
        Caller::Peer {
            lineage: local_lineage,
            token: None,
            remote: false
        }
    );
    assert_ne!(
        remote.caller,
        Caller::Peer {
            lineage: PeerLineage::default(),
            token: None,
            remote: false
        }
    );
    assert!(server
        .shell_endpoint_message(99, request("c"), std::sync::mpsc::channel().0)
        .is_none());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_client_with_no_lineage_is_never_a_trusted_local_caller() {
    let mut server = test_headless_server();
    connect_shell_with_lineage(&mut server, 73, PeerLineage::default());
    let message = server
        .shell_endpoint_message(
            73,
            api::schema::Request {
                id: "a".into(),
                method: Method::Ping(api::schema::PingParams {}),
            },
            std::sync::mpsc::channel().0,
        )
        .expect("client 73");
    // An empty lineage classifies as unknown, never as outside.
    assert_eq!(
        server.app.classify_caller(&message.caller),
        CallerClass::Unknown { remote_hint: false }
    );
    shutdown_test_runtimes(&mut server);
}

// The gate decides again right before dispatch.

#[tokio::test]
async fn policy_bound_during_drain_is_enforced_before_dispatch() {
    let mut f = gate_fixture(2);
    f.server.app.state.access_defaults.default = Some(vec![Principal::Outside]);
    let target = f.public(0);
    let caller = f.pane_caller(1);
    // Pane 0 runs no agent yet, so at the early check the target does not
    // resolve and nothing is protected. The detection is still queued.
    f.server
        .app
        .event_tx
        .try_send(AppEvent::AgentProcessDetected {
            pane_id: f.panes[0],
            agent: Agent::Pi,
            observed_at: Instant::now(),
        })
        .unwrap();

    let response = f.call(caller, agent_prompt(&target));

    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    assert!(error_message(&response).contains("does not accept input"));
    assert!(f.server.app.state.terminals[&f.terminals[0]]
        .access
        .is_some());
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn positional_pane_id_shift_during_drain_is_rechecked() {
    let mut server = test_headless_server();
    // Four single-pane workspaces: A, B (about to die), C (open), D (protected).
    server.app.state.workspaces = ["a", "b", "c", "d"]
        .iter()
        .map(|name| crate::workspace::Workspace::test_new(name))
        .collect();
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let mut panes = Vec::new();
    let mut terminals = Vec::new();
    let mut inputs = Vec::new();
    for ws in &server.app.state.workspaces {
        let pane = ws.tabs[0].layout.pane_ids()[0];
        panes.push(pane);
        terminals.push(ws.terminal_id(pane).unwrap().clone());
    }
    for terminal in &terminals {
        let (runtime, input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        server
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime);
        inputs.push(input);
    }
    server
        .app
        .state
        .terminals
        .get_mut(&terminals[3])
        .unwrap()
        .access = Some(AccessPolicy {
        binding: AccessBindingId::mint().unwrap(),
        accept_from: vec![Principal::Outside],
    });
    // "3-1" is the open workspace C at the early check. B's death closes its
    // workspace, so after the drain "3-1" names D.
    server
        .app
        .event_tx
        .try_send(AppEvent::PaneDied {
            pane_id: panes[1],
            exit_reason: crate::platform::ChildExitReason::Exited,
        })
        .unwrap();

    let response = send_as(
        &mut server,
        Caller::Classified(CallerClass::Pane(terminals[0].clone())),
        send_text("3-1"),
    );

    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    assert_eq!(server.app.state.workspaces.len(), 3);
    for (index, input) in inputs.iter_mut().enumerate() {
        assert!(input.try_recv().is_err(), "bytes reached workspace {index}");
    }
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_policy_bound_mid_harvest_stops_the_wheel_bytes() {
    let mut f = alt_screen_fixture();
    let target = f.public(0);
    let caller = f.pane_caller(1);
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    f.server
        .handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            caller,
            request: api::schema::Request {
                id: "read".into(),
                method: interactive_read(&target),
            },
            respond_to,
            response_write_complete: None,
        });
    assert_eq!(f.server.pending_alt_screen_reads.len(), 1);
    let start = Instant::now();
    f.server
        .poll_pending_alt_screen_reads(start + Duration::from_millis(50));
    assert!(
        f.inputs[0].try_recv().is_ok(),
        "the harvest sends wheel bytes while it is allowed"
    );
    while f.inputs[0].try_recv().is_ok() {}

    // Agent detection binds a policy that refuses the reader, mid-harvest.
    f.protect(0, vec![Principal::Outside]);
    f.server
        .poll_pending_alt_screen_reads(start + Duration::from_millis(500));

    assert!(f.server.pending_alt_screen_reads.is_empty());
    f.assert_no_bytes(0);
    assert!(
        response_rx.try_recv().is_ok(),
        "the reader still gets the frozen snapshot"
    );
    shutdown_test_runtimes(&mut f.server);
}

// ---- access.pane.* and caller tokens through the whole request path ----

fn token_peer(token: &str) -> Caller {
    let Caller::Peer {
        lineage, remote, ..
    } = peer(false, false)
    else {
        unreachable!()
    };
    Caller::Peer {
        lineage,
        token: Some(token.to_string()),
        remote,
    }
}

fn pane_set(pane_id: &str, accept_from: &[&str]) -> Method {
    Method::AccessPaneSet(crate::api::schema::AccessPaneSetParams {
        pane_id: pane_id.into(),
        accept_from: accept_from.iter().map(|s| s.to_string()).collect(),
    })
}

fn pane_get(pane_id: &str) -> Method {
    Method::AccessPaneGet(crate::api::schema::AccessPaneGetParams {
        pane_id: pane_id.into(),
    })
}

fn caller_name(name: &str) -> crate::api::schema::AccessCallerNameParams {
    crate::api::schema::AccessCallerNameParams { name: name.into() }
}

/// The gate let an agent request through and its target resolved to a pane.
fn reached_its_pane(response: &str) -> bool {
    !matches!(
        error_code(response).as_deref(),
        Some("access_denied" | "agent_not_found")
    )
}

fn json(response: &str) -> serde_json::Value {
    serde_json::from_str(response).expect("json response")
}

impl Fixture {
    /// Set a pane's policy through the API, as an outside admin.
    fn set_policy(&mut self, index: usize, accept_from: &[&str]) -> serde_json::Value {
        let pane = self.public(index);
        let response = json(&self.call(outside(), pane_set(&pane, accept_from)));
        assert_eq!(response["result"]["type"], "access_policy", "{response}");
        response
    }

    fn mint_token(&mut self, name: &str, index: usize) -> String {
        let pane = self.public(index);
        let response = json(&self.call(
            outside(),
            Method::AccessCallerAdd(crate::api::schema::AccessCallerAddParams {
                name: name.into(),
                target_pane: pane,
            }),
        ));
        assert_eq!(
            response["result"]["type"], "access_caller_token",
            "{response}"
        );
        response["result"]["token"].as_str().unwrap().to_string()
    }
}

/// Pane 0 is the bridge (accepts `caller:ops`), pane 1 an ordinary agent
/// (accepts `outside`). Returns the fixture, `ops`'s token and both binding ids.
fn bridge_fixture() -> (Fixture, String, String, String) {
    let mut f = gate_fixture(2);
    f.make_agent(0, "bridge");
    f.make_agent(1, "other");
    let bridge = f.set_policy(0, &["caller:ops"])["result"]["binding"]
        .as_str()
        .unwrap()
        .to_string();
    let other = f.set_policy(1, &["outside"])["result"]["binding"]
        .as_str()
        .unwrap()
        .to_string();
    let token = f.mint_token("ops", 0);
    (f, token, bridge, other)
}

#[tokio::test]
async fn caller_token_reaches_bridge_only() {
    let (mut f, token, bridge_binding, other_binding) = bridge_fixture();
    let (bridge, other) = (f.public(0), f.public(1));

    // The bridge takes it, by pane id and by binding id.
    let response = f.call(token_peer(&token), send_text(&bridge));
    assert!(error_code(&response).is_none(), "{response}");
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"x");
    // The bridge is an agent terminal, so its binding id resolves and the keys land.
    let response = f.call(token_peer(&token), agent_send_keys(&bridge_binding));
    assert!(reached_its_pane(&response), "{response}");
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"\r");

    // Nothing else does, though that pane accepts outside callers; no byte lands.
    let response = f.call(token_peer(&token), send_text(&other));
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    assert_eq!(error_message(&response), input_denial(&other, "caller:ops"));
    assert!(!response.contains(&token));
    let response = f.call(token_peer(&token), agent_send_keys(&other_binding));
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    // By name, too: a token holder is confined, whatever the route.
    let response = f.call(token_peer(&token), agent_send_keys("other"));
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    f.assert_no_bytes(1);

    // Holding a token is not holding admin rights: it cannot change any policy.
    for pane in [&bridge, &other] {
        let response = f.call(token_peer(&token), pane_set(pane, &["any"]));
        assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
        assert!(
            error_message(&response).contains("does not accept control requests from caller:ops")
        );
    }
    assert!(f.server.app.state.terminals[&f.terminals[0]]
        .access
        .is_some());
    assert!(f.server.app.state.terminals[&f.terminals[1]]
        .access
        .is_some());

    // An outside caller without the token reaches the other pane and not the bridge.
    let response = f.call(outside(), send_text(&other));
    assert!(error_code(&response).is_none(), "{response}");
    assert_eq!(f.inputs[1].try_recv().unwrap().as_ref(), b"x");
    let response = f.call(outside(), send_text(&bridge));
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn unknown_token_from_outside_is_denied_by_bridge() {
    let (mut f, token, bridge_binding, _) = bridge_fixture();
    let bridge = f.public(0);
    let outside_denial = input_denial(&bridge, "outside");

    // A token nobody holds is an outside caller, and the bridge takes no outside callers.
    for stranger in ["0".repeat(64), String::new(), token[..63].to_string()] {
        for method in [send_text(&bridge), agent_send_keys(&bridge_binding)] {
            let response = f.call(token_peer(&stranger), method);
            assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
            assert_eq!(error_message(&response), outside_denial);
        }
    }
    f.assert_no_bytes(0);

    // The real one still works.
    let response = f.call(token_peer(&token), send_text(&bridge));
    assert!(error_code(&response).is_none(), "{response}");
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"x");

    // A rotated token's predecessor, then a removed caller's token, stop working.
    let rotated = json(&f.call(outside(), Method::AccessCallerRotate(caller_name("ops"))))
        ["result"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let response = f.call(token_peer(&token), send_text(&bridge));
    assert_eq!(error_message(&response), outside_denial);
    let response = f.call(token_peer(&rotated), send_text(&bridge));
    assert!(error_code(&response).is_none(), "{response}");
    assert!(f.inputs[0].try_recv().is_ok());
    let removed = json(&f.call(outside(), Method::AccessCallerRemove(caller_name("ops"))));
    assert_eq!(removed["result"]["type"], "ok", "{removed}");
    let response = f.call(token_peer(&rotated), send_text(&bridge));
    assert_eq!(error_message(&response), outside_denial);
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn retarget_moves_caller_to_new_binding() {
    let (mut f, token, bridge_binding, other_binding) = bridge_fixture();
    let (bridge, other) = (f.public(0), f.public(1));
    let retarget = |pane: &str| {
        Method::AccessCallerRetarget(crate::api::schema::AccessCallerRetargetParams {
            name: "ops".into(),
            target_pane: pane.into(),
        })
    };

    // A pane with no policy cannot be a target, and the caller stays put.
    f.call(outside(), pane_set(&other, &["any"]));
    let refused = json(&f.call(outside(), retarget(&other)));
    assert_eq!(refused["error"]["code"], "no_access_policy", "{refused}");
    let response = f.call(token_peer(&token), send_text(&bridge));
    assert!(error_code(&response).is_none(), "{response}");
    assert!(f.inputs[0].try_recv().is_ok());

    let rebound = f.set_policy(1, &["outside", "caller:ops"])["result"]["binding"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(
        rebound, other_binding,
        "an opened pane gets a fresh binding"
    );
    let moved = json(&f.call(outside(), retarget(&other)));
    assert_eq!(moved["result"]["type"], "ok", "{moved}");

    // The same token now reaches the new pane (by id and by binding), not the old one.
    let response = f.call(token_peer(&token), send_text(&other));
    assert!(error_code(&response).is_none(), "{response}");
    assert_eq!(f.inputs[1].try_recv().unwrap().as_ref(), b"x");
    let response = f.call(token_peer(&token), agent_send_keys(&rebound));
    assert!(reached_its_pane(&response), "{response}");
    for method in [send_text(&bridge), agent_send_keys(&bridge_binding)] {
        let response = f.call(token_peer(&token), method);
        assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    }
    f.assert_no_bytes(0);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn access_pane_set_refused_from_pane_even_when_unprotected() {
    // Nothing is protected, so the gate would otherwise be idle.
    let mut f = gate_fixture(2);
    let (victim, from) = (f.public(0), f.public(1));
    assert!(!f.server.app.any_access_protected());
    let denial = |from: &str| {
        format!("access_denied: pane server does not accept control requests from {from}. {TAIL}")
    };
    for accept_from in [&["any"][..], &["none"], &["outside"]] {
        let caller = f.pane_caller(1);
        let response = f.call(caller, pane_set(&victim, accept_from));
        assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
        assert_eq!(error_message(&response), denial(&format!("pane:{from}")));
    }
    // A remote host is refused as well.
    let response = f.call(peer(false, true), pane_set(&victim, &["none"]));
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    // Nothing changed: no policy, and the pane was not "opened on purpose".
    let state = &f.server.app.state.terminals[&f.terminals[0]];
    assert!(state.access.is_none() && !state.access_opened_explicitly);
    assert!(!f.server.app.any_access_protected());

    // Reading is open to a pane; writing is for outside callers.
    let caller = f.pane_caller(1);
    let got = json(&f.call(caller, pane_get(&victim)));
    assert_eq!(got["result"]["accept_from"], serde_json::json!(["any"]));
    f.set_policy(0, &["outside"]);
    let caller = f.pane_caller(1);
    let got = json(&f.call(caller, pane_get(&victim)));
    assert_eq!(got["result"]["accept_from"], serde_json::json!(["outside"]));
    // ...and once protected, a pane still cannot loosen it.
    let caller = f.pane_caller(1);
    let response = f.call(caller, pane_set(&victim, &["any"]));
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    let state = &f.server.app.state.terminals[&f.terminals[0]];
    assert!(state.access.is_some());
    shutdown_test_runtimes(&mut f.server);
}

// Shell-client pane input and clipboard-image paste are checked against the pane's policy,
// with the client's own caller class.

fn lineage_for(via_ssh: bool, complete: bool) -> PeerLineage {
    PeerLineage {
        chain: vec![
            ProcessIdentity {
                pid: 4_000_001,
                start_token: 11,
            },
            ProcessIdentity {
                pid: 1,
                start_token: 1,
            },
        ],
        complete,
        via_ssh,
    }
}

/// Connect a shell client and keep its control channel, to read the notices it gets.
fn connect_shell_client(
    server: &mut HeadlessServer,
    client_id: u64,
    lineage: PeerLineage,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (writer, control_rx, _render_rx) = test_client_writer();
    server.handle_server_event(ServerEvent::ClientShellConnected {
        client_id,
        surface_cols: 80,
        surface_rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        direct_graphics: false,
        endpoint_keybindings: false,
        mouse_capture: false,
        surface_active: true,
        surface_reuse: false,
        surface_delta: false,
        surface_scroll: false,
        peer_lineage: lineage,
        writer,
    });
    control_rx
}

fn shell_errors(control_rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<String> {
    // The test writer hands messages over on its own thread, so wait briefly for them.
    let mut errors = Vec::new();
    while let Ok(bytes) = control_rx.recv_timeout(Duration::from_millis(150)) {
        if let ServerMessage::ClientShellError { message } = read_server_message(bytes) {
            errors.push(message);
        }
    }
    errors
}

fn type_into(server: &mut HeadlessServer, client_id: u64, pane_id: &str) {
    server.handle_server_event(ServerEvent::ClientShellPaneInput {
        client_id,
        pane_id: pane_id.into(),
        events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
            "typed".into(),
        )],
    });
}

#[tokio::test]
async fn shell_client_pane_input_to_protected_pane_refused() {
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let target = f.public(0);
    // An empty lineage proves nothing, so the client is unknown and not admitted.
    let notices = connect_shell_client(&mut f.server, 81, PeerLineage::default());

    type_into(&mut f.server, 81, &target);

    f.assert_no_bytes(0);
    assert_eq!(
        shell_errors(&notices),
        vec![input_denial(&target, "unknown")]
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn client_connected_before_activation_is_checked() {
    let mut f = gate_fixture(2);
    let target = f.public(0);
    let notices = connect_shell_client(&mut f.server, 82, lineage_for(false, true));

    // Nothing is protected yet, so the typing goes through.
    type_into(&mut f.server, 82, &target);
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"typed");

    // The policy is bound after the client connected; the next input is checked.
    f.protect(0, vec![Principal::Caller("ops".into())]);
    type_into(&mut f.server, 82, &target);

    f.assert_no_bytes(0);
    assert_eq!(
        shell_errors(&notices),
        vec![input_denial(&target, "outside")]
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn outside_shell_client_types_into_protected_pane() {
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let target = f.public(0);
    let notices = connect_shell_client(&mut f.server, 83, lineage_for(false, true));

    type_into(&mut f.server, 83, &target);

    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"typed");
    assert!(shell_errors(&notices).is_empty());
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn shell_client_pane_input_to_open_pane_allowed() {
    let mut f = gate_fixture(2);
    // Pane 1 is protected, so the gate is live; pane 0 holds no policy.
    f.protect(1, vec![Principal::None]);
    let open = f.public(0);
    let notices = connect_shell_client(&mut f.server, 84, PeerLineage::default());

    type_into(&mut f.server, 84, &open);

    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"typed");
    assert!(shell_errors(&notices).is_empty());
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn shell_client_clipboard_image_to_protected_pane_refused() {
    // Calls the paste routing directly: the event handler stages a file on disk first, and the
    // gate sits in the routing step that follows.
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let target = f.public(0);
    let notices = connect_shell_client(&mut f.server, 85, PeerLineage::default());

    let routed = f.server.paste_client_clipboard_image_path(
        85,
        crate::protocol::ClientClipboardImageTarget::Pane(target.clone()),
        "/tmp/client-image.png".into(),
    );

    assert!(!routed);
    f.assert_no_bytes(0);
    assert_eq!(
        shell_errors(&notices),
        vec![input_denial(&target, "unknown")]
    );

    // The same paste from an outside client reaches the pane.
    let _notices = connect_shell_client(&mut f.server, 86, lineage_for(false, true));
    assert!(f.server.paste_client_clipboard_image_path(
        86,
        crate::protocol::ClientClipboardImageTarget::Pane(target),
        "/tmp/client-image.png".into(),
    ));
    assert_eq!(
        f.inputs[0].try_recv().unwrap().as_ref(),
        b"/tmp/client-image.png"
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn client_shell_api_request_uses_client_caller() {
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let target = f.public(0);
    let _unknown = connect_shell_client(&mut f.server, 87, PeerLineage::default());
    let _outside = connect_shell_client(&mut f.server, 88, lineage_for(false, true));

    let call_as_client = |f: &mut Fixture, client_id: u64| {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let message = f
            .server
            .shell_endpoint_message(
                client_id,
                api::schema::Request {
                    id: "shell".into(),
                    method: send_text(&target),
                },
                respond_to,
            )
            .expect("client");
        f.server.handle_api_request_with_shutdown_check(message);
        response_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("api response")
    };

    // The request is judged as the client that sent it, not as a default caller.
    let response = call_as_client(&mut f, 87);
    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    assert_eq!(error_message(&response), input_denial(&target, "unknown"));
    f.assert_no_bytes(0);
    let response = call_as_client(&mut f, 88);
    assert!(error_code(&response).is_none(), "{response}");
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"x");
    shutdown_test_runtimes(&mut f.server);
}

fn bridge_only(f: &mut Fixture, owner_tui: crate::access::policy::RemoteOwnerTui) {
    f.server.app.state.remote_settings.input = RemoteInput::BridgeOnly;
    f.server.app.state.remote_settings.bridge_agent = Some("relay".into());
    f.server.app.state.remote_owner_tui = owner_tui;
}

#[tokio::test]
async fn remote_shell_client_typing_allowed_by_default() {
    use crate::access::policy::RemoteOwnerTui;
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    bridge_only(&mut f, RemoteOwnerTui::Allow);
    let target = f.public(0);
    let notices = connect_shell_client(&mut f.server, 89, lineage_for(true, true));

    // The owner's remote TUI types as an outside client does.
    type_into(&mut f.server, 89, &target);

    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"typed");
    assert!(shell_errors(&notices).is_empty());

    // An unreadable lineage over ssh is unknown, as a local one would be: refused on a
    // protected pane, typing freely on an unbound one.
    let unreadable = connect_shell_client(&mut f.server, 92, lineage_for(true, false));
    let open = f.public(1);
    type_into(&mut f.server, 92, &open);
    assert_eq!(f.inputs[1].try_recv().unwrap().as_ref(), b"typed");
    type_into(&mut f.server, 92, &target);
    f.assert_no_bytes(0);
    assert_eq!(
        shell_errors(&unreadable),
        vec![input_denial(&target, "unknown")]
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn remote_shell_client_denied_when_deny() {
    use crate::access::policy::RemoteOwnerTui;
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    bridge_only(&mut f, RemoteOwnerTui::Deny);
    let target = f.public(0);
    let notices = connect_shell_client(&mut f.server, 90, lineage_for(true, true));
    let local = connect_shell_client(&mut f.server, 91, lineage_for(false, true));

    type_into(&mut f.server, 90, &target);

    f.assert_no_bytes(0);
    let errors = shell_errors(&notices);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("does not accept input from another host"),
        "{}",
        errors[0]
    );
    // A local client is untouched by the setting.
    type_into(&mut f.server, 91, &target);
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"typed");
    assert!(shell_errors(&local).is_empty());
    // The remote TUI is view-only on every pane, one with no policy included.
    let unbound = f.public(1);
    type_into(&mut f.server, 90, &unbound);
    f.assert_no_bytes(1);
    let errors = shell_errors(&notices);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("another host"), "{}", errors[0]);
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn remote_denial_names_the_bridge_binding() {
    let (mut f, _token, bridge_binding, _) = bridge_fixture();
    f.server.app.state.remote_settings.input = RemoteInput::BridgeOnly;
    f.server.app.state.remote_settings.bridge_agent = Some("bridge".into());
    let other = f.public(1);

    let response = f.call(peer(true, false), send_text(&other));

    assert_eq!(error_code(&response).as_deref(), Some("access_denied"));
    let message = error_message(&response);
    assert!(
        message.contains(&format!("(bridge {bridge_binding})")),
        "{message}"
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn remote_shell_client_popup_input_denied_when_deny() {
    use crate::access::policy::RemoteOwnerTui;
    let mut f = gate_fixture(2);
    bridge_only(&mut f, RemoteOwnerTui::Deny);
    let (popup_runtime, mut popup_input) =
        crate::terminal::TerminalRuntime::test_with_channel(40, 12);
    let (_, popup_id) = f.server.app.install_test_popup_runtime(popup_runtime);
    let remote = connect_shell_client(&mut f.server, 93, lineage_for(true, true));
    let local = connect_shell_client(&mut f.server, 94, lineage_for(false, true));
    let popup_input_from = |server: &mut HeadlessServer, client_id: u64| {
        server.handle_server_event(ServerEvent::ClientShellPopupInput {
            client_id,
            terminal_id: popup_id.to_string(),
            events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                "typed".into(),
            )],
        });
    };

    popup_input_from(&mut f.server, 93);

    assert!(popup_input.try_recv().is_err(), "bytes reached the popup");
    let errors = shell_errors(&remote);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("another host"), "{}", errors[0]);
    // A local client is not refused by the setting.
    popup_input_from(&mut f.server, 94);
    assert!(shell_errors(&local).is_empty());
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn repeated_refused_input_is_dropped_but_noticed_once() {
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let (blocked, open) = (f.public(0), f.public(1));
    let notices = connect_shell_client(&mut f.server, 95, PeerLineage::default());

    for _ in 0..3 {
        type_into(&mut f.server, 95, &blocked);
    }
    f.assert_no_bytes(0);
    assert_eq!(
        shell_errors(&notices),
        vec![input_denial(&blocked, "unknown")]
    );

    // An allowed input in between resets it, and the next refusal is told again.
    type_into(&mut f.server, 95, &open);
    assert_eq!(f.inputs[1].try_recv().unwrap().as_ref(), b"typed");
    type_into(&mut f.server, 95, &blocked);
    type_into(&mut f.server, 95, &blocked);
    f.assert_no_bytes(0);
    assert_eq!(
        shell_errors(&notices),
        vec![input_denial(&blocked, "unknown")]
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn remote_input_denial_names_the_bridge_binding() {
    let (mut f, _token, bridge_binding, _) = bridge_fixture();
    bridge_only(&mut f, crate::access::policy::RemoteOwnerTui::Deny);
    f.server.app.state.remote_settings.bridge_agent = Some("bridge".into());
    let other = f.public(1);
    let notices = connect_shell_client(&mut f.server, 96, lineage_for(true, true));

    type_into(&mut f.server, 96, &other);

    f.assert_no_bytes(1);
    let errors = shell_errors(&notices);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains(&format!("(bridge {bridge_binding})")),
        "{}",
        errors[0]
    );
    shutdown_test_runtimes(&mut f.server);
}

#[tokio::test]
async fn invalid_remote_owner_tui_config_makes_a_remote_tui_view_only() {
    let config: crate::config::Config =
        toml::from_str("[access]\nremote_input = \"bridge-only\"\nremote_owner_tui = \"maybe\"\n")
            .unwrap();
    let resolved = config.access.resolve();
    let mut f = gate_fixture(2);
    f.server.app.state.remote_settings = resolved.remote;
    f.server.app.state.remote_owner_tui = resolved.remote_owner_tui;
    let target = f.public(0);
    let notices = connect_shell_client(&mut f.server, 97, lineage_for(true, true));

    type_into(&mut f.server, 97, &target);

    f.assert_no_bytes(0);
    assert_eq!(shell_errors(&notices).len(), 1);
    shutdown_test_runtimes(&mut f.server);
}

// Under remote_input = "any" nothing remote is refused: the same via_ssh TUI gets the same
// answer for its API input and its typing into an open pane.
#[tokio::test]
async fn via_ssh_tui_api_and_typing_agree_under_remote_input_any() {
    let mut f = gate_fixture(2);
    f.protect(0, vec![Principal::Outside]);
    let target = f.public(0);
    let notices = connect_shell_client(&mut f.server, 98, lineage_for(true, true));

    type_into(&mut f.server, 98, &target);
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"typed");
    assert!(shell_errors(&notices).is_empty());

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let message = f
        .server
        .shell_endpoint_message(
            98,
            api::schema::Request {
                id: "s".into(),
                method: send_text(&target),
            },
            respond_to,
        )
        .expect("client 98");
    f.server.handle_api_request_with_shutdown_check(message);
    let response = response_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(error_code(&response).is_none(), "{response}");
    assert_eq!(f.inputs[0].try_recv().unwrap().as_ref(), b"x");
    shutdown_test_runtimes(&mut f.server);
}
