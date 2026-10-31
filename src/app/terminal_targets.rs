// herdr-fork: an agent target starting with `acc_` is a binding id and resolves only to the pane that holds it. Unofficial stopgap; see FORK.md.
use super::{api_helpers::pane_agent_status, App};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalTarget {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalTargetCandidate {
    pub terminal_id: String,
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub cwd: Option<String>,
    pub agent_status: crate::api::schema::AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalTargetError {
    NotFound {
        target: String,
    },
    Ambiguous {
        target: String,
        candidates: Vec<TerminalTargetCandidate>,
    },
}

impl App {
    pub(crate) fn resolve_terminal_target(
        &self,
        target: &str,
    ) -> Result<TerminalTarget, TerminalTargetError> {
        // `acc_` names a binding, which only agent targets address; no name,
        // label or id here may ever match one.
        if target.starts_with(crate::access::policy::BINDING_PREFIX) {
            return Err(TerminalTargetError::NotFound {
                target: target.to_string(),
            });
        }
        let terminal_matches: Vec<_> = self
            .terminal_targets()
            .into_iter()
            .filter(|candidate| candidate.terminal_id == target)
            .collect();
        if let Some(resolved) = self.single_terminal_match(target, terminal_matches)? {
            return Ok(resolved);
        }

        if let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(target) {
            if let Some(resolved) = self.terminal_target_for_pane(ws_idx, pane_id) {
                return Ok(resolved);
            }
        }

        let agent_matches: Vec<_> = self
            .terminal_targets()
            .into_iter()
            .filter(|candidate| {
                self.state
                    .terminals
                    .get(candidate.terminal_id.as_str())
                    .is_some_and(|terminal| {
                        terminal.agent_name.as_deref() == Some(target)
                            || terminal.effective_agent_label() == Some(target)
                    })
            })
            .collect();
        if let Some(resolved) = self.single_terminal_match(target, agent_matches)? {
            return Ok(resolved);
        }

        Err(TerminalTargetError::NotFound {
            target: target.to_string(),
        })
    }

    pub(crate) fn resolve_agent_target(
        &self,
        target: &str,
    ) -> Result<TerminalTarget, TerminalTargetError> {
        // A binding id is not a name and not a position: it is tried as
        // nothing else, so a stale or malformed one can never land on another
        // pane.
        if target.starts_with(crate::access::policy::BINDING_PREFIX) {
            return self.resolve_binding_target(target);
        }
        if let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(target) {
            if let Some(resolved) = self
                .terminal_target_for_pane(ws_idx, pane_id)
                .filter(|resolved| self.target_is_agent(resolved))
            {
                return Ok(resolved);
            }
        }

        let name_matches: Vec<_> = self
            .terminal_targets()
            .into_iter()
            .filter(|candidate| {
                self.state
                    .terminals
                    .get(candidate.terminal_id.as_str())
                    .is_some_and(|terminal| terminal.agent_name.as_deref() == Some(target))
            })
            .collect();
        if let Some(resolved) = self.single_terminal_match(target, name_matches)? {
            return Ok(resolved);
        }

        Err(TerminalTargetError::NotFound {
            target: target.to_string(),
        })
    }

    /// The terminal that holds the binding `target` names. Only a well-formed
    /// id with a holder resolves; a binding is minted per pane, so a second
    /// holder (which should not exist) is ambiguous and delivers nowhere.
    fn resolve_binding_target(&self, target: &str) -> Result<TerminalTarget, TerminalTargetError> {
        let not_found = || TerminalTargetError::NotFound {
            target: target.to_string(),
        };
        let binding =
            crate::access::policy::AccessBindingId::parse(target).ok_or_else(not_found)?;
        let holders: Vec<_> = self
            .terminal_targets()
            .into_iter()
            .filter(|candidate| {
                self.state
                    .terminals
                    .get(candidate.terminal_id.as_str())
                    .and_then(|terminal| terminal.access.as_ref())
                    .is_some_and(|policy| policy.binding == binding)
            })
            // As the pane-id path: a stale binding on a pane that used to
            // hold an agent resolves to nothing.
            .filter(|candidate| self.target_is_agent(candidate))
            .collect();
        self.single_terminal_match(target, holders)?
            .ok_or_else(not_found)
    }

    fn target_is_agent(&self, target: &TerminalTarget) -> bool {
        self.state
            .terminals
            .get(target.terminal_id.as_str())
            .is_some_and(|terminal| terminal.is_agent_terminal())
    }

    fn single_terminal_match(
        &self,
        target: &str,
        matches: Vec<TerminalTarget>,
    ) -> Result<Option<TerminalTarget>, TerminalTargetError> {
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            _ => Err(TerminalTargetError::Ambiguous {
                target: target.to_string(),
                candidates: matches
                    .into_iter()
                    .filter_map(|candidate| {
                        self.terminal_target_candidate(candidate.ws_idx, candidate.pane_id)
                    })
                    .collect(),
            }),
        }
    }

    fn terminal_targets(&self) -> Vec<TerminalTarget> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().enumerate().flat_map(move |(tab_idx, tab)| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| {
                            tab.terminal_id(pane_id).map(|terminal_id| TerminalTarget {
                                ws_idx,
                                tab_idx,
                                pane_id,
                                terminal_id: terminal_id.to_string(),
                            })
                        })
                })
            })
            .collect()
    }

    fn terminal_target_for_pane(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<TerminalTarget> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let terminal_id = ws.terminal_id(pane_id)?.to_string();
        Some(TerminalTarget {
            ws_idx,
            tab_idx,
            pane_id,
            terminal_id,
        })
    }

    fn terminal_target_candidate(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<TerminalTargetCandidate> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let pane = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        Some(TerminalTargetCandidate {
            terminal_id: terminal.id.to_string(),
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            cwd: ws.tabs[tab_idx]
                .cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            agent_status: pane_agent_status(terminal.state, pane.seen),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{AppPolicy, AppState};

    fn test_app() -> App {
        let (_, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn named_targets_follow_terminal_identity_after_pane_and_tab_reordering() {
        let mut app = test_app();
        app.state = AppState::test_with_adversarial_identity_state();
        let targets = app.terminal_targets();
        for (index, target) in targets.iter().enumerate() {
            app.state
                .terminals
                .get_mut(target.terminal_id.as_str())
                .unwrap()
                .set_agent_name(format!("worker-{index}"));
        }
        for (index, target) in targets.iter().enumerate() {
            let name = format!("worker-{index}");
            assert_eq!(app.resolve_terminal_target(&name).unwrap(), *target);
            assert_eq!(app.resolve_agent_target(&name).unwrap(), *target);
            let pane = app.public_pane_id(target.ws_idx, target.pane_id).unwrap();
            assert_eq!(app.resolve_agent_target(&pane).unwrap(), *target);
        }
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn name_lookup_keeps_pane_order_and_ignores_detached_terminals() {
        let mut app = test_app();
        app.state = AppState::test_with_adversarial_identity_state();
        let targets = app.terminal_targets();
        for target in &targets {
            app.state
                .terminals
                .get_mut(target.terminal_id.as_str())
                .unwrap()
                .set_agent_name("shared".into());
        }
        let detached_id = crate::terminal::TerminalId::alloc();
        let mut detached =
            crate::terminal::TerminalState::new(detached_id.clone(), std::env::temp_dir());
        detached.set_agent_name("detached".into());
        app.state.terminals.insert(detached_id, detached);
        assert!(matches!(
            app.resolve_agent_target("detached"),
            Err(TerminalTargetError::NotFound { .. })
        ));
        for result in [
            app.resolve_terminal_target("shared"),
            app.resolve_agent_target("shared"),
        ] {
            let Err(TerminalTargetError::Ambiguous { candidates, .. }) = result else {
                panic!("expected all attached panes to remain ambiguous");
            };
            assert_eq!(
                candidates
                    .iter()
                    .map(|candidate| candidate.terminal_id.as_str())
                    .collect::<Vec<_>>(),
                targets
                    .iter()
                    .map(|target| target.terminal_id.as_str())
                    .collect::<Vec<_>>(),
            );
        }
    }

    fn bind_to(app: &mut App, terminal_id: &str) -> crate::access::policy::AccessBindingId {
        let binding = crate::access::policy::AccessBindingId::mint().unwrap();
        app.state.terminals.get_mut(terminal_id).unwrap().access =
            Some(crate::access::policy::AccessPolicy {
                binding: binding.clone(),
                accept_from: vec![crate::access::policy::Principal::None],
            });
        binding
    }

    fn make_agents(app: &mut App, targets: &[TerminalTarget]) {
        for (index, target) in targets.iter().enumerate() {
            app.state
                .terminals
                .get_mut(target.terminal_id.as_str())
                .unwrap()
                .set_agent_name(format!("agent-{index}"));
        }
    }

    fn app_with_targets() -> (App, Vec<TerminalTarget>) {
        let mut app = test_app();
        app.state = AppState::test_with_adversarial_identity_state();
        let targets = app.terminal_targets();
        assert!(targets.len() >= 2);
        (app, targets)
    }

    #[test]
    fn binding_id_target_resolves_only_its_terminal() {
        let (mut app, targets) = app_with_targets();
        make_agents(&mut app, &targets);
        let bindings: Vec<_> = targets
            .iter()
            .map(|target| bind_to(&mut app, &target.terminal_id))
            .collect();
        // A different terminal wears the first binding's id as its agent name
        // (set directly, past the name check); the id still means its holder.
        app.state
            .terminals
            .get_mut(targets[1].terminal_id.as_str())
            .unwrap()
            .set_agent_name(bindings[0].to_string());
        for (target, binding) in targets.iter().zip(&bindings) {
            assert_eq!(
                app.resolve_agent_target(&binding.to_string()).unwrap(),
                *target
            );
        }
    }

    #[test]
    fn stale_binding_target_is_not_found() {
        let (mut app, targets) = app_with_targets();
        let stale = crate::access::policy::AccessBindingId::mint().unwrap();
        let held = bind_to(&mut app, &targets[0].terminal_id);
        // Even a terminal that carries the stale id as its name is not it.
        app.state
            .terminals
            .get_mut(targets[1].terminal_id.as_str())
            .unwrap()
            .set_agent_name(stale.to_string());
        assert!(matches!(
            app.resolve_agent_target(&stale.to_string()),
            Err(TerminalTargetError::NotFound { .. })
        ));
        // A binding that was dropped is stale too.
        let holder = app
            .state
            .terminals
            .keys()
            .find(|id| id.as_str() == targets[0].terminal_id)
            .cloned()
            .unwrap();
        app.state.unbind_access(&holder);
        assert!(matches!(
            app.resolve_agent_target(&held.to_string()),
            Err(TerminalTargetError::NotFound { .. })
        ));
    }

    #[test]
    fn terminal_resolver_never_matches_acc_targets() {
        let (mut app, targets) = app_with_targets();
        let binding = bind_to(&mut app, &targets[0].terminal_id);
        let terminal = app
            .state
            .terminals
            .get_mut(targets[1].terminal_id.as_str())
            .unwrap();
        terminal.set_agent_name(binding.to_string());
        for (index, target) in [binding.to_string(), "acc_worker".to_string(), "acc_".into()]
            .into_iter()
            .enumerate()
        {
            assert!(
                matches!(
                    app.resolve_terminal_target(&target),
                    Err(TerminalTargetError::NotFound { .. })
                ),
                "target #{index} resolved"
            );
        }
        // Ordinary names still resolve there.
        app.state
            .terminals
            .get_mut(targets[1].terminal_id.as_str())
            .unwrap()
            .set_agent_name("worker".into());
        assert_eq!(app.resolve_terminal_target("worker").unwrap(), targets[1]);
    }

    #[test]
    fn binding_target_of_non_agent_holder_is_not_found() {
        let (mut app, targets) = app_with_targets();
        make_agents(&mut app, &targets);
        let binding = bind_to(&mut app, &targets[0].terminal_id);
        assert_eq!(
            app.resolve_agent_target(&binding.to_string()).unwrap(),
            targets[0]
        );
        // The pane stops holding an agent; the binding is stale.
        app.state
            .terminals
            .get_mut(targets[0].terminal_id.as_str())
            .unwrap()
            .set_agent_name(String::new());
        assert!(!app.target_is_agent(&targets[0]));
        assert!(matches!(
            app.resolve_agent_target(&binding.to_string()),
            Err(TerminalTargetError::NotFound { .. })
        ));
    }

    #[test]
    fn malformed_binding_target_never_matches_name() {
        let (mut app, targets) = app_with_targets();
        let id = "0123456789abcdef0123456789abcdef";
        let malformed = [
            format!("acc_{}", &id[..31]),
            format!("acc_{id}0"),
            format!("acc_{}", id.to_uppercase()),
            "acc_".to_string(),
            "acc_worker".to_string(),
            format!("acc_{id}-x"),
        ];
        for (index, name) in malformed.iter().enumerate() {
            let target = &targets[index % targets.len()];
            // The name is set directly, past the name check.
            app.state
                .terminals
                .get_mut(target.terminal_id.as_str())
                .unwrap()
                .set_agent_name(name.clone());
            assert!(
                matches!(
                    app.resolve_agent_target(name),
                    Err(TerminalTargetError::NotFound { .. })
                ),
                "{name} resolved"
            );
        }
    }

    #[test]
    fn agent_name_with_acc_prefix_refused() {
        use crate::api::schema::{AgentRenameParams, AgentStartParams, Method, Request};
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("acc-name");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().clone();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Idle,
        );
        terminal.set_agent_name("worker".into());
        fn call(app: &mut App, method: Method) -> serde_json::Value {
            serde_json::from_str(&app.handle_api_request(Request {
                id: "t".into(),
                method,
            }))
            .unwrap()
        }
        for name in ["acc_0123456789abcdef0123456789abcdef", "acc_worker", "acc_"] {
            let renamed = call(
                &mut app,
                Method::AgentRename(AgentRenameParams {
                    target: "worker".into(),
                    name: Some(name.into()),
                }),
            );
            assert_eq!(renamed["error"]["code"], "invalid_agent_name", "{name}");
            let started = call(
                &mut app,
                Method::AgentStart(AgentStartParams {
                    name: name.into(),
                    kind: "pi".into(),
                    pane_id: "1".into(),
                    args: Vec::new(),
                    timeout_ms: None,
                    accept_from: None,
                }),
            );
            assert_eq!(started["error"]["code"], "invalid_agent_name", "{name}");
        }
        // The rename was refused, so the old name still holds, and a name
        // that merely contains the prefix is still fine.
        assert!(app.resolve_agent_target("worker").is_ok());
        let ok = call(
            &mut app,
            Method::AgentRename(AgentRenameParams {
                target: "worker".into(),
                name: Some("my-acc_worker".into()),
            }),
        );
        assert_eq!(ok["result"]["type"], "agent_info", "{ok}");
    }

    #[test]
    #[ignore = "manual terminal target lookup scaling profile"]
    fn terminal_target_lookup_profile() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        for count in [1, 15, 128, 512] {
            let mut app = test_app();
            let mut workspace = crate::workspace::Workspace::test_new("lookup-profile");
            for _ in 1..count {
                workspace.test_split(ratatui::layout::Direction::Horizontal);
            }
            app.state.workspaces = vec![workspace];
            app.state.ensure_test_terminals();
            let id = app.state.workspaces[0]
                .terminal_id(app.state.workspaces[0].tabs[0].root_pane)
                .unwrap()
                .clone();
            app.state
                .terminals
                .get_mut(&id)
                .unwrap()
                .set_agent_name("profile-target".into());
            for (label, agent, target) in [
                ("terminal-name", false, "profile-target"),
                ("agent-name", true, "profile-target"),
                ("missing", false, "missing-target"),
            ] {
                let lookup = || {
                    if agent {
                        app.resolve_agent_target(target)
                    } else {
                        app.resolve_terminal_target(target)
                    }
                };
                for _ in 0..32 {
                    black_box(lookup()).ok();
                }
                let mut samples = Vec::new();
                for _ in 0..7 {
                    let start = Instant::now();
                    let mut iterations = 0;
                    while start.elapsed() < Duration::from_millis(20) {
                        black_box(lookup()).ok();
                        iterations += 1;
                    }
                    samples.push(start.elapsed().as_secs_f64() * 1e6 / f64::from(iterations));
                }
                samples.sort_by(f64::total_cmp);
                println!(
                    "terminal-target panes={count} case={label} median_us={:.3}",
                    samples[3]
                );
            }
        }
    }
}
