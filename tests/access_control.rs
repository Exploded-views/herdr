// herdr-fork: end-to-end tests of per-pane sender access control against isolated test servers (the built `herdr`, real panes, real CLI processes, a shell-client socket and a simulated ssh server), including that human-only survives a restart, and that the human-only test waits for the agent; live handoff is admin only while access is protected and access state survives a handoff. Unofficial stopgap; see FORK.md.
//
// How the tests are built:
// - Each test owns a base directory under /tmp. The server, every CLI process and every pane
//   runs with a cleared environment, so nothing of the developer's session (and none of its
//   `HERDR_*` variables) leaks in, and the first thing every test does is prove that the
//   panes see the test's socket and not another one.
// - Panes run `/bin/sh` with a PATH that holds one directory containing only a `herdr` symlink
//   to the built binary. Commands typed into panes call `"$HERDR_BIN_PATH"`.
// - "From a pane" means a command typed into that pane, so the server sees a real process
//   lineage. "From outside" means a process the test starts itself.
// - The three helpers below (`sshd_helper`, `shell_client_helper`, `agent_stub`) are tests
//   marked `#[ignore]`. The suite re-executes its own test binary to run them: once copied to
//   a temporary directory under the name `sshd` (the simulated ssh server), once from a pane
//   (a shell-client protocol client running inside a pane), once from a pane as a stand-in
//   agent. The suite adds no `[[bin]]` or example target, so nothing here
//   can end up in a release build.
// - No assertion depends on how long something takes: every wait polls for an outcome with a
//   generous hang guard, and a denial is proven by a later control input that does arrive.
#![cfg(unix)]

pub mod support;

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use support::{
    cleanup_test_base, client_shell_handshake, drain_messages, register_runtime_dir,
    register_spawned_herdr_pid, send_client_shell_text, unregister_spawned_herdr_pid,
    wait_for_client_shell_bootstrap, wait_for_socket, CURRENT_ENDPOINT_PROTOCOL_GENERATION,
};

/// The guard against a hang. Never a latency expectation: a wait returns as soon as its
/// outcome shows.
const HANG: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(25);

const TAIL: &str = "This is deliberate policy. Do not retry with another herdr command, attach, tmux, or any other route. Use your own messaging channel (e.g. SendMessage) or report to your operator.";

fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

fn unique_test_dir() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/hac-{}-{n}-{nanos}", std::process::id()))
}

/// `'text'`, safe to type into `/bin/sh`.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Polls `check` until it returns a value, or panics with `what` after the hang guard.
fn poll<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    poll_observing(what, || check().ok_or_else(String::new))
}

/// Like `poll`, and the failure message shows what the last attempt saw.
fn poll_observing<T>(what: &str, mut check: impl FnMut() -> Result<T, String>) -> T {
    let deadline = Instant::now() + HANG;
    loop {
        match check() {
            Ok(value) => return value,
            Err(seen) => assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; last seen:\n{seen}"
            ),
        }
        thread::sleep(POLL);
    }
}

/// A shell client connected to the client socket at `path`, past its handshake and bootstrap.
fn connect_shell_client(path: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(path).expect("the client socket");
    let (generation, error) =
        client_shell_handshake(&mut stream, CURRENT_ENDPOINT_PROTOCOL_GENERATION, 54, 23)
            .expect("client shell handshake");
    assert_eq!(generation, CURRENT_ENDPOINT_PROTOCOL_GENERATION);
    assert!(error.is_none(), "client shell handshake: {error:?}");
    wait_for_client_shell_bootstrap(&mut stream, HANG).expect("client shell bootstrap");
    stream
}

/// The ssh-based tests need a harness that is not itself reached through an ssh server, or
/// its own commands would count as another host.
fn harness_under_ssh() -> bool {
    let under = ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some());
    if under {
        eprintln!("skipping: SSH_CONNECTION or SSH_TTY is set, so this harness runs under ssh");
    }
    under
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

struct Server {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

/// What a finished process printed.
struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn describe(&self) -> String {
        format!(
            "exit {}\nstdout: {}\nstderr: {}",
            self.code, self.stdout, self.stderr
        )
    }

    fn assert_ok(&self, what: &str) -> &Self {
        assert_eq!(self.code, 0, "{what} should succeed\n{}", self.describe());
        self
    }

    fn assert_denied(&self, what: &str) -> &Self {
        assert_ne!(
            self.code,
            0,
            "{what} should be refused\n{}",
            self.describe()
        );
        assert!(
            self.stderr.contains("access_denied"),
            "{what} should be refused with access_denied\n{}",
            self.describe()
        );
        self
    }
}

/// Runs `cmd` to the end and returns what it printed. A process that outlives the hang guard
/// is killed and the test fails.
fn run_command(cmd: Command, what: &str) -> Out {
    run_command_within(cmd, what, HANG * 2)
}

/// `run_command` with its own limit.
fn run_command_within(mut cmd: Command, what: &str, limit: Duration) -> Out {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|err| panic!("could not start {what}: {err}"));
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => panic!("could not wait for {what}: {err}"),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what} did not finish within the hang guard");
        }
        thread::sleep(POLL);
    };
    Out {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned(),
    }
}

// ---------------------------------------------------------------------------
// One isolated node
// ---------------------------------------------------------------------------

struct Node {
    base: PathBuf,
    config_home: PathBuf,
    runtime_dir: PathBuf,
    socket: PathBuf,
    client_socket: PathBuf,
    bin_dir: PathBuf,
    server: Option<Server>,
    seq: AtomicU32,
}

impl Drop for Node {
    fn drop(&mut self) {
        // Lets a stand-in agent still running in a pane exit promptly.
        let _ = fs::write(self.base.join("agent-done"), b"done");
        self.stop_server();
        cleanup_test_base(&self.base);
    }
}

impl Node {
    /// A fresh base directory and a running server. `extra_config` is appended to the config.
    fn start(extra_config: &str) -> Node {
        let base = unique_test_dir();
        let config_home = base.join("config");
        let runtime_dir = base.join("runtime");
        let bin_dir = base.join("bin");
        let socket = runtime_dir.join("herdr.sock");
        let client_socket = runtime_dir.join("herdr-client.sock");
        // The isolation rule: the socket this test talks to lives under this test's base.
        assert!(
            socket.starts_with(&base),
            "the test socket {} is not under the test base {}",
            socket.display(),
            base.display()
        );
        fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
        fs::create_dir_all(&runtime_dir).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(base.join("home")).unwrap();
        fs::create_dir_all(base.join("state")).unwrap();
        fs::create_dir_all(base.join("data")).unwrap();
        // The only entry on the panes' PATH: a `herdr` that is the binary under test.
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_herdr"), bin_dir.join("herdr")).unwrap();
        fs::write(
            config_home.join(app_dir_name()).join("config.toml"),
            format!(
                "onboarding = false\n[terminal]\ndefault_shell = \"/bin/sh\"\nshell_mode = \"non_login\"\n{extra_config}\n"
            ),
        )
        .unwrap();
        let mut node = Node {
            base,
            config_home,
            runtime_dir,
            socket,
            client_socket,
            bin_dir,
            server: None,
            seq: AtomicU32::new(0),
        };
        node.spawn_server();
        node
    }

