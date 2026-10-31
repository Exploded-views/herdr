// herdr-fork: adds AccessClass and Operation.access (per-operation access class); the policy-table test lists the access.caller.* and access.pane.* rows; lists access.log as Control and pins that no Input method rides the client-shell lane. Unofficial stopgap; see FORK.md.
use std::sync::LazyLock;

/// How much a caller can do through an operation, used to gate requests per pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessClass {
    /// Sends input to a pane or starts an agent that receives input.
    Input,
    /// Reads pane or agent content, or subscribes to output.
    Read,
    /// Changes server, layout, plugin or pane state.
    Control,
    /// Everything else: listings, focus, identity and bookkeeping.
    Meta,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Operation {
    pub name: &'static str,
    pub changes_ui: bool,
    pub client_shell: bool,
    pub access: AccessClass,
}

macro_rules! define_operations {
    ($( $variant:ident($params:ty) => $name:literal, $changes_ui:literal, $client_shell:literal, $access:ident; )*) => {
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
        #[serde(tag = "method", content = "params")]
        // Request enums are short-lived wire values; keeping variants direct preserves
        // the simple serde shape and avoids boxing churn across every caller.
        #[allow(clippy::large_enum_variant)]
        pub enum Method {
            $(#[serde(rename = $name)] $variant($params),)*
        }

        impl Method {
            pub(crate) const OPERATIONS: &'static [operations::Operation] = &[
                $(operations::Operation {
                    name: $name,
                    changes_ui: $changes_ui,
                    client_shell: $client_shell,
                    access: operations::AccessClass::$access,
                },)*
            ];

            pub(crate) fn operation(&self) -> operations::Operation {
                match self {
                    $(Self::$variant(_) => operations::Operation {
                        name: $name,
                        changes_ui: $changes_ui,
                        client_shell: $client_shell,
                        access: operations::AccessClass::$access,
                    },)*
                }
            }
        }
    };
}

pub(super) use define_operations;

