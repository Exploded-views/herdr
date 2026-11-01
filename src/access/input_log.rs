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
/// The longest a closed entry waits in the channel before the writer writes it, counted from
/// the moment the first entry of the batch was queued (not from when the writer was woken).
/// The writer is woken by the first entry after it went idle, waits out what is left of this
/// delay (`batch_wait`) so a burst is one write, then drains. Entries after the first never
/// wake it. The wake itself comes when the request or event that recorded the entry has been
/// handled, so the file has the entry within the longer of this delay and that handler. An open allow entry (taking repeats)
/// is sent when the next record, a read of the log or a stop closes it, so the latest burst
/// of a quiet pane stays out of the file until then, however old it is.
pub(crate) const WRITER_BATCH_DELAY: std::time::Duration = std::time::Duration::from_millis(50);
/// What tests hold the first entry after idle to, measured from the input (the record) to the
/// line being in the file: the batch delay plus a handler of ordinary length and the write.
#[cfg(test)]
pub(crate) const VISIBILITY_BOUND: std::time::Duration = std::time::Duration::from_millis(100);
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
    /// Write everything received so far, then tell the sender. The writer carries on.
    Sync(SyncSender<()>),
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
    /// What the app thread and the writer thread share: failed writes, and the wake-up state.
    shared: WriterShared,
    /// Where the writer was started, so a failed handoff can start it again.
    #[cfg(unix)]
    writer: Option<(PathBuf, String)>,
}

impl Drop for InputLog {
    fn drop(&mut self) {
        // Let a parked writer see that nothing can reach it any more and end.
        self.tx = None;
        self.shared.pacer.wake_now();
    }
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
            shared: WriterShared::default(),
            #[cfg(unix)]
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
        // The same Pacer for the life of the log, so a guard taken before a restart still
        // reaches the new writer.
        self.shared.pacer.reset();
        spawn_writer(rx, dir.clone(), &node, self.shared.clone())?;
        self.tx = Some(tx);
        #[cfg(unix)]
        {
            self.writer = Some((dir, node));
        }
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

