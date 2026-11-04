// herdr-fork: tests for per-pane access policy state, persistence, config and the sticky default; tests the `node_name` key and the input-mode mark rules; a restored input mode makes the gate active. Unofficial stopgap; see FORK.md.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, Notify};

use super::state::AppState;
use super::App;
use crate::access::policy::{AccessPolicy, Principal, RemoteInput, RemoteOwnerTui};
use crate::config::Config;
use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::persist::SessionSnapshot;
use crate::render_signal::RenderSignal;
use crate::terminal::TerminalId;
use crate::workspace::Workspace;

fn state_with_pane() -> (AppState, crate::layout::PaneId, TerminalId) {
    let mut state = AppState::test_new();
    state.workspaces.push(Workspace::test_new("w"));
    state.ensure_test_terminals();
    state.active = Some(0);
    let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
    let terminal_id = state.workspaces[0]
        .pane_state(pane_id)
        .unwrap()
        .attached_terminal_id
        .clone();
    (state, pane_id, terminal_id)
}

/// The process detection read behind the agent in a pane.
fn identify(state: &mut AppState, pane_id: crate::layout::PaneId, pid: u32) {
    state.handle_app_event(AppEvent::AgentProcessIdentified {
        pane_id,
        process: Some(crate::platform::ProcessIdentity {
            pid,
            start_token: 7,
        }),
    });
}

/// The session key `identify(.., pid)` gives an agent with no hook session.
fn process_key(pid: u32) -> String {
    format!("proc:{pid}:7|")
}

fn agent_seen(state: &mut AppState, pane_id: crate::layout::PaneId) {
    identify(state, pane_id, 4242);
    state.handle_app_event(AppEvent::StateChanged {
        pane_id,
        agent: Some(Agent::Pi),
        state: AgentState::Working,
        visible_blocker: false,
        visible_working: false,
        process_exited: false,
        observed_at: Instant::now(),
    });
}

fn config_from(toml: &str) -> Config {
    toml::from_str::<Config>(toml).unwrap()
}

fn test_app(config: &Config) -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    App::new(
        config,
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    )
}

fn snapshot_of(state: &AppState) -> SessionSnapshot {
    crate::persist::capture(
        &state.workspaces,
        &state.terminals,
        &crate::terminal::TerminalRuntimeRegistry::new(),
        state.active,
        0,
    )
}

fn restore_terminals(
    snapshot: &SessionSnapshot,
) -> HashMap<TerminalId, crate::terminal::TerminalState> {
    let (events, _rx) = mpsc::channel(4);
    let (_ws, terminals, _runtimes) = crate::persist::restore(
        snapshot,
        None,
        24,
        80,
        0,
        "/bin/sh",
        crate::config::ShellModeConfig::NonLogin,
        false,
        events,
        Arc::new(Notify::new()),
        Arc::new(RenderSignal::new()),
    );
    terminals
}

fn policy(accept_from: Vec<Principal>) -> AccessPolicy {
    AccessPolicy {
        binding: crate::access::policy::AccessBindingId::mint().unwrap(),
        accept_from,
    }
}

#[tokio::test]
async fn restore_preserves_access_policy_and_binding() {
    let (mut state, _pane, terminal_id) = state_with_pane();
    let cwd = std::env::current_dir().unwrap();
    state.terminals.get_mut(&terminal_id).unwrap().cwd = cwd;
    let saved = policy(vec![Principal::Outside, Principal::Caller("ops".into())]);
    state.terminals.get_mut(&terminal_id).unwrap().access = Some(saved.clone());

    let json = serde_json::to_string(&snapshot_of(&state)).unwrap();
    assert!(json.contains(saved.binding.as_str()));
    let snapshot: SessionSnapshot = serde_json::from_str(&json).unwrap();
    let terminals = restore_terminals(&snapshot);

    let restored: Vec<_> = terminals
        .values()
        .filter_map(|t| t.access.clone())
        .collect();
    assert_eq!(restored, vec![saved]);
}

#[tokio::test]
async fn snapshot_without_access_field_restores_open() {
    let (mut state, _pane, terminal_id) = state_with_pane();
    state.terminals.get_mut(&terminal_id).unwrap().cwd = std::env::current_dir().unwrap();

    let json = serde_json::to_string(&snapshot_of(&state)).unwrap();
    assert!(!json.contains("access"), "open panes write no access field");
    let snapshot: SessionSnapshot = serde_json::from_str(&json).unwrap();
    let terminals = restore_terminals(&snapshot);

    assert_eq!(terminals.len(), 1);
    assert!(terminals.values().all(|t| t.access.is_none()));
}

#[test]
fn access_config_parses_default_and_named_sections() {
    let config = config_from(
        "[access.default]\naccept_from = [\"outside\", \"caller:ops\"]\n[access.locked]\naccept_from = [\"none\"]\n",
    );
    let resolved = config.access.resolve();

    assert!(
        resolved.diagnostics.is_empty(),
        "{:?}",
        resolved.diagnostics
    );
    assert_eq!(
        resolved.defaults.default,
        Some(vec![Principal::Outside, Principal::Caller("ops".into())])
    );
    assert_eq!(
        resolved.defaults.by_name.get("locked"),
        Some(&vec![Principal::None])
    );
}

#[test]
fn access_config_rejects_bad_principal_with_diagnostic() {
    let config = config_from(
        "[access.default]\naccept_from = [\"caller:Bad Name\"]\n[access.ok]\naccept_from = [\"outside\"]\n",
    );
    let resolved = config.access.resolve();

    assert_eq!(resolved.defaults.default, None);
    assert!(resolved.defaults.by_name.contains_key("ok"));
    assert_eq!(resolved.diagnostics.len(), 1);
    assert!(resolved.diagnostics[0].contains("access.default"));
    assert!(config
        .collect_diagnostics()
        .iter()
        .any(|d| d.contains("access.default")));
}

#[test]
fn access_config_parses_node_keys_and_tables() {
    let config = config_from(
        "[access]\nremote_input = \"bridge-only\"\nbridge_agent = \"relay\"\nremote_owner_tui = \"deny\"\n[access.default]\naccept_from = [\"outside\"]\n",
    );
    let resolved = config.access.resolve();

    assert!(
        resolved.diagnostics.is_empty(),
        "{:?}",
        resolved.diagnostics
    );
    assert_eq!(resolved.remote.input, RemoteInput::BridgeOnly);
    assert_eq!(resolved.remote.bridge_agent.as_deref(), Some("relay"));
    assert_eq!(resolved.remote.bridge_binding, None);
    assert_eq!(resolved.remote_owner_tui, RemoteOwnerTui::Deny);
    assert_eq!(resolved.defaults.default, Some(vec![Principal::Outside]));

    let bare = config_from("").access.resolve();
    assert_eq!(bare.remote.input, RemoteInput::Any);
    assert_eq!(bare.remote_owner_tui, RemoteOwnerTui::Allow);
    assert!(bare.defaults.default.is_none() && bare.diagnostics.is_empty());
}

#[test]
fn remote_owner_tui_ignored_when_remote_input_any() {
    let config = config_from("[access]\nremote_owner_tui = \"deny\"\n");
    let resolved = config.access.resolve();

    assert_eq!(resolved.remote_owner_tui, RemoteOwnerTui::Allow);
    assert_eq!(resolved.diagnostics.len(), 1);
    assert!(resolved.diagnostics[0].contains("remote_owner_tui"));
}

const RESTART_HINT: &str = "[access] changes need a restart";