    fn next(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// The whole environment of every process the test starts, and nothing else.
    fn env(&self) -> Vec<(&'static str, String)> {
        let path = |p: &Path| p.display().to_string();
        vec![
            ("PATH", path(&self.bin_dir)),
            ("HOME", path(&self.base.join("home"))),
            ("XDG_CONFIG_HOME", path(&self.config_home)),
            ("XDG_RUNTIME_DIR", path(&self.runtime_dir)),
            ("XDG_STATE_HOME", path(&self.base.join("state"))),
            ("XDG_DATA_HOME", path(&self.base.join("data"))),
            ("HERDR_SOCKET_PATH", path(&self.socket)),
            ("HERDR_CLIENT_SOCKET_PATH", path(&self.client_socket)),
            ("HERDR_DISABLE_SOUND", "1".to_string()),
            ("SHELL", "/bin/sh".to_string()),
            ("TERM", "xterm-256color".to_string()),
        ]
    }

    fn spawn_server(&mut self) {
        register_runtime_dir(&self.runtime_dir);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
        cmd.env_clear();
        cmd.arg("server");
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
        let child = pair.slave.spawn_command(cmd).unwrap();
        register_spawned_herdr_pid(child.process_id());
        self.server = Some(Server {
            _master: pair.master,
            child,
        });
        wait_for_socket(&self.socket, HANG);
    }

    #[cfg(target_os = "linux")]
    fn server_pid(&self) -> u32 {
        self.server
            .as_ref()
            .and_then(|server| server.child.process_id())
            .expect("a running server")
    }

    /// Stops the server we started, and waits until it is gone.
    fn stop_server(&mut self) {
        let Some(mut server) = self.server.take() else {
            return;
        };
        let pid = server.child.process_id();
        support::stop_spawned_herdr(&mut *server.child);
        if let Some(pid) = pid {
            let deadline = Instant::now() + HANG;
            loop {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                if Instant::now() >= deadline {
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGKILL);
                        libc::waitpid(pid as libc::pid_t, &mut status, 0);
                    }
                    break;
                }
                thread::sleep(POLL);
            }
            unregister_spawned_herdr_pid(Some(pid));
        }
    }

    /// Stops the server and starts a new one on the same directories.
    fn restart_server(&mut self) {
        self.stop_server();
        poll("the old socket to stop answering", || {
            UnixStream::connect(&self.socket).is_err().then_some(())
        });
        self.spawn_server();
    }

    // -- API ----------------------------------------------------------------

    fn try_api(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut stream = UnixStream::connect(&self.socket).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(HANG))
            .map_err(|e| e.to_string())?;
        let request = json!({"id": "e2e", "method": method, "params": params});
        writeln!(stream, "{request}").map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        serde_json::from_str(&line).map_err(|e| format!("{e}: {line:?}"))
    }

    fn api(&self, method: &str, params: Value) -> Value {
        self.try_api(method, params)
            .unwrap_or_else(|err| panic!("{method} failed: {err}"))
    }

    fn api_ok(&self, method: &str, params: Value) -> Value {
        let response = self.api(method, params);
        assert!(response.get("error").is_none(), "{method}: {response}");
        response
    }

    /// One workspace with two panes, `a` and `b`, and the isolation proof for `a`.
    fn two_panes(&self) -> (String, String) {
        let created = self.api_ok(
            "workspace.create",
            json!({"cwd": self.base.display().to_string(), "focus": true}),
        );
        let a = created["result"]["root_pane"]["pane_id"]
            .as_str()
            .expect("root pane id")
            .to_string();
        let split = self.api_ok(
            "pane.split",
            json!({"target_pane_id": a, "direction": "right", "focus": false}),
        );
        let b = split["result"]["pane"]["pane_id"]
            .as_str()
            .expect("second pane id")
            .to_string();
        self.assert_panes_are_isolated(&a);
        (a, b)
    }

    /// What a pane sees as the herdr socket must be this test's socket, under this test's
    /// base. A pane that inherited a live session's socket would otherwise test the wrong
    /// server.
    fn assert_panes_are_isolated(&self, pane: &str) {
        let seen = self.run_in_pane(pane, r#"echo "$HERDR_SOCKET_PATH" "$HERDR_BIN_PATH""#);
        assert_eq!(seen.rc, 0, "the isolation probe failed: {}", seen.text);
        let mut words = seen.text.split_whitespace();
        let socket = words.next().unwrap_or_default();
        let bin = words.next().unwrap_or_default();
        assert!(
            Path::new(socket).starts_with(&self.base),
            "a pane sees the socket {socket:?}, which is not under the test base {}",
            self.base.display()
        );
        assert_eq!(
            Path::new(socket),
            self.socket.as_path(),
            "a pane sees another socket"
        );
        assert!(
            Path::new(bin).file_name() == Some(OsStr::new("herdr")),
            "a pane's HERDR_BIN_PATH is {bin:?}"
        );
    }

    /// Makes `pane` host an agent: a process the server identifies as `pi` (by its
    /// `HERDR_AGENT` variable) in the foreground, and a reported idle state. An agent prompt
    /// is only accepted for a pane whose foreground process is its agent.
    fn start_agent(&self, pane: &str) {
        let exe = std::env::current_exe().unwrap().display().to_string();
        let ready = self.base.join(format!("agent-ready-{}", self.next()));
        self.type_line(
            pane,
            &format!(
                "HERDR_AGENT=pi HERDR_E2E_READY={} HERDR_E2E_DONE={} {} --ignored --exact agent_stub --nocapture > /dev/null 2>&1",
                sh_quote(&ready.display().to_string()),
                sh_quote(&self.base.join("agent-done").display().to_string()),
                sh_quote(&exe),
            ),
        );
        poll("the agent process to start", || {
            ready.exists().then_some(())
        });
        self.api_ok(
            "pane.report_agent",
            json!({"pane_id": pane, "source": "custom:e2e", "agent": "pi", "state": "idle"}),
        );
    }

    /// Types `text` and Enter into a pane the way a person would. Only for panes that accept
    /// input from outside.
    fn type_line(&self, pane: &str, text: &str) {
        self.api_ok("pane.send_text", json!({"pane_id": pane, "text": text}));
        self.api_ok(
            "pane.send_keys",
            json!({"pane_id": pane, "keys": ["Enter"]}),
        );
    }

    fn pane_text(&self, pane: &str) -> String {
        let read = self.api_ok(
            "pane.read",
            json!({"pane_id": pane, "source": "recent_unwrapped", "lines": 400}),
        );
        read["result"]["read"]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn wait_text(&self, pane: &str, needle: &str) {
        poll_observing(&format!("{needle:?} to show in pane {pane}"), || {
            let text = self.pane_text(pane);
            if text.contains(needle) {
                Ok(())
            } else {
                Err(text)
            }
        });
    }

    /// The proof of a denial: the pane shows a later control input, and does not show the
    /// refused one.
    fn assert_never_shows(&self, pane: &str, control: &str, refused: &[&str]) {
        self.wait_text(pane, control);
        let text = self.pane_text(pane);
        for text_refused in refused {
            assert!(
                !text.contains(text_refused),
                "pane {pane} shows {text_refused:?}, which should have been refused:\n{text}"
            );
        }
    }

    // -- CLI ----------------------------------------------------------------

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_herdr"));
        cmd.env_clear().args(args);
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd
    }

    /// `herdr <args>` as a process started by the test: outside every pane.
    fn cli(&self, args: &[&str]) -> Out {
        self.cli_env(args, &[])
    }

    fn cli_env(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        run_command(
            self.command(args, env),
            &format!("herdr {}", args.join(" ")),
        )
    }

    /// `herdr <args>` started by the simulated ssh server: a process named `sshd` between the
    /// CLI and the test, as in a forced-command login.
    fn cli_via_sshd(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let dir = self.base.join("sshd-dir");
        let sshd = dir.join("sshd");
        if !sshd.exists() {
            fs::create_dir_all(&dir).unwrap();
            fs::copy(std::env::current_exe().unwrap(), &sshd).unwrap();
            fs::set_permissions(&sshd, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let prefix = self.base.join(format!("ssh-{}", self.next()));
        let mut cmd = Command::new(sshd);
        cmd.env_clear()
            .args(["--ignored", "--exact", "sshd_helper", "--nocapture"]);
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.env("HERDR_E2E_ARGS", args.join("\u{1f}"));
        cmd.env("HERDR_E2E_FILES", &prefix);
        let helper = run_command(cmd, "the simulated ssh server");
        let read = |ext: &str| {
            fs::read_to_string(format!("{}.{ext}", prefix.display())).unwrap_or_else(|err| {
                panic!(
                    "the simulated ssh server left no .{ext} file ({err})\n{}",
                    helper.describe()
                )
            })
        };
        Out {
            code: read("rc").trim().parse().expect("exit code"),
            stdout: read("out"),
            stderr: read("err"),
        }
    }

    /// Runs `script` in the shell of `pane` and returns what it printed and its status. The
    /// pane must accept typed input from outside.
    fn run_in_pane(&self, pane: &str, script: &str) -> PaneRun {
        let out = self.base.join(format!("pane-out-{}", self.next()));
        let o = sh_quote(&out.display().to_string());
        self.type_line(
            pane,
            &format!("{{ {script}; }} > {o} 2>&1; echo \"rc=$?\" >> {o}"),
        );
        let text = poll("a command typed into a pane to finish", || {
            let text = fs::read_to_string(&out).ok()?;
            text.lines()
                .any(|line| line.starts_with("rc="))
                .then_some(text)
        });
        let mut lines: Vec<&str> = text.lines().collect();
        let rc = lines
            .pop()
            .and_then(|line| line.strip_prefix("rc="))
            .and_then(|code| code.parse().ok())
            .expect("an rc line");
        PaneRun {
            rc,
            text: lines.join("\n"),
        }
    }

    /// `"$HERDR_BIN_PATH" <args>` typed into `pane`.
    fn herdr_in_pane(&self, pane: &str, args: &[&str]) -> PaneRun {
        let quoted: Vec<String> = args.iter().map(|arg| sh_quote(arg)).collect();
        self.run_in_pane(pane, &format!("\"$HERDR_BIN_PATH\" {}", quoted.join(" ")))
    }

    // -- Access control -----------------------------------------------------

    fn protect(&self, pane: &str, accept_from: &str) -> String {
        self.cli(&["access", "pane", "set", pane, "--accept-from", accept_from])
            .assert_ok("access pane set");
        self.binding_of(pane)
    }

    /// The pane's binding id, from `access pane get`.
    fn binding_of(&self, pane: &str) -> String {
        let got = self.cli(&["access", "pane", "get", pane]);
        got.assert_ok("access pane get");
        let binding = got
            .stdout
            .split('\t')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        assert!(
            is_binding_id(&binding),
            "access pane get shows no binding id: {}",
            got.stdout
        );
        binding
    }

    /// Adds a caller bound to `pane` and returns its token file and the binding id.
    fn add_caller(&self, name: &str, pane: &str) -> (String, String) {
        let file = self
            .base
            .join(format!("{name}.token"))
            .display()
            .to_string();
        let added = self.cli(&[
            "access",
            "caller",
            "add",
            name,
            "--target-pane",
            pane,
            "--token-file",
            &file,
        ]);
        added.assert_ok("access caller add");
        let binding = added.stdout.trim().to_string();
        assert!(
            is_binding_id(&binding),
            "unexpected output: {}",
            added.stdout
        );
        (file, binding)
    }

    /// The events `herdr access log --json` shows, once `done` accepts them.
    fn wait_log(&self, what: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        poll_observing(&format!("the input log to show {what}"), || {
            let out = self.cli(&["access", "log", "--json"]);
            out.assert_ok("access log");
            let document: Value = serde_json::from_str(&out.stdout).unwrap_or_else(|err| {
                panic!("access log --json is not JSON ({err}): {}", out.stdout)
            });
            let events = document["events"].as_array().cloned().unwrap_or_default();
            if done(&events) {
                Ok(events)
            } else {
                Err(format!("{events:?}"))
            }
        })
    }

    // -- Shell clients --------------------------------------------------------

    /// A shell client started by the test (outside every pane), past its bootstrap.
    fn outside_shell_client(&self) -> UnixStream {
        connect_shell_client(&self.client_socket)
    }

    /// Starts a shell client inside `pane` (the test binary again, see `shell_client_helper`)
    /// that types `text` into `target`, and waits until it has sent it.
    fn start_client_in_pane(&self, pane: &str, target: &str, text: &str) {
        let exe = std::env::current_exe().unwrap().display().to_string();
        let base = self.base.display().to_string();
        let _ = fs::remove_file(self.base.join("helper-sent"));
        let _ = fs::remove_file(self.base.join("helper-done"));
        self.type_line(
            pane,
            &format!(
                "HERDR_E2E_BASE={} HERDR_E2E_PANE={} HERDR_E2E_TEXT={} {} --ignored --exact shell_client_helper --nocapture > {} 2>&1",
                sh_quote(&base),
                sh_quote(target),
                sh_quote(text),
                sh_quote(&exe),
                sh_quote(&format!("{base}/helper-out")),
            ),
        );
        let sent = self.base.join("helper-sent");
        poll("the client in the pane to send its input", || {
            if sent.exists() {
                return Some(());
            }
            let out = fs::read_to_string(self.base.join("helper-out")).unwrap_or_default();
            assert!(
                !out.contains("panicked"),
                "the client in the pane failed:\n{out}"
            );
            None
        });
    }

    /// Lets the client in the pane exit.
    fn finish_client_in_pane(&self) {
        fs::write(self.base.join("helper-done"), b"done").unwrap();
    }
}

struct PaneRun {
    rc: i32,
    /// What the command printed, standard output and error together.
    text: String,
}

impl PaneRun {
    fn assert_denied(&self, what: &str) {
        assert_ne!(
            self.rc, 0,
            "{what} from a pane should be refused: {}",
            self.text
        );
        assert!(
            self.text.contains("access_denied"),
            "{what} from a pane should be refused with access_denied: {}",
            self.text
        );
    }

    fn assert_ok(&self, what: &str) {
        assert_eq!(self.rc, 0, "{what} from a pane should work: {}", self.text);
    }

    /// Refused with `access_denied`, and the text names the sender's class as a pane.
    fn assert_denied_from_pane(&self, what: &str) {
        self.assert_denied(what);
        assert!(
            self.text.contains("from pane"),
            "{what} should name the pane class: {}",
            self.text
        );
    }

    /// Refused, and the refusal names the sender as a pane: the server recognised the process
    /// as running inside one, rather than refusing it as unknown.
    fn assert_refused_as_pane(&self, what: &str) {
        self.assert_denied(what);
        assert!(
            self.text.contains("does not accept input from pane:"),
            "{what} should be refused as coming from a pane: {}",
            self.text
        );
    }
}

fn is_binding_id(text: &str) -> bool {
    text.strip_prefix("acc_").is_some_and(|hex| {
        hex.len() == 32 && hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    })
}

/// A node with two panes; `b` hosts an agent, so it can be addressed as one. `a` is a plain
/// shell that the tests type commands into.
fn setup(extra_config: &str) -> (Node, String, String) {
    let node = Node::start(extra_config);
    let (a, b) = node.two_panes();
    node.start_agent(&b);
    (node, a, b)
}

const BRIDGE_CONFIG: &str = "[access]\nremote_input = \"bridge-only\"\nbridge_agent = \"relay\"\n";

// ---------------------------------------------------------------------------
// Helpers that run as other processes (ignored; re-executed by the tests above)
// ---------------------------------------------------------------------------

/// Runs as `sshd`: a copy of this test binary, started by `Node::cli_via_sshd`. It starts the
/// CLI as its child, as a forced command would be, and hands back what the CLI printed.
#[test]
#[ignore = "started by the ssh tests under the name sshd; does nothing on its own"]
fn sshd_helper() {
    let (Ok(args), Ok(files)) = (
        std::env::var("HERDR_E2E_ARGS"),
        std::env::var("HERDR_E2E_FILES"),
    ) else {
        return;
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.args(args.split('\u{1f}'))
        .env_remove("HERDR_E2E_ARGS")
        .env_remove("HERDR_E2E_FILES");
    // The CLI child gets no longer than the test's hang guard, and is killed (it alone) after.
    let output = run_command_within(cmd, "the CLI", HANG);
    fs::write(format!("{files}.out"), &output.stdout).unwrap();
    fs::write(format!("{files}.err"), &output.stderr).unwrap();
    fs::write(format!("{files}.rc"), output.code.to_string()).unwrap();
    std::process::exit(0);
}

/// Runs in the foreground of a pane, typed there by `Node::start_agent` with `HERDR_AGENT=pi`
/// in its environment, which is how the server comes to see an agent in the pane. It reads
/// nothing: what is typed into the pane is echoed by the terminal and shows on its screen.
#[test]
#[ignore = "started inside a pane by the tests as a stand-in agent; does nothing on its own"]
fn agent_stub() {
    let (Ok(ready), Ok(done)) = (
        std::env::var("HERDR_E2E_READY"),
        std::env::var("HERDR_E2E_DONE"),
    ) else {
        return;
    };
    fs::write(ready, b"ready").unwrap();
    let done = PathBuf::from(done);
    let deadline = Instant::now() + HANG * 4;
    while !done.exists() && Instant::now() < deadline {
        thread::sleep(POLL);
    }
}

/// Runs inside a pane, typed there by `Node::start_client_in_pane`: a shell client of the
/// server that types into another pane. The server sees a client whose process lineage runs
/// through the pane.
#[test]
#[ignore = "started inside a pane by the client tests; does nothing on its own"]
fn shell_client_helper() {
    let (Ok(base), Ok(pane), Ok(text)) = (
        std::env::var("HERDR_E2E_BASE"),
        std::env::var("HERDR_E2E_PANE"),
        std::env::var("HERDR_E2E_TEXT"),
    ) else {
        return;
    };
    let base = PathBuf::from(base);
    let mut stream = connect_shell_client(&base.join("runtime/herdr-client.sock"));
    send_client_shell_text(&mut stream, &pane, &text).unwrap();
    fs::write(base.join("helper-sent"), b"sent").unwrap();
    // Stay connected until the test has seen the outcome.
    let done = base.join("helper-done");
    let deadline = Instant::now() + HANG * 2;
    while !done.exists() && Instant::now() < deadline {
        drain_messages(&mut stream);
        thread::sleep(POLL);
    }
}

// ---------------------------------------------------------------------------
// Isolation, the plan's list
// ---------------------------------------------------------------------------

#[test]
fn prompt_from_pane_a_to_protected_pane_b_denied() {
    let (node, a, b) = setup("");
    node.protect(&b, "outside");

    node.herdr_in_pane(&a, &["agent", "prompt", &b, "PROMPT_DENIED_1"])
        .assert_refused_as_pane("agent prompt");

    // The proof: a later prompt from outside arrives, the refused one never did.
    node.cli(&["agent", "prompt", &b, "PROMPT_ALLOWED_1"])
        .assert_ok("agent prompt from outside");
    node.assert_never_shows(&b, "PROMPT_ALLOWED_1", &["PROMPT_DENIED_1"]);
}

#[test]
fn every_cli_input_route_denied_from_pane() {
    let (node, a, b) = setup("");
    node.protect(&b, "outside");

    node.herdr_in_pane(&a, &["agent", "prompt", &b, "ROUTE_PROMPT_1"])
        .assert_refused_as_pane("agent prompt");
    node.herdr_in_pane(&a, &["agent", "send-keys", &b, "q", "z", "7", "w"])
        .assert_refused_as_pane("agent send-keys");
    node.herdr_in_pane(&a, &["pane", "send-text", &b, "ROUTE_TEXT_1"])
        .assert_refused_as_pane("pane send-text");
    node.herdr_in_pane(&a, &["pane", "send-keys", &b, "k", "7", "j", "9"])
        .assert_refused_as_pane("pane send-keys");
    node.herdr_in_pane(&a, &["pane", "run", &b, "echo ROUTE_RUN_1"])
        .assert_refused_as_pane("pane run");

    node.cli(&["pane", "send-text", &b, "ROUTE_CONTROL_1"])
        .assert_ok("pane send-text from outside");
    node.assert_never_shows(
        &b,
        "ROUTE_CONTROL_1",
        &[
            "ROUTE_PROMPT_1",
            "qz7w",
            "ROUTE_TEXT_1",
            "k7j9",
            "ROUTE_RUN_1",
        ],
    );
}

#[test]
fn watch_token_works_from_outside_ignored_from_pane() {
    let (node, a, b) = setup("");
    node.protect(&b, "caller:watch");
    let (token_file, binding) = node.add_caller("watch", &b);
    assert_eq!(
        node.binding_of(&b),
        binding,
        "the caller is bound to the pane's binding"
    );

    // From outside, the token reaches the pane by its binding id.
    node.cli(&[
        "--caller-token-file",
        &token_file,
        "agent",
        "prompt",
        &binding,
        "WATCH_OK_1",
    ])
    .assert_ok("agent prompt with the token");
    node.wait_text(&b, "WATCH_OK_1");

    // From outside without the token, the pane's list does not admit it.
    node.cli(&["agent", "prompt", &binding, "WATCH_NOTOKEN_1"])
        .assert_denied("agent prompt without the token");

    // A pane that presents the token is still a pane: the token is ignored.
    node.herdr_in_pane(
        &a,
        &[
            "--caller-token-file",
            &token_file,
            "agent",
            "prompt",
            &binding,
            "WATCH_PANE_FLAG_1",
        ],
    )
    .assert_refused_as_pane("agent prompt from a pane with --caller-token-file");
    node.run_in_pane(
        &a,
        &format!(
            "HERDR_CALLER_TOKEN_FILE={} \"$HERDR_BIN_PATH\" pane send-text {} WATCH_PANE_ENV_1",
            sh_quote(&token_file),
            sh_quote(&b)
        ),
    )
    .assert_refused_as_pane("pane send-text from a pane with HERDR_CALLER_TOKEN_FILE");

    // The token is confined to its own pane: it does not reach an open pane either.
    node.cli(&[
        "--caller-token-file",
        &token_file,
        "pane",
        "send-text",
        &a,
        "WATCH_ELSEWHERE_1",
    ])
    .assert_denied("pane send-text with the token to a pane it is not bound to");

    node.cli(&[
        "--caller-token-file",
        &token_file,
        "agent",
        "prompt",
        &binding,
        "WATCH_OK_2",
    ])
    .assert_ok("agent prompt with the token, again");
    node.assert_never_shows(
        &b,
        "WATCH_OK_2",
        &["WATCH_NOTOKEN_1", "WATCH_PANE_FLAG_1", "WATCH_PANE_ENV_1"],
    );
    node.cli(&["pane", "send-text", &a, "WATCH_A_CONTROL_1"])
        .assert_ok("pane send-text from outside to the open pane");
    node.assert_never_shows(&a, "WATCH_A_CONTROL_1", &["WATCH_ELSEWHERE_1"]);
}

#[test]
fn token_works_on_first_request_after_restart() {
    let (mut node, _a, b) = setup("");
    node.api_ok("pane.rename", json!({"pane_id": b, "label": "protected-b"}));
    let binding = node.protect(&b, "caller:watch");
    let (token_file, token_binding) = node.add_caller("watch", &b);
    assert_eq!(binding, token_binding);

    // The session must be on disk before the server stops, or there is nothing to restore.
    let session = node.config_home.join(app_dir_name()).join("session.json");
    poll("the session file to hold the pane's binding", || {
        fs::read_to_string(&session)
            .ok()
            .filter(|saved| saved.contains(&binding))
            .map(|_| ())
    });

    node.restart_server();

    // The restored pane, found by its label: ids and terminals may be renumbered.
    let restored = poll("the restored pane", || {
        let listed = node.try_api("pane.list", json!({})).ok()?;
        listed["result"]["panes"]
            .as_array()?
            .iter()
            .find(|pane| pane["label"] == "protected-b")
            .and_then(|pane| pane["pane_id"].as_str().map(str::to_string))
    });

    // The first request that touches the pane's policy: the token, from outside.
    node.cli(&[
        "--caller-token-file",
        &token_file,
        "pane",
        "send-text",
        &restored,
        "RESTART_TOKEN_1",
    ])
    .assert_ok("pane send-text with the token after a restart");
    node.wait_text(&restored, "RESTART_TOKEN_1");

    // And the policy came back with the pane: outside without the token is still refused.
    node.cli(&["pane", "send-text", &restored, "RESTART_NOTOKEN_1"])
        .assert_denied("pane send-text without the token after a restart");
    assert_eq!(
        node.binding_of(&restored),
        binding,
        "the binding survives a restart"
    );
    node.cli(&[
        "--caller-token-file",
        &token_file,
        "pane",
        "send-text",
        &restored,
        "RESTART_TOKEN_2",
    ])
    .assert_ok("pane send-text with the token, again");
    node.assert_never_shows(&restored, "RESTART_TOKEN_2", &["RESTART_NOTOKEN_1"]);
}

/// `server.live_handoff` with no `import_exe` hands the panes to a replacement of the same
/// build. The policy, its binding id and the panes' lineage come across with them.
///
/// Linux only: upstream leaves its live_handoff binary out of the macOS jobs, and this test
/// finds the replacement server the same way those do.
#[cfg(target_os = "linux")]
#[test]
fn live_handoff_carries_access_policy() {
    let (node, a, b) = setup("");
    let binding = node.protect(&b, "outside");
    let before = node.cli(&["access", "pane", "get", &b]);
    before.assert_ok("access pane get");
    let old_pid = node.server_pid();

    node.api_ok("server.live_handoff", json!({}));
    poll("the replacement server", || {
        support::herdr_server_pids_for_runtime_dir(&node.runtime_dir)
            .ok()?
            .into_iter()
            .find(|pid| *pid != old_pid)
    });
    poll("the replacement server to answer", || {
        node.try_api("ping", json!({}))
            .ok()
            .filter(|response| response.get("result").is_some())
    });

    let after = poll("access pane get on the replacement", || {
        let out = node.cli(&["access", "pane", "get", &b]);
        (out.code == 0).then_some(out)
    });
    assert_eq!(
        after.stdout, before.stdout,
        "the pane's policy and binding id are unchanged by a handoff"
    );
    assert_eq!(node.binding_of(&b), binding);

    // The pane's shell now has a new server as its grandparent; it must still be a pane.
    node.herdr_in_pane(&a, &["pane", "send-text", &b, "HANDOFF_DENIED_1"])
        .assert_refused_as_pane("pane send-text after a handoff");
    node.cli(&["pane", "send-text", &b, "HANDOFF_ALLOWED_1"])
        .assert_ok("pane send-text from outside after a handoff");
    node.assert_never_shows(&b, "HANDOFF_ALLOWED_1", &["HANDOFF_DENIED_1"]);

    let _ = node.try_api("server.stop", json!({}));
}

#[test]
fn defaults_leave_behaviour_unchanged() {
    let (node, a, b) = setup("");

    // No policy, no mode: nothing is bound and the pane accepts input from everyone.
    let got = node.cli(&["access", "pane", "get", &b]);
    got.assert_ok("access pane get");
    let fields: Vec<&str> = got.stdout.trim_end().split('\t').collect();
    assert_eq!(
        &fields[1..],
        ["-", "any", "any", "default"],
        "{}",
        got.stdout
    );

    node.herdr_in_pane(&a, &["agent", "prompt", &b, "DEFAULT_PROMPT_1"])
        .assert_ok("agent prompt");
    node.herdr_in_pane(&a, &["agent", "send-keys", &b, "q", "z", "7", "w"])
        .assert_ok("agent send-keys");
    node.herdr_in_pane(&a, &["pane", "send-text", &b, "DEFAULT_TEXT_1"])
        .assert_ok("pane send-text");
    node.herdr_in_pane(&a, &["pane", "send-keys", &b, "k", "7", "j", "9"])
        .assert_ok("pane send-keys");
    node.herdr_in_pane(&a, &["pane", "run", &b, "echo DEFAULT_RUN_1"])
        .assert_ok("pane run");
    node.cli(&["pane", "send-text", &b, "DEFAULT_OUTSIDE_1"])
        .assert_ok("pane send-text from outside");

    node.wait_text(&b, "DEFAULT_OUTSIDE_1");
    let text = node.pane_text(&b);
    for expected in [
        "DEFAULT_PROMPT_1",
        "qz7w",
        "DEFAULT_TEXT_1",
        "k7j9",
        "DEFAULT_RUN_1",
    ] {
        assert!(
            text.contains(expected),
            "pane b should show {expected:?}:\n{text}"
        );
    }

    // A client outside every pane types into the open pane too.
    let mut outside = node.outside_shell_client();
    send_client_shell_text(&mut outside, &b, "DEFAULT_CLIENT_1").unwrap();
    node.wait_text(&b, "DEFAULT_CLIENT_1");
}

// ---------------------------------------------------------------------------
// Cover for what was built after the plan (T10 to T10c)
// ---------------------------------------------------------------------------

#[test]
fn client_in_a_pane_is_refused_and_an_outside_client_is_allowed() {
    let (node, a, b) = setup("");
    node.protect(&b, "outside");

    // A client started by the test is outside every pane: allowed.
    let mut outside = node.outside_shell_client();
    send_client_shell_text(&mut outside, &b, "CLIENT_OUTSIDE_1").unwrap();
    node.wait_text(&b, "CLIENT_OUTSIDE_1");

    // The same client started inside pane a is a pane: refused.
    node.start_client_in_pane(&a, &b, "CLIENT_PANE_1");
    node.wait_log("the refused client input", |events| {
        events.iter().any(|event| {
            event["path"] == "client"
                && event["route"] == "shell.pane_input"
                && event["decision"] == "deny"
                && event["class"] == "pane"
        })
    });

    send_client_shell_text(&mut outside, &b, "CLIENT_CONTROL_1").unwrap();
    node.assert_never_shows(&b, "CLIENT_CONTROL_1", &["CLIENT_PANE_1"]);
    node.finish_client_in_pane();
}

#[test]
fn input_log_shows_a_refused_and_an_allowed_input_without_content() {
    let (node, a, b) = setup("");
    node.protect(&b, "outside");

    node.herdr_in_pane(&a, &["pane", "send-text", &b, "LOG_REFUSED_SECRET_1"])
        .assert_refused_as_pane("pane send-text");
    node.cli(&["pane", "send-text", &b, "LOG_ALLOWED_SECRET_1"])
        .assert_ok("pane send-text from outside");
    node.wait_text(&b, "LOG_ALLOWED_SECRET_1");

    let events = node.wait_log("a refusal and an allowed input", |events| {
        let has = |decision: &str, class: &str| {
            events.iter().any(|event| {
                event["path"] == "api"
                    && event["route"] == "pane.send_text"
                    && event["decision"] == decision
                    && event["class"] == class
            })
        };
        has("deny", "pane") && has("allow", "outside")
    });
    let refused = events
        .iter()
        .find(|event| event["decision"] == "deny" && event["route"] == "pane.send_text")
        .expect("the refused entry");
    assert_eq!(
        refused["pane"],
        b.as_str(),
        "the entry names the target pane"
    );
    assert!(
        refused["principal"]
            .as_str()
            .is_some_and(|sender| !sender.is_empty()),
        "the entry names the sending pane: {refused}"
    );
    assert!(refused["bytes"].is_number(), "{refused}");
    let allowed = events
        .iter()
        .find(|event| event["decision"] == "allow" && event["route"] == "pane.send_text")
        .expect("the allowed entry");
    assert_eq!(allowed["bytes"], "LOG_ALLOWED_SECRET_1".len());

    // No input content, in the JSON or in the text form.
    let json = node.cli(&["access", "log", "--json"]);
    let text = node.cli(&["access", "log"]);
    for shown in [&json.stdout, &text.stdout] {
        assert!(
            !shown.contains("SECRET"),
            "the log must not show what was typed:\n{shown}"
        );
    }
    assert!(
        text.stdout.contains("deny") && text.stdout.contains("allow"),
        "{}",
        text.stdout
    );
}

#[test]
fn human_only_refuses_the_api_and_takes_an_outside_client() {
    let (node, a, b) = setup("");
    // A guard, not the fix: the pending-mark rule is what closes the race with the process read.
    // This makes the test set the mode on a pane the server already sees as an agent.
    poll("pane b to report its agent", || {
        let got = node.try_api("agent.get", json!({"target": b})).ok()?;
        (got["result"]["agent"]["agent"] == "pi").then_some(())
    });
    node.cli(&["access", "pane", "set", &b, "--input-mode", "human-only"])
        .assert_ok("access pane set --input-mode");
    let got = node.cli(&["access", "pane", "get", &b]);
    let fields: Vec<&str> = got.stdout.trim_end().split('\t').collect();
    assert_eq!(&fields[3..], ["human-only", "explicit"], "{}", got.stdout);

    let human_only =
        format!("accepts only typed input; input sent through the herdr API is refused. {TAIL}");
    let refused = node.cli(&["pane", "send-text", &b, "HUMAN_API_1"]);
    refused.assert_denied("pane send-text from outside");
    assert!(
        refused.stderr.contains(&human_only),
        "{}",
        refused.describe()
    );
    let refused = node.herdr_in_pane(&a, &["agent", "prompt", &b, "HUMAN_API_2"]);
    refused.assert_denied("agent prompt from a pane");
    assert!(refused.text.contains(&human_only), "{}", refused.text);

    // A person at a terminal outside every pane still types into it.
    let mut outside = node.outside_shell_client();
    send_client_shell_text(&mut outside, &b, "HUMAN_CLIENT_OUTSIDE_1").unwrap();
    node.wait_text(&b, "HUMAN_CLIENT_OUTSIDE_1");

    // A client inside a pane does not.
    node.start_client_in_pane(&a, &b, "HUMAN_CLIENT_PANE_1");
    node.wait_log("the refused client input", |events| {
        events.iter().any(|event| {
            event["path"] == "client"
                && event["route"] == "shell.pane_input"
                && event["decision"] == "deny"
                && event["class"] == "pane"
        })
    });
    send_client_shell_text(&mut outside, &b, "HUMAN_CLIENT_CONTROL_1").unwrap();
    node.assert_never_shows(
        &b,
        "HUMAN_CLIENT_CONTROL_1",
        &["HUMAN_API_1", "HUMAN_API_2", "HUMAN_CLIENT_PANE_1"],
    );
    node.finish_client_in_pane();
}

#[test]
fn human_only_survives_a_server_restart() {
    let (mut node, _a, b) = setup("");
    node.api_ok(
        "pane.rename",
        json!({"pane_id": b, "label": "human-only-b"}),
    );
    node.cli(&["access", "pane", "set", &b, "--input-mode", "human-only"])
        .assert_ok("access pane set --input-mode");

    // The session must be on disk before the server stops, or there is nothing to restore, and
    // the saved mark must be complete: a pending mark is not restored, by design.
    let session = node.config_home.join(app_dir_name()).join("session.json");
    poll(
        "the session file to hold the pane's mode and a complete mark",
        || {
            fs::read_to_string(&session)
                .ok()
                .filter(|saved| {
                    saved.contains("human-only")
                        && saved.contains("input_mode_mark_session")
                        && !saved.contains("pending")
                })
                .map(|_| ())
        },
    );

    node.restart_server();

    // The restored pane, found by its label: ids and terminals may be renumbered.
    let restored = poll("the restored pane", || {
        let listed = node.try_api("pane.list", json!({})).ok()?;
        listed["result"]["panes"]
            .as_array()?
            .iter()
            .find(|pane| pane["label"] == "human-only-b")
            .and_then(|pane| pane["pane_id"].as_str().map(str::to_string))
    });

    let got = node.cli(&["access", "pane", "get", &restored]);
    let fields: Vec<&str> = got.stdout.trim_end().split('\t').collect();
    assert_eq!(&fields[3..], ["human-only", "explicit"], "{}", got.stdout);

    let refused = node.cli(&["pane", "send-text", &restored, "RESTART_HUMAN_1"]);
    refused.assert_denied("pane send-text into a restored human-only pane");
    assert!(
        refused
            .stderr
            .contains("accepts only typed input; input sent through the herdr API is refused."),
        "{}",
        refused.describe()
    );
}

#[test]
fn live_handoff_cli_is_refused_from_a_pane_when_access_is_protected() {
    let (node, a, b) = setup("");
    node.cli(&["access", "pane", "set", &b, "--input-mode", "human-only"])
        .assert_ok("access pane set --input-mode");
    let before = node.try_api("ping", json!({})).expect("a ping");
    let refused = node.herdr_in_pane(
        &a,
        &[
            "server",
            "live-handoff",
            "--import-exe",
            "/nonexistent/herdr-handoff-target",
        ],
    );
    refused.assert_denied("server live-handoff");
    // Nothing changed: the same server still answers, and the policy is still there.
    let after = node.try_api("ping", json!({})).expect("a ping");
    assert_eq!(before["result"], after["result"]);
    let got = node.cli(&["access", "pane", "get", &b]);
    let fields: Vec<&str> = got.stdout.trim_end().split('\t').collect();
    assert_eq!(&fields[3..], ["human-only", "explicit"], "{}", got.stdout);
}

#[test]
fn live_handoff_is_refused_from_a_pane_right_after_a_restart() {
    let (mut node, a, b) = setup("");
    node.api_ok("pane.rename", json!({"pane_id": a, "label": "caller-a"}));
    node.cli(&["access", "pane", "set", &b, "--input-mode", "human-only"])
        .assert_ok("access pane set --input-mode");
    let session = node.config_home.join(app_dir_name()).join("session.json");
    poll("the session file to hold the pane's input mode", || {
        fs::read_to_string(&session)
            .ok()
            .filter(|saved| saved.contains("human-only") && saved.contains("caller-a"))
            .map(|_| ())
    });

    node.restart_server();

    let restored = poll("the restored caller pane", || {
        let listed = node.try_api("pane.list", json!({})).ok()?;
        listed["result"]["panes"]
            .as_array()?
            .iter()
            .find(|pane| pane["label"] == "caller-a")
            .and_then(|pane| pane["pane_id"].as_str().map(str::to_string))
    });
    let refused = node.herdr_in_pane(
        &restored,
        &[
            "server",
            "live-handoff",
            "--import-exe",
            "/nonexistent/herdr-handoff-target",
        ],
    );
    refused.assert_denied("server live-handoff right after a restart");
    node.try_api("ping", json!({}))
        .expect("the same server still answers");
}

/// Closing the only protected pane does not open the handoff to a pane. The nonexistent
/// import executable makes a request that is let through fail as `handoff_failed`.
#[test]
fn live_handoff_stays_refused_from_a_pane_after_every_protected_pane_is_closed() {
    let (node, _a, b) = setup("");
    node.cli(&["access", "pane", "set", &b, "--input-mode", "human-only"])
        .assert_ok("access pane set --input-mode");
    node.cli(&["pane", "close", &b])
        .assert_ok("pane close from outside");
    let a = poll("only the caller pane to remain", || {
        let listed = node.try_api("pane.list", json!({})).ok()?;
        let panes = listed["result"]["panes"].as_array()?;
        if panes.len() == 1 {
            panes[0]["pane_id"].as_str().map(str::to_string)
        } else {
            None
        }
    });
    let refused = node.herdr_in_pane(
        &a,
        &[
            "server",
            "live-handoff",
            "--import-exe",
            "/nonexistent/herdr-handoff-target",
        ],
    );
    refused.assert_denied_from_pane("server live-handoff after the protected pane closed");
    node.try_api("ping", json!({}))
        .expect("the same server still answers");
}

/// The same, across a restart of the server on the same state.
#[test]
fn live_handoff_stays_refused_from_a_pane_after_the_close_and_a_restart() {
    let (mut node, a, b) = setup("");
    node.api_ok("pane.rename", json!({"pane_id": a, "label": "caller-a"}));
    node.cli(&["access", "pane", "set", &b, "--input-mode", "human-only"])
        .assert_ok("access pane set --input-mode");
    node.cli(&["pane", "close", &b])
        .assert_ok("pane close from outside");
    let session = node.config_home.join(app_dir_name()).join("session.json");
    poll(
        "the session file to hold the latch and no protected pane",
        || {
            fs::read_to_string(&session)
                .ok()
                .filter(|saved| {
                    saved.contains("access_seen")
                        && saved.contains("caller-a")
                        && !saved.contains("human-only")
                })
                .map(|_| ())
        },
    );

    node.restart_server();

    let restored = poll("the restored caller pane", || {
        let listed = node.try_api("pane.list", json!({})).ok()?;
        listed["result"]["panes"]
            .as_array()?
            .iter()
            .find(|pane| pane["label"] == "caller-a")
            .and_then(|pane| pane["pane_id"].as_str().map(str::to_string))
    });
    let refused = node.herdr_in_pane(
        &restored,
        &[
            "server",
            "live-handoff",
            "--import-exe",
            "/nonexistent/herdr-handoff-target",
        ],
    );
    refused.assert_denied_from_pane("server live-handoff after the close and a restart");
    node.try_api("ping", json!({}))
        .expect("the same server still answers");
}

/// A real handoff to the test's own binary, asked from outside every pane. The new server
/// is not this test's child, so the test stops it through the API.
#[cfg(unix)]
#[test]
fn access_state_survives_a_live_handoff() {
    let (node, a, b) = setup("");
    // The replacement server is not this test's child: stop it on every exit path.
    struct StopReplacement<'a>(&'a Node);
    impl Drop for StopReplacement<'_> {
        fn drop(&mut self) {
            let _ = self.0.try_api("server.stop", json!({}));
        }
    }
    let _stop = StopReplacement(&node);
    node.api_ok("pane.rename", json!({"pane_id": a, "label": "caller-a"}));
    node.api_ok("pane.rename", json!({"pane_id": b, "label": "protected-b"}));
    node.cli(&["access", "pane", "set", &b, "--input-mode", "human-only"])
        .assert_ok("access pane set --input-mode");
    let session = node.config_home.join(app_dir_name()).join("session.json");
    poll("the session file to hold the pane's input mode", || {
        fs::read_to_string(&session)
            .ok()
            .filter(|saved| saved.contains("human-only") && saved.contains("protected-b"))
            .map(|_| ())
    });
    // One refusal from outside before the handoff, to find again in the log afterwards.
    node.cli(&["pane", "send-text", &b, "HANDOFF_BEFORE_1"])
        .assert_denied("pane send-text before the handoff");
    node.wait_log("the refusal before the handoff", |events| {
        events
            .iter()
            .any(|e| e["route"] == "pane.send_text" && e["decision"] == "deny")
    });

    node.cli(&[
        "server",
        "live-handoff",
        "--import-exe",
        env!("CARGO_BIN_EXE_herdr"),
    ])
    .assert_ok("server live-handoff from outside");

    let find = |label: &str| {
        poll(&format!("the pane {label} on the new server"), || {
            let listed = node.try_api("pane.list", json!({})).ok()?;
            listed["result"]["panes"]
                .as_array()?
                .iter()
                .find(|pane| pane["label"] == label)
                .and_then(|pane| pane["pane_id"].as_str().map(str::to_string))
        })
    };
    let (a, b) = (find("caller-a"), find("protected-b"));

    let got = node.cli(&["access", "pane", "get", &b]);
    let fields: Vec<&str> = got.stdout.trim_end().split('\t').collect();
    // The mode is what must survive; whether it still counts as set on purpose is not asserted.
    assert_eq!(fields[3], "human-only", "{}", got.stdout);
    node.cli(&["pane", "send-text", &b, "HANDOFF_AFTER_1"])
        .assert_denied("pane send-text after the handoff");
    node.herdr_in_pane(
        &a,
        &[
            "server",
            "live-handoff",
            "--import-exe",
            env!("CARGO_BIN_EXE_herdr"),
        ],
    )
    .assert_denied("server live-handoff from a pane after a handoff");
    // The log still holds the entry from before the handoff and records a new one: this
    // refusal comes from a pane, which no earlier entry does.
    node.herdr_in_pane(&a, &["pane", "send-text", &b, "HANDOFF_AFTER_2"])
        .assert_denied("pane send-text from a pane after the handoff");
    node.wait_log("entries from before and after the handoff", |events| {
        let denied = |class: &str| {
            events.iter().any(|e| {
                e["route"] == "pane.send_text" && e["decision"] == "deny" && e["class"] == class
            })
        };
        denied("outside") && denied("pane")
    });
}

#[test]
fn bridge_pane_refuses_human_only() {
    let (node, _a, b) = setup(BRIDGE_CONFIG);
    // `b` is the bridge: the agent this node's config names as `bridge_agent`.
    node.cli(&["agent", "rename", &b, "relay"])
        .assert_ok("agent rename to the bridge's name");

    let refused = node.cli(&["access", "pane", "set", &b, "--input-mode", "human-only"]);
    assert_ne!(refused.code, 0, "{}", refused.describe());
    assert!(
        refused.stderr.contains("access_bridge_input_mode"),
        "{}",
        refused.describe()
    );
    assert!(
        refused
            .stderr
            .contains("input mode must stay \"any\" on the bridge (relay)"),
        "{}",
        refused.describe()
    );
    let got = node.cli(&["access", "pane", "get", &b]);
    let fields: Vec<&str> = got.stdout.trim_end().split('\t').collect();
    assert_eq!(
        &fields[3..],
        ["any", "default"],
        "the bridge keeps mode any: {}",
        got.stdout
    );

    // The guard is about the bridge only: another pane takes the mode.
    let other = node.api_ok(
        "workspace.create",
        json!({"cwd": node.base.display().to_string(), "focus": false}),
    );
    let other = other["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    node.cli(&[
        "access",
        "pane",
        "set",
        &other,
        "--input-mode",
        "human-only",
    ])
    .assert_ok("access pane set --input-mode on a pane that is not the bridge");
}

