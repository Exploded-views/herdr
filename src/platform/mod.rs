// herdr-fork: peer lineage (socket peer -> verified process ancestry) for access classification, and create_private_file. Unofficial stopgap; see FORK.md.
//! Platform-specific process and filesystem operations.
//!
//! Centralizes OS-dependent behavior behind a clean boundary so core
//! modules don't scatter `#[cfg]` branches through product logic.

#[cfg(unix)]
pub(crate) mod ssh_agent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Native adapters construct only the event classifications their OS can report.
#[allow(dead_code)]
pub(crate) enum HostPowerEvent {
    Suspend,
    AutomaticResume,
    InteractiveResume,
    UnclassifiedResume,
}

pub(crate) struct HostPowerMonitor {
    events: tokio::sync::mpsc::UnboundedReceiver<HostPowerEvent>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl HostPowerMonitor {
    pub(crate) fn start(enabled: bool) -> Self {
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let task = if enabled {
            monitor_host_power(sender)
        } else {
            None
        };
        Self { events, task }
    }

    pub(crate) fn try_recv(&mut self) -> Option<HostPowerEvent> {
        self.events.try_recv().ok()
    }

    pub(crate) async fn recv(&mut self) -> HostPowerEvent {
        match self.events.recv().await {
            Some(event) => event,
            None => std::future::pending().await,
        }
    }
}

impl Drop for HostPowerMonitor {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn monitor_host_power(
    _sender: tokio::sync::mpsc::UnboundedSender<HostPowerEvent>,
) -> Option<tokio::task::JoinHandle<()>> {
    None
}

pub(crate) struct HostShutdownMonitor {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl HostShutdownMonitor {
    pub(crate) fn start(
        requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
        intent: HostShutdownIntentCell,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let task = monitor_host_shutdown(requested, intent, wake);
        Self { task }
    }
}

impl Drop for HostShutdownMonitor {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A restart or shutdown the host announced but has not committed to yet.
///
/// Live handoff carries it so a replacement server still recognizes the
/// commit point. The value is captured when the old server builds its handoff
/// manifest; announcements or cancellations that arrive after that and before
/// the replacement starts its monitor reach only the old server. Each handoff
/// phase may take up to 30 seconds, so that window can last tens of seconds.
/// Only macOS reports an intent; the timestamp is in its boot-wide monotonic
/// clock, which is shared by the old and new server processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HostShutdownIntent {
    announced_at_ns: u64,
}

/// The pending host shutdown intent shared by the server and its monitor.
// Windows has no live handoff and no announcement to record, so nothing reads it there.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Default)]
pub(crate) struct HostShutdownIntentCell(
    std::sync::Arc<std::sync::Mutex<Option<HostShutdownIntent>>>,
);

// Windows has no live handoff and no announcement to record, so nothing reads it there.
#[cfg_attr(windows, allow(dead_code))]
impl HostShutdownIntentCell {
    pub(crate) fn get(&self) -> Option<HostShutdownIntent> {
        *self.lock()
    }