fn reload_with(app: &mut App, content: &str) -> crate::config::ConfigReloadReport {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = std::env::temp_dir().join(format!(
        "herdr-access-reload-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, content).unwrap();
    std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
    let report = app.reload_config();
    std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
    let _ = std::fs::remove_dir_all(&dir);
    report
}

#[test]
fn reload_config_does_not_loosen_access_defaults() {
    let mut app = test_app(&config_from("[access.default]\naccept_from = [\"none\"]\n"));
    let before = app.state.access_defaults.clone();
    assert_eq!(before.default, Some(vec![Principal::None]));

    let report = reload_with(&mut app, "[access.default]\naccept_from = [\"outside\"]\n");
    assert_eq!(app.state.access_defaults, before);
    assert!(report.diagnostics.iter().any(|d| d == RESTART_HINT));

    reload_with(&mut app, "");
    assert_eq!(app.state.access_defaults, before);
}

#[test]
fn reload_config_does_not_change_remote_input() {
    let mut app = test_app(&config_from("[access]\nremote_input = \"bridge-only\"\n"));
    assert_eq!(app.state.remote_settings.input, RemoteInput::BridgeOnly);

    let report = reload_with(&mut app, "[access]\nremote_input = \"any\"\n");
    assert_eq!(app.state.remote_settings.input, RemoteInput::BridgeOnly);
    assert!(report.diagnostics.iter().any(|d| d == RESTART_HINT));

    reload_with(&mut app, "");
    assert_eq!(app.state.remote_settings.input, RemoteInput::BridgeOnly);
}

#[test]
fn access_bind_schedules_session_save() {
    let mut app = test_app(&Config::default());
    app.policy.persist_session = true;
    app.state.workspaces.push(Workspace::test_new("w"));
    app.state.ensure_test_terminals();
    let terminal_id = app.state.terminals.keys().next().unwrap().clone();
    app.state.session_dirty = false;

    let first = app
        .state
        .bind_access(&terminal_id, vec![Principal::Outside])
        .unwrap();
    assert!(app.state.session_dirty, "bind marks the session dirty");
    app.sync_session_save_schedule();
    assert!(app.session_save_deadline.is_some(), "bind schedules a save");

    // Rebinding keeps the binding id.
    let second = app
        .state
        .bind_access(&terminal_id, vec![Principal::None])
        .unwrap();
    assert_eq!(first, second);
    app.sync_session_save_schedule();

    app.session_save_deadline = None;
    app.state.unbind_access(&terminal_id);
    assert!(app.state.terminals[&terminal_id].access.is_none());
    assert!(app.state.session_dirty, "unbind marks the session dirty");
    app.sync_session_save_schedule();
    assert!(
        app.session_save_deadline.is_some(),
        "unbind schedules a save"
    );
}

#[test]
fn default_binds_on_first_detection_and_survives_agent_exit() {
    let (mut state, pane_id, terminal_id) = state_with_pane();
    state.access_defaults.default = Some(vec![Principal::Outside]);
    assert!(state.terminals[&terminal_id].access.is_none());
    state.session_dirty = false;

    agent_seen(&mut state, pane_id);
    let bound = state.terminals[&terminal_id].access.clone().unwrap();
    assert_eq!(bound.accept_from, vec![Principal::Outside]);
    assert!(state.session_dirty);

    state.handle_app_event(AppEvent::StateChanged {
        pane_id,
        agent: None,
        state: AgentState::Unknown,
        visible_blocker: false,
        visible_working: false,
        process_exited: true,
        observed_at: Instant::now(),
    });
    assert_eq!(state.terminals[&terminal_id].access, Some(bound.clone()));

    // A later detection does not mint a new binding.
    agent_seen(&mut state, pane_id);
    assert_eq!(state.terminals[&terminal_id].access, Some(bound));
}

#[test]
fn self_release_does_not_unprotect_defaulted_pane() {
    let (mut state, pane_id, terminal_id) = state_with_pane();
    state.access_defaults.default = Some(vec![Principal::None]);
    state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_hook_authority(
            "custom:agent".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            Some(1),
        );
    state.bind_default_access(&terminal_id);
    let bound = state.terminals[&terminal_id].access.clone().unwrap();

    state.handle_app_event(AppEvent::HookAgentReleased {
        pane_id,
        source: "custom:agent".into(),
        agent_label: "custom-agent".into(),
        known_agent: None,
        seq: Some(2),
    });

    let terminal = &state.terminals[&terminal_id];
    assert!(terminal.hook_authority.is_none(), "the agent was released");
    assert_eq!(terminal.access, Some(bound));
}

#[test]
fn plain_shell_pane_stays_open_with_default() {
    let (mut state, pane_id, terminal_id) = state_with_pane();
    state.access_defaults.default = Some(vec![Principal::None]);

    state.handle_app_event(AppEvent::StateChanged {
        pane_id,
        agent: None,
        state: AgentState::Idle,
        visible_blocker: false,
        visible_working: false,
        process_exited: false,
        observed_at: Instant::now(),
    });
    state.bind_default_access(&terminal_id);

    assert!(state.terminals[&terminal_id].access.is_none());
}

#[test]
fn reload_config_notes_access_changes_need_a_restart() {
    let startup =
        "[access]\nremote_input = \"bridge-only\"\n[access.default]\naccept_from = [\"none\"]\n";
    let mut app = test_app(&config_from(startup));

    let same = reload_with(&mut app, startup);
    assert!(!same.diagnostics.iter().any(|d| d == RESTART_HINT));
    assert_eq!(same.status, crate::config::ConfigReloadStatus::Applied);

    let changed = reload_with(&mut app, "[access.default]\naccept_from = [\"outside\"]\n");
    assert!(changed.diagnostics.iter().any(|d| d == RESTART_HINT));

    let dropped = reload_with(&mut app, "");
    assert!(dropped.diagnostics.iter().any(|d| d == RESTART_HINT));

    let unrelated = test_app(&Config::default());
    let mut unrelated = unrelated;
    let none = reload_with(&mut unrelated, "[ui]\nagent_panel_sort = \"priority\"\n");
    assert!(!none.diagnostics.iter().any(|d| d == RESTART_HINT));
}

#[test]
fn access_config_unknown_scalar_is_soft_and_keeps_the_rest() {
    let config = config_from(
        "[ui]\nagent_panel_sort = \"priority\"\n[access]\nremote_input = \"bridge-only\"\nfoo = 1\n[access.default]\naccept_from=[\"none\"]\n",
    );
    let resolved = config.access.resolve();

    assert_eq!(resolved.remote.input, RemoteInput::BridgeOnly);
    assert_eq!(resolved.defaults.default, Some(vec![Principal::None]));
    assert_eq!(
        resolved.diagnostics,
        vec!["unknown config key access.foo".to_string()]
    );
    assert_eq!(
        config.ui.agent_panel_sort,
        crate::config::AgentPanelSortConfig::Priority
    );
    assert_eq!(
        config
            .collect_diagnostics()
            .iter()
            .filter(|d| d.contains("access.foo"))
            .count(),
        1
    );
}

#[test]
fn access_config_invalid_remote_input_fails_closed() {
    let resolved = config_from("[access]\nremote_input = \"bridge_only\"\n")
        .access
        .resolve();
    assert_eq!(resolved.remote.input, RemoteInput::BridgeOnly);
    assert_eq!(resolved.diagnostics.len(), 1);
    assert!(resolved.diagnostics[0].contains("treating it as \"bridge-only\""));

    let wrong_type = config_from("[access]\nremote_input = 3\n").access.resolve();
    assert_eq!(wrong_type.remote.input, RemoteInput::BridgeOnly);
}

fn state_with_default() -> (AppState, crate::layout::PaneId, TerminalId) {
    let (mut state, pane_id, terminal_id) = state_with_pane();
    state.access_defaults.default = Some(vec![Principal::None]);
    (state, pane_id, terminal_id)
}

fn agent_state(state: &mut AppState, pane_id: crate::layout::PaneId, agent_state: AgentState) {
    state.handle_app_event(AppEvent::StateChanged {
        pane_id,
        agent: Some(Agent::Pi),
        state: agent_state,
        visible_blocker: false,
        visible_working: false,
        process_exited: false,
        observed_at: Instant::now(),
    });
}

#[test]
fn explicit_open_survives_the_next_detection() {
    let (mut state, pane_id, terminal_id) = state_with_default();
    agent_state(&mut state, pane_id, AgentState::Working);
    assert!(state.terminals[&terminal_id].access.is_some());

    state.unbind_access(&terminal_id);
    assert!(state.terminals[&terminal_id].access.is_none());
    assert!(state.terminals[&terminal_id].access_opened_explicitly);

    agent_state(&mut state, pane_id, AgentState::Idle);
    agent_state(&mut state, pane_id, AgentState::Working);
    assert!(state.terminals[&terminal_id].access.is_none());

    // Binding on purpose clears the mark again.
    state
        .bind_access(&terminal_id, vec![Principal::Outside])
        .unwrap();
    assert!(!state.terminals[&terminal_id].access_opened_explicitly);
}

#[test]
fn explicit_open_survives_bind_default_access_all() {
    let (mut state, pane_id, terminal_id) = state_with_default();
    agent_state(&mut state, pane_id, AgentState::Working);
    state.unbind_access(&terminal_id);

    state.bind_default_access_all();
    assert!(state.terminals[&terminal_id].access.is_none());

    // Without the mark the same pane would have been bound.
    state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .access_opened_explicitly = false;
    state.bind_default_access_all();
    assert!(state.terminals[&terminal_id].access.is_some());
}

#[tokio::test]
async fn explicit_open_survives_a_snapshot_round_trip() {
    let (mut state, pane_id, terminal_id) = state_with_default();
    agent_state(&mut state, pane_id, AgentState::Working);
    state.unbind_access(&terminal_id);
    state.terminals.get_mut(&terminal_id).unwrap().cwd = std::env::current_dir().unwrap();

    let json = serde_json::to_string(&snapshot_of(&state)).unwrap();
    assert!(json.contains("access_opened_explicitly"));
    let snapshot: SessionSnapshot = serde_json::from_str(&json).unwrap();
    let mut terminals = restore_terminals(&snapshot);
    let restored = terminals.values_mut().next().unwrap();
    assert!(restored.access.is_none());
    assert!(restored.access_opened_explicitly);

    // Make the restored pane look like an agent pane and apply the default.
    restored.set_agent_name("worker".into());
    let defaults = crate::access::policy::AccessDefaults {
        default: Some(vec![Principal::None]),
        ..Default::default()
    };
    assert!(!super::access::apply_default_access(
        &defaults, None, restored
    ));
    assert!(restored.access.is_none());
}

#[test]
fn default_binds_on_label_switch_without_a_transition() {
    let (mut state, pane_id, terminal_id) = state_with_pane();
    agent_state(&mut state, pane_id, AgentState::Working);
    assert!(state.terminals[&terminal_id].access.is_none());

    state.access_defaults.default = Some(vec![Principal::None]);
    state.session_dirty = false;
    // Same agent, same state: no effective transition, still first sight under a default.
    agent_state(&mut state, pane_id, AgentState::Working);
    assert!(state.terminals[&terminal_id].access.is_some());
    assert!(state.session_dirty);
}

#[test]
fn no_default_means_no_binding_and_no_dirty_flag() {
    let (mut state, pane_id, terminal_id) = state_with_pane();
    state.session_dirty = false;
    agent_state(&mut state, pane_id, AgentState::Working);
    agent_state(&mut state, pane_id, AgentState::Idle);
    assert!(state.terminals[&terminal_id].access.is_none());
    assert!(!state.terminals[&terminal_id].access_opened_explicitly);
}

fn snapshot_json_with_two_policies() -> serde_json::Value {
    let (mut state, _pane, terminal_id) = state_with_pane();
    let cwd = std::env::current_dir().unwrap();
    state.terminals.get_mut(&terminal_id).unwrap().cwd = cwd;
    state.terminals.get_mut(&terminal_id).unwrap().access = Some(policy(vec![Principal::Outside]));
    serde_json::to_value(snapshot_of(&state)).unwrap()
}

fn only_pane_access(json: &mut serde_json::Value) -> &mut serde_json::Value {
    let panes = json["workspaces"][0]["tabs"][0]["panes"]
        .as_object_mut()
        .unwrap();
    let key = panes.keys().next().unwrap().clone();
    &mut panes.get_mut(&key).unwrap()["access"]
}

#[tokio::test]
async fn malformed_pane_access_restores_that_pane_closed() {
    for corrupt in ["binding", "principal", "shape"] {
        let mut json = snapshot_json_with_two_policies();
        let access = only_pane_access(&mut json);
        match corrupt {
            "binding" => access["binding"] = serde_json::json!("not-a-binding"),
            "principal" => access["accept_from"] = serde_json::json!(["caller:Bad Name"]),
            _ => *access = serde_json::json!(7),
        }
        let snapshot: SessionSnapshot = serde_json::from_value(json)
            .unwrap_or_else(|err| panic!("{corrupt}: the session must still parse: {err}"));
        let terminals = restore_terminals(&snapshot);

        let restored = terminals.values().next().unwrap().access.clone().unwrap();
        assert_eq!(restored.accept_from, vec![Principal::None], "{corrupt}");
    }
}

#[tokio::test]
async fn malformed_pane_access_leaves_other_panes_alone() {
    let mut state = AppState::test_new();
    let mut ws = Workspace::test_new("w");
    let second = ws.test_split(ratatui::layout::Direction::Horizontal);
    state.workspaces.push(ws);
    state.ensure_test_terminals();
    let cwd = std::env::current_dir().unwrap();
    let mut ids = Vec::new();
    for pane in state.workspaces[0].panes.values() {
        ids.push(pane.attached_terminal_id.clone());
    }
    assert_eq!(ids.len(), 2, "{second:?}");
    let good = policy(vec![Principal::Caller("ops".into())]);
    for id in &ids {
        state.terminals.get_mut(id).unwrap().cwd = cwd.clone();
    }
    state.terminals.get_mut(&ids[0]).unwrap().access = Some(good.clone());
    state.terminals.get_mut(&ids[1]).unwrap().access = Some(policy(vec![Principal::Outside]));
    let mut json = serde_json::to_value(snapshot_of(&state)).unwrap();
    let panes = json["workspaces"][0]["tabs"][0]["panes"]
        .as_object_mut()
        .unwrap();
    let mut bad_done = false;
    for pane in panes.values_mut() {
        if !bad_done && pane["access"]["accept_from"] == serde_json::json!(["outside"]) {
            pane["access"]["binding"] = serde_json::json!("junk");
            bad_done = true;
        }
    }
    assert!(bad_done);

    let snapshot: SessionSnapshot = serde_json::from_value(json).unwrap();
    let terminals = restore_terminals(&snapshot);

    let mut lists: Vec<_> = terminals
        .values()
        .map(|t| t.access.clone().unwrap())
        .collect();
    lists.sort_by_key(|p| format!("{:?}", p.accept_from));
    assert_eq!(lists.len(), 2);
    assert!(lists.contains(&good));
    assert!(lists.iter().any(|p| p.accept_from == vec![Principal::None]));
}

/// A snapshot with one pane that holds a resumable pi session, which restores
/// without spawning a shell and looks like an agent pane.
fn agent_pane_snapshot() -> SessionSnapshot {
    let (mut state, _pane, terminal_id) = state_with_pane();
    state.terminals.get_mut(&terminal_id).unwrap().cwd = std::env::current_dir().unwrap();
    let mut json = serde_json::to_value(snapshot_of(&state)).unwrap();
    let pane = json["workspaces"][0]["tabs"][0]["panes"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    pane["agent_session"] = serde_json::json!({
        "source": "herdr:pi",
        "agent": "pi",
        "kind": "path",
        "value": std::env::current_dir().unwrap().join("pi-session.jsonl").display().to_string(),
    });
    serde_json::from_value(json).unwrap()
}

fn default_none_config() -> Config {
    config_from("[access.default]\naccept_from = [\"none\"]\n")
}

fn scratch_dirs(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "herdr-access-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
    std::env::set_var("XDG_STATE_HOME", dir.join("state"));
    dir
}

#[tokio::test]
async fn startup_restore_binds_the_default_to_restored_agent_panes() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = scratch_dirs("startup");
    std::fs::create_dir_all(crate::config::config_dir()).unwrap();
    let snapshot = agent_pane_snapshot();
    std::fs::write(
        crate::config::config_dir().join("session.json"),
        serde_json::to_string(&snapshot).unwrap(),
    )
    .unwrap();

    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let policy = crate::app::AppPolicy {
        restore_session: true,
        ..crate::app::AppPolicy::TEST
    };
    let app = App::try_new(
        &default_none_config(),
        policy,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_STATE_HOME");
    let _ = std::fs::remove_dir_all(&dir);
    let app = app.unwrap();

    assert_eq!(app.state.terminals.len(), 1);
    let terminal = app.state.terminals.values().next().unwrap();
    assert!(terminal.is_agent_terminal());
    assert_eq!(
        terminal.access.as_ref().map(|p| p.accept_from.clone()),
        Some(vec![Principal::None])
    );
}

#[tokio::test]
async fn a_restored_input_mode_makes_the_gate_active_before_any_detection() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = scratch_dirs("restored-mode");
    std::fs::create_dir_all(crate::config::config_dir()).unwrap();
    let mut json = serde_json::to_value(agent_pane_snapshot()).unwrap();
    let pane = json["workspaces"][0]["tabs"][0]["panes"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    pane["input_mode"] = serde_json::json!("human-only");
    std::fs::write(
        crate::config::config_dir().join("session.json"),
        serde_json::to_string(&json).unwrap(),
    )
    .unwrap();

    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let policy = crate::app::AppPolicy {
        restore_session: true,
        ..crate::app::AppPolicy::TEST
    };
    let app = App::try_new(
        &Config::default(),
        policy,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_STATE_HOME");
    let _ = std::fs::remove_dir_all(&dir);
    let app = app.unwrap();

    // Right after construction, with no detection pass run, the restored mode counts.
    assert!(app.any_access_protected());
    let terminal = app.state.terminals.keys().next().unwrap().clone();
    let from_pane = crate::access::caller::Caller::Classified(
        crate::access::policy::CallerClass::Pane(terminal),
    );
    let handoff = crate::api::schema::Method::ServerLiveHandoff(Default::default());
    assert!(app.check_access(&handoff, &from_pane).is_err());
}

/// Start an app from a saved session, the way a server restart does.
fn app_restored_from(name: &str, json: &serde_json::Value) -> App {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = scratch_dirs(name);
    std::fs::create_dir_all(crate::config::config_dir()).unwrap();
    std::fs::write(
        crate::config::config_dir().join("session.json"),
        serde_json::to_string(json).unwrap(),
    )
    .unwrap();
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let policy = crate::app::AppPolicy {
        restore_session: true,
        ..crate::app::AppPolicy::TEST
    };
    let app = App::try_new(
        &Config::default(),
        policy,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_STATE_HOME");
    let _ = std::fs::remove_dir_all(&dir);
    app.unwrap()
}

fn handoff_from_a_pane_is_refused(app: &App) -> bool {
    let terminal = app.state.terminals.keys().next().unwrap().clone();
    let from_pane = crate::access::caller::Caller::Classified(
        crate::access::policy::CallerClass::Pane(terminal),
    );
    let handoff = crate::api::schema::Method::ServerLiveHandoff(Default::default());
    app.check_access(&handoff, &from_pane).is_err()
}

#[tokio::test]
async fn a_saved_handoff_latch_is_restored_before_the_api_is_served() {
    let mut json = serde_json::to_value(agent_pane_snapshot()).unwrap();
    json["access_seen"] = serde_json::json!(true);
    let app = app_restored_from("latch-restored", &json);
    // Nothing is protected after the restore; the saved latch alone keeps the gate shut.
    assert!(!app.any_access_protected());
    assert!(handoff_from_a_pane_is_refused(&app));
}

#[tokio::test]
async fn a_saved_state_without_the_latch_restores_open() {
    let json = serde_json::to_value(agent_pane_snapshot()).unwrap();
    assert!(json.get("access_seen").is_none());
    let app = app_restored_from("latch-absent", &json);
    assert!(!app.any_access_protected());
    assert!(!handoff_from_a_pane_is_refused(&app));
}

#[tokio::test]
async fn a_restored_protected_state_sets_the_latch() {
    let mut json = serde_json::to_value(agent_pane_snapshot()).unwrap();
    let pane = json["workspaces"][0]["tabs"][0]["panes"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    pane["input_mode"] = serde_json::json!("human-only");
    let mut app = app_restored_from("latch-from-protected", &json);
    assert!(app.any_access_protected());
    // The protection ends before any request is checked.
    for terminal in app.state.terminals.values_mut() {
        terminal.input_mode = crate::access::policy::InputMode::Any;
    }
    assert!(!app.any_access_protected());
    assert!(handoff_from_a_pane_is_refused(&app));
}

#[tokio::test]
async fn the_handoff_latch_is_saved_with_the_session() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = scratch_dirs("latch-saved");
    std::env::remove_var(crate::session::SESSION_ENV_VAR);
    let saved = |app: &mut App| {
        app.policy.persist_session = true;
        app.start_background_session_save();
        if let Some(thread) = app.session_save_thread.take() {
            thread.join().unwrap();
        }
        let snapshot = crate::persist::load().expect("the session is saved");
        serde_json::to_value(&snapshot).unwrap()
    };

    let mut latched = test_app(&Config::default());
    let (state, _pane, terminal_id) = state_with_pane();
    latched.state.workspaces = state.workspaces;
    latched.state.terminals = state.terminals;
    latched.state.active = Some(0);
    latched
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .input_mode = crate::access::policy::InputMode::HumanOnly;
    let handoff = crate::api::schema::Method::ServerLiveHandoff(Default::default());
    let outside =
        crate::access::caller::Caller::Classified(crate::access::policy::CallerClass::Outside {
            via_ssh: false,
        });
    assert!(latched.check_access(&handoff, &outside).is_ok());
    latched
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .input_mode = crate::access::policy::InputMode::Any;
    assert_eq!(saved(&mut latched)["access_seen"], true);

    let _ = std::fs::remove_dir_all(dir.join("config"));
    let _ = std::fs::remove_dir_all(dir.join("state"));
    let mut never = test_app(&Config::default());
    let (state, _pane, _terminal) = state_with_pane();
    never.state.workspaces = state.workspaces;
    never.state.terminals = state.terminals;
    never.state.active = Some(0);
    assert_ne!(saved(&mut never)["access_seen"], true);

    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_STATE_HOME");
    let _ = std::fs::remove_dir_all(dir);
}

fn pane_handoff_refused(app: &App, terminal: &TerminalId) -> bool {
    let from_pane = crate::access::caller::Caller::Classified(
        crate::access::policy::CallerClass::Pane(terminal.clone()),
    );
    let handoff = crate::api::schema::Method::ServerLiveHandoff(Default::default());
    app.check_access(&handoff, &from_pane).is_err()
}

#[tokio::test]
async fn a_policy_bound_by_detection_then_removed_still_latches_the_handoff() {
    // The app starts with no access config, so only the detection can arm the latch.
    let mut app = test_app(&Config::default());
    app.state.access_defaults = default_none_config().access.resolve().defaults;
    let (state, pane, terminal_id) = state_with_pane();
    app.state.workspaces = state.workspaces;
    app.state.terminals = state.terminals;
    app.state.active = Some(0);
    assert!(!app.any_access_protected());
    assert!(!pane_handoff_refused(&app, &terminal_id));

    agent_seen(&mut app.state, pane);
    assert!(app.state.terminals[&terminal_id].access.is_some());
    // The protection ends with no request checked in between.
    app.state.terminals.get_mut(&terminal_id).unwrap().access = None;
    assert!(!app.any_access_protected());
    assert!(pane_handoff_refused(&app, &terminal_id));
}

#[tokio::test]
async fn a_mode_bound_at_agent_start_latches_the_handoff() {
    let mut app = test_app(&Config::default());
    app.state.access_defaults = config_from("[access.worker]\ninput_mode = \"human-only\"\n")
        .access
        .resolve()
        .defaults;
    let (state, _pane, terminal_id) = state_with_pane();
    app.state.workspaces = state.workspaces;
    app.state.terminals = state.terminals;
    app.state.active = Some(0);
    app.state
        .bind_access_for_start(&terminal_id, "worker", None)
        .unwrap();
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .input_mode = crate::access::policy::InputMode::Any;
    assert!(!app.any_access_protected());
    assert!(pane_handoff_refused(&app, &terminal_id));
}

#[tokio::test]
async fn a_config_that_asks_for_access_latches_the_handoff_before_any_pane_carries_it() {
    for toml in [
        "[access.default]\naccept_from = [\"none\"]\n",
        "[access.worker]\naccept_from = [\"outside\"]\n",
        "[access.default]\ninput_mode = \"human-only\"\n",
        "[access]\nremote_input = \"bridge-only\"\nbridge_agent = \"relay\"\n",
    ] {
        let mut app = test_app(&config_from(toml));
        let (state, _pane, terminal_id) = state_with_pane();
        app.state.workspaces = state.workspaces;
        app.state.terminals = state.terminals;
        app.state.active = Some(0);
        // No pane carries anything and the node is not bridge-only any more.
        app.state.remote_settings.input = RemoteInput::Any;
        assert!(!app.any_access_protected(), "{toml}");
        assert!(pane_handoff_refused(&app, &terminal_id), "{toml}");
    }
    // A config with no protection leaves the handoff open.
    let mut plain = test_app(&config_from("[access]\nnode_name = \"n\"\n"));
    let (state, _pane, terminal_id) = state_with_pane();
    plain.state.workspaces = state.workspaces;
    plain.state.terminals = state.terminals;
    assert!(!pane_handoff_refused(&plain, &terminal_id));
}

fn restore_in_place() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let policy = crate::app::AppPolicy {
        restore_session: true,
        ..crate::app::AppPolicy::TEST
    };
    App::try_new(
        &Config::default(),
        policy,
        None,
        api_rx,
        crate::api::EventHub::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn the_latch_survives_closing_the_last_workspace_and_a_restart() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = scratch_dirs("latch-empty-save");
    std::env::remove_var(crate::session::SESSION_ENV_VAR);
    let mut app = test_app(&Config::default());
    let (state, _pane, terminal_id) = state_with_pane();
    app.state.workspaces = state.workspaces;
    app.state.terminals = state.terminals;
    app.state.active = Some(0);
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .input_mode = crate::access::policy::InputMode::HumanOnly;
    let handoff = crate::api::schema::Method::ServerLiveHandoff(Default::default());
    let outside =
        crate::access::caller::Caller::Classified(crate::access::policy::CallerClass::Outside {
            via_ssh: false,
        });
    assert!(app.check_access(&handoff, &outside).is_ok());
    // Every workspace is closed, so there is nothing left to save but the latch.
    app.state.workspaces.clear();
    app.state.terminals.clear();
    app.state.active = None;
    app.policy.persist_session = true;
    app.start_background_session_save();
    if let Some(thread) = app.session_save_thread.take() {
        thread.join().unwrap();
    }

    let restored = restore_in_place();
    assert!(restored.state.workspaces.is_empty());
    assert!(restored.state.active.is_none());
    assert!(pane_handoff_refused(&restored, &terminal_id));

    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_STATE_HOME");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn closing_the_last_workspace_of_a_never_protected_node_still_clears_the_session() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = scratch_dirs("latch-empty-clear");
    std::env::remove_var(crate::session::SESSION_ENV_VAR);
    let mut app = test_app(&Config::default());
    let (state, _pane, _terminal) = state_with_pane();
    app.state.workspaces = state.workspaces;
    app.state.terminals = state.terminals;
    app.state.active = Some(0);
    app.policy.persist_session = true;
    app.start_background_session_save();
    if let Some(thread) = app.session_save_thread.take() {
        thread.join().unwrap();
    }
    assert!(crate::persist::load().is_some());
    app.state.workspaces.clear();
    app.state.terminals.clear();
    app.start_background_session_save();
    if let Some(thread) = app.session_save_thread.take() {
        thread.join().unwrap();
    }
    assert!(crate::persist::load().is_none());

    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_STATE_HOME");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_latch_going_from_false_to_true_schedules_a_save() {
    let mut app = test_app(&Config::default());
    app.policy.persist_session = true;
    let (state, _pane, terminal_id) = state_with_pane();
    app.state.workspaces = state.workspaces;
    app.state.terminals = state.terminals;
    app.state.active = Some(0);
    app.sync_session_save_schedule();
    assert!(app.session_save_deadline.is_none());

    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .input_mode = crate::access::policy::InputMode::HumanOnly;
    let handoff = crate::api::schema::Method::ServerLiveHandoff(Default::default());
    let outside =
        crate::access::caller::Caller::Classified(crate::access::policy::CallerClass::Outside {
            via_ssh: false,
        });
    assert!(app.check_access(&handoff, &outside).is_ok());
    app.sync_session_save_schedule();
    assert!(app.session_save_deadline.is_some());
    // The latch is already set: a second sighting schedules nothing new.
    app.session_save_deadline = None;
    assert!(app.check_access(&handoff, &outside).is_ok());
    app.sync_session_save_schedule();
    assert!(app.session_save_deadline.is_none());
}

#[tokio::test]
async fn an_unreadable_saved_session_starts_with_the_latch_set() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = scratch_dirs("latch-corrupt");
    std::fs::create_dir_all(crate::config::config_dir()).unwrap();
    let session = crate::config::config_dir().join("session.json");
    let terminal_id = state_with_pane().2;
    for content in [
        "{ this is not json",
        "",
        "{\"version\": 999, \"workspaces\": 7}",
    ] {
        std::fs::write(&session, content).unwrap();
        let app = restore_in_place();
        assert!(!app.any_access_protected());
        assert!(pane_handoff_refused(&app, &terminal_id), "{content:?}");
    }
    // A missing file starts unlatched.
    std::fs::remove_file(&session).unwrap();
    let app = restore_in_place();
    assert!(!pane_handoff_refused(&app, &terminal_id));

    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_STATE_HOME");
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[tokio::test]
async fn handoff_restore_binds_the_default_to_restored_agent_panes() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = scratch_dirs("handoff");
    let snapshot = agent_pane_snapshot();

    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let app = App::new_from_handoff(
        &default_none_config(),
        None,
        api_rx,
        crate::api::EventHub::default(),
        &snapshot,
        &mut HashMap::new(),
    );
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_STATE_HOME");
    let _ = std::fs::remove_dir_all(&dir);
    let app = app.unwrap();

    assert_eq!(app.state.terminals.len(), 1);
    let terminal = app.state.terminals.values().next().unwrap();
    assert!(terminal.is_agent_terminal());
    assert_eq!(
        terminal.access.as_ref().map(|p| p.accept_from.clone()),
        Some(vec![Principal::None])
    );
}

#[test]
fn access_input_log_key_is_accepted_and_defaults_on() {
    let bare = config_from("").access.resolve();
    assert!(bare.input_log);

    let off = config_from("[access]\ninput_log = false\n")
        .access
        .resolve();
    assert!(!off.input_log);
    assert!(off.diagnostics.is_empty(), "{:?}", off.diagnostics);

    let on =
        config_from("[access]\ninput_log = true\n[access.default]\naccept_from = [\"outside\"]\n")
            .access
            .resolve();
    assert!(on.input_log);
    assert!(on.diagnostics.is_empty(), "{:?}", on.diagnostics);
    assert_eq!(on.defaults.default, Some(vec![Principal::Outside]));

    // A wrong type is a soft diagnostic; the rest of the config survives.
    let bad = config_from(
        "[access]\ninput_log = \"yes\"\n[access.default]\naccept_from = [\"outside\"]\n",
    )
    .access
    .resolve();
    assert!(bad.input_log);
    assert_eq!(bad.diagnostics.len(), 1, "{:?}", bad.diagnostics);
    assert!(bad.diagnostics[0].contains("access.input_log"));
    assert_eq!(bad.defaults.default, Some(vec![Principal::Outside]));
}

#[test]
fn access_node_name_defaults_and_is_validated() {
    assert_eq!(config_from("").access.resolve().node_name, "node");
    let named = config_from("[access]\nnode_name = \"node-a\"\n")
        .access
        .resolve();
    assert_eq!(named.node_name, "node-a");
    assert!(named.diagnostics.is_empty(), "{:?}", named.diagnostics);
    // A bad value falls back to the default and says so; the rest of the config survives.
    for bad in ["\"has space\"", "\"\"", "7", "\"a/b\""] {
        let resolved = config_from(&format!(
            "[access]\nnode_name = {bad}\n[access.default]\naccept_from = [\"outside\"]\n"
        ))
        .access
        .resolve();
        assert_eq!(resolved.node_name, "node", "{bad}");
        assert_eq!(
            resolved.diagnostics.len(),
            1,
            "{bad}: {:?}",
            resolved.diagnostics
        );
        assert!(resolved.diagnostics[0].contains("access.node_name"));
        assert_eq!(resolved.defaults.default, Some(vec![Principal::Outside]));
    }
}

#[test]
fn invalid_remote_owner_tui_denies_remote_shell_client_under_bridge_only() {
    // Under bridge-only a value that is neither "allow" nor "deny" fails closed.
    let config =
        config_from("[access]\nremote_input = \"bridge-only\"\nremote_owner_tui = \"maybe\"\n");
    let resolved = config.access.resolve();

    assert_eq!(resolved.remote_owner_tui, RemoteOwnerTui::Deny);
    assert_eq!(resolved.diagnostics.len(), 1, "{:?}", resolved.diagnostics);
    assert!(resolved.diagnostics[0].contains("remote_owner_tui"));
    assert!(resolved.diagnostics[0].contains("deny"));

    // A non-string value is kept as text and fails closed the same way.
    let config = config_from("[access]\nremote_input = \"bridge-only\"\nremote_owner_tui = 3\n");
    assert_eq!(
        config.access.resolve().remote_owner_tui,
        RemoteOwnerTui::Deny
    );

    // With remote input left at any, the value has no effect.
    let config = config_from("[access]\nremote_owner_tui = \"maybe\"\n");
    let resolved = config.access.resolve();
    assert_eq!(resolved.remote_owner_tui, RemoteOwnerTui::Allow);
    assert_eq!(resolved.remote.input, RemoteInput::Any);
}

// Per-pane input mode.

fn set_mode_request(pane: &str, mode: &str) -> crate::api::schema::Request {
    crate::api::schema::Request {
        id: "mode".into(),
        method: crate::api::schema::Method::AccessPaneSet(
            crate::api::schema::AccessPaneSetParams {
                pane_id: pane.into(),
                accept_from: None,
                input_mode: Some(mode.into()),
            },
        ),
    }
}

#[tokio::test]
async fn input_mode_defaults_any_and_persists() {
    use crate::access::policy::InputMode;
    let mut app = test_app(&Config::default());
    app.state.workspaces.push(Workspace::test_new("w"));
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    let pane = *app.state.workspaces[0].panes.keys().next().unwrap();
    let terminal = app.state.workspaces[0]
        .pane_state(pane)
        .unwrap()
        .attached_terminal_id
        .clone();
    app.state.terminals.get_mut(&terminal).unwrap().cwd = std::env::current_dir().unwrap();
    let public = app.public_pane_id(0, pane).unwrap();

    // The default is `any`, and an `any` pane writes nothing about it.
    assert_eq!(app.state.terminals[&terminal].input_mode, InputMode::Any);
    let json = serde_json::to_string(&snapshot_of(&app.state)).unwrap();
    assert!(!json.contains("input_mode"), "{json}");

    // A mode set through the API survives a snapshot round trip. It needs no policy.
    app.state.session_dirty = false;
    let response = app.handle_api_request(set_mode_request(&public, "human-only"));
    assert!(
        response.contains("\"input_mode\":\"human-only\""),
        "{response}"
    );
    assert!(app.state.terminals[&terminal].access.is_none());
    assert!(app.state.session_dirty);
    let json = serde_json::to_string(&snapshot_of(&app.state)).unwrap();
    assert!(json.contains("\"input_mode\":\"human-only\""), "{json}");
    let snapshot: SessionSnapshot = serde_json::from_str(&json).unwrap();
    let restored = restore_terminals(&snapshot);
    assert_eq!(restored.len(), 1);
    assert!(restored
        .values()
        .all(|t| t.input_mode == InputMode::HumanOnly && t.access.is_none()));

    // The mark that an admin chose the mode on purpose is saved too.
    app.state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .input_mode_set_explicitly = true;
    app.state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .input_mode_mark_owner = Some("pi|worker".into());
    app.state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .input_mode_mark_session = Some("proc:4242:7|".into());
    let json_marked = serde_json::to_string(&snapshot_of(&app.state)).unwrap();
    assert!(
        json_marked.contains("input_mode_set_explicitly"),
        "{json_marked}"
    );
    let marked: SessionSnapshot = serde_json::from_str(&json_marked).unwrap();
    assert!(restore_terminals(&marked).values().all(|t| {
        t.input_mode_set_explicitly
            && t.input_mode_mark_owner.as_deref() == Some("pi|worker")
            && t.input_mode_mark_session.as_deref() == Some("proc:4242:7|")
    }));
    app.state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .input_mode_set_explicitly = false;

    // A saved mode that can't be read comes back human-only, never loosened.
    let garbled = json.replace("\"human-only\"", "\"flag\"");
    let snapshot: SessionSnapshot = serde_json::from_str(&garbled).unwrap();
    assert!(restore_terminals(&snapshot)
        .values()
        .all(|t| t.input_mode == InputMode::HumanOnly));

    // Lowering it back drops the mode, and keeps the mark that an admin chose it.
    app.handle_api_request(set_mode_request(&public, "any"));
    let json = serde_json::to_string(&snapshot_of(&app.state)).unwrap();
    assert!(!json.contains("\"input_mode\""), "{json}");
    assert!(json.contains("input_mode_set_explicitly"), "{json}");
}

#[test]
fn access_config_parses_input_modes() {
    use crate::access::policy::InputMode;
    let config = config_from(
        "[access.default]\ninput_mode = \"human-only\"\n[access.locked]\naccept_from = [\"none\"]\ninput_mode = \"human-only\"\n[access.plain]\ninput_mode = \"any\"\n[access.moded]\ninput_mode = \"human-only\"\n",
    );
    let resolved = config.access.resolve();

    assert!(
        resolved.diagnostics.is_empty(),
        "{:?}",
        resolved.diagnostics
    );
    // A section with only a mode binds no policy.
    assert_eq!(resolved.defaults.default, None);
    assert_eq!(resolved.defaults.default_mode, InputMode::HumanOnly);
    assert_eq!(
        resolved.defaults.by_name.get("locked"),
        Some(&vec![Principal::None])
    );
    assert!(!resolved.defaults.by_name.contains_key("moded"));
    assert_eq!(
        resolved.defaults.mode_by_name.get("locked"),
        Some(&InputMode::HumanOnly)
    );
    assert_eq!(
        resolved.defaults.mode_by_name.get("moded"),
        Some(&InputMode::HumanOnly)
    );
    // A named `any` is kept: it overrides a default mode for that name.
    assert_eq!(
        resolved.defaults.mode_by_name.get("plain"),
        Some(&InputMode::Any)
    );

    let config = config_from(
        "[access.worker]\ninput_mode = \"flag\"\n[access.other]\ninput_mode = \"sometimes\"\n",
    );
    let resolved = config.access.resolve();
    // A mode that can't be read fails closed to `human-only`, with a diagnostic.
    assert_eq!(resolved.defaults.mode_by_name.len(), 2);
    assert!(resolved
        .defaults
        .mode_by_name
        .values()
        .all(|mode| *mode == InputMode::HumanOnly));
    assert_eq!(resolved.diagnostics.len(), 2, "{:?}", resolved.diagnostics);
    assert!(resolved
        .diagnostics
        .iter()
        .all(|d| d.contains("human-only")));
    assert!(resolved.diagnostics[0].contains("access.other.input_mode"));
    assert!(!resolved.diagnostics[0].contains("sometimes"));
    assert!(resolved.diagnostics[1].contains("not available in v1"));
    let config = config_from("[access.default]\ninput_mode = \"human_only\"\n");
    let resolved = config.access.resolve();
    assert_eq!(resolved.defaults.default_mode, InputMode::HumanOnly);
    assert_eq!(resolved.diagnostics.len(), 1);
    // A section with neither key is still a mistake.
    let config = config_from("[access.empty]\n");
    assert_eq!(config.access.resolve().diagnostics.len(), 1);
}

#[test]
fn bridge_only_with_human_only_on_bridge_is_config_error() {
    use crate::access::policy::InputMode;
    let config = config_from(
        "[access]\nremote_input = \"bridge-only\"\nbridge_agent = \"relay\"\n[access.relay]\naccept_from = [\"outside\"]\ninput_mode = \"human-only\"\n[access.worker]\ninput_mode = \"human-only\"\n",
    );
    let resolved = config.access.resolve();

    // The key is ignored, so the bridge stays `any`; its policy and the others are untouched.
    assert!(!resolved.defaults.mode_by_name.contains_key("relay"));
    assert_eq!(
        resolved.defaults.mode_by_name.get("worker"),
        Some(&InputMode::HumanOnly)
    );
    assert_eq!(
        resolved.defaults.by_name.get("relay"),
        Some(&vec![Principal::Outside])
    );
    assert_eq!(resolved.diagnostics.len(), 1, "{:?}", resolved.diagnostics);
    assert!(resolved.diagnostics[0].contains("access.relay.input_mode"));
    assert!(resolved.diagnostics[0].contains("bridge"));
    assert!(config
        .collect_diagnostics()
        .iter()
        .any(|d| d.contains("access.relay.input_mode")));

    // The same section is fine when remote input is open to everyone, or names no bridge.
    for toml in [
        "[access.relay]\ninput_mode = \"human-only\"\n[access]\nbridge_agent = \"relay\"\n",
        "[access]\nremote_input = \"bridge-only\"\n[access.relay]\ninput_mode = \"human-only\"\n",
    ] {
        let resolved = config_from(toml).access.resolve();
        assert!(
            resolved.diagnostics.is_empty(),
            "{:?}",
            resolved.diagnostics
        );
        assert_eq!(
            resolved.defaults.mode_by_name.get("relay"),
            Some(&InputMode::HumanOnly)
        );
    }
}

#[tokio::test]
async fn sticky_default_mode_skips_bridge() {
    use crate::access::policy::{AccessDefaults, InputMode, RemoteInput};
    let defaults = AccessDefaults {
        default: Some(vec![Principal::Outside]),
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    let agent_pane = |name: &str| {
        let (mut state, _pane, terminal) = state_with_pane();
        state.access_defaults = defaults.clone();
        state.remote_settings.input = RemoteInput::BridgeOnly;
        state.remote_settings.bridge_agent = Some("relay".into());
        state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_agent_name(name.into());
        state.bind_default_access(&terminal);
        state.terminals.remove(&terminal).unwrap()
    };

    // An ordinary agent gets the default policy and the default mode.
    let worker = agent_pane("worker");
    assert!(worker.access.is_some());
    assert_eq!(worker.input_mode, InputMode::HumanOnly);

    // The bridge gets the policy but never the mode: it receives by API by design.
    let relay = agent_pane("relay");
    assert!(relay.access.is_some());
    assert_eq!(relay.input_mode, InputMode::Any);

    // A mode-only default needs no policy.
    let (mut state, _pane, terminal) = state_with_pane();
    state.access_defaults.default_mode = InputMode::HumanOnly;
    state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .set_agent_name("worker".into());
    state.bind_default_access_all();
    assert!(state.terminals[&terminal].access.is_none());
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn default_mode_applies_whatever_the_policy_and_yields_to_a_named_mode() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let mut defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    defaults
        .by_name
        .insert("worker".into(), vec![Principal::Outside]);
    defaults.mode_by_name.insert("loose".into(), InputMode::Any);
    let agent_pane = |name: &str, setup: &dyn Fn(&mut crate::terminal::TerminalState)| {
        let (mut state, _pane, terminal) = state_with_pane();
        state.access_defaults = defaults.clone();
        let t = state.terminals.get_mut(&terminal).unwrap();
        t.set_agent_name(name.into());
        setup(t);
        state.bind_default_access(&terminal);
        state.terminals.remove(&terminal).unwrap()
    };
    // A pane with a policy of its own, and one opened on purpose, still get the default mode.
    let with_policy = agent_pane("worker", &|t| {
        t.access = Some(policy(vec![Principal::Outside]))
    });
    assert_eq!(with_policy.input_mode, InputMode::HumanOnly);
    let opened = agent_pane("worker", &|t| t.access_opened_explicitly = true);
    assert_eq!(opened.input_mode, InputMode::HumanOnly);
    // A name with its own `input_mode` (even `any`) is left to that section.
    assert_eq!(agent_pane("loose", &|_| {}).input_mode, InputMode::Any);
}

#[tokio::test]
async fn explicit_mode_lowering_survives_the_next_detection() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, _pane, terminal) = state_with_pane();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    let t = state.terminals.get_mut(&terminal).unwrap();
    t.set_agent_name("worker".into());
    t.input_mode_set_explicitly = true;
    state.bind_default_access(&terminal);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::Any);
    state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .input_mode_set_explicitly = false;
    state.bind_default_access(&terminal);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn a_bridge_pane_with_a_mode_is_reported() {
    use crate::access::policy::InputMode;
    let (mut state, _pane, terminal) = state_with_pane();
    state.remote_settings.input = RemoteInput::BridgeOnly;
    state.remote_settings.bridge_agent = Some("relay".into());
    let t = state.terminals.get_mut(&terminal).unwrap();
    t.set_agent_name("relay".into());
    assert!(state.bridge_terminals_with_a_mode().is_empty());
    state.terminals.get_mut(&terminal).unwrap().input_mode = InputMode::HumanOnly;
    assert_eq!(state.bridge_terminals_with_a_mode(), vec![terminal]);
}

fn detect(
    state: &mut AppState,
    pane_id: crate::layout::PaneId,
    agent: Option<Agent>,
    exited: bool,
) {
    state.handle_app_event(AppEvent::StateChanged {
        pane_id,
        agent,
        state: AgentState::Working,
        visible_blocker: false,
        visible_working: false,
        process_exited: exited,
        observed_at: Instant::now(),
    });
}

#[tokio::test]
async fn the_explicit_mark_clears_when_a_different_agent_takes_the_pane() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, pane, terminal) = state_with_pane();
    agent_seen(&mut state, pane);
    state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .mark_input_mode_explicit();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    // The same agent reporting again keeps the mark, and the default stays out.
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert!(state.terminals[&terminal].input_mode_set_explicitly);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::Any);
    // A different agent, with no `agent start`, is a new occupant: the mark goes and the
    // default mode applies.
    detect(&mut state, pane, Some(Agent::Claude), false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn the_explicit_mark_clears_when_the_agent_exits_and_another_arrives() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, pane, terminal) = state_with_pane();
    agent_seen(&mut state, pane);
    state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .mark_input_mode_explicit();
    assert_eq!(
        state.terminals[&terminal].input_mode_mark_owner.as_deref(),
        Some("pi|")
    );
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    detect(&mut state, pane, None, true);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    identify(&mut state, pane, 5151);
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn the_explicit_mark_clears_when_the_agent_exits_unseen_and_another_of_its_kind_starts() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, pane, terminal) = state_with_pane();
    agent_seen(&mut state, pane);
    state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .mark_input_mode_explicit();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    // The exit shows up only as "no agent", the same as a blink does.
    detect(&mut state, pane, None, false);
    assert!(state.terminals[&terminal].input_mode_set_explicitly);
    // A new agent of the same kind, another process, takes the pane.
    identify(&mut state, pane, 5151);
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    assert!(state.terminals[&terminal].input_mode_mark_session.is_none());
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);
}

