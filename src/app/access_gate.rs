// herdr-fork: the access gate: classify an api caller and decide whether a method may reach a pane's input. Unofficial stopgap; see FORK.md.
//! One check in front of every Input-class api method. `check_access` is the
//! only entry point; the server calls it right after the shutdown check and
//! again right before it dispatches, once queued events have been applied.
//!
//! The gate does nothing until something asks for it: with no bound policy,
//! no caller token and remote input left at `any`, it returns before it
//! classifies anything.

use crate::access::caller::{classify, Caller};
use crate::access::policy::{
    decide, AccessDenied, AccessPolicy, CallerClass, GateTarget, RemoteInput,
};
use crate::api::schema::{AccessClass, Method};
use crate::terminal::TerminalId;

use super::App;

#[cfg(test)]
thread_local! {
    static CLASSIFY_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times `classify_caller` ran on this thread. Lets a test prove the
/// off path never classifies.
#[cfg(test)]
pub(crate) fn classify_calls() -> usize {
    CLASSIFY_CALLS.with(std::cell::Cell::get)
}

/// The caller class a refusal names, for the log. Never the token.
fn denied_from(denied: &AccessDenied) -> &str {
    match denied {
        AccessDenied::Input { from, .. } | AccessDenied::Control { from, .. } => from,
        AccessDenied::Remote { .. } => "remote",
    }
}

impl App {
    /// True when some pane holds a policy.
    pub(crate) fn any_access_protected(&self) -> bool {
        self.state
            .terminals
            .values()
            .any(|terminal| terminal.access.is_some())
    }

    /// The gate has nothing to enforce for this caller: no pane is protected,
    /// the caller carries no token, and remote input is not bridge-only.
    fn access_gate_idle(&self, caller: &Caller) -> bool {
        let carries_token = matches!(caller, Caller::Peer { token: Some(_), .. });
        !carries_token
            && self.state.remote_settings.input != RemoteInput::BridgeOnly
            && !self.any_access_protected()
    }

    /// Classify a caller. A pane shell is matched by pid first (an integer
    /// compare per pane, no syscall) and only a pid that appears in the
    /// caller's lineage has its identity read, so the cost follows the lineage
    /// and not the pane count.
    pub(crate) fn classify_caller(&self, caller: &Caller) -> CallerClass {
        #[cfg(test)]
        CLASSIFY_CALLS.with(|calls| calls.set(calls.get() + 1));
        match caller {
            #[cfg(test)]
            Caller::Classified(class) => class.clone(),
            Caller::Peer {
                lineage,
                token,
                remote,
            } => {
                let mut panes = Vec::new();
                for (terminal, runtime) in self.terminal_runtimes.iter() {
                    let Some(pid) = runtime.child_pid() else {
                        continue;
                    };
                    if !lineage.chain.iter().any(|entry| entry.pid == pid) {
                        continue;
                    }
                    match crate::platform::process_identity(pid) {
                        Some(identity) => panes.push((terminal.clone(), identity)),
                        // A pane shell that sits in the caller's lineage but can't be
                        // read can't be ruled out, so the caller is not shown to be outside.
                        None => {
                            return CallerClass::Unknown {
                                remote_hint: *remote || lineage.via_ssh,
                            }
                        }
                    }
                }
                let token_match = token
                    .as_deref()
                    .and_then(|token| self.match_caller_token(token));
                classify(lineage, *remote, &panes, std::process::id(), token_match)
            }
        }
    }

    /// Named callers are registered by `access.caller.*`, which has not landed:
    /// no token matches yet, so a token never raises a caller above `Outside`.
    fn match_caller_token(
        &self,
        _token: &str,
    ) -> Option<(String, crate::access::policy::AccessBindingId)> {
        None
    }

    /// Allow or refuse `method` for `caller`. Returns before classifying when
    /// the gate is idle and the method is not an `access.*` one.
    pub(crate) fn check_access(
        &self,
        method: &Method,
        caller: &Caller,
    ) -> Result<(), AccessDenied> {
        let operation = method.operation();
        if self.access_gate_idle(caller) && !operation.name.starts_with("access.") {
            return Ok(());
        }
        let class = self.classify_caller(caller);
        let target = self.gate_target(method);
        if matches!(target, GateTarget::Open) && operation.access == AccessClass::Input {
            tracing::debug!(
                method = operation.name,
                "input method target did not resolve; the handler reports it"
            );
        }
        decide(&target, &class, &self.state.remote_settings).map_err(|denied| {
            let denied = self.with_public_pane_id(&class, denied);
            let terminal = match &target {
                GateTarget::Input { terminal, .. } | GateTarget::PaneControl { terminal, .. } => {
                    terminal.as_str()
                }
                GateTarget::Open | GateTarget::AccessAdmin => "",
            };
            tracing::warn!(
                method = operation.name,
                access_class = ?operation.access,
                from = denied_from(&denied),
                terminal,
                "access denied"
            );
            denied
        })
    }

    /// Would an Input request from `caller` be allowed on this pane? Reads use
    /// it before they inject anything into a pane.
    fn input_allowed_on(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        caller: &Caller,
    ) -> bool {
        if self.access_gate_idle(caller) {
            return true;
        }
        let class = self.classify_caller(caller);
        let target = self.pane_gate_target(ws_idx, pane_id, false);
        decide(&target, &class, &self.state.remote_settings).is_ok()
    }

    /// False when this read could send synthetic input into its pane (the
    /// alt-screen wheel harvest) and `caller` would be refused Input there.
    /// Resolves the target exactly as `alt_screen_read_spec` does.
    pub(crate) fn read_may_inject_input(&self, method: &Method, caller: &Caller) -> bool {
        // Nothing is protected: resolve nothing, allocate nothing.
        if self.access_gate_idle(caller) {
            return true;
        }
        let target = match method {
            Method::AgentRead(params) => self.resolve_agent_target(&params.target),
            Method::PaneRead(params) => self.resolve_terminal_target(&params.pane_id),
            _ => return true,
        };
        match target {
            Ok(target) => self.input_allowed_on(target.ws_idx, target.pane_id, caller),
            // An unresolved target has no pane to inject into.
            Err(_) => true,
        }
    }

    /// Would an Input request from `caller` be allowed on this terminal right
    /// now? A running wheel harvest asks before each step. A terminal that no
    /// longer resolves has nothing to inject into.
    pub(crate) fn terminal_input_allowed(&self, terminal: &TerminalId, caller: &Caller) -> bool {
        if self.access_gate_idle(caller) {
            return true;
        }
        match self.resolve_terminal_target(terminal.as_str()) {
            Ok(target) => self.input_allowed_on(target.ws_idx, target.pane_id, caller),
            Err(_) => true,
        }
    }

    /// Which gate rule a method falls under, and on which pane.
    ///
    /// A target that does not resolve is `Open`, so the handler answers with
    /// its own not-found error. That is safe only because every resolver here
    /// is the very function the handler calls on the same string, so the gate
    /// and the handler can never pick different panes:
    ///
    /// - `agent.prompt`: `resolve_agent_target`, as `validate_agent_prompt`.
    /// - `agent.send_keys`: `resolve_agent_target`, as `handle_agent_send_keys`.
    /// - `pane.send_text`, `pane.send_keys`, `pane.send_input`: `parse_pane_id`,
    ///   as `handle_pane_send_text`, `handle_pane_send_keys`,
    ///   `handle_pane_send_input` (it takes current ids, moved-pane aliases and
    ///   the legacy `p_<n>`, `p_<ws>_<n>` and `<ws>-<n>` forms).
    /// - `agent.start`: `parse_current_public_pane_id`, as `start_agent`.
    ///
    /// The match has no `_` arm: a new method does not compile until it is
    /// placed here.
    fn gate_target(&self, method: &Method) -> GateTarget<'_> {
        match method {
            Method::AgentPrompt(params) => self.agent_gate_target(&params.target),
            Method::AgentSendKeys(params) => self.agent_gate_target(&params.target),
            Method::PaneSendText(params) => self.parsed_pane_gate_target(&params.pane_id),
            Method::PaneSendKeys(params) => self.parsed_pane_gate_target(&params.pane_id),
            Method::PaneSendInput(params) => self.parsed_pane_gate_target(&params.pane_id),
            Method::AgentStart(params) => {
                match self.parse_current_public_pane_id(&params.pane_id) {
                    Some((ws_idx, pane_id)) => self.pane_gate_target(ws_idx, pane_id, true),
                    None => GateTarget::Open,
                }
            }
            // Open: nothing here writes caller-chosen bytes into an existing pane.
            Method::Ping(_)
            | Method::ServerStop(_)
            | Method::ServerLiveHandoff(_)
            | Method::ServerReloadConfig(_)
            | Method::ServerSshAgentRegister(_)
            | Method::ServerAgentManifests(_)
            | Method::ServerReloadAgentManifests(_)
            | Method::NotificationShow(_)
            | Method::ProductAnnouncementDismiss(_)
            | Method::ReleaseNotesDismiss(_)
            | Method::CommandInvoke(_)
            | Method::ClientWindowTitleSet(_)
            | Method::ClientWindowTitleClear(_)
            | Method::ClientShellSurfaceSet(_)
            | Method::SessionSnapshot(_)
            | Method::WorkspaceCreate(_)
            | Method::WorkspaceList(_)
            | Method::WorkspaceGet(_)
            | Method::WorkspaceFocus(_)
            | Method::WorkspaceRename(_)
            | Method::WorkspaceMove(_)
            | Method::WorkspaceMoveBlock(_)
            | Method::WorkspaceReportMetadata(_)
            | Method::WorkspaceClose(_)
            | Method::WorktreeList(_)
            | Method::WorktreeCreate(_)
            | Method::WorktreeOpen(_)
            | Method::WorktreeRemove(_)
            | Method::TabCreate(_)
            | Method::TabList(_)
            | Method::TabGet(_)
            | Method::TabFocus(_)
            | Method::TabRename(_)
            | Method::TabMove(_)
            | Method::TabClose(_)
            | Method::AgentList(_)
            | Method::AgentGet(_)
            | Method::AgentRead(_)
            | Method::AgentExplain(_)
            | Method::AgentRename(_)
            | Method::AgentViewSet(_)
            | Method::AgentViewClear(_)
            | Method::AgentFocus(_)
            | Method::AgentWait(_)
            | Method::PaneSplit(_)
            | Method::PaneSwap(_)
            | Method::PaneMove(_)
            | Method::PaneZoom(_)
            | Method::PaneLayout(_)
            | Method::PaneProcessInfo(_)
            | Method::LayoutExport(_)
            | Method::LayoutApply(_)
            | Method::LayoutSetSplitRatio(_)
            | Method::PaneNeighbor(_)
            | Method::PaneEdges(_)
            | Method::PaneFocusDirection(_)
            | Method::PaneResize(_)
            | Method::PaneScroll(_)
            | Method::PaneClear(_)
            | Method::PaneEditScrollback(_)
            | Method::PaneSelectionRead(_)
            | Method::PaneCopyMotion(_)
            | Method::PaneCopySearch(_)
            | Method::PaneList(_)
            | Method::PaneCurrent(_)
            | Method::PaneGet(_)
            | Method::PaneFocus(_)
            | Method::PaneInputSet(_)
            | Method::PaneLinkActivate(_)
            | Method::PaneLinkResolve(_)
            | Method::PaneRename(_)
            | Method::PaneRead(_)
            | Method::PaneReportAgent(_)
            | Method::PaneReportAgentSession(_)
            | Method::PaneReportMetadata(_)
            | Method::PaneClearAgentAuthority(_)
            | Method::PaneReleaseAgent(_)
            | Method::PaneClose(_)
            | Method::PopupClose(_)
            | Method::EventsSubscribe(_)
            | Method::EventsWait(_)
            | Method::PaneWaitForOutput(_)
            | Method::IntegrationList(_)
            | Method::IntegrationInstall(_)
            | Method::IntegrationUninstall(_)
            | Method::PluginLink(_)
            | Method::PluginList(_)
            | Method::PluginUnlink(_)
            | Method::PluginEnable(_)
            | Method::PluginDisable(_)
            | Method::PluginActionList(_)
            | Method::PluginActionInvoke(_)
            | Method::PluginLogList(_)
            | Method::PluginPaneOpen(_)
            | Method::PluginPaneFocus(_)
            | Method::PluginPaneClose(_) => GateTarget::Open,
        }
    }

    fn agent_gate_target(&self, target: &str) -> GateTarget<'_> {
        match self.resolve_agent_target(target) {
            Ok(resolved) => self.pane_gate_target(resolved.ws_idx, resolved.pane_id, false),
            Err(_) => GateTarget::Open,
        }
    }

    fn parsed_pane_gate_target(&self, pane_id: &str) -> GateTarget<'_> {
        match self.parse_pane_id(pane_id) {
            Some((ws_idx, pane_id)) => self.pane_gate_target(ws_idx, pane_id, false),
            None => GateTarget::Open,
        }
    }

    /// The pane's gate target. With `control_if_policy`, a pane that holds a
    /// policy is a control target (`agent.start`); one without is still an
    /// Input target, so remote-input rules apply to it.
    fn pane_gate_target(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        control_if_policy: bool,
    ) -> GateTarget<'_> {
        let Some(terminal) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
        else {
            return GateTarget::Open;
        };
        let label = self
            .public_pane_id(ws_idx, pane_id)
            .unwrap_or_else(|| "unknown".to_string());
        let policy: Option<&AccessPolicy> = self
            .state
            .terminals
            .get(terminal)
            .and_then(|state| state.access.as_ref());
        if control_if_policy && policy.is_some() {
            GateTarget::PaneControl {
                label,
                terminal,
                policy,
            }
        } else {
            GateTarget::Input {
                label,
                terminal,
                policy,
            }
        }
    }

    /// The pure model names a pane caller by terminal id; the contract names
    /// it by public pane id.
    fn with_public_pane_id(&self, class: &CallerClass, denied: AccessDenied) -> AccessDenied {
        let CallerClass::Pane(terminal) = class else {
            return denied;
        };
        let from = format!("pane:{}", self.public_pane_id_of_terminal(terminal));
        match denied {
            AccessDenied::Input { label, .. } => AccessDenied::Input { label, from },
            AccessDenied::Control { label, .. } => AccessDenied::Control { label, from },
            remote @ AccessDenied::Remote { .. } => remote,
        }
    }

    fn public_pane_id_of_terminal(&self, terminal: &TerminalId) -> String {
        self.resolve_terminal_target(terminal.as_str())
            .ok()
            .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id))
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::policy::{AccessBindingId, Principal};
    use crate::api::schema::{
        AgentPromptParams, AgentSendKeysParams, AgentStartParams, PaneSendInputParams,
        PaneSendKeysParams, PaneSendTextParams, PingParams,
    };
    use crate::app::AppPolicy;
    use crate::platform::PeerLineage;
    use crate::workspace::Workspace;

    fn app() -> App {
        let (_, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    /// An app with one workspace of `panes` panes and a terminal state each.
    fn app_with_panes(panes: usize) -> App {
        let mut app = app();
        let mut workspace = Workspace::test_new("gate");
        for _ in 1..panes {
            workspace.test_split(ratatui::layout::Direction::Horizontal);
        }
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app
    }

    fn pane_ids(app: &App) -> Vec<crate::layout::PaneId> {
        app.state.workspaces[0].tabs[0].layout.pane_ids()
    }

    fn terminal_of(app: &App, pane: crate::layout::PaneId) -> TerminalId {
        app.state.workspaces[0].terminal_id(pane).unwrap().clone()
    }

    fn protect(app: &mut App, terminal: &TerminalId, accept_from: Vec<Principal>) {
        app.state.terminals.get_mut(terminal).unwrap().access = Some(AccessPolicy {
            binding: AccessBindingId::mint().unwrap(),
            accept_from,
        });
    }

    fn outside() -> Caller {
        Caller::Classified(CallerClass::Outside { via_ssh: false })
    }

    fn send_text(pane_id: &str) -> Method {
        Method::PaneSendText(PaneSendTextParams {
            pane_id: pane_id.into(),
            text: "x".into(),
        })
    }

    /// One resolvable instance of every Input-class method. A new Input
    /// method has no case here, which fails the test below on purpose.
    fn input_method_for(name: &str, public_pane_id: &str, agent_name: &str) -> Method {
        match name {
            "agent.send_keys" => Method::AgentSendKeys(AgentSendKeysParams {
                target: agent_name.into(),
                keys: vec!["Enter".into()],
            }),
            "agent.start" => Method::AgentStart(AgentStartParams {
                name: "worker".into(),
                kind: "pi".into(),
                pane_id: public_pane_id.into(),
                args: Vec::new(),
                timeout_ms: None,
            }),
            "agent.prompt" => Method::AgentPrompt(AgentPromptParams {
                target: agent_name.into(),
                text: "x".into(),
                wait: None,
            }),
            "pane.send_text" => send_text(public_pane_id),
            "pane.send_keys" => Method::PaneSendKeys(PaneSendKeysParams {
                pane_id: public_pane_id.into(),
                keys: vec!["Enter".into()],
            }),
            "pane.send_input" => Method::PaneSendInput(PaneSendInputParams {
                pane_id: public_pane_id.into(),
                text: "x".into(),
                keys: Vec::new(),
            }),
            other => {
                panic!("{other} is Input class: add a case here and an arm in App::gate_target")
            }
        }
    }

    #[test]
    fn every_input_class_method_maps_to_an_input_gate_target() {
        let mut app = app_with_panes(1);
        let pane = pane_ids(&app)[0];
        let terminal = terminal_of(&app, pane);
        app.state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_agent_name("reviewer".into());
        let public = app.public_pane_id(0, pane).unwrap();
        let mut seen = 0;
        for operation in Method::OPERATIONS
            .iter()
            .filter(|operation| operation.access == AccessClass::Input)
        {
            let method = input_method_for(operation.name, &public, "reviewer");
            assert_eq!(method.operation().name, operation.name);
            assert!(
                matches!(
                    app.gate_target(&method),
                    GateTarget::Input { .. } | GateTarget::PaneControl { .. }
                ),
                "{} maps to an open gate target",
                operation.name
            );
            seen += 1;
        }
        assert_eq!(seen, 6, "the Input class changed; update this test");
    }

    #[test]
    fn non_input_methods_stay_open() {
        let app = app_with_panes(1);
        let method = Method::Ping(PingParams {});
        assert!(matches!(app.gate_target(&method), GateTarget::Open));
    }

    #[test]
    fn gate_target_resolves_with_the_handlers_parse_pane_id() {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let terminal = terminal_of(&app, panes[1]);
        protect(&mut app, &terminal, vec![Principal::None]);
        let raw = format!("p_{}", panes[1].raw());
        let GateTarget::Input {
            terminal: resolved,
            policy,
            ..
        } = app.gate_target(&send_text(&raw))
        else {
            panic!("a legacy id must resolve to an input target");
        };
        assert_eq!(resolved, &terminal);
        assert!(policy.is_some());
    }

    #[test]
    fn agent_start_into_a_protected_pane_is_control_and_into_an_open_pane_is_input() {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let protected = terminal_of(&app, panes[1]);
        protect(&mut app, &protected, vec![Principal::Outside]);
        let start = |pane: crate::layout::PaneId, app: &App| {
            Method::AgentStart(AgentStartParams {
                name: "worker".into(),
                kind: "pi".into(),
                pane_id: app.public_pane_id(0, pane).unwrap(),
                args: Vec::new(),
                timeout_ms: None,
            })
        };
        assert!(matches!(
            app.gate_target(&start(panes[1], &app)),
            GateTarget::PaneControl {
                policy: Some(_),
                ..
            }
        ));
        assert!(matches!(
            app.gate_target(&start(panes[0], &app)),
            GateTarget::Input { policy: None, .. }
        ));
    }

    #[test]
    fn idle_gate_does_not_classify() {
        let app = app_with_panes(4);
        let before = classify_calls();
        let method = send_text("1");
        let carrier = Caller::Peer {
            lineage: PeerLineage::default(),
            token: None,
            remote: false,
        };
        for caller in [outside(), carrier] {
            assert!(app.check_access(&method, &caller).is_ok());
        }
        assert_eq!(classify_calls(), before);
    }

    #[test]
    fn a_caller_token_or_a_policy_or_bridge_only_wakes_the_gate() {
        let tokened = Caller::Peer {
            lineage: PeerLineage::default(),
            token: Some("t".into()),
            remote: false,
        };
        let app = app_with_panes(1);
        let before = classify_calls();
        let _ = app.check_access(&send_text("1"), &tokened);
        assert_eq!(classify_calls(), before + 1);

        let mut app = app_with_panes(1);
        let terminal = terminal_of(&app, pane_ids(&app)[0]);
        protect(&mut app, &terminal, vec![Principal::Outside]);
        let before = classify_calls();
        let _ = app.check_access(&send_text("1"), &outside());
        assert_eq!(classify_calls(), before + 1);

        let mut app = app_with_panes(1);
        app.state.remote_settings.input = RemoteInput::BridgeOnly;
        let before = classify_calls();
        let _ = app.check_access(&send_text("1"), &outside());
        assert_eq!(classify_calls(), before + 1);
    }

    #[test]
    fn denial_names_the_callers_public_pane_id() {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let victim = terminal_of(&app, panes[1]);
        protect(&mut app, &victim, vec![Principal::Outside]);
        let caller_terminal = terminal_of(&app, panes[0]);
        let caller = Caller::Classified(CallerClass::Pane(caller_terminal.clone()));
        let victim_id = app.public_pane_id(0, panes[1]).unwrap();
        let caller_id = app.public_pane_id(0, panes[0]).unwrap();
        let denied = app
            .check_access(&send_text(&victim_id), &caller)
            .unwrap_err();
        let text = denied.message();
        assert!(
            text.contains(&format!("does not accept input from pane:{caller_id}.")),
            "{text}"
        );
        assert!(!text.contains(caller_terminal.as_str()), "{text}");
        assert!(text.contains(&format!("pane {victim_id} ")), "{text}");
    }

    #[test]
    fn a_missing_target_is_left_to_the_handler() {
        let mut app = app_with_panes(1);
        let terminal = terminal_of(&app, pane_ids(&app)[0]);
        protect(&mut app, &terminal, vec![Principal::None]);
        assert!(app
            .check_access(&send_text("no-such-pane"), &outside())
            .is_ok());
    }

    /// A shell pid for a channel-backed runtime: a real process, so its
    /// identity can be read.
    #[cfg(unix)]
    fn spawn_sleeper() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_process_inside_a_pane_classifies_as_that_pane() {
        let mut app = app_with_panes(3);
        let panes = pane_ids(&app);
        let mut shell = spawn_sleeper();
        let target = terminal_of(&app, panes[1]);
        for (index, pane) in panes.iter().enumerate() {
            let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
            // Only the middle pane has the real shell; the others a pid nobody has.
            runtime.test_set_child_pid(if index == 1 {
                shell.id()
            } else {
                3_999_000 + index as u32
            });
            let terminal = terminal_of(&app, *pane);
            app.terminal_runtimes.insert(terminal, runtime);
        }
        // The shell itself stands in for a process running in that pane.
        let lineage = crate::platform::process_lineage(shell.id());
        assert!(!lineage.chain.is_empty());
        let caller = Caller::Peer {
            lineage,
            token: None,
            remote: false,
        };
        assert_eq!(app.classify_caller(&caller), CallerClass::Pane(target));
        let _ = shell.kill();
        let _ = shell.wait();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pane_shell_that_cannot_be_read_makes_the_caller_unknown() {
        let mut app = app_with_panes(1);
        let pane = pane_ids(&app)[0];
        let mut gone = std::process::Command::new("true").spawn().expect("spawn");
        let pid = gone.id();
        gone.wait().expect("wait");
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.test_set_child_pid(pid);
        let terminal = terminal_of(&app, pane);
        app.terminal_runtimes.insert(terminal, runtime);
        let caller = Caller::Peer {
            lineage: PeerLineage {
                chain: vec![crate::platform::ProcessIdentity {
                    pid,
                    start_token: 7,
                }],
                complete: true,
                via_ssh: false,
            },
            token: None,
            remote: false,
        };
        assert_eq!(
            app.classify_caller(&caller),
            CallerClass::Unknown { remote_hint: false }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_reused_pane_pid_does_not_match() {
        let mut app = app_with_panes(1);
        let pane = pane_ids(&app)[0];
        let mut shell = spawn_sleeper();
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.test_set_child_pid(shell.id());
        let terminal = terminal_of(&app, pane);
        app.terminal_runtimes.insert(terminal, runtime);
        let mut lineage = crate::platform::process_lineage(shell.id());
        // Same pid, different process: the token must tell them apart.
        lineage.chain[0].start_token ^= 0xFFFF;
        let caller = Caller::Peer {
            lineage,
            token: None,
            remote: false,
        };
        assert!(!matches!(
            app.classify_caller(&caller),
            CallerClass::Pane(_)
        ));
        let _ = shell.kill();
        let _ = shell.wait();
    }

    /// Budget (global constraints): off p95 <= 1 us; on p95 <= 50 us with 16
    /// panes, 1 policy and 2 callers; the lineage walk p95 <= 250 us on macOS
    /// and <= 1 ms on the aarch64 Linux image. Run by `just bench-access`.
    #[tokio::test]
    #[ignore = "manual access gate budget benchmark"]
    async fn access_gate_profile() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        const ITERATIONS: usize = 10_000;
        const OFF_BUDGET: Duration = Duration::from_micros(1);
        const ON_BUDGET: Duration = Duration::from_micros(50);
        let lineage_budget = if cfg!(target_os = "macos") {
            Duration::from_micros(250)
        } else {
            Duration::from_millis(1)
        };

        fn p95(mut samples: Vec<Duration>) -> Duration {
            samples.sort_unstable();
            samples[samples.len() * 95 / 100]
        }
        fn measure(mut run: impl FnMut()) -> Duration {
            for _ in 0..256 {
                run();
            }
            let mut samples = Vec::with_capacity(ITERATIONS);
            for _ in 0..ITERATIONS {
                let start = Instant::now();
                run();
                samples.push(start.elapsed());
            }
            p95(samples)
        }

        let mut app = app_with_panes(16);
        let panes = pane_ids(&app);
        // Pane 0's shell is this process, so one caller really is inside a pane;
        // the others hold pids no lineage contains.
        for (index, pane) in panes.iter().enumerate() {
            let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
            runtime.test_set_child_pid(if index == 0 {
                std::process::id()
            } else {
                3_999_000 + index as u32
            });
            let terminal = terminal_of(&app, *pane);
            app.terminal_runtimes.insert(terminal, runtime);
        }
        let target = app.public_pane_id(0, panes[0]).unwrap();
        let method = send_text(&target);
        let own_lineage = crate::platform::process_lineage(std::process::id());
        let in_pane = Caller::Peer {
            lineage: own_lineage,
            token: None,
            remote: false,
        };
        let mut stranger_chain = crate::platform::process_lineage(std::process::id());
        stranger_chain
            .chain
            .retain(|entry| entry.pid != std::process::id());
        let stranger = Caller::Peer {
            lineage: stranger_chain,
            token: None,
            remote: false,
        };

        let off = measure(|| {
            let _ = black_box(app.check_access(black_box(&method), black_box(&stranger)));
        });

        let terminal = terminal_of(&app, panes[0]);
        protect(&mut app, &terminal, vec![Principal::Outside]);
        let on_pane = measure(|| {
            let _ = black_box(app.check_access(black_box(&method), black_box(&in_pane)));
        });
        let on_outside = measure(|| {
            let _ = black_box(app.check_access(black_box(&method), black_box(&stranger)));
        });
        let on = on_pane.max(on_outside);

        // Delta R1: bridge-only turns the gate on with no policy bound.
        app.state.terminals.get_mut(&terminal).unwrap().access = None;
        app.state.remote_settings.input = RemoteInput::BridgeOnly;
        let bridge_only_no_policy = measure(|| {
            let _ = black_box(app.check_access(black_box(&method), black_box(&stranger)));
        });

        let lineage = measure(|| {
            black_box(crate::platform::process_lineage(black_box(
                std::process::id(),
            )));
        });

        println!("access_gate off_p95_ns={}", off.as_nanos());
        println!("access_gate on_pane_p95_ns={}", on_pane.as_nanos());
        println!("access_gate on_outside_p95_ns={}", on_outside.as_nanos());
        println!(
            "access_gate bridge_only_no_policy_p95_ns={}",
            bridge_only_no_policy.as_nanos()
        );
        println!("access_gate lineage_walk_p95_ns={}", lineage.as_nanos());
        assert!(off <= OFF_BUDGET, "off p95 {off:?} is over {OFF_BUDGET:?}");
        assert!(on <= ON_BUDGET, "on p95 {on:?} is over {ON_BUDGET:?}");
        assert!(
            bridge_only_no_policy <= ON_BUDGET,
            "bridge_only_no_policy p95 {bridge_only_no_policy:?} is over {ON_BUDGET:?}"
        );
        assert!(
            lineage <= lineage_budget,
            "lineage walk p95 {lineage:?} is over {lineage_budget:?}"
        );
    }
}