    pub(crate) fn set(&self, intent: Option<HostShutdownIntent>) {
        *self.lock() = intent;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<HostShutdownIntent>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn monitor_host_shutdown(
    _requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _intent: HostShutdownIntentCell,
    _wake: impl Fn() + Send + Sync + 'static,
) -> Option<tokio::task::JoinHandle<()>> {
    None
}

#[cfg(not(windows))]
pub(crate) fn host_shutdown_in_progress() -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    pub name: String,
    pub argv0: Option<String>,
    pub argv: Option<Vec<String>>,
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundJob {
    pub process_group_id: u32,
    pub processes: Vec<ForegroundProcess>,
}

/// One process in a peer lineage. `start_token` tells it apart from a later
/// process that reuses the pid: start time on Linux, `p_uniqueid` on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub pid: u32,
    pub start_token: u64,
}

/// The processes behind a socket connection, `chain[0]` being the peer.
///
/// `complete` is true only when the walk reached pid 1 with every link
/// verified and the peer's start token unchanged after the walk. Anything
/// less must be treated as unknown, never as "outside every pane".
///
/// The kernel records the peer pid at `connect()`. If that process exits and
/// the pid is reused before the walk's first read, both reads see the
/// newcomer and the re-read of the peer token can't notice: it only catches
/// replacement during the walk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PeerLineage {
    pub chain: Vec<ProcessIdentity>,
    pub complete: bool,
    /// Some process in the chain, `chain[0]` included, is a remote-login
    /// server or tunnel daemon (see `is_ssh_server_name`). When `complete`
    /// is false this is a lower bound: the unread part of the chain may hold
    /// one. Names are chosen by the process itself, so a process can make
    /// this true; callers may use it only to restrict access, never to
    /// grant it.
    pub via_ssh: bool,
}

/// What one `/proc` or `proc_pidinfo` read says about a process: enough to
/// name it, find its parent and verify the link between the two.
#[cfg(unix)]
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkInfo {
    pub name: String,
    pub ppid: u32,
    /// Start time (Linux) or `p_uniqueid` (macOS).
    pub start_token: u64,
    /// macOS `p_puniqueid`: the `p_uniqueid` of the parent this process was
    /// spawned by. Unused on Linux.
    pub parent_ref: u64,
}

/// Longest ancestry the walk follows before it gives up as incomplete.
#[allow(dead_code)]
pub(crate) const MAX_LINEAGE_DEPTH: usize = 64;

/// Executable names of remote-login servers. Exact match only.
#[allow(dead_code)]
fn is_ssh_server_name(name: &str) -> bool {
    matches!(
        name,
        "sshd" | "sshd-session" | "mosh-server" | "etserver" | "tailscaled" | "dropbear"
    )
}

#[allow(dead_code)]
fn names_include_ssh_server<'a>(names: impl IntoIterator<Item = &'a str>) -> bool {
    names.into_iter().any(is_ssh_server_name)
}

#[cfg(unix)]
pub(crate) use unix_common::{
    create_private_file, local_stream_peer_lineage, process_identity, random_secret,
};
// Only tests and the access gate benchmark walk a lineage by pid.
#[cfg(all(test, unix))]
pub(crate) use unix_common::process_lineage;

#[cfg(not(unix))]
pub(crate) fn local_stream_peer_lineage(_stream: &crate::ipc::LocalStream) -> PeerLineage {
    PeerLineage::default()
}

#[cfg(all(test, not(unix)))]
pub(crate) fn process_lineage(_pid: u32) -> PeerLineage {
    PeerLineage::default()
}

#[cfg(not(unix))]
pub(crate) fn process_identity(_pid: u32) -> Option<ProcessIdentity> {
    None
}

#[cfg(not(unix))]
pub(crate) fn create_private_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(not(unix))]
pub(crate) fn random_secret() -> std::io::Result<[u8; 32]> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// A request from outside the process to stop the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerQuitSignal {
    #[cfg(unix)]
    Interrupt,
    #[cfg(unix)]
    Terminate,
    #[cfg(not(unix))]
    ConsoleControl,
}

impl std::fmt::Display for ServerQuitSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(unix)]
            Self::Interrupt => "SIGINT",
            #[cfg(unix)]
            Self::Terminate => "SIGTERM",
            #[cfg(not(unix))]
            Self::ConsoleControl => "console control event",
        })
    }
}

#[cfg(not(unix))]
pub(crate) fn spawn_server_signal_monitor(
    on_quit: impl Fn(ServerQuitSignal) + Send + Sync + 'static,
) {
    if let Err(err) = ctrlc::set_handler(move || on_quit(ServerQuitSignal::ConsoleControl)) {
        tracing::warn!(%err, "failed to install server stop handler");
    }
}

#[cfg(not(unix))]
pub(crate) fn ignore_server_hangup() {}

#[cfg(not(unix))]
pub(crate) fn local_stream_peer_description(_stream: &crate::ipc::LocalStream) -> Option<String> {
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Hangup,
    Terminate,
    Kill,
}

/// Why a pane runtime ended, before application persistence policy is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExitReason {
    Exited,
    Interrupted,
    /// Imported runtimes have no child wait handle in the replacement server.
    #[cfg(unix)]
    Handoff,
    WaitFailed,
}

impl ChildExitReason {
    pub(crate) fn requires_session_checkpoint(self) -> bool {
        match self {
            Self::Interrupted => true,
            #[cfg(unix)]
            Self::Handoff => true,
            _ => false,
        }
    }
}

#[cfg(unix)]
pub(crate) use unix_common::{
    classify_child_exit, poll_fd_readable, read_fd, shared_ssh_control_path,
};