/// A pane as the session would bring it back: named, marked for `owner`, no agent seen yet.
fn restored_marked_pane(
    owner: &str,
    session: Option<String>,
) -> (AppState, crate::layout::PaneId, TerminalId) {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, pane, terminal) = state_with_pane();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    let t = state.terminals.get_mut(&terminal).unwrap();
    t.set_agent_name("worker".into());
    t.input_mode_set_explicitly = true;
    t.input_mode_mark_owner = Some(owner.into());
    t.input_mode_mark_session = session;
    (state, pane, terminal)
}

#[tokio::test]
async fn a_restored_pane_keeps_its_mark_only_for_the_agent_it_was_set_for() {
    use crate::access::policy::InputMode;
    let (mut state, pane, terminal) = restored_marked_pane("pi|worker", Some(process_key(4242)));
    identify(&mut state, pane, 4242);
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert!(state.terminals[&terminal].input_mode_set_explicitly);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::Any);

    // Another agent came back in the pane: the mark goes and the default mode applies.
    let (mut state, pane, terminal) = restored_marked_pane("pi|worker", Some(process_key(4242)));
    identify(&mut state, pane, 4242);
    detect(&mut state, pane, Some(Agent::Claude), false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    assert!(state.terminals[&terminal].input_mode_mark_owner.is_none());
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn a_restored_pane_with_another_session_or_an_unknown_one_loses_its_mark() {
    use crate::access::policy::InputMode;
    // Same kind and name, a different session.
    let (mut state, pane, terminal) = restored_marked_pane("pi|worker", Some(process_key(4242)));
    identify(&mut state, pane, 9999);
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);

    // A saved mark with no session (an older file) reads as unknown: fail closed.
    let (mut state, pane, terminal) = restored_marked_pane("pi|worker", None);
    identify(&mut state, pane, 4242);
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);

    // The restored agent's session cannot be read: also unknown.
    let (mut state, pane, terminal) = restored_marked_pane("pi|worker", Some(process_key(4242)));
    state.handle_app_event(AppEvent::AgentProcessIdentified {
        pane_id: pane,
        process: None,
    });
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
}

