// herdr-fork: the access gate (a pane's input mode is checked after its allow-list, on the api path, the shell-client path and for attached terminals): classify an api caller and decide whether a method may reach a pane's input; an `agent.start` that loosens a pane's policy and every `access.caller.*` method and `access.pane.set` are admin only, and a caller token is matched against the caller registry; a shell client's pane and popup input are judged with its own lineage, and a remote refusal names the bridge's binding; classification uses the shell identity cached at spawn or adoption, and Input requests are recorded in the input log when it is on. Unofficial stopgap; see FORK.md.
//! One check in front of every Input-class api method. `check_access` is the
//! only entry point; the server calls it right after the shutdown check and
//! again right before it dispatches, once queued events have been applied.
//!
//! The gate does nothing until something asks for it: with no bound policy,
//! no caller token and remote input left at `any`, it returns before it
//! classifies anything. The exceptions are `access.*` methods and an
//! `agent.start` that carries `accept_from`, which are always checked.

use crate::access::caller::{classify, Caller};
use crate::access::input_log::InputPath;
use crate::access::policy::{
    check_input_mode, decide, is_looser, parse_accept_from, AcceptFrom, AccessDenied, AccessPolicy,
    CallerClass, GateTarget, InputMode, RemoteInput, RemoteOwnerTui,
};
use crate::api::schema::{AccessClass, AgentStartParams, Method};
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
        AccessDenied::HumanOnly { .. } => "human-only",
    }
}

impl App {
    /// True when some pane holds a policy or an input mode.
    pub(crate) fn any_access_protected(&self) -> bool {
        self.state
            .terminals
            .values()
            .any(|terminal| terminal.access.is_some() || !terminal.input_mode.is_any())
    }