// ---------------------------------------------------------------------------
// Delta R1: a login through an ssh server
// ---------------------------------------------------------------------------

#[test]
fn ssh_server_lineage_denied_on_open_pane_when_bridge_only() {
    if harness_under_ssh() {
        return;
    }
    let (node, a, _b) = setup(BRIDGE_CONFIG);
    // `a` holds no policy: it is open.
    let open = node.cli(&["access", "pane", "get", &a]);
    let fields: Vec<&str> = open.stdout.trim_end().split('\t').collect();
    assert_eq!(&fields[1..3], ["-", "any"], "{}", open.stdout);

    // The same command from outside is fine, so the test's own lineage is not remote.
    node.cli(&["pane", "send-text", &a, "SSH_OUTSIDE_1"])
        .assert_ok("pane send-text from outside under bridge-only");
    node.wait_text(&a, "SSH_OUTSIDE_1");

    let refused = node.cli_via_sshd(&["pane", "send-text", &a, "SSH_REFUSED_1"], &[]);
    refused.assert_denied("pane send-text through an ssh server");
    assert!(
        refused
            .stderr
            .contains("does not accept input from another host"),
        "{}",
        refused.describe()
    );
    node.cli(&["pane", "send-text", &a, "SSH_CONTROL_1"])
        .assert_ok("pane send-text from outside");
    node.assert_never_shows(&a, "SSH_CONTROL_1", &["SSH_REFUSED_1"]);
}