#[tokio::test]
async fn a_mark_set_on_an_empty_pane_clears_when_the_first_agent_arrives() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, pane, terminal) = state_with_pane();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .mark_input_mode_explicit();
    assert!(state.terminals[&terminal].input_mode_mark_owner.is_none());
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn a_detection_that_blinks_does_not_clear_the_mark() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, pane, terminal) = state_with_pane();
    agent_seen(&mut state, pane);
    state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .mark_input_mode_explicit();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    // The label drops for a moment while the process is still there, then comes back.
    detect(&mut state, pane, None, false);
    identify(&mut state, pane, 4242);
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert!(state.terminals[&terminal].input_mode_set_explicitly);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::Any);
}

#[tokio::test]
async fn a_name_change_on_the_detection_path_clears_the_mark() {
    let (mut state, pane, terminal) = state_with_pane();
    agent_seen(&mut state, pane);
    let t = state.terminals.get_mut(&terminal).unwrap();
    t.set_agent_name("worker".into());
    t.mark_input_mode_explicit();
    // A report that renames the agent, with the label unchanged.
    state.test_update_terminal_state(pane, |t| {
        t.set_agent_name("builder".into());
        None
    });
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
}

#[tokio::test]
async fn a_pane_named_into_a_section_gets_that_sections_mode_at_detection() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, pane, terminal) = state_with_pane();
    let mut defaults = AccessDefaults::default();
    defaults
        .mode_by_name
        .insert("worker".into(), InputMode::HumanOnly);
    state.access_defaults = defaults;
    state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .set_agent_name("worker".into());
    detect(&mut state, pane, Some(Agent::Pi), false);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);
}