pub(crate) fn client_shell_method_names() -> &'static [&'static str] {
    static NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        let mut names: Vec<_> = super::Method::OPERATIONS
            .iter()
            .filter(|operation| operation.client_shell)
            .map(|operation| operation.name)
            .collect();
        names.sort_unstable();
        names
    });
    &NAMES
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::{Method, Request};

    fn sample_value(
        schema: &serde_json::Value,
        definitions: &serde_json::Value,
    ) -> serde_json::Value {
        if let Some(reference) = schema.get("$ref").and_then(serde_json::Value::as_str) {
            return sample_value(
                &definitions[reference.rsplit('/').next().unwrap()],
                definitions,
            );
        }
        if let Some(value) = schema.get("const") {
            return value.clone();
        }
        if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
            return values[0].clone();
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(branches) = schema.get(key).and_then(serde_json::Value::as_array) {
                return sample_value(&branches[0], definitions);
            }
        }
        match schema.get("type").and_then(serde_json::Value::as_str) {
            Some("object") => {
                let mut object = serde_json::Map::new();
                if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array)
                {
                    for name in required {
                        let name = name.as_str().unwrap();
                        object.insert(
                            name.into(),
                            sample_value(&schema["properties"][name], definitions),
                        );
                    }
                }
                serde_json::Value::Object(object)
            }
            Some("array") => serde_json::json!([]),
            Some("string") => serde_json::json!("sample"),
            Some("integer" | "number") => serde_json::json!(1),
            Some("boolean") => serde_json::json!(false),
            _ => serde_json::Value::Null,
        }
    }

    #[test]
    fn operation_catalog_matches_every_wire_method() {
        let schema = serde_json::to_value(schemars::schema_for!(Request)).unwrap();
        let branches = schema["oneOf"].as_array().unwrap();
        let names: BTreeSet<_> = Method::OPERATIONS
            .iter()
            .map(|operation| operation.name)
            .collect();
        assert_eq!(
            names.len(),
            Method::OPERATIONS.len(),
            "duplicate operation name"
        );
        assert_eq!(branches.len(), names.len());
        for branch in branches {
            let name = branch["properties"]["method"]["const"].as_str().unwrap();
            assert!(names.contains(name), "missing operation {name}");
            let request: Request = serde_json::from_value(serde_json::json!({
                "id": "catalog",
                "method": name,
                "params": sample_value(&branch["properties"]["params"], &schema["$defs"]),
            }))
            .unwrap_or_else(|error| panic!("sample for {name}: {error}"));
            let operation = request.method.operation();
            assert_eq!(operation.name, name);
            assert_eq!(
                operation.access,
                Method::OPERATIONS
                    .iter()
                    .find(|candidate| candidate.name == name)
                    .unwrap()
                    .access
            );
            assert_eq!(crate::api::api_method_name(&request.method), name);
            assert_eq!(serde_json::to_value(&request).unwrap()["method"], name);
            assert_eq!(
                crate::api::request_changes_ui(&request),
                operation.changes_ui
            );
            assert_eq!(
                crate::server::client_commands::supports_client_shell_method(&request.method),
                operation.client_shell
            );
            assert_eq!(
                crate::server::client_commands::supports_client_shell_method_name(name),
                operation.client_shell
            );
        }
    }

    #[test]
    fn operation_access_classes_match_policy_table() {
        use super::super::AccessClass;

        let input = [
            "agent.send_keys",
            "agent.start",
            "agent.prompt",
            "pane.send_text",
            "pane.send_keys",
            "pane.send_input",
        ];
        let read = [
            "agent.read",
            "agent.explain",
            "pane.read",
            "pane.selection.read",
            "pane.copy_motion",
            "pane.copy_search",
            "pane.edit_scrollback",
            "pane.link.resolve",
            "pane.wait_for_output",
            "events.subscribe",
            "events.wait",
        ];
        let control = [
            "server.stop",
            "server.live_handoff",
            "server.reload_config",
            "server.ssh_agent.register",
            "command.invoke",
            "plugin.link",
            "plugin.enable",
            "plugin.action.invoke",
            "integration.install",
            "integration.uninstall",
            "layout.apply",
            "worktree.remove",
            "workspace.close",
            "tab.close",
            "pane.close",
            "pane.clear",
            "pane.rename",
            "agent.rename",
            "pane.report_agent",
            "pane.report_agent_session",
            "pane.report_metadata",
            "pane.clear_agent_authority",
            "pane.release_agent",
            "access.caller.add",
            "access.caller.list",
            "access.caller.rotate",
            "access.caller.retarget",
            "access.caller.remove",
            "access.pane.set",
            "access.log",
        ];
        let meta = [
            "ping",
            "server.agent_manifests",
            "server.reload_agent_manifests",
            "notification.show",
            "product_announcement.dismiss",
            "release_notes.dismiss",
            "client.window_title.set",
            "client.window_title.clear",
            "client_shell.surface.set",
            "session.snapshot",
            "workspace.create",
            "workspace.list",
            "workspace.get",
            "workspace.focus",
            "workspace.rename",
            "workspace.move",
            "workspace.move_block",
            "workspace.report_metadata",
            "worktree.list",
            "worktree.create",
            "worktree.open",
            "tab.create",
            "tab.list",
            "tab.get",
            "tab.focus",
            "tab.rename",
            "tab.move",
            "agent.list",
            "agent.get",
            "agent.view.set",
            "agent.view.clear",
            "agent.focus",
            "agent.wait",
            "pane.split",
            "pane.swap",
            "pane.move",
            "pane.zoom",
            "pane.layout",
            "pane.process_info",
            "layout.export",
            "layout.set_split_ratio",
            "pane.neighbor",
            "pane.edges",
            "pane.focus_direction",
            "pane.resize",
            "pane.scroll",
            "pane.list",
            "pane.current",
            "pane.get",
            "pane.focus",
            "pane.input.set",
            "pane.link.activate",
            "popup.close",
            "integration.list",
            "plugin.list",
            "plugin.unlink",
            "plugin.disable",
            "plugin.action.list",
            "plugin.log.list",
            "plugin.pane.open",
            "plugin.pane.focus",
            "plugin.pane.close",
            "access.pane.get",
        ];
        for operation in Method::OPERATIONS {
            let expected = if input.contains(&operation.name) {
                AccessClass::Input
            } else if read.contains(&operation.name) {
                AccessClass::Read
            } else if control.contains(&operation.name) {
                AccessClass::Control
            } else if meta.contains(&operation.name) {
                AccessClass::Meta
            } else {
                panic!(
                    "{} is in no access list; it needs triage into Input, Read, Control or Meta",
                    operation.name
                );
            };
            assert_eq!(operation.access, expected, "{}", operation.name);
        }
        let names: BTreeSet<_> = Method::OPERATIONS.iter().map(|o| o.name).collect();
        for listed in input.iter().chain(&read).chain(&control).chain(&meta) {
            assert!(names.contains(listed), "unknown operation {listed}");
        }
    }

    /// The client-shell endpoint lane carries only methods with `client_shell: true`. An
    /// Input method there would reach a pane's PTY on a path the input log does not see, so a
    /// new row (an upstream merge, say) fails here until the lane or the log is extended.
    #[test]
    fn no_input_method_on_client_shell_lane() {
        use super::super::AccessClass;

        for operation in Method::OPERATIONS {
            assert!(
                !(operation.client_shell && operation.access == AccessClass::Input),
                "{} is an Input method on the client-shell lane",
                operation.name
            );
        }
        // The log itself is not reachable from the lane either.
        let log = Method::OPERATIONS
            .iter()
            .find(|operation| operation.name == "access.log")
            .unwrap();
        assert!(!log.client_shell);
        assert_eq!(log.access, AccessClass::Control);
    }

    #[test]
    fn operation_ui_impact_preserves_existing_classification() {
        let expected = [
            "server.reload_config",
            "server.reload_agent_manifests",
            "notification.show",
            "product_announcement.dismiss",
            "release_notes.dismiss",
            "command.invoke",
            "workspace.create",
            "workspace.focus",
            "workspace.rename",
            "workspace.move",
            "workspace.move_block",
            "workspace.report_metadata",
            "workspace.close",
            "worktree.create",
            "worktree.open",
            "worktree.remove",
            "tab.create",
            "tab.focus",
            "tab.rename",
            "tab.move",
            "tab.close",
            "layout.apply",
            "layout.set_split_ratio",
            "agent.rename",
            "agent.view.set",
            "agent.view.clear",
            "agent.focus",
            "agent.start",
            "agent.prompt",
            "agent.send_keys",
            "pane.split",
            "pane.swap",
            "pane.move",
            "pane.zoom",
            "pane.focus_direction",
            "pane.resize",
            "pane.scroll",
            "pane.clear",
            "pane.edit_scrollback",
            "pane.focus",
            "pane.input.set",
            "pane.rename",
            "pane.report_agent",
            "pane.report_agent_session",
            "pane.report_metadata",
            "pane.clear_agent_authority",
            "pane.release_agent",
            "pane.close",
            "popup.close",
            "plugin.unlink",
            "plugin.disable",
            "plugin.action.invoke",
            "plugin.pane.open",
            "plugin.pane.focus",
            "plugin.pane.close",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let actual = Method::OPERATIONS
            .iter()
            .filter(|operation| operation.changes_ui)
            .map(|operation| operation.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
    }
}