    /// How many times the writer thread woke up (tests).
    #[cfg(test)]
    pub fn writer_wakes(&self) -> u64 {
        self.shared
            .pacer
            .wakes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Deliver the wake-up the input path owes the writer, as the server does after a reply
    /// (through `WakeAfterReply`). Tests and the bench call it directly.
    #[cfg(test)]
    pub fn wake_writer(&self) {
        self.shared.pacer.flush_wake();
    }

    /// The wake-up state, for the server to deliver the wake-up after a reply.
    pub fn pacer(&self) -> std::sync::Arc<Pacer> {
        self.shared.pacer.clone()
    }

    /// Make everything recorded so far reach the file before returning: a read of the log
    /// calls this first, so what it shows and what is on disk agree.
    pub fn sync(&mut self) {
        self.close_open();
        // What was just written takes no repeats: input typed after a read is a new entry,
        // not a count added to one the file already has.
        self.sealed = true;
        if let Some(tx) = self.tx.as_ref() {
            Self::round_trip(tx, &self.shared.pacer, LogMsg::Sync);
        }
    }

    /// Queue a request the writer answers, wake it, and wait (bounded) for the answer. Not the
    /// input path.
    fn round_trip(tx: &SyncSender<LogMsg>, pacer: &Pacer, build: fn(SyncSender<()>) -> LogMsg) {
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let mut msg = build(done_tx);
        for _ in 0..400 {
            match tx.try_send(msg) {
                Ok(()) => break,
                Err(TrySendError::Full(back)) => {
                    msg = back;
                    pacer.wake_now();
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
        pacer.wake_now();
        let _ = done_rx.recv_timeout(std::time::Duration::from_secs(3));
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
            + self
                .shared
                .failures
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Send the open entry, drain the channel to the writer and stop it. Used at
    /// server stop and before a live handoff, so the file is complete before another
    /// process opens it. Later events still reach the ring.
    pub fn flush_and_stop(&mut self) {
        self.close_open();
        self.sealed = true;
        if let Some(tx) = self.tx.take() {
            Self::round_trip(&tx, &self.shared.pacer, LogMsg::Stop);
        }
    }

    /// Start the writer again after `flush_and_stop` when the server carries on (a handoff
    /// that failed).
    #[cfg(unix)]
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
        } else {
            self.shared.pacer.notify();
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
        // A file that already has entries keeps its header: one per file.
        if this.size == 0 {
            this.write_line(&header_line(node, now_ms()))?;
        }
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

/// Wake-up state shared with the writer thread. The writer sleeps with no timer while nothing
/// is queued. The input path only queues the entry and, for the first one after the writer went
/// idle, notes the time and sets `wake_pending` (atomic stores and one clock read, no lock and
/// no system call). The server delivers the wake-up once the request or client event has been
/// handled and its reply sent (`flush_wake`): the writer then waits what is left of
/// `WRITER_BATCH_DELAY` since that first entry (`batch_wait`) so a burst is one write, drains,
/// clears `armed` and sleeps again. Entries queued while `armed` is set add no wake-up, so the
/// server pays for at most one per batch, off the input path.
///
/// Visibility bound: the first entry after idle is in the file at (queued + `WRITER_BATCH_DELAY`)
/// or, if the handler is still running then, when it returns, plus the write itself.
#[derive(Default)]
pub(crate) struct Pacer {
    armed: std::sync::atomic::AtomicBool,
    /// A wake-up is owed to the writer and has not been delivered yet.
    wake_pending: std::sync::atomic::AtomicBool,
    /// A flush or a read is waiting: skip the batch wait.
    urgent: std::sync::atomic::AtomicBool,
    /// Unix milliseconds when the first entry of the batch was queued.
    first_ms: std::sync::atomic::AtomicU64,
    /// The writer's thread handle. The input path (`notify`, `record`) must never take this
    /// lock; only the wake after the reply (`flush_wake`, `wake_now`) and the writer's start do.
    thread: std::sync::Mutex<Option<std::thread::Thread>>,
    wakes: std::sync::atomic::AtomicU64,
    /// Times `thread` was locked (tests: the input path must not add to it).
    #[cfg(test)]
    thread_locks: std::sync::atomic::AtomicU64,
}

/// How long the writer still waits for a batch whose first entry was queued at `first_ms`,
/// when it is `now_ms`: the rest of `WRITER_BATCH_DELAY`, none once it has passed. A clock that
/// went backwards counts as no time having passed, so the wait never exceeds the delay.
pub(crate) fn batch_wait(first_ms: u64, now_ms: u64) -> std::time::Duration {
    WRITER_BATCH_DELAY.saturating_sub(std::time::Duration::from_millis(
        now_ms.saturating_sub(first_ms),
    ))
}

impl Pacer {
    /// App thread, after queueing an entry: record that the writer has work. Never blocks and
    /// makes no system call.
    pub(crate) fn notify(&self) {
        if !self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            self.notify_at(now_ms());
        }
    }

    /// `notify` with the time of the entry given (tests).
    pub(crate) fn notify_at(&self, now_ms: u64) {
        use std::sync::atomic::Ordering::SeqCst;
        if !self.armed.swap(true, SeqCst) {
            self.first_ms.store(now_ms, SeqCst);
            self.wake_pending.store(true, SeqCst);
        }
    }

    /// Writer: how long to wait before draining a batch woken for at `now_ms`.
    pub(crate) fn wait_at(&self, now_ms: u64) -> std::time::Duration {
        batch_wait(
            self.first_ms.load(std::sync::atomic::Ordering::SeqCst),
            now_ms,
        )
    }

    /// Back to the idle state, for a writer that is about to start.
    fn reset(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.armed.store(false, SeqCst);
        self.wake_pending.store(false, SeqCst);
        self.urgent.store(false, SeqCst);
    }

    /// Lock the writer handle. Never call this from the input path.
    fn lock_thread(&self) -> std::sync::MutexGuard<'_, Option<std::thread::Thread>> {
        #[cfg(test)]
        self.thread_locks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_thread(&self, thread: std::thread::Thread) {
        *self.lock_thread() = Some(thread);
    }

    /// Deliver the wake-up `notify` owes, if any. The server calls this after the reply to the
    /// request (or the client event) that recorded the entry has been sent.
    pub(crate) fn flush_wake(&self) {
        if self
            .wake_pending
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.unpark();
        }
    }

    /// Wake the writer now (a flush, a read): not on the input path.
    pub(crate) fn wake_now(&self) {
        self.urgent.store(true, std::sync::atomic::Ordering::SeqCst);
        self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.wake_pending
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.unpark();
    }

    fn unpark(&self) {
        let guard = self.lock_thread();
        if let Some(thread) = guard.as_ref() {
            thread.unpark();
        }
    }
}

#[derive(Default, Clone)]
pub(crate) struct WriterShared {
    failures: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pacer: std::sync::Arc<Pacer>,
}

/// Counts failed writes so a full disk warns once and not once per entry.
#[derive(Default)]
struct WriteHealth {
    failing: bool,
    failed: u64,
}

impl WriteHealth {
    /// Note one write. True when the caller should warn about it.
    fn record(&mut self, ok: bool) -> bool {
        if ok {
            self.failing = false;
            return false;
        }
        self.failed += 1;
        let first = !self.failing;
        self.failing = true;
        first
    }
}

/// Write one message. True when the writer should end.
fn handle_msg(
    msg: LogMsg,
    file: &mut LogFile,
    health: &mut WriteHealth,
    failures: &std::sync::atomic::AtomicU64,
) -> bool {
    let line = match &msg {
        LogMsg::Event(ev) => serde_json::to_string(ev),
        LogMsg::Dropped { n, at_ms } => Ok(format!(r#"{{"dropped":{n},"at_ms":{at_ms}}}"#)),
        LogMsg::Sync(done) => {
            let _ = done.try_send(());
            return false;
        }
        LogMsg::Stop(done) => {
            let _ = done.try_send(());
            return true;
        }
    };
    match line {
        Ok(line) => {
            let written = file.write_line(&line);
            if health.record(written.is_ok()) {
                if let Err(err) = &written {
                    tracing::warn!(%err, "input log write failed; further failures are counted as dropped");
                }
            }
            if written.is_err() {
                failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Err(err) => tracing::warn!(%err, "input log entry not serializable"),
    }
    false
}

/// The writer thread, appending JSON lines to `dir/access-input.log`, no fsync. With nothing
/// queued it parks with no timer (idle cost is zero). The app thread wakes it for the first
/// entry after it went idle; it then waits `WRITER_BATCH_DELAY`, writes the batch and parks
/// again. Returns once the file is open.
pub(crate) fn spawn_writer(
    rx: Receiver<LogMsg>,
    dir: PathBuf,
    node: &str,
    shared: WriterShared,
) -> io::Result<()> {
    use std::sync::atomic::Ordering;
    let WriterShared { failures, pacer } = shared;
    let mut file = LogFile::open(dir.join(LOG_FILE), node)?;
    let writer_pacer = pacer.clone();
    let handle = crate::thread_spawn::spawn_named("herdr-input-log", move || {
        let pacer = writer_pacer;
        let mut health = WriteHealth::default();
        loop {
            while let Ok(msg) = rx.try_recv() {
                if handle_msg(msg, &mut file, &mut health, &failures) {
                    return;
                }
            }
            pacer.armed.store(false, Ordering::SeqCst);
            // An entry queued between the drain and the flag is caught here, so it is not
            // left waiting for a wake-up that was never sent.
            match rx.try_recv() {
                Ok(msg) => {
                    pacer.armed.store(true, Ordering::SeqCst);
                    if handle_msg(msg, &mut file, &mut health, &failures) {
                        return;
                    }
                    continue;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            std::thread::park();
            pacer.wakes.fetch_add(1, Ordering::Relaxed);
            // A flush or a read is waiting for the answer: no batch wait.
            if !pacer.urgent.swap(false, Ordering::SeqCst) {
                std::thread::park_timeout(pacer.wait_at(now_ms()));
                pacer.wakes.fetch_add(1, Ordering::Relaxed);
            }
        }
    })?;
    pacer.set_thread(handle.thread().clone());
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
        let shared = WriterShared::default();
        spawn_writer(rx, dir.path().to_path_buf(), "node", shared.clone()).unwrap();
        tx.send(LogMsg::Dropped { n: 7, at_ms: 99 }).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        tx.send(LogMsg::Stop(done_tx)).unwrap();
        shared.pacer.wake_now();
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
        let (tx, rx) = std::sync::mpsc::sync_channel(512);
        let shared = WriterShared::default();
        spawn_writer(rx, dir.path().to_path_buf(), "node", shared.clone()).unwrap();
        let mut big = ev("t1", DECISION_ALLOW, 1, 1);
        big.pane = "p".repeat(20_000);
        for _ in 0..200 {
            tx.send(LogMsg::Event(big.clone())).unwrap();
        }
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        tx.send(LogMsg::Stop(done_tx)).unwrap();
        shared.pacer.wake_now();
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
        assert!(spawn_writer(rx, dir.path().to_path_buf(), "node", Default::default()).is_err());
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
        // A failed handoff (unix only) starts the writer again.
        #[cfg(unix)]
        {
            log.resume();
            log.record(ev("t3", DECISION_DENY, 3, 1));
            log.flush_and_stop();
            assert!(read(&dir).contains(r#""terminal":"t3""#));
        }
    }

    #[test]
    fn a_failing_disk_warns_once_and_counts_every_failure() {
        let mut health = WriteHealth::default();
        assert!(!health.record(true));
        assert!(health.record(false), "the first failure warns");
        assert!(!health.record(false), "a repeat does not");
        assert!(!health.record(false));
        assert_eq!(health.failed, 3);
        assert!(!health.record(true), "a success is quiet");
        assert!(
            health.record(false),
            "a new failure after a success warns again"
        );
        assert_eq!(health.failed, 4);
    }

    #[test]
    fn a_second_writer_on_the_same_file_adds_no_second_header() {
        let dir = TestDir::new("header-once");
        for node in ["node-a", "node-b"] {
            let (mut log, _) = InputLog::with_channel(1);
            log.attach_writer(dir.path().to_path_buf(), node.into())
                .unwrap();
            log.record(ev("t1", DECISION_DENY, 1, 1));
            log.flush_and_stop();
        }
        let text = read(&dir);
        let headers = text.lines().filter(|l| l.starts_with(r#"{"node""#)).count();
        assert_eq!(headers, 1, "{text}");
        assert_eq!(text.lines().count(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn log_open_refuses_a_fifo_without_blocking() {
        let dir = TestDir::new("fifo");
        let path = dir.path().join(LOG_FILE);
        let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: c_path is a valid NUL-terminated string.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let dir_path = dir.path().to_path_buf();
        std::thread::spawn(move || {
            let (_tx, rx) = std::sync::mpsc::sync_channel(1);
            let _ = done_tx.send(spawn_writer(rx, dir_path, "node-a", Default::default()).is_err());
        });
        let refused = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("opening a FIFO must not block");
        assert!(refused, "a FIFO is not a log file");
    }

    #[cfg(unix)]
    #[test]
    fn log_open_refuses_a_hard_linked_file() {
        let dir = TestDir::new("hardlink");
        let other = dir.path().join("elsewhere");
        std::fs::write(&other, "keep").unwrap();
        std::fs::hard_link(&other, dir.path().join(LOG_FILE)).unwrap();
        let (_tx, rx) = std::sync::mpsc::sync_channel(1);
        assert!(spawn_writer(rx, dir.path().to_path_buf(), "node-a", Default::default()).is_err());
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn rotated_files_stay_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new("rotate-mode");
        let (tx, rx) = std::sync::mpsc::sync_channel(512);
        let shared = WriterShared::default();
        spawn_writer(rx, dir.path().to_path_buf(), "node-a", shared.clone()).unwrap();
        let mut big = ev("t1", DECISION_ALLOW, 1, 1);
        big.pane = "p".repeat(20_000);
        for _ in 0..80 {
            tx.send(LogMsg::Event(big.clone())).unwrap();
        }
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        tx.send(LogMsg::Stop(done_tx)).unwrap();
        shared.pacer.wake_now();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        for name in [LOG_FILE.to_string(), format!("{LOG_FILE}.1")] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn an_idle_writer_never_wakes() {
        let dir = TestDir::new("idle");
        let mut log = InputLog::new();
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(
            log.writer_wakes(),
            0,
            "no timer, no polling while nothing is pending"
        );
        log.flush_and_stop();
    }

    #[test]
    fn a_burst_wakes_the_writer_once_not_once_per_entry() {
        let dir = TestDir::new("burst");
        let mut log = InputLog::new();
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        // Entries arrive for a while, a millisecond apart. The writer wakes for the first of
        // each batch window, waits out the window and writes: about two wake-ups a window.
        let start = std::time::Instant::now();
        for n in 0..150u64 {
            log.record(ev(&format!("t{n}"), DECISION_DENY, n, 1));
            log.wake_writer();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let elapsed = start.elapsed();
        // Every entry is written eventually; wait for it without a time assert (a loaded
        // machine may be slow, only a lost entry hangs).
        let waited = std::time::Instant::now();
        while read(&dir).lines().count() < 151 {
            assert!(
                waited.elapsed() < std::time::Duration::from_secs(10),
                "entries never reached the file"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let windows = elapsed.as_millis() / WRITER_BATCH_DELAY.as_millis() + 2;
        let wakes = log.writer_wakes();
        assert!(wakes >= 1, "the writer never woke");
        assert!(
            u128::from(wakes) <= windows * 2 + 2,
            "{wakes} wake-ups for 150 entries over {elapsed:?}"
        );
        assert_eq!(read(&dir).lines().count(), 151);
        log.flush_and_stop();
    }

    /// Wait for `needle` in the file; the time since `start`. A hang guard, not the bound.
    fn wait_in_file(dir: &TestDir, needle: &str, start: std::time::Instant) -> std::time::Duration {
        loop {
            if read(dir).contains(needle) {
                return start.elapsed();
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(5),
                "{needle} never reached the file"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn batch_wait_counts_from_the_first_entry_not_from_the_wake() {
        let ms = std::time::Duration::from_millis;
        assert_eq!(batch_wait(1000, 1000), WRITER_BATCH_DELAY);
        assert_eq!(batch_wait(1000, 1020), ms(30));
        assert_eq!(batch_wait(1000, 1000 + 50), ms(0));
        // The handler outlasted the delay: the writer drains at once, not 50 ms later.
        assert_eq!(batch_wait(1000, 1000 + 80), ms(0));
        // A clock that went backwards never makes the wait longer than the delay.
        assert_eq!(batch_wait(1000, 900), WRITER_BATCH_DELAY);
        // Whatever the handler time, input to write is at most max(delay, handler).
        for handler in 0..200u64 {
            let waited = batch_wait(1000, 1000 + handler);
            let total = ms(handler) + waited;
            assert!(total <= WRITER_BATCH_DELAY.max(ms(handler)), "{handler}");
        }
    }

    #[test]
    fn the_batch_delay_leaves_room_under_the_visibility_bound() {
        assert!(WRITER_BATCH_DELAY * 2 <= VISIBILITY_BOUND);
    }

    #[test]
    fn input_to_write_stays_within_the_bound_for_any_handler_under_it() {
        // No clock: the wake comes `handler` ms after the first entry; the writer then waits
        // `batch_wait`. Input to drain is `handler + wait`, which must be within the bound for
        // every handler shorter than the bound. Counting the delay from the wake instead of the
        // first entry gives handler + 50, over the bound for a handler above 50 ms.
        let bound = u64::try_from(VISIBILITY_BOUND.as_millis()).unwrap_or(u64::MAX);
        for handler in 0..=bound {
            let wait = batch_wait(1000, 1000 + handler);
            let total = handler + u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
            assert!(total <= bound, "handler {handler} ms: {total} ms to write");
        }
        // The same through the Pacer the writer reads.
        let pacer = Pacer::default();
        pacer.notify_at(1000);
        assert_eq!(pacer.wait_at(1000 + 70), std::time::Duration::ZERO);
    }

    #[test]
    fn the_input_path_never_takes_the_writer_handle_lock() {
        let dir = TestDir::new("nolock");
        let mut log = InputLog::new();
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        fn locks(log: &InputLog) -> u64 {
            log.pacer()
                .thread_locks
                .load(std::sync::atomic::Ordering::Relaxed)
        }
        let before = locks(&log);
        // Deny (sent at once), an allow burst, and a closing entry: every record path.
        log.record(ev("t1", DECISION_DENY, 1, 1));
        log.record(ev("t2", DECISION_ALLOW, 2, 1));
        log.record(ev("t2", DECISION_ALLOW, 3, 1));
        log.record(ev("t3", DECISION_DENY, 4, 1));
        assert_eq!(locks(&log), before, "the input path took the handle lock");
        // The wake after the reply does take it, so the counter is live.
        log.wake_writer();
        assert_eq!(locks(&log), before + 1);
        log.flush_and_stop();
    }

    #[test]
    fn the_pacer_keeps_the_time_of_the_first_entry_of_the_batch() {
        let pacer = Pacer::default();
        pacer.notify_at(5000);
        // Later entries of the same batch don't move it.
        pacer.notify_at(5030);
        assert_eq!(
            pacer.wait_at(5010),
            WRITER_BATCH_DELAY - std::time::Duration::from_millis(10)
        );
        assert_eq!(pacer.wait_at(5049), std::time::Duration::from_millis(1));
        assert_eq!(pacer.wait_at(5051), std::time::Duration::ZERO);
    }

    /// Record one refusal into an idle log, hand the writer its wake-up after `handler`, and
    /// time record to file.
    fn first_entry_latency(name: &str, handler: std::time::Duration) -> std::time::Duration {
        let dir = TestDir::new(name);
        let mut log = InputLog::new();
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        // Let the writer settle into its idle park.
        std::thread::sleep(WRITER_BATCH_DELAY);
        let start = std::time::Instant::now();
        log.record(ev("t1", DECISION_DENY, 1, 1));
        std::thread::sleep(handler);
        log.wake_writer();
        let took = wait_in_file(&dir, r#""terminal":"t1""#, start);
        log.flush_and_stop();
        took
    }

    fn best_of_three(name: &str, handler: std::time::Duration) -> std::time::Duration {
        (0..3)
            .map(|n| first_entry_latency(&format!("{name}-{n}"), handler))
            .min()
            .unwrap_or_default()
    }

    #[test]
    #[ignore = "timing: run by just bench-access"]
    fn the_first_entry_after_idle_is_in_the_file_within_the_bound_of_the_input() {
        // A handler of ordinary length: the wake comes at once.
        let quick = best_of_three("bound-quick", std::time::Duration::ZERO);
        assert!(quick <= VISIBILITY_BOUND, "{quick:?}");
        // A handler that takes 70 ms: the entry is in the file when it returns, about 70 ms
        // from the input; counting the delay from the return would give 120 ms.
        let slow = best_of_three("bound-slow", std::time::Duration::from_millis(70));
        assert!(slow <= VISIBILITY_BOUND, "{slow:?}");
    }

    #[test]
    #[ignore = "timing: run by just bench-access"]
    fn a_handler_that_outlasts_the_delay_is_not_waited_for_twice() {
        // The delay has passed by the time the wake-up comes, so the writer drains at once:
        // the entry is in the file well before another full delay (50 ms) has gone by.
        let handler = WRITER_BATCH_DELAY + std::time::Duration::from_millis(20);
        let took = best_of_three("bound-late", handler);
        assert!(
            took < handler + WRITER_BATCH_DELAY / 2,
            "{took:?} after a {handler:?} handler"
        );
    }

    #[test]
    #[ignore = "timing: run by just bench-access"]
    fn every_entry_reaches_the_file_within_the_bound() {
        let dir = TestDir::new("bound");
        let mut log = InputLog::new();
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        std::thread::sleep(WRITER_BATCH_DELAY);
        let start = std::time::Instant::now();
        // A refusal, and an allow entry that a later different entry closes.
        log.record(ev("t1", DECISION_DENY, 1, 1));
        log.record(ev("t2", DECISION_ALLOW, 2, 1));
        log.record(ev("t3", DECISION_DENY, 3, 1));
        log.wake_writer();
        let arrived = wait_in_file(&dir, r#""terminal":"t3""#, start);
        let text = read(&dir);
        assert!(
            text.contains(r#""terminal":"t1""#) && text.contains(r#""terminal":"t2""#),
            "{text}"
        );
        assert!(arrived <= VISIBILITY_BOUND, "{arrived:?}");
        log.flush_and_stop();
    }

    #[test]
    fn sync_writes_everything_recorded_before_it() {
        let dir = TestDir::new("sync");
        let mut log = InputLog::new();
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        log.record(ev("t1", DECISION_ALLOW, 1, 5));
        log.record(ev("t1", DECISION_ALLOW, 2, 5));
        log.sync();
        // No sleep: the open entry and the queue are in the file when sync returns.
        let text = read(&dir);
        assert!(
            text.contains(r#""bytes":10"#) && text.contains(r#""count":2"#),
            "{text}"
        );
        log.flush_and_stop();
    }

    #[test]
    fn sync_seals_the_entry_it_closes_so_later_input_is_not_lost() {
        let dir = TestDir::new("sync-seal");
        let mut log = InputLog::new();
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        log.record(ev("t1", DECISION_ALLOW, 1000, 5));
        log.sync();
        // The same sender types again within the coalescing window, after a read.
        log.record(ev("t1", DECISION_ALLOW, 1500, 7));
        log.flush_and_stop();
        let text = read(&dir);
        let entries: Vec<&str> = text
            .lines()
            .filter(|l| l.contains(r#""terminal":"t1""#))
            .collect();
        assert_eq!(entries.len(), 2, "both bursts reach the file: {text}");
        assert!(
            text.contains(r#""bytes":7"#) && text.contains(r#""bytes":5"#),
            "{text}"
        );
    }

    #[test]
    fn recording_does_not_wake_the_writer_the_reply_path_does() {
        let dir = TestDir::new("deferred");
        let mut log = InputLog::new();
        log.attach_writer(dir.path().to_path_buf(), "node-a".into())
            .unwrap();
        // Let the writer start and settle into its idle park, or it would find the entry on
        // its first pass.
        std::thread::sleep(WRITER_BATCH_DELAY);
        log.record(ev("t1", DECISION_DENY, 1, 1));
        std::thread::sleep(WRITER_BATCH_DELAY * 3);
        // The input path only queued the entry and set a flag: no wake-up, nothing written.
        assert_eq!(log.writer_wakes(), 0, "record woke the writer");
        assert!(!read(&dir).contains(r#""terminal":"t1""#), "{}", read(&dir));
        // After the reply the server delivers the one wake-up; the entry is in the file
        // within the bound.
        let start = std::time::Instant::now();
        log.wake_writer();
        let _ = wait_in_file(&dir, r#""terminal":"t1""#, start);
        log.flush_and_stop();
    }
}