fn set_before_detection(
    state: &mut AppState,
    terminal: &TerminalId,
    mode: crate::access::policy::InputMode,
    default_mode: crate::access::policy::InputMode,
) {
    state.access_defaults = crate::access::policy::AccessDefaults {
        default_mode,
        ..Default::default()
    };
    // What `handle_access_pane_set` does, with no agent in the pane yet.
    let t = state.terminals.get_mut(terminal).unwrap();
    t.input_mode = mode;
    t.mark_input_mode_explicit();
    assert!(t.agent_identity().is_none());
}

#[tokio::test]
async fn a_mode_set_before_any_agent_is_detected_keeps_its_mode_at_the_first_detection() {
    use crate::access::policy::InputMode;
    let (mut state, pane, terminal) = state_with_pane();
    set_before_detection(&mut state, &terminal, InputMode::HumanOnly, InputMode::Any);
    agent_seen(&mut state, pane);
    assert_eq!(state.terminals[&terminal].input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn an_explicit_any_set_on_an_empty_pane_is_raised_by_the_default_when_the_first_agent_arrives(
) {
    use crate::access::policy::InputMode;
    let (mut state, pane, terminal) = state_with_pane();
    set_before_detection(&mut state, &terminal, InputMode::Any, InputMode::HumanOnly);
    agent_seen(&mut state, pane);
    let t = &state.terminals[&terminal];
    // Fail closed: a looser mode chosen with nobody in the pane does not shield the agent
    // that arrives from a stricter default.
    assert_eq!(t.input_mode, InputMode::HumanOnly);
    assert!(!t.input_mode_set_explicitly);
}

/// A pane with a mark set while a Pi agent is detected but its process is not read yet, under
/// a stricter default.
fn pending_mark_pane() -> (AppState, crate::layout::PaneId, TerminalId) {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, pane, terminal) = state_with_pane();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::Any,
        ..Default::default()
    };
    detect(&mut state, pane, Some(Agent::Pi), false);
    let t = state.terminals.get_mut(&terminal).unwrap();
    t.input_mode = InputMode::HumanOnly;
    t.mark_input_mode_explicit();
    assert!(t.input_mode_set_explicitly);
    assert!(t.agent_identity().is_some() && t.agent_session_key().is_none());
    (state, pane, terminal)
}

