// herdr-fork: the input-origin log: who sent each pane input (path, sender class, client, host kind, byte count; never the content), a bounded ring, and a 0600 size-capped file written off the app thread. Unofficial stopgap; see FORK.md.
//! Records the origin of every input that reaches a pane's PTY. The app thread
//! only ever calls [`InputLog::record`], which is O(1) and never blocks; a
//! writer thread appends JSON lines to `access-input.log` in the data dir.
//! There is no field for the input itself: only its length.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use serde::Serialize;

/// Entries kept in memory for `access.log`.
pub(crate) const RING_CAP: usize = 4096;
/// Closed entries waiting for the writer.
pub(crate) const CHANNEL_CAP: usize = 1024;
/// An allow entry takes repeats for this long.
pub(crate) const COALESCE_MS: u64 = 2000;
/// The log file rotates to `.1` at this size.
pub(crate) const ROTATE_BYTES: u64 = 1024 * 1024;
/// File name inside the data dir.
pub(crate) const LOG_FILE: &str = "access-input.log";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum InputPath {
    Api,
    Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LoggedClass {
    Pane,
    Outside,
    OutsideSsh,
    Remote,
    Caller,
    Unknown,
}

pub(crate) const DECISION_ALLOW: &str = "allow";
pub(crate) const DECISION_DENY: &str = "deny";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InputEvent {
    /// Unix milliseconds.
    pub at_ms: u64,
    /// Public pane id at the time.
    pub pane: String,
    /// Terminal id, stable through moves.
    pub terminal: String,
    pub path: InputPath,
    /// Method name, or `shell.pane_input`, `shell.clipboard_image`, `shell.popup_input`,
    /// `attach.input`, `attach.mouse`.
    pub route: &'static str,
    pub class: LoggedClass,
    /// Caller name (never the token), or the sending pane's public id for a pane.
    pub principal: Option<String>,
    /// `api:<conn seq>`, `shell:<client id>` or `attach:<client id>`.
    pub client: String,
    /// `local`, `ssh` or `remote-stamp`.
    pub host: &'static str,
    /// Payload byte count, summed over coalesced events.
    pub bytes: u64,
    /// `allow` or `deny`.
    pub decision: &'static str,
    /// Coalesced repeats.
    pub count: u32,
}

impl InputEvent {
    /// Same sender, same route, same pane, same verdict: repeats of one burst.
    fn merges_with(&self, other: &InputEvent) -> bool {
        self.terminal == other.terminal
            && self.path == other.path
            && self.route == other.route
            && self.class == other.class
            && self.principal == other.principal
            && self.client == other.client
            && self.decision == other.decision
    }
}

/// What the writer thread receives.
#[derive(Debug)]
pub(crate) enum LogMsg {
    Event(InputEvent),
    /// `n` events never reached the writer.
    Dropped {
        n: u64,
        at_ms: u64,
    },
    /// Write everything received so far, then stop. The sender is told when done.
    Stop(SyncSender<()>),
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// `{"node":"<node_name>","started_ms":...}`: the first line of every file.
fn header_line(node: &str, started_ms: u64) -> String {
    let node = serde_json::to_string(node).unwrap_or_else(|_| "\"node\"".to_owned());
    format!(r#"{{"node":{node},"started_ms":{started_ms}}}"#)
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

pub(crate) struct InputLog {
    /// Newest last.
    ring: VecDeque<InputEvent>,
    /// `None` when no writer is attached (tests, or after `flush_and_stop`).
    tx: Option<SyncSender<LogMsg>>,
    dropped: u64,
    /// Drops the writer has not been told about yet.
    unreported: u64,
    /// The newest ring entry is an allow entry the writer has not seen yet.
    open: bool,
    /// The newest entry has been handed to the writer for good (a flush): it takes no repeats.
    sealed: bool,
    /// Where the writer was started, so a failed handoff can start it again.
    writer: Option<(PathBuf, String)>,
}

impl Default for InputLog {
    fn default() -> Self {
        Self::new()
    }
}

impl InputLog {
    /// A log with a ring and no file.
    pub fn new() -> Self {
        Self {
            ring: VecDeque::new(),
            tx: None,
            dropped: 0,
            unreported: 0,
            open: false,
            sealed: false,
            writer: None,
        }
    }

    /// A log whose closed entries go to the returned receiver, which holds `cap` of them.
    #[cfg(test)]
    pub fn with_channel(cap: usize) -> (Self, Receiver<LogMsg>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(cap);
        let mut log = Self::new();
        log.tx = Some(tx);
        (log, rx)
    }

    /// Start the writer thread for `dir`. Called once by the active server.
    pub fn attach_writer(&mut self, dir: PathBuf, node: String) -> io::Result<()> {
        let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_CAP);
        spawn_writer(rx, dir.clone(), &node)?;
        self.tx = Some(tx);
        self.writer = Some((dir, node));
        Ok(())
    }

    /// App thread, O(1), never blocks.
    pub fn record(&mut self, ev: InputEvent) {
        let mergeable = !std::mem::take(&mut self.sealed);
        if ev.decision == DECISION_DENY {
            // A refusal is never merged and never waits: it goes out as its own entry.
            self.close_open();
            self.push_ring(ev.clone());
            self.send(ev);
            return;
        }
        if mergeable {
            if let Some(last) = self.ring.back_mut() {
                if last.merges_with(&ev) && ev.at_ms.saturating_sub(last.at_ms) < COALESCE_MS {
                    last.count = last.count.saturating_add(ev.count);
                    last.bytes = last.bytes.saturating_add(ev.bytes);
                    return;
                }
            }
        }
        self.close_open();
        self.push_ring(ev);
        self.open = true;
    }

    /// Newest first.
    pub fn snapshot(&self, limit: usize) -> Vec<InputEvent> {
        self.ring.iter().rev().take(limit).cloned().collect()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Send the open entry, drain the channel to the writer and stop it. Used at
    /// server stop and before a live handoff, so the file is complete before another
    /// process opens it. Later events still reach the ring.
    pub fn flush_and_stop(&mut self) {
        self.close_open();
        self.sealed = true;
        let Some(tx) = self.tx.take() else {
            return;
        };
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let mut msg = LogMsg::Stop(done_tx);
        // Not the hot path: wait a bounded time for room.
        for _ in 0..400 {
            match tx.try_send(msg) {
                Ok(()) => break,
                Err(TrySendError::Full(back)) => {
                    msg = back;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
        let _ = done_rx.recv_timeout(std::time::Duration::from_secs(3));
    }

    /// Start the writer again after `flush_and_stop` when the server carries on (a handoff
    /// that failed).
    pub fn resume(&mut self) {
        if self.tx.is_some() {
            return;
        }
        if let Some((dir, node)) = self.writer.clone() {
            if let Err(err) = self.attach_writer(dir, node) {
                tracing::warn!(%err, "input log writer did not restart");
            }
        }
    }

    fn push_ring(&mut self, ev: InputEvent) {
        if self.ring.len() >= RING_CAP {
            self.ring.pop_front();
        }
        self.ring.push_back(ev);
    }

    /// The open entry has taken its last repeat: hand it to the writer.
    fn close_open(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        if let Some(last) = self.ring.back().cloned() {
            self.send(last);
        }
    }

    fn send(&mut self, ev: InputEvent) {
        let Some(tx) = self.tx.as_ref() else {
            return;
        };
        if self.unreported > 0 {
            let marker = LogMsg::Dropped {
                n: self.unreported,
                at_ms: ev.at_ms,
            };
            if tx.try_send(marker).is_err() {
                self.dropped += 1;
                self.unreported += 1;
                return;
            }
            self.unreported = 0;
        }
        if tx.try_send(LogMsg::Event(ev)).is_err() {
            self.dropped += 1;
            self.unreported += 1;
        }
    }
}

/// One file with its size, rotated at [`ROTATE_BYTES`].
struct LogFile {
    path: PathBuf,
    file: std::fs::File,
    size: u64,
    node: String,
}

impl LogFile {
    fn open(path: PathBuf, node: &str) -> io::Result<Self> {
        let file = crate::platform::open_private_append(&path)?;
        let size = file.metadata()?.len();
        let mut this = Self {
            path,
            file,
            size,
            node: node.to_owned(),
        };
        this.write_line(&header_line(node, now_ms()))?;
        Ok(this)
    }

    /// One `write()` per line.
    fn write_line(&mut self, line: &str) -> io::Result<()> {
        if self.size >= ROTATE_BYTES {
            self.rotate()?;
        }
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        self.file.write_all(buf.as_bytes())?;
        self.size += buf.len() as u64;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        std::fs::rename(&self.path, rotated_path(&self.path))?;
        self.file = crate::platform::open_private_append(&self.path)?;
        self.size = 0;
        let header = header_line(&self.node, now_ms());
        let mut buf = header;
        buf.push('\n');
        self.file.write_all(buf.as_bytes())?;
        self.size = buf.len() as u64;
        Ok(())
    }
}

/// The writer thread: blocks in `recv` (zero idle cost), appends JSON lines to
/// `dir/access-input.log`, no fsync. Returns once the file is open.
pub(crate) fn spawn_writer(rx: Receiver<LogMsg>, dir: PathBuf, node: &str) -> io::Result<()> {
    let mut file = LogFile::open(dir.join(LOG_FILE), node)?;
    crate::thread_spawn::spawn_named("herdr-input-log", move || {
        while let Ok(msg) = rx.recv() {
            let line = match &msg {
                LogMsg::Event(ev) => serde_json::to_string(ev),
                LogMsg::Dropped { n, at_ms } => Ok(format!(r#"{{"dropped":{n},"at_ms":{at_ms}}}"#)),
                LogMsg::Stop(done) => {
                    let _ = done.try_send(());
                    return;
                }
            };
            match line {
                Ok(line) => {
                    if let Err(err) = file.write_line(&line) {
                        tracing::warn!(%err, "input log write failed");
                    }
                }
                Err(err) => tracing::warn!(%err, "input log entry not serializable"),
            }
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::registry::TestDir;

    fn ev(terminal: &str, decision: &'static str, at_ms: u64, bytes: u64) -> InputEvent {
        InputEvent {
            at_ms,
            pane: "1-1".into(),
            terminal: terminal.into(),
            path: InputPath::Api,
            route: "pane.send_text",
            class: LoggedClass::Outside,
            principal: None,
            client: "api:1".into(),
            host: "local",
            bytes,
            decision,
            count: 1,
        }
    }

    fn drain(rx: &Receiver<LogMsg>) -> Vec<LogMsg> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(msg);
        }
        out
    }

    fn read(dir: &TestDir) -> String {
        std::fs::read_to_string(dir.path().join(LOG_FILE)).unwrap_or_default()
    }

    #[test]
    fn input_event_serializes_without_content() {
        let mut event = ev("t1", DECISION_ALLOW, 5, 12);
        event.principal = Some("ci".into());
        let value = serde_json::to_value(&event).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "at_ms",
                "bytes",
                "class",
                "client",
                "count",
                "decision",
                "host",
                "pane",
                "path",
                "principal",
                "route",
                "terminal"
            ]
        );
        for forbidden in ["text", "keys", "data"] {
            assert!(
                value.get(forbidden).is_none(),
                "{forbidden} must not be logged"
            );
        }
        assert_eq!(value["path"], "api");
        assert_eq!(value["class"], "outside");
        assert_eq!(
            serde_json::to_value(LoggedClass::OutsideSsh).unwrap(),
            "outside_ssh"
        );
    }

    #[test]
    fn ring_is_bounded_and_evicts_oldest() {
        let mut log = InputLog::new();
        for n in 0..(RING_CAP + 904) {
            log.record(ev(&format!("t{n}"), DECISION_ALLOW, n as u64, 1));
        }
        let all = log.snapshot(usize::MAX);
        assert_eq!(all.len(), RING_CAP);
        assert_eq!(all[0].terminal, format!("t{}", RING_CAP + 903));
        assert_eq!(all[RING_CAP - 1].terminal, "t904");
        assert_eq!(log.snapshot(3).len(), 3);
    }

    #[test]
    fn record_coalesces_burst_and_splits_on_decision_change() {
        let (mut log, rx) = InputLog::with_channel(16);
        log.record(ev("t1", DECISION_ALLOW, 1000, 5));
        log.record(ev("t1", DECISION_ALLOW, 1500, 7));
        log.record(ev("t1", DECISION_ALLOW, 2900, 1));
        // One entry, nothing written yet.
        assert!(drain(&rx).is_empty());
        let snap = log.snapshot(10);
        assert_eq!(snap.len(), 1);
        assert_eq!((snap[0].count, snap[0].bytes), (3, 13));
        // A refusal on the same key is its own entry; the open allow entry closes first.
        log.record(ev("t1", DECISION_DENY, 3000, 4));
        let sent = drain(&rx);
        assert_eq!(sent.len(), 2);
        assert!(
            matches!(&sent[0], LogMsg::Event(e) if e.decision == DECISION_ALLOW && e.count == 3)
        );
        assert!(
            matches!(&sent[1], LogMsg::Event(e) if e.decision == DECISION_DENY && e.count == 1)
        );
        // An allow after a deny does not merge into it.
        log.record(ev("t1", DECISION_ALLOW, 3100, 1));
        assert_eq!(log.snapshot(10).len(), 3);
        // Past 2 s the entry is closed and a new one opens.
        log.record(ev("t1", DECISION_ALLOW, 5200, 1));
        assert_eq!(log.snapshot(10).len(), 4);
        assert_eq!(drain(&rx).len(), 1);
        // A different pane never merges.
        log.record(ev("t2", DECISION_ALLOW, 5300, 1));
        assert_eq!(log.snapshot(10).len(), 5);
    }

    #[test]
    fn deny_event_written_immediately() {
        let (mut log, rx) = InputLog::with_channel(4);
        log.record(ev("t1", DECISION_DENY, 10, 3));
        let sent = drain(&rx);
        assert_eq!(sent.len(), 1, "a deny must reach the writer at once");
        assert!(matches!(&sent[0], LogMsg::Event(e) if e.decision == DECISION_DENY));
        // Two denials in a row stay two entries.
        log.record(ev("t1", DECISION_DENY, 11, 3));
        assert_eq!(drain(&rx).len(), 1);
        assert_eq!(log.snapshot(10).len(), 2);
    }

    #[test]
    fn record_never_blocks_when_writer_full() {
        let (mut log, _rx) = InputLog::with_channel(2);
        let start = std::time::Instant::now();
        for n in 0..200 {
            log.record(ev("t1", DECISION_DENY, n, 1));
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(log.dropped(), 198);
        assert_eq!(log.snapshot(usize::MAX).len(), 200);
    }

    #[test]
    fn dropped_marker_written_and_shown() {
        let (mut log, rx) = InputLog::with_channel(2);
        log.record(ev("t1", DECISION_DENY, 1, 1));
        log.record(ev("t1", DECISION_DENY, 2, 1));
        log.record(ev("t1", DECISION_DENY, 3, 1));
        assert_eq!(log.dropped(), 1);
        assert_eq!(drain(&rx).len(), 2);
        // Room again: the marker comes first, then the event.
        log.record(ev("t1", DECISION_DENY, 4, 1));
        let sent = drain(&rx);
        assert!(matches!(sent[0], LogMsg::Dropped { n: 1, .. }), "{sent:?}");
        assert!(matches!(&sent[1], LogMsg::Event(e) if e.at_ms == 4));
        assert_eq!(log.dropped(), 1, "the total is kept for access.log");

        // On disk the marker is one JSON line.
        let dir = TestDir::new("dropped");
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        spawn_writer(rx, dir.path().to_path_buf(), "node").unwrap();
        tx.send(LogMsg::Dropped { n: 7, at_ms: 99 }).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        tx.send(LogMsg::Stop(done_tx)).unwrap();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(read(&dir)
            .lines()
            .any(|l| l == r#"{"dropped":7,"at_ms":99}"#));
    }

    #[test]
    fn writer_rotates_at_one_mib_keeping_one_old_file() {
        let dir = TestDir::new("rotate");
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        spawn_writer(rx, dir.path().to_path_buf(), "node").unwrap();
        let mut big = ev("t1", DECISION_ALLOW, 1, 1);
        big.pane = "p".repeat(20_000);
        for _ in 0..200 {
            tx.send(LogMsg::Event(big.clone())).unwrap();
        }
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        tx.send(LogMsg::Stop(done_tx)).unwrap();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        let live = std::fs::metadata(dir.path().join(LOG_FILE)).unwrap().len();
        let old = std::fs::metadata(dir.path().join(format!("{LOG_FILE}.1")))
            .unwrap()
            .len();
        assert!(live <= ROTATE_BYTES + 21_000, "live file is {live}");
        assert!(old >= ROTATE_BYTES, "old file is {old}");
        assert!(!dir.path().join(format!("{LOG_FILE}.2")).exists());
        assert!(live + old <= 2 * ROTATE_BYTES + 42_000);
        // Every file starts with a header.
        for name in [LOG_FILE.to_string(), format!("{LOG_FILE}.1")] {
            let text = std::fs::read_to_string(dir.path().join(name)).unwrap();
            assert!(text
                .lines()
                .next()
                .unwrap()
                .starts_with(r#"{"node":"node""#));
        }
    }

    #[cfg(unix)]
    #[test]
    fn log_file_mode_0600_and_node_header_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new("mode");
        // A file left with loose permissions is fixed, not trusted.
        let path = dir.path().join(LOG_FILE);
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (mut log, _) = InputLog::with_channel(1);
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        for n in 0..3 {
            log.record(ev(&format!("t{n}"), DECISION_DENY, n, 1));
        }
        log.flush_and_stop();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let text = read(&dir);
        let headers = text.lines().filter(|l| l.starts_with(r#"{"node""#)).count();
        assert_eq!(headers, 1, "{text}");
        assert!(text
            .lines()
            .next()
            .unwrap()
            .starts_with(r#"{"node":"node-a","started_ms":"#));
        assert_eq!(text.lines().count(), 4);
    }

    #[test]
    fn log_header_uses_node_name_never_hostname() {
        let dir = TestDir::new("header");
        let (mut log, _) = InputLog::with_channel(1);
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        log.record(ev("t1", DECISION_DENY, 1, 1));
        log.flush_and_stop();
        let text = read(&dir);
        let header: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(header["node"], "node-a");
        assert_eq!(header.as_object().unwrap().len(), 2);
        #[cfg(unix)]
        {
            let mut buf = [0u8; 256];
            // SAFETY: the buffer is valid for its length.
            let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
            assert_eq!(rc, 0);
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            let host = String::from_utf8_lossy(&buf[..end]).to_string();
            if host.len() > 3 {
                assert!(
                    !text.contains(&host),
                    "the log must not contain the hostname"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn log_open_uses_nofollow_and_fixes_mode() {
        let dir = TestDir::new("nofollow");
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(LOG_FILE)).unwrap();
        let (_tx, rx) = std::sync::mpsc::sync_channel(1);
        assert!(spawn_writer(rx, dir.path().to_path_buf(), "node").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }

    #[test]
    fn log_flushed_on_shutdown_and_handoff() {
        let dir = TestDir::new("flush");
        let mut log = InputLog::new();
        log.attach_writer(dir.path().to_path_buf(), "node".into())
            .unwrap();
        // A lone allow entry is still open: it is not in the file yet.
        log.record(ev("t1", DECISION_ALLOW, 1, 9));
        log.flush_and_stop();
        let text = read(&dir);
        assert!(text.contains(r#""decision":"allow""#), "{text}");
        assert!(text.contains(r#""bytes":9"#));
        // After the stop the ring still takes events and nothing blocks.
        log.record(ev("t2", DECISION_DENY, 2, 1));
        assert_eq!(log.snapshot(10).len(), 2);
        // A failed handoff starts the writer again.
        log.resume();
        log.record(ev("t3", DECISION_DENY, 3, 1));
        log.flush_and_stop();
        assert!(read(&dir).contains(r#""terminal":"t3""#));
    }
}