    /// Apply the pane's input mode to an input the allow-list admitted. A target that is
    /// not a pane has no mode. `Any` reads one field and returns.
    fn input_mode_check(
        &self,
        target: &GateTarget<'_>,
        class: &CallerClass,
        path: InputPath,
    ) -> Result<(), AccessDenied> {
        let (GateTarget::Input {
            label, terminal, ..
        }
        | GateTarget::PaneControl {
            label, terminal, ..
        }) = target
        else {
            return Ok(());
        };
        let mode = self
            .state
            .terminals
            .get(*terminal)
            .map_or(InputMode::Any, |state| state.input_mode);
        check_input_mode(mode, path, class, label)
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
    /// compare per pane), and only a pid that appears in the caller's lineage
    /// has its cached identity compared. Nothing is read from the system here.
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
                ..
            } => {
                let mut panes = Vec::new();
                for (terminal, runtime) in self.terminal_runtimes.iter() {
                    let Some(pid) = runtime.child_pid() else {
                        continue;
                    };
                    if !lineage.chain.iter().any(|entry| entry.pid == pid) {
                        continue;
                    }
                    // The identity cached at spawn or adoption: no proc walk here.
                    match runtime.child_identity() {
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

    /// The registered caller a token belongs to, compared in constant time.
    /// A token that matches nobody leaves the caller `Outside`.
    fn match_caller_token(
        &self,
        token: &str,
    ) -> Option<(String, crate::access::policy::AccessBindingId)> {
        if self.state.access_callers.is_empty() {
            return None;
        }
        self.state.access_callers.lookup(token)
    }

    /// Allow or refuse `method` for `caller`. Returns before classifying when
    /// the gate is idle and the method is neither an `access.*` one nor an
    /// `agent.start` with `accept_from`. An `agent.start` that loosens a policy
    /// must pass both the pane's rule and the admin rule.
    #[cfg(test)]
    pub(crate) fn check_access(
        &self,
        method: &Method,
        caller: &Caller,
    ) -> Result<(), AccessDenied> {
        self.check_access_logging(method, caller, None)
    }

    /// `check_access` for a request the server is about to run: it also records the
    /// request's origin when `input_log` is on. A refusal is always recorded; an allow is
    /// recorded only when `last` is set, the check right before dispatch, so one request
    /// is one entry.
    pub(crate) fn gate_input(
        &self,
        method: &Method,
        caller: &Caller,
        last: bool,
    ) -> Result<(), AccessDenied> {
        self.check_access_logging(method, caller, Some(last))
    }

    fn check_access_logging(
        &self,
        method: &Method,
        caller: &Caller,
        log: Option<bool>,
    ) -> Result<(), AccessDenied> {
        let operation = method.operation();
        let binds_at_start =
            matches!(method, Method::AgentStart(params) if params.accept_from.is_some());
        let logs_input =
            log.is_some() && self.state.input_log && operation.access == AccessClass::Input;
        // With the log on, the last check of an Input request classifies the caller even when
        // nothing is protected, to name it in the log. The early check of an idle gate cannot
        // refuse anything and has nothing to record, so it still returns first.
        let classifies_for_log = logs_input && log == Some(true);
        if self.access_gate_idle(caller)
            && !operation.name.starts_with("access.")
            && !binds_at_start
            && !classifies_for_log
        {
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
        // A loosening start must pass the pane's own rule and the admin rule.
        // The pane rule answers first.
        let pane_result = decide(&target, &class, &self.state.remote_settings);
        let admin_result = if self.start_escalates(method) {
            decide(
                &GateTarget::AccessAdmin,
                &class,
                &self.state.remote_settings,
            )
        } else {
            Ok(())
        };
        // The mode check runs last: an allow-list refusal keeps its own text.
        let result = pane_result
            .and(admin_result)
            .and_then(|()| self.input_mode_check(&target, &class, InputPath::Api));
        if let (true, Some(last)) = (logs_input, log) {
            if let GateTarget::Input {
                label, terminal, ..
            }
            | GateTarget::PaneControl {
                label, terminal, ..
            } = &target
            {
                if result.is_err() || last {
                    self.log_api_input(
                        method,
                        caller,
                        &class,
                        label.clone(),
                        terminal,
                        result.is_ok(),
                    );
                }
            }
        }
        result.map_err(|denied| {
            let denied = self.with_bridge_binding(self.with_public_pane_id(&class, denied));
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
            && self
                .input_mode_check(&target, &class, InputPath::Api)
                .is_ok()
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
            Method::AgentStart(params) => self.agent_start_gate_target(params),
            // Admin: only an outside caller manages named callers.
            Method::AccessCallerAdd(_)
            | Method::AccessCallerList(_)
            | Method::AccessCallerRotate(_)
            | Method::AccessCallerRetarget(_)
            | Method::AccessCallerRemove(_) => GateTarget::AccessAdmin,
            // Admin: only an outside caller changes a pane's policy, even to
            // tighten it and even when nothing is protected yet.
            Method::AccessPaneSet(_) => GateTarget::AccessAdmin,
            // Admin: the input log names every sender, so only an outside caller reads it.
            Method::AccessLog(_) => GateTarget::AccessAdmin,
            // Open (Meta): reading a policy writes nothing into any pane.
            Method::AccessPaneGet(_) => GateTarget::Open,
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

    /// Does this `agent.start` ask for more access than the pane gives today
    /// (or would give from its section or default)? An explicit `accept_from`
    /// that is looser than that, or `any` on a pane that was not already opened
    /// on purpose, loosens a policy: it needs the admin check on top of the
    /// pane check, never instead of it. An `accept_from` that does not parse
    /// is the handler's error to report.
    fn start_escalates(&self, method: &Method) -> bool {
        let Method::AgentStart(params) = method else {
            return false;
        };
        let Some(values) = params.accept_from.as_deref() else {
            return false;
        };
        let Ok(explicit) = parse_accept_from(values) else {
            return false;
        };
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id) else {
            return false;
        };
        let Some(terminal) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
        else {
            return false;
        };
        let opens_new = matches!(explicit, AcceptFrom::Any)
            && !self
                .state
                .terminals
                .get(terminal)
                .is_some_and(|state| state.access_opened_explicitly);
        opens_new
            || is_looser(
                &explicit,
                self.state.start_baseline_access(terminal, &params.name),
            )
    }

    /// `agent.start`: control of the pane if it holds a policy, else input.
    fn agent_start_gate_target(&self, params: &AgentStartParams) -> GateTarget<'_> {
        match self.parse_current_public_pane_id(&params.pane_id) {
            Some((ws_idx, pane_id)) => self.pane_gate_target(ws_idx, pane_id, true),
            None => GateTarget::Open,
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
            other @ (AccessDenied::Remote { .. } | AccessDenied::HumanOnly { .. }) => other,
        }
    }

    /// A remote refusal names the bridge's binding id next to its agent name. The bridge
    /// pane may start after the server, so the binding is read when the denial is made.
    fn with_bridge_binding(&self, denied: AccessDenied) -> AccessDenied {
        let AccessDenied::Remote {
            label,
            bridge_agent: Some(agent),
            bridge_binding: None,
        } = denied
        else {
            return denied;
        };
        let bridge_binding = self
            .resolve_agent_target(&agent)
            .ok()
            .and_then(|target| self.state.terminals.get(target.terminal_id.as_str()))
            .and_then(|terminal| terminal.access.as_ref())
            .map(|policy| policy.binding.clone());
        AccessDenied::Remote {
            label,
            bridge_agent: Some(agent),
            bridge_binding,
        }
    }

    /// May a shell client type into, or paste an image into, `pane_id`? Judged as `Input`
    /// against the pane's policy with the class of the client's own connection. Reads the
    /// stored lineage only; nothing is walked, cloned or allocated before the idle check. A
    /// pane id that does not resolve is left to the handler. Logging is the caller's job, so
    /// it can rate-limit.
    ///
    /// The owner's remote TUI (a `via_ssh` client) types like an outside client unless
    /// `remote_input = "bridge-only"` and `remote_owner_tui = "deny"`, where rule 0 applies.
    pub(crate) fn check_client_shell_input(
        &self,
        caller: &Caller,
        pane_id: &str,
    ) -> Result<(), AccessDenied> {
        if self.access_gate_idle(caller) {
            return Ok(());
        }
        let Some((ws_idx, pane)) = self.parse_pane_id(pane_id) else {
            return Ok(());
        };
        let class = self.owner_tui_class(self.classify_caller(caller));
        let target = self.pane_gate_target(ws_idx, pane, false);
        decide(&target, &class, &self.state.remote_settings)
            .and_then(|()| self.input_mode_check(&target, &class, InputPath::Client))
            .map_err(|denied| self.with_bridge_binding(self.with_public_pane_id(&class, denied)))
    }

    /// May an attached terminal's client send keystrokes, mouse reports or a pasted image
    /// into `terminal`? Attach input is not gated by the allow-list (the documented attach
    /// bypass), but it counts for the input mode: `human-only` takes it only from a client
    /// classified `Outside`. A pane with mode `any` reads one field and returns.
    pub(crate) fn check_attach_input(
        &self,
        caller: &Caller,
        terminal: &TerminalId,
    ) -> Result<(), AccessDenied> {
        let mode = self
            .state
            .terminals
            .get(terminal)
            .map_or(InputMode::Any, |state| state.input_mode);
        if mode.is_any() {
            return Ok(());
        }
        let class = self.owner_tui_class(self.classify_caller(caller));
        let label = self.pane_label_of_terminal(terminal);
        check_input_mode(mode, InputPath::Client, &class, &label)
            .map_err(|denied| self.with_public_pane_id(&class, denied))
    }

    /// May a shell client type into, or paste an image into, the popup terminal? Pane
    /// policy does not apply: the popup terminal can't carry a binding. Rule 0 does, so a
    /// remote TUI under `bridge-only` + `deny` is view-only here too.
    pub(crate) fn check_client_popup_input(
        &self,
        caller: &Caller,
        terminal: &TerminalId,
    ) -> Result<(), AccessDenied> {
        if self.state.remote_settings.input != RemoteInput::BridgeOnly
            || self.state.remote_owner_tui != RemoteOwnerTui::Deny
        {
            return Ok(());
        }
        let class = self.classify_caller(caller);
        let target = GateTarget::Input {
            label: terminal.as_str().to_string(),
            terminal,
            policy: None,
        };
        decide(&target, &class, &self.state.remote_settings)
            .map_err(|denied| self.with_bridge_binding(self.with_public_pane_id(&class, denied)))
    }

    /// Under `allow`, the owner's remote TUI types as a local client does: the ssh marks
    /// that only rule 0 reads are cleared.
    fn owner_tui_class(&self, mut class: CallerClass) -> CallerClass {
        if self.state.remote_owner_tui == RemoteOwnerTui::Allow {
            match &mut class {
                CallerClass::Outside { via_ssh } => *via_ssh = false,
                CallerClass::Unknown { remote_hint } => *remote_hint = false,
                _ => {}
            }
        }
        class
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
                accept_from: None,
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
                accept_from: None,
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
            conn: 0,
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
            conn: 0,
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
            conn: 0,
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
            conn: 0,
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
            conn: 0,
        };
        assert!(!matches!(
            app.classify_caller(&caller),
            CallerClass::Pane(_)
        ));
        let _ = shell.kill();
        let _ = shell.wait();
    }

    #[tokio::test]
    async fn read_harvest_and_pane_input_checks_honor_human_only() {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let terminal = terminal_of(&app, panes[0]);
        assert!(app.terminal_input_allowed(&terminal, &outside()));
        // A mode alone, with no policy anywhere, turns the gate on.
        app.state.terminals.get_mut(&terminal).unwrap().input_mode = InputMode::HumanOnly;
        assert!(app.any_access_protected());
        assert!(!app.terminal_input_allowed(&terminal, &outside()));
        assert!(!app.input_allowed_on(0, panes[0], &outside()));
        assert!(app.input_allowed_on(0, panes[1], &outside()));
    }

    /// `check_access` is the gate without the log: its idle shortcut is the only thing that
    /// lets a request through unexamined, so a protected pane and bridge-only must defeat it.
    #[tokio::test]
    async fn idle_shortcut_yields_to_a_protected_pane_and_to_bridge_only() {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let target = app.public_pane_id(0, panes[0]).unwrap();
        let from_pane = Caller::Classified(CallerClass::Pane(terminal_of(&app, panes[1])));
        // Nothing protected: open.
        assert!(app.check_access(&send_text(&target), &from_pane).is_ok());
        // A protected pane refuses a pane caller.
        let terminal = terminal_of(&app, panes[0]);
        protect(&mut app, &terminal, vec![Principal::Outside]);
        assert!(app.check_access(&send_text(&target), &from_pane).is_err());
        // Bridge-only refuses a remote caller with no policy bound anywhere.
        app.state.terminals.get_mut(&terminal).unwrap().access = None;
        app.state.remote_settings.input = RemoteInput::BridgeOnly;
        let remote = Caller::Classified(CallerClass::Remote);
        assert!(app.check_access(&send_text(&target), &remote).is_err());
    }

    #[tokio::test]
    async fn classification_reads_the_cached_identity_not_the_system() {
        let mut app = app_with_panes(1);
        let pane = pane_ids(&app)[0];
        // A pid no process has: only a cached identity can make this pane match.
        let identity = crate::platform::ProcessIdentity {
            pid: 3_999_500,
            start_token: 77,
        };
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.test_set_child_pid(identity.pid);
        runtime.test_set_child_identity(Some(identity));
        let terminal = terminal_of(&app, pane);
        app.terminal_runtimes.insert(terminal.clone(), runtime);
        let caller = Caller::Peer {
            lineage: PeerLineage {
                chain: vec![identity],
                complete: true,
                via_ssh: false,
            },
            token: None,
            remote: false,
            conn: 0,
        };
        assert_eq!(
            app.classify_caller(&caller),
            CallerClass::Pane(terminal.clone())
        );
        // A reused pid carries another start token and never matches the cached one.
        let mut reused = caller.clone();
        if let Caller::Peer { lineage, .. } = &mut reused {
            lineage.chain[0].start_token += 1;
        }
        assert!(!matches!(
            app.classify_caller(&reused),
            CallerClass::Pane(_)
        ));
        // Once the child has exited its identity is gone, and a lineage holding that pid is
        // not shown to be outside.
        app.terminal_runtimes
            .get(&terminal)
            .unwrap()
            .test_set_child_identity(None);
        assert_eq!(
            app.classify_caller(&caller),
            CallerClass::Unknown { remote_hint: false }
        );
    }

    /// Budget (global constraints): off p95 <= 1 us; on p95 <= 50 us with 16
    /// panes, 1 policy and 2 callers; the lineage walk p95 <= 250 us on macOS
    /// and <= 1 ms on the aarch64 Linux image; the input mode check on an `any` pane <= 1 us.
    /// Run by `just bench-access`.
    #[tokio::test]
    #[ignore = "manual access gate budget benchmark"]
    async fn access_gate_profile() {
        use crate::access::input_log::{
            InputEvent, InputLog, InputPath, LoggedClass, DECISION_ALLOW, DECISION_DENY, RING_CAP,
        };
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
            conn: 1,
        };
        let mut stranger_chain = crate::platform::process_lineage(std::process::id());
        stranger_chain
            .chain
            .retain(|entry| entry.pid != std::process::id());
        let stranger = Caller::Peer {
            lineage: stranger_chain,
            token: None,
            remote: false,
            conn: 2,
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

        // Delta R2 (§5): the input-origin log, with the work measured directly. The gate is
        // back to its defaults (nothing protected, remote input `any`) and the log is on.
        const LOG_BUDGET: Duration = Duration::from_micros(5);
        app.state.remote_settings.input = RemoteInput::Any;
        let classify_cached = measure(|| {
            black_box(app.classify_caller(black_box(&in_pane)));
        })
        .max(measure(|| {
            black_box(app.classify_caller(black_box(&stranger)));
        }));

        // Delta R2 (§5): the input mode check on a pane whose mode is `any`, which is every
        // pane unless an operator set one: a lookup of the pane's mode and one compare.
        let mode_target = app.gate_target(&method);
        let mode_class = CallerClass::Outside { via_ssh: false };
        let input_mode_any = [InputPath::Api, InputPath::Client]
            .into_iter()
            .map(|path| {
                measure(|| {
                    let _ = black_box(app.input_mode_check(
                        black_box(&mode_target),
                        black_box(&mode_class),
                        black_box(path),
                    ));
                })
            })
            .max()
            .unwrap();

        let log_event = |decision: &'static str, at_ms: u64| InputEvent {
            at_ms,
            pane: "1-1".into(),
            terminal: "term_bench".into(),
            path: InputPath::Api,
            route: "pane.send_text",
            class: LoggedClass::Outside,
            principal: None,
            client: "api:1".into(),
            host: "local",
            bytes: 1,
            decision,
            count: 1,
        };
        // These cases time the successful path through `record`: an entry is queued, and now
        // and then the writer is woken. The channel (1024) must therefore never be full while
        // timing, or `send` returns early on the drop path and the numbers mean nothing. The
        // writer is the one the server runs (it sleeps with no timer and is woken by the
        // first entry after it went idle), and `sync` below, outside the timed region, lets
        // it empty the channel. Every case asserts that nothing was dropped.
        const CHUNK: usize = 400;
        let fill_ring = |log: &mut InputLog| {
            for chunk in 0..(RING_CAP / 512) {
                for n in 0..512usize {
                    let mut event = log_event(DECISION_ALLOW, (chunk * 512 + n) as u64);
                    event.terminal = format!("term_fill_{chunk}_{n}");
                    log.record(event);
                }
                log.sync();
            }
        };
        let measure_synced = |sync: &mut dyn FnMut(), run: &mut dyn FnMut()| -> Duration {
            for _ in 0..128 {
                run();
            }
            let mut samples = Vec::with_capacity(ITERATIONS);
            while samples.len() < ITERATIONS {
                sync();
                for _ in 0..CHUNK {
                    let start = Instant::now();
                    run();
                    samples.push(start.elapsed());
                }
            }
            sync();
            p95(samples)
        };
        let bench_dir = crate::access::registry::TestDir::new("bench-log");
        let writer_log = |label: &str| {
            std::fs::create_dir_all(bench_dir.path().join(label)).unwrap();
            let mut log = InputLog::new();
            log.attach_writer(bench_dir.path().join(label), "node-a".into())
                .unwrap();
            log
        };

        let allow_log = std::cell::RefCell::new(writer_log("allow"));
        fill_ring(&mut allow_log.borrow_mut());
        let record_allow_coalesce =
            measure_synced(&mut || allow_log.borrow_mut().sync(), &mut || {
                allow_log
                    .borrow_mut()
                    .record(black_box(log_event(DECISION_ALLOW, 1_000_000)));
            });
        let allow_dropped = allow_log.borrow().dropped();
        allow_log.borrow_mut().flush_and_stop();

        let deny_log = std::cell::RefCell::new(writer_log("deny"));
        fill_ring(&mut deny_log.borrow_mut());
        let record_deny_immediate =
            measure_synced(&mut || deny_log.borrow_mut().sync(), &mut || {
                deny_log
                    .borrow_mut()
                    .record(black_box(log_event(DECISION_DENY, 2_000_000)));
            });
        let deny_dropped = deny_log.borrow().dropped();
        deny_log.borrow_mut().flush_and_stop();

        // The first entry after the writer has gone idle pays for waking it. Each sample is
        // taken with the writer parked: `sync` returns once it has written everything, and a
        // short sleep lets it reach its park. Half the samples are a refusal (sent at once),
        // half an entry closed by the next one.
        let idle_log = std::cell::RefCell::new(writer_log("idle"));
        fill_ring(&mut idle_log.borrow_mut());
        let mut idle_samples = Vec::with_capacity(200);
        for turn in 0..200u64 {
            let mut log = idle_log.borrow_mut();
            log.sync();
            std::thread::sleep(Duration::from_millis(3));
            let mut event = log_event(DECISION_ALLOW, 3_000_000 + turn * 10_000);
            event.terminal = format!("term_idle_{turn}");
            if turn % 2 == 0 {
                // A refusal is sent the moment it is recorded.
                event.decision = DECISION_DENY;
            } else {
                // An open entry, closed (and sent) by the timed record below.
                log.record(event.clone());
                event.terminal = format!("term_idle_closer_{turn}");
            }
            let start = Instant::now();
            log.record(black_box(event));
            idle_samples.push(start.elapsed());
        }
        let record_after_idle = p95(idle_samples);
        let idle_dropped = idle_log.borrow().dropped();
        // Information only, off the input path: what the server pays after the reply to wake
        // an idle writer for the entry the request recorded. It has no per-input budget.
        let mut unpark_samples = Vec::with_capacity(200);
        for turn in 0..200u64 {
            let mut log = idle_log.borrow_mut();
            log.sync();
            std::thread::sleep(Duration::from_millis(3));
            let mut event = log_event(DECISION_DENY, 4_000_000 + turn);
            event.terminal = format!("term_unpark_{turn}");
            log.record(event);
            let start = Instant::now();
            log.wake_writer();
            unpark_samples.push(start.elapsed());
        }
        let deferred_unpark_after_reply = p95(unpark_samples);
        idle_log.borrow_mut().flush_and_stop();

        // The per-keystroke client path and the whole API gate, on a full ring. The callers
        // and the clients alternate, so each record closes the one before it and sends it, and
        // the pane caller makes the principal scan run.
        *app.input_log.lock().unwrap() = writer_log("whole");
        fill_ring(&mut app.input_log.lock().unwrap());
        let client_terminal = terminal_of(&app, panes[0]);
        let client_pane = target.clone();
        let callers = [&stranger, &in_pane];
        let mut turn = 0usize;
        let client_keystroke = measure_synced(&mut || app.sync_input_log(), &mut || {
            turn += 1;
            let caller = callers[turn % 2];
            let _ = black_box(app.check_client_shell_input(black_box(caller), &client_pane));
            app.log_client_input(crate::app::access_log::ClientInput {
                terminal: &client_terminal,
                // Alternately the pane id is not at hand and has to be found, as for attach.
                pane: (turn % 4 < 2).then_some(client_pane.as_str()),
                route: "shell.pane_input",
                client: format!("shell:{}", turn % 3),
                caller,
                bytes: 1,
                allowed: true,
            });
        });
        let mut turn = 0usize;
        let api_log_on = measure_synced(&mut || app.sync_input_log(), &mut || {
            turn += 1;
            let caller = callers[turn % 2];
            let _ = black_box(app.gate_input(black_box(&method), black_box(caller), true));
        });
        let whole_dropped = app.input_log.lock().unwrap().dropped();
        app.input_log.lock().unwrap().flush_and_stop();
        *app.input_log.lock().unwrap() = InputLog::new();

        println!(
            "access_gate classify_cached_16_panes_p95_ns={}",
            classify_cached.as_nanos()
        );
        println!(
            "access_gate input_mode_any_p95_ns={}",
            input_mode_any.as_nanos()
        );
        println!(
            "access_gate record_allow_coalesce_p95_ns={}",
            record_allow_coalesce.as_nanos()
        );
        println!(
            "access_gate record_deny_immediate_p95_ns={}",
            record_deny_immediate.as_nanos()
        );
        println!(
            "access_gate client_keystroke_16_panes_full_ring_p95_ns={}",
            client_keystroke.as_nanos()
        );
        println!(
            "access_gate api_input_log_on_16_panes_p95_ns={}",
            api_log_on.as_nanos()
        );
        println!(
            "access_gate record_after_idle_p95_ns={}",
            record_after_idle.as_nanos()
        );
        println!(
            "access_gate deferred_unpark_after_reply_p95_ns={} (information, no budget)",
            deferred_unpark_after_reply.as_nanos()
        );
        println!(
            "access_gate dropped allow={allow_dropped} deny={deny_dropped} idle={idle_dropped} whole={whole_dropped}"
        );
        // A case that drops is timing the cheapest path through `send`, not a real send.
        assert_eq!(
            (allow_dropped, deny_dropped, idle_dropped, whole_dropped),
            (0, 0, 0, 0),
            "a timed case dropped entries"
        );
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
        for (name, measured, budget) in [
            ("input_mode_any", input_mode_any, OFF_BUDGET),
            ("classify_cached_16_panes", classify_cached, LOG_BUDGET),
            ("record_allow_coalesce", record_allow_coalesce, LOG_BUDGET),
            ("record_deny_immediate", record_deny_immediate, LOG_BUDGET),
            ("record_after_idle", record_after_idle, LOG_BUDGET),
            (
                "client_keystroke_16_panes_full_ring",
                client_keystroke,
                LOG_BUDGET,
            ),
            (
                "api_input_log_on_16_panes",
                api_log_on,
                OFF_BUDGET + LOG_BUDGET,
            ),
        ] {
            assert!(
                measured <= budget,
                "{name} p95 {measured:?} is over {budget:?}"
            );
        }
    }

    fn start_with(app: &App, pane: usize, accept_from: &[&str]) -> Method {
        let panes = pane_ids(app);
        Method::AgentStart(AgentStartParams {
            name: "worker".into(),
            kind: "pi".into(),
            pane_id: app.public_pane_id(0, panes[pane]).unwrap(),
            args: Vec::new(),
            timeout_ms: None,
            accept_from: Some(accept_from.iter().map(|v| v.to_string()).collect()),
        })
    }

    #[test]
    fn start_loosening_is_measured_against_a_bound_policy() {
        let mut app = app_with_panes(1);
        let terminal = terminal_of(&app, pane_ids(&app)[0]);
        protect(&mut app, &terminal, vec![Principal::Outside]);
        assert!(app.start_escalates(&start_with(&app, 0, &["caller:ci"])));
        assert!(app.start_escalates(&start_with(&app, 0, &["any"])));
        assert!(!app.start_escalates(&start_with(&app, 0, &["outside"])));
        assert!(!app.start_escalates(&start_with(&app, 0, &["none"])));
        // The pane target stays the pane's own: control of a bound pane.
        assert!(matches!(
            app.gate_target(&start_with(&app, 0, &["caller:ci"])),
            GateTarget::PaneControl { .. }
        ));
    }

    #[test]
    fn start_loosening_is_measured_against_the_section_or_default_an_unbound_pane_would_get() {
        let mut app = app_with_panes(1);
        // No default: a narrowing list is fine, `any` still opens a pane on purpose.
        assert!(!app.start_escalates(&start_with(&app, 0, &["caller:ci"])));
        app.state.access_defaults.default = Some(vec![Principal::Outside]);
        assert!(app.start_escalates(&start_with(&app, 0, &["caller:ci"])));
        // A section for the agent's name replaces the default.
        app.state
            .access_defaults
            .by_name
            .insert("worker".into(), vec![Principal::Caller("ci".into())]);
        assert!(!app.start_escalates(&start_with(&app, 0, &["caller:ci"])));
        assert!(app.start_escalates(&start_with(&app, 0, &["outside"])));
        // The pane target of an unbound pane is input.
        assert!(matches!(
            app.gate_target(&start_with(&app, 0, &["outside"])),
            GateTarget::Input { policy: None, .. }
        ));
    }

    #[test]
    fn start_any_escalates_unless_the_pane_was_already_opened_on_purpose() {
        let mut app = app_with_panes(1);
        let terminal = terminal_of(&app, pane_ids(&app)[0]);
        assert!(app.start_escalates(&start_with(&app, 0, &["any"])));
        app.state.access_defaults.default = Some(vec![Principal::None]);
        app.state.unbind_access(&terminal);
        assert!(!app.start_escalates(&start_with(&app, 0, &["any"])));
        assert!(!app.start_escalates(&start_with(&app, 0, &["caller:ci"])));
    }

    #[test]
    fn start_with_an_unparsable_accept_from_is_left_to_the_handler() {
        let mut app = app_with_panes(1);
        let terminal = terminal_of(&app, pane_ids(&app)[0]);
        protect(&mut app, &terminal, vec![Principal::None]);
        assert!(!app.start_escalates(&start_with(&app, 0, &["caller:*"])));
        assert!(matches!(
            app.gate_target(&start_with(&app, 0, &["caller:*"])),
            GateTarget::PaneControl { .. }
        ));
    }

    fn caller_methods(pane: &str) -> Vec<Method> {
        use crate::api::schema::{
            AccessCallerAddParams, AccessCallerNameParams, AccessCallerRetargetParams, EmptyParams,
        };
        vec![
            Method::AccessCallerAdd(AccessCallerAddParams {
                name: "ops".into(),
                target_pane: pane.into(),
            }),
            Method::AccessCallerList(EmptyParams::default()),
            Method::AccessCallerRotate(AccessCallerNameParams { name: "ops".into() }),
            Method::AccessCallerRetarget(AccessCallerRetargetParams {
                name: "ops".into(),
                target_pane: pane.into(),
            }),
            Method::AccessCallerRemove(AccessCallerNameParams { name: "ops".into() }),
        ]
    }

    fn call(app: &mut App, method: Method) -> serde_json::Value {
        let request = crate::api::schema::Request {
            id: "t".into(),
            method,
        };
        serde_json::from_str(&app.handle_api_request(request)).unwrap()
    }

    fn token_of(response: &serde_json::Value) -> String {
        assert_eq!(
            response["result"]["type"], "access_caller_token",
            "{response}"
        );
        response["result"]["token"].as_str().unwrap().to_string()
    }

    #[test]
    fn access_caller_methods_refused_from_pane_even_when_unprotected() {
        // Nothing is protected, so the gate is otherwise idle.
        let app = app_with_panes(2);
        assert!(!app.any_access_protected());
        let panes = pane_ids(&app);
        let from_pane = Caller::Classified(CallerClass::Pane(terminal_of(&app, panes[0])));
        let binding = AccessBindingId::mint().unwrap();
        let refused = [
            from_pane,
            Caller::Classified(CallerClass::Unknown { remote_hint: false }),
            Caller::Classified(CallerClass::Remote),
            Caller::Classified(CallerClass::Caller {
                name: "ops".into(),
                target: binding,
                via_ssh: false,
            }),
        ];
        for method in caller_methods("1") {
            for caller in &refused {
                let denied = app.check_access(&method, caller).unwrap_err();
                assert!(
                    matches!(denied, AccessDenied::Control { .. }),
                    "{}",
                    method.operation().name
                );
                assert!(denied
                    .message()
                    .contains("does not accept control requests from"));
            }
            assert!(
                app.check_access(&method, &outside()).is_ok(),
                "{}",
                method.operation().name
            );
        }
    }

    #[test]
    fn caller_add_requires_policy() {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let pane_id = app.public_pane_id(0, panes[0]).unwrap();
        let add = |name: &str| {
            caller_methods(&pane_id)
                .into_iter()
                .next()
                .map(|method| match method {
                    Method::AccessCallerAdd(mut params) => {
                        params.name = name.into();
                        Method::AccessCallerAdd(params)
                    }
                    other => other,
                })
                .unwrap()
        };
        let refused = call(&mut app, add("ops"));
        assert_eq!(refused["error"]["code"], "no_access_policy");
        assert_eq!(
            refused["error"]["message"],
            format!("pane {pane_id} has no access policy; set one first")
        );
        assert!(app.state.access_callers.is_empty());

        let terminal = terminal_of(&app, panes[0]);
        protect(&mut app, &terminal, vec![Principal::None]);
        let binding = app.state.terminals[&terminal]
            .access
            .as_ref()
            .unwrap()
            .binding
            .clone();
        let added = call(&mut app, add("ops"));
        let token = token_of(&added);
        assert_eq!(token.len(), 64);
        assert_eq!(added["result"]["name"], "ops");
        assert_eq!(added["result"]["binding"], binding.to_string());
        assert_eq!(
            app.state.access_callers.lookup(&token),
            Some(("ops".to_string(), binding.clone()))
        );

        // The token appears nowhere else.
        let listed = call(&mut app, Method::AccessCallerList(Default::default()));
        assert!(!listed.to_string().contains(&token));
        assert_eq!(listed["result"]["callers"][0]["name"], "ops");
        assert_eq!(
            listed["result"]["callers"][0]["binding"],
            binding.to_string()
        );
        assert_eq!(listed["result"]["callers"][0]["pane_id"], pane_id);

        // A bad or repeated name, and an unknown pane, are refused without a token.
        assert_eq!(call(&mut app, add("ops"))["error"]["code"], "caller_exists");
        assert_eq!(
            call(&mut app, add("Bad Name"))["error"]["code"],
            "invalid_caller_name"
        );
        let missing = call(
            &mut app,
            Method::AccessCallerAdd(crate::api::schema::AccessCallerAddParams {
                name: "other".into(),
                target_pane: "no-such-pane".into(),
            }),
        );
        assert_eq!(missing["error"]["code"], "pane_not_found");
    }

    #[test]
    fn caller_rotate_retarget_remove_through_the_api() {
        use crate::api::schema::{AccessCallerNameParams, AccessCallerRetargetParams};
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let (first, second) = (terminal_of(&app, panes[0]), terminal_of(&app, panes[1]));
        protect(&mut app, &first, vec![Principal::None]);
        protect(&mut app, &second, vec![Principal::None]);
        let second_binding = app.state.terminals[&second]
            .access
            .as_ref()
            .unwrap()
            .binding
            .clone();
        let first_id = app.public_pane_id(0, panes[0]).unwrap();
        let second_id = app.public_pane_id(0, panes[1]).unwrap();
        let old = token_of(&call(
            &mut app,
            Method::AccessCallerAdd(crate::api::schema::AccessCallerAddParams {
                name: "ops".into(),
                target_pane: first_id,
            }),
        ));
        let name = || AccessCallerNameParams { name: "ops".into() };
        let new = token_of(&call(&mut app, Method::AccessCallerRotate(name())));
        assert_ne!(old, new);
        assert!(app.state.access_callers.lookup(&old).is_none());
        assert!(app.state.access_callers.lookup(&new).is_some());
        assert_eq!(
            call(
                &mut app,
                Method::AccessCallerRotate(AccessCallerNameParams { name: "x".into() })
            )["error"]["code"],
            "caller_not_found"
        );

        let moved = call(
            &mut app,
            Method::AccessCallerRetarget(AccessCallerRetargetParams {
                name: "ops".into(),
                target_pane: second_id,
            }),
        );
        assert_eq!(moved["result"]["type"], "ok", "{moved}");
        assert_eq!(
            app.state.access_callers.lookup(&new),
            Some(("ops".to_string(), second_binding))
        );

        assert_eq!(
            call(&mut app, Method::AccessCallerRemove(name()))["result"]["type"],
            "ok"
        );
        assert!(app.state.access_callers.lookup(&new).is_none());
        assert_eq!(
            call(&mut app, Method::AccessCallerRemove(name()))["error"]["code"],
            "caller_not_found"
        );
    }

    #[test]
    fn caller_changes_are_saved_and_a_failed_save_changes_nothing() {
        let dir = crate::access::registry::TestDir::new("app-save");
        let path = dir.path().join("access-callers.json");
        let mut app = app_with_panes(1);
        let pane = pane_ids(&app)[0];
        let terminal = terminal_of(&app, pane);
        protect(&mut app, &terminal, vec![Principal::None]);
        let pane_id = app.public_pane_id(0, pane).unwrap();
        let add = |name: &str| {
            Method::AccessCallerAdd(crate::api::schema::AccessCallerAddParams {
                name: name.into(),
                target_pane: pane_id.clone(),
            })
        };
        app.state.access_callers_path = Some(path.clone());
        let token = token_of(&call(&mut app, add("ops")));
        let on_disk = crate::access::registry::CallerRegistry::load(&path).unwrap();
        assert_eq!(on_disk, app.state.access_callers);
        assert!(on_disk.lookup(&token).is_some());

        // The registry path is a directory: the save fails and memory is unchanged.
        let blocked = dir.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        app.state.access_callers_path = Some(blocked);
        let failed = call(&mut app, add("later"));
        assert_eq!(failed["error"]["code"], "internal_error");
        assert!(!failed.to_string().contains(&token));
        assert!(app.state.access_callers.lookup(&token).is_some());
        assert_eq!(app.state.access_callers.callers().count(), 1);

        // An unreadable registry at startup refuses changes.
        app.state.access_callers_path = Some(path.clone());
        app.state.access_callers_unreadable = true;
        assert_eq!(
            call(&mut app, add("later"))["error"]["code"],
            "internal_error"
        );
        assert_eq!(
            crate::access::registry::CallerRegistry::load(&path).unwrap(),
            on_disk
        );
    }

    fn token_peer(token: &str) -> Caller {
        Caller::Peer {
            lineage: PeerLineage {
                chain: vec![crate::platform::ProcessIdentity {
                    pid: 999_999,
                    start_token: 1,
                }],
                complete: true,
                via_ssh: false,
            },
            token: Some(token.to_string()),
            remote: false,
            conn: 0,
        }
    }

    #[test]
    fn a_registered_token_classifies_as_its_caller_and_only_reaches_its_pane() {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let (bound, other) = (terminal_of(&app, panes[0]), terminal_of(&app, panes[1]));
        protect(&mut app, &bound, vec![Principal::Caller("ops".into())]);
        protect(&mut app, &other, vec![Principal::Outside]);
        let binding = app.state.terminals[&bound]
            .access
            .as_ref()
            .unwrap()
            .binding
            .clone();
        let token = app
            .state
            .access_callers
            .add("ops", binding.clone())
            .unwrap();

        assert_eq!(
            app.classify_caller(&token_peer(&token)),
            CallerClass::Caller {
                name: "ops".into(),
                target: binding,
                via_ssh: false
            }
        );
        // A token nobody holds is just an outside caller.
        assert_eq!(
            app.classify_caller(&token_peer(&"0".repeat(64))),
            CallerClass::Outside { via_ssh: false }
        );

        let bound_id = app.public_pane_id(0, panes[0]).unwrap();
        let other_id = app.public_pane_id(0, panes[1]).unwrap();
        assert!(app
            .check_access(&send_text(&bound_id), &token_peer(&token))
            .is_ok());
        // The other pane admits outside callers, but a caller stays on its own pane.
        let denied = app
            .check_access(&send_text(&other_id), &token_peer(&token))
            .unwrap_err();
        assert!(denied.message().contains("caller:ops"));
        assert!(!denied.message().contains(&token));
        // And a caller cannot manage callers.
        for method in caller_methods(&bound_id) {
            assert!(app.check_access(&method, &token_peer(&token)).is_err());
        }
    }

    #[test]
    fn a_token_never_reaches_debug_output() {
        let mut app = app_with_panes(1);
        let pane = pane_ids(&app)[0];
        let terminal = terminal_of(&app, pane);
        protect(&mut app, &terminal, vec![Principal::None]);
        let pane_id = app.public_pane_id(0, pane).unwrap();
        let token = token_of(&call(
            &mut app,
            Method::AccessCallerAdd(crate::api::schema::AccessCallerAddParams {
                name: "ops".into(),
                target_pane: pane_id,
            }),
        ));
        let parsed: crate::api::schema::SuccessResponse =
            serde_json::from_value(serde_json::json!({
                "id": "t",
                "result": {"type": "access_caller_token", "name": "ops",
                    "binding": "acc_00000000000000000000000000000001", "token": token}
            }))
            .unwrap();
        for text in [
            format!("{parsed:?}"),
            format!("{:?}", token_peer(&token)),
            format!("{:?}", app.state.access_callers),
        ] {
            assert!(!text.contains(&token), "{text}");
        }
    }
    fn pane_set(pane: &str, accept_from: &[&str]) -> Method {
        Method::AccessPaneSet(crate::api::schema::AccessPaneSetParams {
            pane_id: pane.into(),
            accept_from: Some(accept_from.iter().map(|s| s.to_string()).collect()),
            input_mode: None,
        })
    }

    fn pane_get(pane: &str) -> Method {
        Method::AccessPaneGet(crate::api::schema::AccessPaneGetParams {
            pane_id: pane.into(),
        })
    }

    #[test]
    fn gate_access_pane_set_refused_from_pane_even_when_unprotected() {
        // Nothing is protected, so the gate is otherwise idle.
        let app = app_with_panes(2);
        assert!(!app.any_access_protected());
        let panes = pane_ids(&app);
        let pane_id = app.public_pane_id(0, panes[1]).unwrap();
        let from_pane = Caller::Classified(CallerClass::Pane(terminal_of(&app, panes[0])));
        let binding = AccessBindingId::mint().unwrap();
        let refused = [
            from_pane,
            Caller::Classified(CallerClass::Unknown { remote_hint: false }),
            Caller::Classified(CallerClass::Remote),
            Caller::Classified(CallerClass::Caller {
                name: "ops".into(),
                target: binding,
                via_ssh: false,
            }),
        ];
        // Even `any`, and even a list that only tightens, is an admin act.
        for accept_from in [&["any"][..], &["none"], &["outside"]] {
            let method = pane_set(&pane_id, accept_from);
            for caller in &refused {
                let denied = app.check_access(&method, caller).unwrap_err();
                assert!(matches!(denied, AccessDenied::Control { .. }));
                assert!(denied
                    .message()
                    .contains("does not accept control requests from"));
            }
            assert!(app.check_access(&method, &outside()).is_ok());
        }
        // Reading a policy is Meta: open to every class.
        for caller in refused.iter().chain([&outside()]) {
            assert!(app.check_access(&pane_get(&pane_id), caller).is_ok());
        }
    }

    #[test]
    fn a_pane_policy_cannot_be_changed_by_a_pane_a_token_or_a_protected_pane_peer() {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let (bridge, other) = (terminal_of(&app, panes[0]), terminal_of(&app, panes[1]));
        protect(&mut app, &bridge, vec![Principal::Caller("ops".into())]);
        protect(&mut app, &other, vec![Principal::Outside]);
        let bridge_id = app.public_pane_id(0, panes[0]).unwrap();
        let other_id = app.public_pane_id(0, panes[1]).unwrap();
        let binding = app.state.terminals[&bridge]
            .access
            .as_ref()
            .unwrap()
            .binding
            .clone();
        let token = app.state.access_callers.add("ops", binding).unwrap();
        for target in [&bridge_id, &other_id] {
            let method = pane_set(target, &["any"]);
            assert!(app.check_access(&method, &token_peer(&token)).is_err());
            assert!(app
                .check_access(
                    &method,
                    &Caller::Classified(CallerClass::Pane(bridge.clone()))
                )
                .is_err());
            assert!(app.check_access(&method, &outside()).is_ok());
        }
    }

    #[test]
    fn pane_set_binds_keeps_the_binding_and_any_unbinds() {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let terminal = terminal_of(&app, panes[0]);
        let pane_id = app.public_pane_id(0, panes[0]).unwrap();
        let policy = |app: &App| app.state.terminals[&terminal].access.clone();

        // Nothing yet: open, no binding.
        let got = call(&mut app, pane_get(&pane_id));
        assert_eq!(got["result"]["type"], "access_policy", "{got}");
        assert_eq!(got["result"]["pane_id"], pane_id);
        assert!(got["result"].get("binding").is_none());
        assert_eq!(got["result"]["accept_from"], serde_json::json!(["any"]));

        let set = call(&mut app, pane_set(&pane_id, &["outside", "caller:ops"]));
        assert_eq!(set["result"]["type"], "access_policy", "{set}");
        let bound = policy(&app).unwrap();
        assert_eq!(
            bound.accept_from,
            vec![Principal::Outside, Principal::Caller("ops".into())]
        );
        assert_eq!(set["result"]["binding"], bound.binding.to_string());
        assert_eq!(set["result"]["pane_id"], pane_id);
        assert_eq!(
            set["result"]["accept_from"],
            serde_json::json!(["outside", "caller:ops"])
        );
        assert!(!app.state.terminals[&terminal].access_opened_explicitly);

        // A rebind keeps the binding id.
        let again = call(&mut app, pane_set(&pane_id, &["none"]));
        assert_eq!(again["result"]["binding"], bound.binding.to_string());
        assert_eq!(policy(&app).unwrap().accept_from, vec![Principal::None]);
        let got = call(&mut app, pane_get(&pane_id));
        assert_eq!(got["result"]["binding"], bound.binding.to_string());
        assert_eq!(got["result"]["accept_from"], serde_json::json!(["none"]));

        // The other pane is untouched.
        assert!(app.state.terminals[&terminal_of(&app, panes[1])]
            .access
            .is_none());

        // A bad list changes nothing.
        let bad_lists: [&[&str]; 5] = [
            &[],
            &["any", "none"],
            &["bogus"],
            &["caller:Bad Name"],
            &["*"],
        ];
        for bad in bad_lists {
            let refused = call(&mut app, pane_set(&pane_id, bad));
            assert_eq!(refused["error"]["code"], "invalid_accept_from", "{bad:?}");
            assert_eq!(policy(&app).unwrap().accept_from, vec![Principal::None]);
        }
        let missing = call(&mut app, pane_set("no-such-pane", &["none"]));
        assert_eq!(missing["error"]["code"], "pane_not_found");
        let missing = call(&mut app, pane_get("no-such-pane"));
        assert_eq!(missing["error"]["code"], "pane_not_found");

        // `any` removes the policy and opens the pane on purpose.
        let opened = call(&mut app, pane_set(&pane_id, &["any"]));
        assert_eq!(opened["result"]["type"], "access_policy", "{opened}");
        assert!(opened["result"].get("binding").is_none());
        assert_eq!(opened["result"]["accept_from"], serde_json::json!(["any"]));
        assert!(policy(&app).is_none());
        assert!(app.state.terminals[&terminal].access_opened_explicitly);

        // Binding again clears that mark and mints a fresh binding.
        let rebound = call(&mut app, pane_set(&pane_id, &["outside"]));
        assert_ne!(rebound["result"]["binding"], bound.binding.to_string());
        assert!(!app.state.terminals[&terminal].access_opened_explicitly);
    }

    /// Two protected panes: the first accepts `caller:ops`, the second `outside`.
    /// Returns the app, both public ids, and a token for `ops` bound to the first.
    fn bridge_and_other() -> (App, String, String, String) {
        let mut app = app_with_panes(2);
        let panes = pane_ids(&app);
        let bridge_id = app.public_pane_id(0, panes[0]).unwrap();
        let other_id = app.public_pane_id(0, panes[1]).unwrap();
        // Binding-id targets resolve only to agent terminals, so both panes are agents.
        for (index, pane) in panes.iter().enumerate() {
            let terminal = terminal_of(&app, *pane);
            app.state
                .terminals
                .get_mut(&terminal)
                .unwrap()
                .set_agent_name(format!("agent-{index}"));
        }
        for (id, list) in [(&bridge_id, "caller:ops"), (&other_id, "outside")] {
            let set = call(&mut app, pane_set(id, &[list]));
            assert_eq!(set["result"]["type"], "access_policy", "{set}");
        }
        let token = token_of(&call(
            &mut app,
            Method::AccessCallerAdd(crate::api::schema::AccessCallerAddParams {
                name: "ops".into(),
                target_pane: bridge_id.clone(),
            }),
        ));
        (app, bridge_id, other_id, token)
    }

    fn binding_of(app: &mut App, pane_id: &str) -> String {
        let got = call(app, pane_get(pane_id));
        got["result"]["binding"].as_str().unwrap().to_string()
    }

    fn prompt_to(target: &str) -> Method {
        Method::AgentPrompt(AgentPromptParams {
            target: target.into(),
            text: "x".into(),
            wait: None,
        })
    }

    #[test]
    fn gate_caller_token_reaches_bridge_only() {
        let (mut app, bridge_id, other_id, token) = bridge_and_other();
        let peer = token_peer(&token);
        assert!(app.check_access(&send_text(&bridge_id), &peer).is_ok());
        let denied = app.check_access(&send_text(&other_id), &peer).unwrap_err();
        assert!(denied.message().contains("caller:ops"));
        assert!(!denied.message().contains(&token));
        // By binding id, too: the bridge's id is reachable, the other pane's is not.
        let (bridge_binding, other_binding) = (
            binding_of(&mut app, &bridge_id),
            binding_of(&mut app, &other_id),
        );
        assert!(app.check_access(&prompt_to(&bridge_binding), &peer).is_ok());
        assert!(app.check_access(&prompt_to(&other_binding), &peer).is_err());
        // An outside caller reaches the other pane and not the bridge.
        assert!(app.check_access(&send_text(&other_id), &outside()).is_ok());
        assert!(app
            .check_access(&send_text(&bridge_id), &outside())
            .is_err());
    }

    #[test]
    fn gate_unknown_token_from_outside_is_denied_by_bridge() {
        let (mut app, bridge_id, _other_id, token) = bridge_and_other();
        let denied_for = |app: &App, peer: &Caller| {
            app.check_access(&send_text(&bridge_id), peer)
                .unwrap_err()
                .message()
        };
        // A token nobody holds is only an outside caller, and the bridge does not take those.
        for peer in [token_peer(&"0".repeat(64)), token_peer("")] {
            let message = denied_for(&app, &peer);
            assert!(
                message.contains("does not accept input from outside"),
                "{message}"
            );
        }
        assert!(app
            .check_access(&send_text(&bridge_id), &token_peer(&token))
            .is_ok());

        // A rotated token's predecessor, and a removed caller's token, stop working.
        let rotated = token_of(&call(
            &mut app,
            Method::AccessCallerRotate(crate::api::schema::AccessCallerNameParams {
                name: "ops".into(),
            }),
        ));
        assert!(denied_for(&app, &token_peer(&token)).contains("outside"));
        assert!(app
            .check_access(&send_text(&bridge_id), &token_peer(&rotated))
            .is_ok());
        call(
            &mut app,
            Method::AccessCallerRemove(crate::api::schema::AccessCallerNameParams {
                name: "ops".into(),
            }),
        );
        assert!(denied_for(&app, &token_peer(&rotated)).contains("outside"));
    }

    /// Everything the caller-token path logs, at TRACE, while a caller is
    /// registered, authenticates, is turned away with a wrong token and a
    /// malformed one, is rotated and is removed. Neither a token nor its hex
    /// sha256 may appear.
    #[test]
    fn caller_token_and_hash_never_reach_the_log() {
        use sha2::{Digest, Sha256};
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct SharedLog(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for SharedLog {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        fn sha_hex(token: &str) -> String {
            Sha256::digest(token.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }

        let log = SharedLog::default();
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();

        let secrets = tracing::subscriber::with_default(subscriber, || {
            let (mut app, bridge_id, other_id, token) = bridge_and_other();
            let good = token_peer(&token);
            assert!(app.check_access(&send_text(&bridge_id), &good).is_ok());
            assert!(app.check_access(&send_text(&other_id), &good).is_err());
            let wrong = "f".repeat(64);
            let oversized = "x".repeat(5000);
            let bad_tokens = [
                wrong.as_str(),
                "",
                "not a token\n",
                "tok\u{0}en",
                oversized.as_str(),
            ];
            for bad in bad_tokens {
                assert!(app
                    .check_access(&send_text(&bridge_id), &token_peer(bad))
                    .is_err());
            }
            let rotated = token_of(&call(
                &mut app,
                Method::AccessCallerRotate(crate::api::schema::AccessCallerNameParams {
                    name: "ops".into(),
                }),
            ));
            assert!(app
                .check_access(&send_text(&bridge_id), &token_peer(&token))
                .is_err());
            assert!(app
                .check_access(&send_text(&bridge_id), &token_peer(&rotated))
                .is_ok());
            call(
                &mut app,
                Method::AccessCallerRemove(crate::api::schema::AccessCallerNameParams {
                    name: "ops".into(),
                }),
            );
            assert!(app
                .check_access(&send_text(&bridge_id), &token_peer(&rotated))
                .is_err());
            vec![token, rotated, wrong]
        });

        let logged = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("access denied"),
            "the denials were logged: {logged}"
        );
        for secret in &secrets {
            assert!(!logged.contains(secret.as_str()), "token leaked: {logged}");
            assert!(
                !logged.contains(&sha_hex(secret)),
                "token hash leaked: {logged}"
            );
        }
    }

    #[test]
    fn gate_retarget_moves_caller_to_new_binding() {
        let (mut app, bridge_id, other_id, token) = bridge_and_other();
        let peer = token_peer(&token);
        // A pane with no policy cannot be a caller's target, and nothing moves.
        let opened = call(&mut app, pane_set(&other_id, &["any"]));
        assert_eq!(opened["result"]["type"], "access_policy");
        let refused = call(
            &mut app,
            Method::AccessCallerRetarget(crate::api::schema::AccessCallerRetargetParams {
                name: "ops".into(),
                target_pane: other_id.clone(),
            }),
        );
        assert_eq!(refused["error"]["code"], "no_access_policy", "{refused}");
        assert!(app.check_access(&send_text(&bridge_id), &peer).is_ok());

        // Once the other pane holds a policy that admits the caller, it can move.
        call(&mut app, pane_set(&other_id, &["caller:ops"]));
        let moved = call(
            &mut app,
            Method::AccessCallerRetarget(crate::api::schema::AccessCallerRetargetParams {
                name: "ops".into(),
                target_pane: other_id.clone(),
            }),
        );
        assert_eq!(moved["result"]["type"], "ok", "{moved}");
        // The same token now reaches the new pane and no longer the old one.
        assert!(app.check_access(&send_text(&other_id), &peer).is_ok());
        assert!(app.check_access(&send_text(&bridge_id), &peer).is_err());
        let listed = call(&mut app, Method::AccessCallerList(Default::default()));
        assert_eq!(listed["result"]["callers"][0]["pane_id"], other_id);
        assert_eq!(
            listed["result"]["callers"][0]["binding"],
            binding_of(&mut app, &other_id)
        );
    }
}