#[tokio::test]
async fn a_mode_set_while_the_agent_is_present_keeps_its_mark_when_the_session_becomes_known() {
    use crate::access::policy::InputMode;
    let (mut state, pane, terminal) = pending_mark_pane();
    // The agent is reported first, as a hook or `pane.report_agent` does; its process is
    // read a moment later.
    identify(&mut state, pane, 4242);
    detect(&mut state, pane, Some(Agent::Pi), false);
    let t = &state.terminals[&terminal];
    assert_eq!(t.input_mode, InputMode::HumanOnly);
    assert!(
        t.input_mode_set_explicitly,
        "the mark was dropped: owner {:?}, session {:?}",
        t.input_mode_mark_owner, t.input_mode_mark_session
    );
    assert_eq!(t.input_mode_mark_session, Some(process_key(4242)));
}

#[tokio::test]
async fn a_pending_mark_clears_when_the_agent_is_released() {
    use crate::access::policy::InputMode;
    let (mut state, pane, terminal) = pending_mark_pane();
    detect(&mut state, pane, None, true);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    identify(&mut state, pane, 5151);
    detect(&mut state, pane, Some(Agent::Pi), false);
    let t = &state.terminals[&terminal];
    assert!(!t.input_mode_set_explicitly);
    assert_eq!(t.input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn a_pending_mark_clears_when_a_detection_reports_no_agent() {
    use crate::access::policy::InputMode;
    let (mut state, pane, terminal) = pending_mark_pane();
    // An exit seen only as "no agent", the same as a blink looks.
    detect(&mut state, pane, None, false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    identify(&mut state, pane, 5151);
    detect(&mut state, pane, Some(Agent::Pi), false);
    let t = &state.terminals[&terminal];
    assert!(!t.input_mode_set_explicitly);
    assert_eq!(t.input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn a_pending_mark_clears_when_a_different_agent_appears() {
    use crate::access::policy::InputMode;
    let (mut state, pane, terminal) = pending_mark_pane();
    // The other agent is seen before any session is known, so only its identity differs.
    detect(&mut state, pane, Some(Agent::Claude), false);
    assert!(!state.terminals[&terminal].input_mode_set_explicitly);
    identify(&mut state, pane, 5151);
    detect(&mut state, pane, Some(Agent::Claude), false);
    let t = &state.terminals[&terminal];
    assert!(!t.input_mode_set_explicitly);
    assert_eq!(t.input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn a_restored_legacy_mark_without_a_session_is_dropped() {
    use crate::access::policy::{AccessDefaults, InputMode};
    // A pane saved before marks had owners: explicit `any`, no owner, no session.
    let (mut state, pane, terminal) = state_with_pane();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    let t = state.terminals.get_mut(&terminal).unwrap();
    t.input_mode_set_explicitly = true;
    assert!(t.input_mode_mark_owner.is_none() && t.input_mode_mark_session.is_none());
    agent_seen(&mut state, pane);
    let t = &state.terminals[&terminal];
    assert!(!t.input_mode_set_explicitly);
    assert_eq!(t.input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn a_restored_mark_with_an_owner_and_no_session_is_dropped() {
    use crate::access::policy::{AccessDefaults, InputMode};
    // A pane saved with an owner but no session: not a pending mark of this run.
    let (mut state, pane, terminal) = state_with_pane();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    let t = state.terminals.get_mut(&terminal).unwrap();
    t.input_mode_set_explicitly = true;
    t.input_mode_mark_owner = Some("pi|".into());
    agent_seen(&mut state, pane);
    let t = &state.terminals[&terminal];
    assert!(!t.input_mode_set_explicitly);
    assert_eq!(t.input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn a_restored_pending_mark_is_dropped_and_the_mode_kept() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, _pane, terminal) = pending_mark_pane();
    // The saved mark of a pane whose agent had no known session; no default applies here.
    state.access_defaults = AccessDefaults::default();
    let t = state.terminals.get_mut(&terminal).unwrap();
    let (explicit, owner, session) = (
        t.input_mode_set_explicitly,
        t.input_mode_mark_owner.clone(),
        t.input_mode_mark_session.clone(),
    );
    t.restore_input_mode_mark(explicit, owner, session);
    assert_eq!(t.input_mode, InputMode::HumanOnly);
    assert!(!t.input_mode_set_explicitly);
    assert!(t.input_mode_mark_owner.is_none() && t.input_mode_mark_session.is_none());
    // A completed mark does round-trip.
    t.input_mode_set_explicitly = true;
    t.restore_input_mode_mark(true, Some("pi|".into()), Some(process_key(4242)));
    assert!(t.input_mode_set_explicitly);
    assert_eq!(t.input_mode_mark_session, Some(process_key(4242)));
}

#[tokio::test]
async fn an_explicit_any_set_before_the_session_is_known_is_raised_by_the_default() {
    use crate::access::policy::AccessDefaults;
    use crate::access::policy::InputMode;
    let (mut state, pane, terminal) = state_with_pane();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::HumanOnly,
        ..Default::default()
    };
    detect(&mut state, pane, Some(Agent::Pi), false);
    let t = state.terminals.get_mut(&terminal).unwrap();
    t.input_mode = InputMode::Any;
    t.mark_input_mode_explicit();
    assert!(t.agent_session_key().is_none());
    identify(&mut state, pane, 4242);
    detect(&mut state, pane, Some(Agent::Pi), false);
    let t = &state.terminals[&terminal];
    assert!(!t.input_mode_set_explicitly);
    assert_eq!(t.input_mode, InputMode::HumanOnly);
}

#[tokio::test]
async fn a_pending_human_only_mark_completes_for_the_same_agent() {
    use crate::access::policy::{AccessDefaults, InputMode};
    let (mut state, pane, terminal) = state_with_pane();
    state.access_defaults = AccessDefaults {
        default_mode: InputMode::Any,
        ..Default::default()
    };
    detect(&mut state, pane, Some(Agent::Pi), false);
    let t = state.terminals.get_mut(&terminal).unwrap();
    t.input_mode = InputMode::HumanOnly;
    t.mark_input_mode_explicit();
    identify(&mut state, pane, 4242);
    detect(&mut state, pane, Some(Agent::Pi), false);
    let t = &state.terminals[&terminal];
    assert!(t.input_mode_set_explicitly);
    assert_eq!(t.input_mode, InputMode::HumanOnly);
    assert_eq!(t.input_mode_mark_session, Some(process_key(4242)));
}
