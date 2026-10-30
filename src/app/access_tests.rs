// herdr-fork: tests for per-pane access policy state, persistence, config and the sticky default. Unofficial stopgap; see FORK.md.
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

fn agent_seen(state: &mut AppState, pane_id: crate::layout::PaneId) {
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

fn reload_with(app: &mut App, content: &str) {
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
    assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
}

#[test]
fn reload_config_does_not_loosen_access_defaults() {
    let mut app = test_app(&config_from("[access.default]\naccept_from = [\"none\"]\n"));
    let before = app.state.access_defaults.clone();
    assert_eq!(before.default, Some(vec![Principal::None]));

    reload_with(&mut app, "[access.default]\naccept_from = [\"outside\"]\n");
    assert_eq!(app.state.access_defaults, before);

    reload_with(&mut app, "");
    assert_eq!(app.state.access_defaults, before);
}

#[test]
fn reload_config_does_not_change_remote_input() {
    let mut app = test_app(&config_from("[access]\nremote_input = \"bridge-only\"\n"));
    assert_eq!(app.state.remote_settings.input, RemoteInput::BridgeOnly);

    reload_with(&mut app, "[access]\nremote_input = \"any\"\n");
    assert_eq!(app.state.remote_settings.input, RemoteInput::BridgeOnly);

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
