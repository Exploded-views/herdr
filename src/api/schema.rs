// herdr-fork: adds an access-class column to every operation row, re-exports AccessClass, and adds the access.caller.* rows. Unofficial stopgap; see FORK.md.
use serde::{Deserialize, Serialize};

pub mod access;
pub mod agents;
pub mod commands;
pub mod common;
pub mod events;
pub mod integrations;
pub(crate) mod operations;
pub mod panes;
pub mod plugins;
pub mod response;
pub mod server;
pub mod session;
pub mod tabs;
pub mod workspaces;
pub mod worktrees;

pub(crate) use operations::AccessClass;

pub use access::*;
pub use agents::*;
pub use commands::*;
pub use common::*;
pub use events::*;
pub use integrations::*;
pub use panes::*;
pub use plugins::*;
pub use response::*;
pub use server::*;
pub use session::*;
pub use tabs::*;
pub use workspaces::*;
pub use worktrees::*;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Request {
    pub id: String,
    #[serde(flatten)]
    pub method: Method,
}

// Variant order and parameter types preserve the existing wire contract.
// The two flags are independent: presentation impact, then client-shell admission.
// The last column is the access class (Input, Read, Control, Meta); every row must name one.
operations::define_operations! {
    Ping(PingParams) => "ping", false, false, Meta;
    ServerStop(EmptyParams) => "server.stop", false, false, Control;
    ServerLiveHandoff(ServerLiveHandoffParams) => "server.live_handoff", false, false, Control;
    ServerReloadConfig(EmptyParams) => "server.reload_config", true, true, Control;
    ServerSshAgentRegister(ServerSshAgentRegisterParams) => "server.ssh_agent.register", false, false, Control;
    ServerAgentManifests(EmptyParams) => "server.agent_manifests", false, false, Meta;
    ServerReloadAgentManifests(EmptyParams) => "server.reload_agent_manifests", true, false, Meta;
    NotificationShow(NotificationShowParams) => "notification.show", true, false, Meta;
    ProductAnnouncementDismiss(ProductAnnouncementDismissParams) => "product_announcement.dismiss", true, true, Meta;
    ReleaseNotesDismiss(ReleaseNotesDismissParams) => "release_notes.dismiss", true, true, Meta;
    CommandInvoke(CommandInvokeParams) => "command.invoke", true, true, Control;
    ClientWindowTitleSet(ClientWindowTitleSetParams) => "client.window_title.set", false, false, Meta;
    ClientWindowTitleClear(EmptyParams) => "client.window_title.clear", false, false, Meta;
    ClientShellSurfaceSet(ClientShellSurfaceSetParams) => "client_shell.surface.set", false, true, Meta;
    SessionSnapshot(EmptyParams) => "session.snapshot", false, false, Meta;
    WorkspaceCreate(WorkspaceCreateParams) => "workspace.create", true, true, Meta;
    WorkspaceList(EmptyParams) => "workspace.list", false, false, Meta;
    WorkspaceGet(WorkspaceTarget) => "workspace.get", false, false, Meta;
    WorkspaceFocus(WorkspaceTarget) => "workspace.focus", true, true, Meta;
    WorkspaceRename(WorkspaceRenameParams) => "workspace.rename", true, true, Meta;
    WorkspaceMove(WorkspaceMoveParams) => "workspace.move", true, true, Meta;
    WorkspaceMoveBlock(WorkspaceMoveBlockParams) => "workspace.move_block", true, true, Meta;
    WorkspaceReportMetadata(WorkspaceReportMetadataParams) => "workspace.report_metadata", true, false, Meta;
    WorkspaceClose(WorkspaceCloseParams) => "workspace.close", true, true, Control;
    WorktreeList(WorktreeListParams) => "worktree.list", false, true, Meta;
    WorktreeCreate(WorktreeCreateParams) => "worktree.create", true, true, Meta;
    WorktreeOpen(WorktreeOpenParams) => "worktree.open", true, true, Meta;
    WorktreeRemove(WorktreeRemoveParams) => "worktree.remove", true, true, Control;
    TabCreate(TabCreateParams) => "tab.create", true, true, Meta;
    TabList(TabListParams) => "tab.list", false, false, Meta;
    TabGet(TabTarget) => "tab.get", false, false, Meta;
    TabFocus(TabTarget) => "tab.focus", true, true, Meta;
    TabRename(TabRenameParams) => "tab.rename", true, true, Meta;
    TabMove(TabMoveParams) => "tab.move", true, true, Meta;
    TabClose(TabTarget) => "tab.close", true, true, Control;
    AgentList(EmptyParams) => "agent.list", false, false, Meta;
    AgentGet(AgentTarget) => "agent.get", false, false, Meta;
    AgentRead(AgentReadParams) => "agent.read", false, false, Read;
    AgentExplain(AgentTarget) => "agent.explain", false, false, Read;
    AgentSendKeys(AgentSendKeysParams) => "agent.send_keys", true, false, Input;
    AgentRename(AgentRenameParams) => "agent.rename", true, false, Control;
    AgentViewSet(AgentViewSetParams) => "agent.view.set", true, false, Meta;
    AgentViewClear(AgentViewClearParams) => "agent.view.clear", true, false, Meta;
    AgentFocus(AgentTarget) => "agent.focus", true, false, Meta;
    AgentStart(AgentStartParams) => "agent.start", true, false, Input;
    AgentPrompt(AgentPromptParams) => "agent.prompt", true, false, Input;
    AgentWait(AgentWaitParams) => "agent.wait", false, false, Meta;
    PaneSplit(PaneSplitParams) => "pane.split", true, true, Meta;
    PaneSwap(PaneSwapParams) => "pane.swap", true, true, Meta;
    PaneMove(PaneMoveParams) => "pane.move", true, false, Meta;
    PaneZoom(PaneZoomParams) => "pane.zoom", true, true, Meta;
    PaneLayout(PaneLayoutParams) => "pane.layout", false, false, Meta;
    PaneProcessInfo(PaneProcessInfoParams) => "pane.process_info", false, false, Meta;
    LayoutExport(LayoutExportParams) => "layout.export", false, false, Meta;
    LayoutApply(LayoutApplyParams) => "layout.apply", true, false, Control;
    LayoutSetSplitRatio(LayoutSetSplitRatioParams) => "layout.set_split_ratio", true, true, Meta;
    PaneNeighbor(PaneNeighborParams) => "pane.neighbor", false, false, Meta;
    PaneEdges(PaneEdgesParams) => "pane.edges", false, false, Meta;
    PaneFocusDirection(PaneFocusDirectionParams) => "pane.focus_direction", true, true, Meta;
    PaneResize(PaneResizeParams) => "pane.resize", true, true, Meta;
    PaneScroll(PaneScrollParams) => "pane.scroll", true, true, Meta;
    PaneClear(PaneTarget) => "pane.clear", true, true, Control;
    PaneEditScrollback(PaneTarget) => "pane.edit_scrollback", true, true, Read;
    PaneSelectionRead(PaneSelectionReadParams) => "pane.selection.read", false, true, Read;
    PaneCopyMotion(PaneCopyMotionParams) => "pane.copy_motion", false, true, Read;
    PaneCopySearch(PaneCopySearchParams) => "pane.copy_search", false, true, Read;
    PaneList(PaneListParams) => "pane.list", false, false, Meta;
    PaneCurrent(PaneCurrentParams) => "pane.current", false, false, Meta;
    PaneGet(PaneTarget) => "pane.get", false, false, Meta;
    PaneFocus(PaneTarget) => "pane.focus", true, true, Meta;
    PaneInputSet(PaneInputSetParams) => "pane.input.set", true, true, Meta;
    PaneLinkActivate(PaneLinkActivateParams) => "pane.link.activate", false, true, Meta;
    PaneLinkResolve(PaneLinkActivateParams) => "pane.link.resolve", false, true, Read;
    PaneRename(PaneRenameParams) => "pane.rename", true, true, Control;
    PaneSendText(PaneSendTextParams) => "pane.send_text", false, false, Input;
    PaneSendKeys(PaneSendKeysParams) => "pane.send_keys", false, false, Input;
    PaneSendInput(PaneSendInputParams) => "pane.send_input", false, false, Input;
    PaneRead(PaneReadParams) => "pane.read", false, false, Read;
    PaneReportAgent(PaneReportAgentParams) => "pane.report_agent", true, false, Control;
    PaneReportAgentSession(PaneReportAgentSessionParams) => "pane.report_agent_session", true, false, Control;
    PaneReportMetadata(PaneReportMetadataParams) => "pane.report_metadata", true, false, Control;
    PaneClearAgentAuthority(PaneClearAgentAuthorityParams) => "pane.clear_agent_authority", true, false, Control;
    PaneReleaseAgent(PaneReleaseAgentParams) => "pane.release_agent", true, false, Control;
    PaneClose(PaneTarget) => "pane.close", true, true, Control;
    PopupClose(EmptyParams) => "popup.close", true, false, Meta;
    EventsSubscribe(EventsSubscribeParams) => "events.subscribe", false, false, Read;
    EventsWait(EventsWaitParams) => "events.wait", false, false, Read;
    PaneWaitForOutput(PaneWaitForOutputParams) => "pane.wait_for_output", false, false, Read;
    IntegrationList(EmptyParams) => "integration.list", false, true, Meta;
    IntegrationInstall(IntegrationInstallParams) => "integration.install", false, true, Control;
    IntegrationUninstall(IntegrationUninstallParams) => "integration.uninstall", false, false, Control;
    PluginLink(PluginLinkParams) => "plugin.link", false, false, Control;
    PluginList(PluginListParams) => "plugin.list", false, false, Meta;
    PluginUnlink(PluginUnlinkParams) => "plugin.unlink", true, false, Meta;
    PluginEnable(PluginSetEnabledParams) => "plugin.enable", false, false, Control;
    PluginDisable(PluginSetEnabledParams) => "plugin.disable", true, false, Meta;
    PluginActionList(PluginActionListParams) => "plugin.action.list", false, false, Meta;
    PluginActionInvoke(PluginActionInvokeParams) => "plugin.action.invoke", true, false, Control;
    PluginLogList(PluginLogListParams) => "plugin.log.list", false, false, Meta;
    PluginPaneOpen(PluginPaneOpenParams) => "plugin.pane.open", true, false, Meta;
    PluginPaneFocus(PluginPaneFocusParams) => "plugin.pane.focus", true, false, Meta;
    PluginPaneClose(PluginPaneCloseParams) => "plugin.pane.close", true, false, Meta;
    AccessCallerAdd(AccessCallerAddParams) => "access.caller.add", false, false, Control;
    AccessCallerList(EmptyParams) => "access.caller.list", false, false, Control;
    AccessCallerRotate(AccessCallerNameParams) => "access.caller.rotate", false, false, Control;
    AccessCallerRetarget(AccessCallerRetargetParams) => "access.caller.retarget", false, false, Control;
    AccessCallerRemove(AccessCallerNameParams) => "access.caller.remove", false, false, Control;
}

#[cfg(test)]
mod tests;