// Tests inject their own probe because the recorded hangup is process-global.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn server_hangup_is_recent() -> bool {
    #[cfg(unix)]
    {
        unix_common::server_hangup_is_recent()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn classify_child_exit(_status: &portable_pty::ExitStatus) -> ChildExitReason {
    ChildExitReason::Exited
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn launch_executable() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe()
}

pub(crate) fn detached_custom_command_process(command: &str) -> std::process::Command {
    let mut process = detached_custom_command_process_platform(command);
    configure_background_command(&mut process);
    process
}

pub(crate) fn pane_custom_command_pty_builder(command: &str) -> portable_pty::CommandBuilder {
    pane_custom_command_pty_builder_platform(command)
}

pub(crate) fn apply_pane_runtime_marker(command: &mut portable_pty::CommandBuilder) {
    apply_pane_runtime_marker_platform(command);
}

pub(crate) fn prepare_paste_text_for_pty(text: String) -> String {
    prepare_paste_text_for_pty_platform(text)
}

pub(crate) fn plugin_runtime_path(path: &std::path::Path) -> std::path::PathBuf {
    plugin_runtime_path_platform(path)
}

pub(crate) fn normalize_cwd_for_launch(path: &std::path::Path) -> std::path::PathBuf {
    normalize_cwd_for_launch_platform(path)
}

#[cfg(not(windows))]
fn normalize_cwd_for_launch_platform(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
fn plugin_runtime_path_platform(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
fn prepare_paste_text_for_pty_platform(text: String) -> String {
    text
}

#[cfg(not(windows))]
pub(crate) fn terminal_title_for_presentation(title: &str) -> &str {
    title
}

#[cfg(not(windows))]
fn apply_pane_runtime_marker_platform(_command: &mut portable_pty::CommandBuilder) {}

pub(crate) fn configure_background_command(command: &mut std::process::Command) {
    configure_background_command_platform(command);
}

#[cfg(not(windows))]
fn configure_background_command_platform(_command: &mut std::process::Command) {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlatformCapabilities {
    pub(crate) live_handoff: bool,
    pub(crate) direct_terminal_attach: bool,
    pub(crate) preserve_legacy_doubled_escape_input: bool,
}

pub(crate) const fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        live_handoff: cfg!(unix),
        direct_terminal_attach: cfg!(unix),
        preserve_legacy_doubled_escape_input: cfg!(target_os = "macos"),
    }
}

/// The byte the host terminal sends for Backspace according to the tty's erase
/// setting (e.g. `^H` for MobaXterm and PuTTY-style terminals), when known.
pub(crate) fn terminal_erase_byte() -> Option<u8> {
    #[cfg(unix)]
    return unix_common::terminal_erase_byte();
    #[cfg(not(unix))]
    None
}

/// Error kind marker for a terminal that momentarily has no usable grid.
///
/// A window drag can sample a zero-sized grid before the terminal settles, so
/// this is distinct from a real terminal failure. Startup still rejects it; the
/// resize watcher treats it as a transient sample instead of a dead terminal.
#[derive(Debug)]
pub(crate) struct ZeroSizedGridError;

impl std::fmt::Display for ZeroSizedGridError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("terminal reported a zero-sized grid")
    }
}

impl std::error::Error for ZeroSizedGridError {}

pub(crate) fn zero_sized_terminal_grid_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, ZeroSizedGridError)
}

/// Whether an error only means the terminal has no usable size yet, so a
/// resizing terminal should keep its last known geometry instead of detaching.
pub(crate) fn is_transient_terminal_grid_error(err: &std::io::Error) -> bool {
    err.get_ref()
        .is_some_and(|inner| inner.is::<ZeroSizedGridError>())
}