#[test]
fn forced_command_token_reaches_bridge_only() {
    if harness_under_ssh() {
        return;
    }
    let (node, a, b) = setup(BRIDGE_CONFIG);
    // `b` is the bridge; it accepts its token holder and nobody else.
    node.cli(&["agent", "rename", &b, "relay"])
        .assert_ok("agent rename to the bridge's name");
    node.protect(&b, "caller:ops");
    let (token_file, binding) = node.add_caller("ops", &b);

    // A forced command that carries the token (by environment, as sshd sets it) gets through.
    node.cli_via_sshd(
        &["pane", "send-text", &b, "FORCED_TOKEN_1"],
        &[("HERDR_CALLER_TOKEN_FILE", &token_file)],
    )
    .assert_ok("pane send-text through an ssh server with the token");
    node.wait_text(&b, "FORCED_TOKEN_1");
    // So does the flag, addressing the bridge by its binding id.
    node.cli_via_sshd(
        &[
            "--caller-token-file",
            &token_file,
            "agent",
            "prompt",
            &binding,
            "FORCED_TOKEN_2",
        ],
        &[],
    )
    .assert_ok("agent prompt through an ssh server with the token");
    node.wait_text(&b, "FORCED_TOKEN_2");

    // Without the token it is another host. A token for another pane does not help either: the
    // open pane `a` has no policy that could admit it, so only the own-binding check of the
    // remote rule refuses it (the token confinement rule itself is checked by the watch test).
    let no_token = node.cli_via_sshd(&["pane", "send-text", &b, "FORCED_NOTOKEN_1"], &[]);
    no_token.assert_denied("pane send-text through an ssh server without the token");
    assert!(
        no_token
            .stderr
            .contains("does not accept input from another host"),
        "{}",
        no_token.describe()
    );
    node.cli_via_sshd(
        &["pane", "send-text", &a, "FORCED_WRONGPANE_1"],
        &[("HERDR_CALLER_TOKEN_FILE", &token_file)],
    )
    .assert_denied("pane send-text through an ssh server, with the token, to another pane");

    node.cli(&[
        "--caller-token-file",
        &token_file,
        "pane",
        "send-text",
        &b,
        "FORCED_CONTROL_1",
    ])
    .assert_ok("pane send-text with the token from outside");
    node.assert_never_shows(&b, "FORCED_CONTROL_1", &["FORCED_NOTOKEN_1"]);
    node.cli(&["pane", "send-text", &a, "FORCED_CONTROL_2"])
        .assert_ok("pane send-text from outside");
    node.assert_never_shows(&a, "FORCED_CONTROL_2", &["FORCED_WRONGPANE_1"]);
}