pub(crate) fn terminal_grid_size() -> std::io::Result<(u16, u16)> {
    #[cfg(unix)]
    let (cols, rows) = unix_common::read_terminal_grid_size()?;
    #[cfg(windows)]
    let (cols, rows) = windows::read_terminal_grid_size()?;
    #[cfg(not(any(unix, windows)))]
    let (cols, rows) = fallback::read_terminal_grid_size()?;

    if cols == 0 || rows == 0 {
        return Err(zero_sized_terminal_grid_error());
    }
    Ok((cols, rows))
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn launch_server_daemon_command(command: &mut std::process::Command) -> std::io::Result<u32> {
    command.spawn().map(|child| child.id())
}

/// Starts the daemon through launchd, so quitting the terminal app that
/// started it does not take the server down (herdr#4920). Over SSH, or when
/// launchd cannot carry the command exactly, it spawns a detached child.
/// Returns the child's pid, or 0 when launchd started it.
#[cfg(target_os = "macos")]
pub fn launch_server_daemon_command(command: &mut std::process::Command) -> std::io::Result<u32> {
    match macos::spawn_launchd_job(command) {
        Ok(_label) => Ok(0),
        Err(err) => {
            tracing::info!(%err, "starting the server directly instead of through launchd");
            command.spawn().map(|child| child.id())
        }
    }
}

/// First step of a server process; see `launch_server_daemon_command`.
/// Returns a warning to log once logging is initialized.
#[cfg(target_os = "macos")]
pub(crate) fn leave_launcher_job() -> Option<String> {
    macos::leave_launchd_job()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn leave_launcher_job() -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn prepare_server_process(_handoff_import: bool) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn detach_server_daemon_command(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    #[cfg(target_os = "macos")]
    macos::configure_server_daemon_context(command);

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn current_process_is_detached_server_daemon() -> bool {
    unsafe { libc::getsid(0) == libc::getpid() }
}

/// Raised by the SIGWINCH handler, consumed by the host resize watcher.
#[cfg(unix)]
static TERMINAL_RESIZE_SIGNALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn record_terminal_resize_signal(_signal: libc::c_int) {
    TERMINAL_RESIZE_SIGNALLED.store(true, std::sync::atomic::Ordering::Release);
}

/// Records SIGWINCH events that size polling can miss.
#[cfg(unix)]
pub(crate) fn watch_terminal_resize_signal() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction =
        record_terminal_resize_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // Keep blocking stdin and socket reads from failing with EINTR.
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
pub(crate) fn watch_terminal_resize_signal() {}

/// Returns whether a terminal size change was signalled since the last call.
#[cfg(unix)]
pub(crate) fn take_terminal_resize_signal() -> bool {
    TERMINAL_RESIZE_SIGNALLED.swap(false, std::sync::atomic::Ordering::AcqRel)
}

/// Windows relies on size polling.
#[cfg(not(unix))]
pub(crate) fn take_terminal_resize_signal() -> bool {
    false
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardCommand {
    pub program: &'static str,
    pub args: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LimitedRead {
    Empty,
    Complete(Vec<u8>),
    Oversized,
}

pub(crate) fn read_limited_reader(
    mut reader: impl std::io::Read,
    max_bytes: usize,
) -> std::io::Result<LimitedRead> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];

    while bytes.len() < max_bytes {
        let remaining = max_bytes - bytes.len();
        let read_len = remaining.min(buffer.len());
        let bytes_read = match reader.read(&mut buffer[..read_len]) {
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if bytes_read == 0 {
            return if bytes.is_empty() {
                Ok(LimitedRead::Empty)
            } else {
                Ok(LimitedRead::Complete(bytes))
            };
        }
        bytes.extend_from_slice(&buffer[..bytes_read]);
    }

    let mut sentinel = [0_u8; 1];
    loop {
        return match reader.read(&mut sentinel) {
            Ok(0) if bytes.is_empty() => Ok(LimitedRead::Empty),
            Ok(0) => Ok(LimitedRead::Complete(bytes)),
            Ok(_) => Ok(LimitedRead::Oversized),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => Err(err),
        };
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteSshConfigPaths {
    pub(crate) user_config: Option<std::path::PathBuf>,
    pub(crate) system_config: Option<std::path::PathBuf>,
    pub(crate) multiplexing: bool,
}

pub(crate) const REMOTE_BRIDGE_IDLE_TIMEOUT_SUPPORTED: bool =
    cfg!(any(target_os = "linux", target_os = "macos"));

#[cfg(unix)]
mod remote_bridge;
#[cfg(all(test, unix))]
mod remote_bridge_tests;
#[cfg(unix)]
mod unix_common;
/// Other Unix platforms keep their existing input admission policy.
/// Their _PC_MAX_CANON values need not describe the actual queue capacity.
#[cfg(all(unix, not(target_os = "macos")))]
#[derive(Clone, Default)]
pub(crate) struct PtyInputGuard;

#[cfg(all(unix, not(target_os = "macos")))]
impl PtyInputGuard {
    pub(crate) fn new(_master: &std::sync::Arc<std::fs::File>) -> Self {
        Self
    }

    pub(crate) fn validate(&self, _bytes: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
}
#[cfg(unix)]
pub(crate) mod unix_image_files;
#[cfg(unix)]
pub(crate) use unix_common::{
    begin_cli_output, end_cli_output, forward_remote_bridge_stdio, ignore_server_hangup,
    local_stream_peer_description, spawn_server_signal_monitor, ChildPipe, RemoteBridgeWake,
};

mod client_state;
pub(crate) use client_state::{create_private_state_file, replace_file, sync_parent_directory};

#[cfg(not(unix))]
pub(crate) fn begin_cli_output() {}

#[cfg(not(unix))]
pub(crate) fn end_cli_output() {}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(windows))]
pub(crate) use process_cwd as pane_process_cwd;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod fallback;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub use fallback::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn available_pane_shell_from_job(child_pid: u32, job: ForegroundJob) -> Option<String> {
    if job.process_group_id != child_pid
        || job.processes.iter().any(|process| process.pid != child_pid)
    {
        return None;
    }
    job.processes
        .into_iter()
        .find(|process| process.pid == child_pid)
        .map(|process| process.name)
        .filter(|name| is_pane_shell_process_name(name))
}

fn normalized_process_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn is_powershell_process_name(name: &str) -> bool {
    matches!(
        normalized_process_name(name).as_str(),
        "pwsh" | "powershell"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn interactive_unix_shell_command(
    argv: &[String],
    shell_name: &str,
    quote_posix_arg: fn(&str) -> String,
) -> Option<String> {
    let quote = if is_powershell_process_name(shell_name) {
        quote_powershell_arg
    } else {
        quote_posix_arg
    };
    let mut parts = argv.iter();
    let mut command = quote(parts.next()?);
    for part in parts {
        command.push(' ');
        command.push_str(&quote(part));
    }
    Some(command)
}

pub(crate) fn quote_powershell_arg(value: &str) -> String {
    if !value.is_empty()
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'+' | b'=')
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn quote_windows_command_line_arg(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\x0b' | '"'))
    {
        return value.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
        }
        backslashes = 0;
        quoted.push(ch);
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

pub(crate) fn is_pane_shell_process_name(name: &str) -> bool {
    let normalized = normalized_process_name(name);
    matches!(
        normalized.as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ksh"
            | "mksh"
            | "csh"
            | "tcsh"
            | "elvish"
            | "xonsh"
            | "nu"
            | "pwsh"
            | "powershell"
            | "cmd"
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_agent_hint(_pid: u32) -> Option<crate::detect::Agent> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn parse_agent_env_hint(environ: &[u8]) -> Option<crate::detect::Agent> {
    for record in environ.split(|&byte| byte == 0) {
        let Some(value) = record.strip_prefix(b"HERDR_AGENT=") else {
            continue;
        };
        return crate::detect::parse_agent_label(std::str::from_utf8(value).ok()?);
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Debug)]
pub(crate) struct InputSourceRestore;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn pump_input_source_runloop() {}

/// Switches the host keyboard input source while prefix mode is active.
///
/// `App` drives this through a trait so the prefix-mode transitions can be
/// tested with a fake, without touching the real macOS APIs or leaking a
/// platform-specific restore type into `App`.
pub(crate) trait PrefixInputSource {
    /// Switch to an ASCII-capable input source for prefix commands. No-op if
    /// the current source is already ASCII-capable, the platform is
    /// unsupported, or the switch fails. Calling it again before `restore`
    /// keeps the source saved by the first call.
    fn switch_to_ascii(&mut self);

    /// Restore whatever `switch_to_ascii` saved. No-op if nothing was switched.
    fn restore(&mut self);
}

/// Production [`PrefixInputSource`] backed by the per-platform API.
#[derive(Default)]
pub(crate) struct RealPrefixInputSource {
    restore: Option<InputSourceRestore>,
}

impl PrefixInputSource for RealPrefixInputSource {
    fn switch_to_ascii(&mut self) {
        if self.restore.is_none() {
            // Drain pending input-source-change notifications so the read below is fresh (see
            // `pump_input_source_runloop`); a no-op on non-macOS.
            pump_input_source_runloop();
            self.restore = switch_to_ascii_input_source();
        }
    }

    fn restore(&mut self) {
        let _ = self.restore.take();
    }
}

#[cfg(all(test, any(unix, windows)))]
#[test]
fn child_exit_classification_only_checkpoints_interruptions() {
    for code in [0, 1, 130, 255, 0xC0000005] {
        let reason = classify_child_exit(&portable_pty::ExitStatus::with_exit_code(code));
        assert_eq!(reason, ChildExitReason::Exited, "exit code {code:#x}");
        assert!(!reason.requires_session_checkpoint());
    }
    #[cfg(windows)]
    let status = portable_pty::ExitStatus::with_exit_code(0xC000013A);
    #[cfg(not(windows))]
    let status = portable_pty::ExitStatus::with_signal("Terminated: 15");
    assert_eq!(classify_child_exit(&status), ChildExitReason::Interrupted);
    assert!(classify_child_exit(&status).requires_session_checkpoint());
    #[cfg(unix)]
    assert!(ChildExitReason::Handoff.requires_session_checkpoint());
    assert!(!ChildExitReason::WaitFailed.requires_session_checkpoint());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn live_pane_process_group_rejects_processes_outside_the_pane_session() {
        use std::os::unix::process::CommandExt;

        let mut detached = std::process::Command::new("sleep");
        detached.arg("30");
        // SAFETY: setsid is async-signal-safe and touches only the child.
        unsafe {
            detached.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut detached = detached.spawn().expect("spawn detached");
        let token = process_start_token(detached.id()).expect("start token");
        let mut gone = std::process::Command::new("true").spawn().expect("spawn");
        let gone_pid = gone.id();
        gone.wait().expect("reap");

        assert_eq!(
            live_pane_process_group(std::process::id(), detached.id(), token),
            None,
            "a live process in another terminal session"
        );
        assert_eq!(
            live_pane_process_group(gone_pid, detached.id(), token),
            None,
            "a pane shell that is gone"
        );
        let _ = detached.kill();
        let _ = detached.wait();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn live_pane_process_group_follows_the_agent_process_not_its_job() {
        use std::os::unix::process::CommandExt;

        let shell_pid = std::process::id();
        let mut wrapper = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn wrapper");
        let job = wrapper.id();
        let mut agent = std::process::Command::new("sleep")
            .arg("30")
            .process_group(job as i32)
            .spawn()
            .expect("spawn agent");
        let agent_pid = agent.id();
        let token = process_start_token(agent_pid).expect("agent start token");
        let wrapper_token = process_start_token(job).expect("wrapper start token");

        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token),
            Some(job)
        );
        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token + 1),
            None,
            "a reused pid has a different start token"
        );
        unsafe {
            libc::kill(agent_pid as libc::pid_t, libc::SIGSTOP);
        }
        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token),
            Some(job)
        );

        unsafe {
            libc::kill(agent_pid as libc::pid_t, libc::SIGKILL);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while live_pane_process_group(shell_pid, agent_pid, token).is_some()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token),
            None,
            "an unreaped agent must not count as alive while its wrapper lives"
        );
        assert_eq!(
            live_pane_process_group(shell_pid, job, wrapper_token),
            Some(job)
        );
        agent.wait().expect("reap agent");
        assert_eq!(live_pane_process_group(shell_pid, agent_pid, token), None);
        let _ = wrapper.kill();
        let _ = wrapper.wait();
    }

    #[test]
    fn terminal_resize_signal_is_recorded_once_per_delivery() {
        watch_terminal_resize_signal();
        assert!(!take_terminal_resize_signal());

        unsafe {
            libc::raise(libc::SIGWINCH);
        }

        assert!(take_terminal_resize_signal());
        assert!(!take_terminal_resize_signal());
    }

    #[test]
    fn pane_shell_process_names_reject_exec_replacement_programs() {
        for shell in ["bash", "-zsh", "/bin/fish", "pwsh", "powershell.exe"] {
            assert!(is_pane_shell_process_name(shell), "{shell}");
        }
        for program in ["vim", "nvim", "cargo", "test-runner", "opencode"] {
            assert!(!is_pane_shell_process_name(program), "{program}");
        }
    }

    #[test]
    fn detached_custom_command_preserves_unix_login_shell_flag() {
        let cmd = detached_custom_command_process("echo hello");
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/bin/sh"));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("-lc"),
                std::ffi::OsStr::new("echo hello")
            ]
        );
    }

    #[test]
    fn pane_custom_command_builder_preserves_unix_shell_flag() {
        let expected: Vec<std::ffi::OsString> =
            vec!["/bin/sh".into(), "-c".into(), "echo hello".into()];
        assert_eq!(
            pane_custom_command_pty_builder("echo hello").get_argv(),
            &expected
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_accepts_known_agents() {
        assert_eq!(
            parse_agent_env_hint(b"PATH=/bin\0HERDR_AGENT=claude\0TERM=xterm\0"),
            Some(crate::detect::Agent::Claude)
        );
        assert_eq!(
            parse_agent_env_hint(b"HERDR_AGENT=codex"),
            Some(crate::detect::Agent::Codex)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_ignores_missing_or_unknown_agents() {
        assert_eq!(parse_agent_env_hint(b"PATH=/bin\0TERM=xterm\0"), None);
        assert_eq!(parse_agent_env_hint(b"HERDR_AGENT=not-an-agent\0"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn interactive_shell_command_quotes_for_posix_and_powershell() {
        let argv = vec![
            "pi".into(),
            String::new(),
            "two words".into(),
            "a'b".into(),
            "$HOME".into(),
            "semi;colon".into(),
            "@options".into(),
        ];
        assert_eq!(
            interactive_shell_command(&argv, "bash").as_deref(),
            Some("pi '' 'two words' 'a'\\''b' '$HOME' 'semi;colon' @options")
        );
        assert_eq!(
            interactive_shell_command(&argv, "pwsh").as_deref(),
            Some("pi '' 'two words' 'a''b' '$HOME' 'semi;colon' '@options'")
        );
    }

    #[test]
    fn read_limited_reader_returns_complete_data_under_limit() {
        let input = std::io::Cursor::new(b"image".to_vec());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_returns_empty_for_empty_input() {
        let input = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Empty
        );
    }

    #[test]
    fn read_limited_reader_accepts_data_exactly_at_limit() {
        let input = std::io::Cursor::new(b"four".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Complete(b"four".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_rejects_data_over_limit() {
        let input = std::io::Cursor::new(b"oversized".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Oversized
        );
    }

    #[test]
    fn read_limited_reader_retries_interrupted_reads() {
        struct InterruptedOnce {
            interrupted: bool,
            inner: std::io::Cursor<Vec<u8>>,
        }

        impl std::io::Read for InterruptedOnce {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.inner.read(buffer)
            }
        }

        let input = InterruptedOnce {
            interrupted: false,
            inner: std::io::Cursor::new(b"image".to_vec()),
        };
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    /// Every hop of the real ancestry of `pid` as the walk sees it, for
    /// assertion messages when a platform read fails.
    fn explain(pid: u32) -> String {
        let mut out = String::new();
        let mut current = pid;
        for _ in 0..MAX_LINEAGE_DEPTH {
            let info = process_link_info(current);
            out.push_str(&format!("\n  {current}: {info:?}"));
            match info {
                Some(info) if current != 1 && info.ppid != 0 => current = info.ppid,
                _ => break,
            }
        }
        out
    }

    #[test]
    fn process_identity_matches_the_token_the_lineage_walk_records() {
        // A pane shell is matched against lineage entries, so the two must use
        // one token. On macOS `process_start_token` is a start time, the walk
        // records `p_uniqueid`.
        let own = process_lineage(std::process::id());
        let identity = process_identity(std::process::id()).expect("own identity");
        assert_eq!(own.chain.first().copied(), Some(identity));
        assert!(process_identity(u32::MAX - 1).is_none());
    }

    #[test]
    fn own_lineage_starts_with_self_and_completes() {
        let lineage = process_lineage(std::process::id());
        assert_eq!(
            lineage.chain.first().map(|p| p.pid),
            Some(std::process::id())
        );
        assert!(
            lineage.complete,
            "{lineage:?}{}",
            explain(std::process::id())
        );
        assert_eq!(lineage.chain.last().map(|p| p.pid), Some(1));
        assert!(lineage.chain.len() <= MAX_LINEAGE_DEPTH);
    }

    #[test]
    fn lineage_of_child_includes_parent() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn sleep");
        let lineage = process_lineage(child.id());
        let detail = explain(child.id());
        child.kill().expect("kill child");
        child.wait().expect("reap child");
        assert_eq!(lineage.chain.first().map(|p| p.pid), Some(child.id()));
        assert_eq!(
            lineage.chain.get(1).map(|p| p.pid),
            Some(std::process::id())
        );
        assert!(lineage.complete, "{lineage:?}{detail}");
    }

    #[test]
    fn lineage_of_exited_pid_is_incomplete() {
        let mut child = std::process::Command::new("true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("reap");
        let lineage = process_lineage(pid);
        assert!(!lineage.complete, "{lineage:?}");
        assert!(!lineage.via_ssh);
    }

    #[test]
    fn socket_peer_lineage_matches_connecting_process() {
        let dir = std::env::temp_dir().join(format!("herdr-lineage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let stream = crate::ipc::connect_local_stream(&path).expect("connect");
        // The peer of our end is the listener, which is this process.
        let lineage = local_stream_peer_lineage(&stream);
        drop((stream, listener));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            lineage.chain.first().map(|p| p.pid),
            Some(std::process::id())
        );
        assert!(
            lineage.complete,
            "{lineage:?}{}",
            explain(std::process::id())
        );
    }

    #[test]
    fn orphaned_grandchild_lineage_classification() {
        let out = std::process::Command::new("sh")
            .arg("-c")
            // sleep drops the inherited stdout so `output()` returns at once.
            .arg("(sleep 5 >/dev/null 2>&1 & echo $!) &")
            .output()
            .expect("run sh");
        let gc: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("grandchild pid");
        let lineage = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let first = process_name_and_parent(gc).map(|(_, parent)| parent);
            while std::time::Instant::now() < deadline
                && process_name_and_parent(gc).map(|(_, parent)| parent) == first
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            process_lineage(gc)
        };
        // SAFETY: gc is the pid of the sleep this test started.
        unsafe { libc::kill(gc as libc::pid_t, libc::SIGKILL) };
        // Observation only: complete may be true or false per OS.
        eprintln!(
            "orphan observation: complete={} chain_len={}",
            lineage.complete,
            lineage.chain.len()
        );
        assert_eq!(lineage.chain.first().map(|p| p.pid), Some(gc));
    }

    #[test]
    fn lineage_marks_ssh_server_ancestor() {
        assert!(names_include_ssh_server(["zsh", "sshd", "launchd"]));
        assert!(names_include_ssh_server(["tailscaled"]));
        assert!(!names_include_ssh_server(["zsh", "herdr", "launchd"]));
        assert!(!names_include_ssh_server([]));
    }

    #[test]
    fn lineage_matches_ssh_names_exactly() {
        for name in [
            "sshd-foo",
            "xsshd",
            "sshd ",
            "SSHD",
            "ssh",
            "mosh",
            "sshd-session2",
        ] {
            assert!(!is_ssh_server_name(name), "{name}");
        }
        for name in [
            "sshd",
            "sshd-session",
            "mosh-server",
            "etserver",
            "tailscaled",
            "dropbear",
        ] {
            assert!(is_ssh_server_name(name), "{name}");
        }
    }

    #[test]
    fn lineage_checks_chain_zero() {
        // The peer itself is the ssh server; no ancestor is.
        let lineage = unix_common::walk_lineage(10, |pid| {
            fake_link(pid, if pid == 10 { "sshd" } else { "zsh" })
        });
        assert!(lineage.via_ssh, "{lineage:?}");
        let lineage = unix_common::walk_lineage(10, |pid| fake_link(pid, "zsh"));
        assert!(!lineage.via_ssh);
        assert!(lineage.complete, "{lineage:?}");
    }

    #[test]
    fn lineage_walk_rejects_broken_chains() {
        // pid 10 -> 5 -> 1; each pid's token grows with pid.
        assert!(unix_common::walk_lineage(10, |p| fake_link(p, "x")).complete);
        // A vanished ancestor.
        let l = unix_common::walk_lineage(10, |p| (p != 5).then(|| fake_link(p, "x")).flatten());
        assert!(!l.complete);
        // The peer's token changes between the walk and the recheck.
        let calls = std::cell::Cell::new(0);
        let l = unix_common::walk_lineage(10, |p| {
            let mut link = fake_link(p, "x")?;
            if p == 10 {
                calls.set(calls.get() + 1);
                if calls.get() > 1 {
                    link.start_token += 1;
                }
            }
            Some(link)
        });
        assert!(!l.complete);
        // A cycle never reaches pid 1.
        let l = unix_common::walk_lineage(10, |p| {
            let mut link = fake_link(p, "x")?;
            link.ppid = 10;
            Some(link)
        });
        assert!(!l.complete);
        // Too deep.
        let l = unix_common::walk_lineage(1000, |p| {
            let mut link = fake_link(p, "x")?;
            link.ppid = p - 1;
            link.parent_ref = p as u64 - 1;
            Some(link)
        });
        assert!(!l.complete);
        assert!(l.chain.len() <= MAX_LINEAGE_DEPTH);
    }

    /// A link for a fake process tree 10 -> 5 -> 1 (other pids parent to 1)
    /// that passes `verify_link` on every OS: tokens equal the pid and a
    /// child's `parent_ref` is its parent's token.
    fn fake_link(pid: u32, name: &str) -> Option<LinkInfo> {
        let ppid = match pid {
            10 => 5,
            1 => 0,
            _ => 1,
        };
        Some(LinkInfo {
            name: name.to_string(),
            ppid,
            start_token: pid as u64,
            parent_ref: ppid as u64,
        })
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn uniqinfo_struct_size_matches_kernel() {
        assert_eq!(std::mem::size_of::<macos::ProcUniqIdentifierInfo>(), 56);
        assert_eq!(
            std::mem::offset_of!(macos::ProcUniqIdentifierInfo, p_uniqueid),
            16
        );
        assert_eq!(
            std::mem::offset_of!(macos::ProcUniqIdentifierInfo, p_puniqueid),
            24
        );
        // The kernel fills exactly this many bytes for our own pid.
        let mut info: macos::ProcUniqIdentifierInfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a writable buffer of exactly 56 bytes.
        let ret = unsafe {
            libc::proc_pidinfo(
                std::process::id() as libc::c_int,
                17,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                56,
            )
        };
        assert_eq!(ret, 56);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn lineage_reads_root_owned_launchd() {
        let launchd = macos::process_link_info(1).expect("pid 1 readable");
        assert_ne!(launchd.start_token, 0);
        assert_eq!(launchd.name, "launchd");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn lineage_link_check_rejects_mismatched_parent() {
        let parent = LinkInfo {
            name: "p".into(),
            ppid: 1,
            start_token: 100,
            parent_ref: 1,
        };
        let mut child = LinkInfo {
            name: "c".into(),
            ppid: 5,
            start_token: 200,
            parent_ref: 100,
        };
        assert!(verify_link(&parent, &child));
        child.parent_ref = 101;
        assert!(!verify_link(&parent, &child));
        // Zero tokens verify nothing, even against each other.
        let zero = LinkInfo {
            start_token: 0,
            ..parent.clone()
        };
        let orphan = LinkInfo {
            parent_ref: 0,
            ..child.clone()
        };
        assert!(!verify_link(&zero, &orphan));
        assert!(!verify_link(&zero, &child));
    }
}

#[cfg(not(unix))]
pub(crate) fn shared_ssh_control_path(
    _namespace: &std::path::Path,
    _target: &str,
) -> std::io::Result<std::path::PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "interactive SSH recovery requires Unix OpenSSH multiplexing",
    ))
}
