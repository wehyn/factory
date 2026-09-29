use crate::mcp::FactoryMcpConfig;
use crate::{Event, EventKind, Ledger, RedactedOutput, SessionId, SessionProcessState};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_JSON_LINE_BYTES: usize = 1024 * 1024;
const MAX_PROMPT_CHARS: usize = 16_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

type RpcResult = std::result::Result<Value, String>;

struct ProtocolState {
    thread_id: Option<String>,
    active_turn: Option<String>,
    terminal_turns: HashSet<String>,
    startup_failure_recorded: bool,
}

fn should_record_thread_started(state: &ProtocolState, thread_id: &str) -> bool {
    !state.startup_failure_recorded
        && state.terminal_turns.is_empty()
        && state.thread_id.as_deref() != Some(thread_id)
}

fn should_ignore_late_turn_notification(state: &ProtocolState) -> bool {
    state.startup_failure_recorded || !state.terminal_turns.is_empty()
}

struct RunnerShared {
    ledger: Arc<Ledger>,
    session_id: SessionId,
    state: Mutex<ProtocolState>,
    pending: Mutex<HashMap<u64, Sender<RpcResult>>>,
    writer: Mutex<ChildStdin>,
    next_request_id: AtomicU64,
    protocol_error: Mutex<Option<String>>,
    process_group_id: i32,
}

pub struct CodexRunner {
    child: Child,
    shared: Arc<RunnerShared>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    exited: bool,
}

impl CodexRunner {
    pub fn start(cwd: &Path, ledger: Arc<Ledger>, session_id: SessionId) -> Result<Self> {
        let executable = match resolve_codex_binary() {
            Ok(executable) => executable,
            Err(error) => {
                record_start_failure(&ledger, session_id, &error.to_string())?;
                return Err(error);
            }
        };
        let mut command = Command::new(executable);
        command.args(["app-server", "--stdio"]);
        Self::start_with_command(command, cwd, ledger, session_id)
    }

    /// Starts a manager/worker App Server with this task's temporary Factory MCP overrides.
    /// The overrides are CLI arguments and leave the user's persistent Codex config untouched.
    pub fn start_with_factory_mcp(
        cwd: &Path,
        ledger: Arc<Ledger>,
        session_id: SessionId,
        config: FactoryMcpConfig,
    ) -> Result<Self> {
        let executable = match resolve_codex_binary() {
            Ok(executable) => executable,
            Err(error) => {
                record_start_failure(&ledger, session_id, &error.to_string())?;
                return Err(error);
            }
        };
        let mut command = Command::new(executable);
        command.args(["app-server", "--stdio"]);
        for override_value in config.config_overrides()? {
            command.arg("--config").arg(override_value);
        }
        Self::start_with_command(command, cwd, ledger, session_id)
    }

    /// Starts a runner with an injected executable for protocol integration tests.
    #[doc(hidden)]
    pub fn start_with_command(
        command: Command,
        cwd: &Path,
        ledger: Arc<Ledger>,
        session_id: SessionId,
    ) -> Result<Self> {
        let result = Self::start_with_command_inner(command, cwd, Arc::clone(&ledger), session_id);
        if let Err(error) = &result {
            record_start_failure(&ledger, session_id, &error.to_string())?;
        }
        result
    }

    fn start_with_command_inner(
        mut command: Command,
        cwd: &Path,
        ledger: Arc<Ledger>,
        session_id: SessionId,
    ) -> Result<Self> {
        let cwd = cwd
            .canonicalize()
            .with_context(|| format!("resolving disposable repository {}", cwd.display()))?;
        if !cwd.join(".git").exists() {
            bail!("Codex session directory must be an initialized Git repository");
        }

        command
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().context("starting Codex App Server")?;
        let process_group_id = child.id() as i32;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Codex App Server stdin was not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Codex App Server stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Codex App Server stderr was not piped"))?;

        let shared = Arc::new(RunnerShared {
            ledger,
            session_id,
            state: Mutex::new(ProtocolState {
                thread_id: None,
                active_turn: None,
                terminal_turns: HashSet::new(),
                startup_failure_recorded: false,
            }),
            pending: Mutex::new(HashMap::new()),
            writer: Mutex::new(stdin),
            next_request_id: AtomicU64::new(1),
            protocol_error: Mutex::new(None),
            process_group_id,
        });

        let reader_shared = Arc::clone(&shared);
        let stdout_thread = thread::Builder::new()
            .name("codex-app-server-stdout".to_owned())
            .spawn(move || read_server_output(stdout, reader_shared))
            .context("starting Codex App Server stdout reader")?;
        let stderr_thread = thread::Builder::new()
            .name("codex-app-server-stderr".to_owned())
            .spawn(move || {
                // Drain stderr to prevent a child pipe from filling. The stream is not retained.
                let _ = io::copy(&mut io::BufReader::new(stderr), &mut io::sink());
            })
            .context("starting Codex App Server stderr reader")?;

        let mut runner = Self {
            child,
            shared,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            exited: false,
        };

        if let Err(error) = runner.initialize(&cwd) {
            let safe = RedactedOutput::new(error.to_string());
            let _ = runner.shared.record_failure(safe.as_str());
            return Err(error);
        }

        Ok(runner)
    }

    pub fn start_turn(&mut self, input: &str) -> Result<()> {
        self.start_turn_with_sandbox_policy(input, None)
    }

    /// Starts a turn with write access restricted to the given canonical worker worktree.
    /// Network access remains disabled for this turn and later turns on the same thread.
    pub fn start_turn_in_worktree(&mut self, input: &str, worktree: &Path) -> Result<()> {
        let writable_root = worktree
            .canonicalize()
            .with_context(|| format!("resolving worker worktree {}", worktree.display()))?;
        if !writable_root.join(".git").exists() {
            bail!("worker writable root must be an initialized Git worktree");
        }
        self.start_turn_with_sandbox_policy(
            input,
            Some(json!({
                "type": "workspaceWrite",
                "writableRoots": [writable_root.to_string_lossy()],
                "networkAccess": false,
            })),
        )
    }

    fn start_turn_with_sandbox_policy(
        &mut self,
        input: &str,
        sandbox_policy: Option<Value>,
    ) -> Result<()> {
        let input = input.trim();
        if input.is_empty() {
            bail!("turn prompt must not be empty");
        }
        if input.chars().count() > MAX_PROMPT_CHARS {
            bail!("turn prompt exceeds the 16,000 character limit");
        }
        let thread_id = self
            .shared
            .state
            .lock()
            .map_err(|_| anyhow!("Codex runner state lock was poisoned"))?
            .thread_id
            .clone()
            .ok_or_else(|| anyhow!("Codex App Server thread has not started"))?;

        let mut params = json!({
            "threadId": thread_id,
            "input": [{"type": "text", "text": input}],
        });
        if let Some(policy) = sandbox_policy {
            params["sandboxPolicy"] = policy;
        }
        let response = self.request("turn/start", params);
        let result = match response {
            Ok(result) => result,
            Err(error) => {
                let safe = RedactedOutput::new(error.to_string());
                let _ = self.shared.record_failure(safe.as_str());
                return Err(error);
            }
        };
        let turn_id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Codex App Server turn/start result lacked turn.id"));
        let turn_id = match turn_id {
            Ok(turn_id) => turn_id,
            Err(error) => {
                self.shared.record_failure(&error.to_string())?;
                return Err(error);
            }
        };
        self.shared.record_turn_started(turn_id)?;
        Ok(())
    }

    pub fn interrupt(&mut self) -> Result<()> {
        let (thread_id, turn_id) = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| anyhow!("Codex runner state lock was poisoned"))?;
            let turn_id = state
                .active_turn
                .clone()
                .ok_or_else(|| anyhow!("there is no active Codex turn to interrupt"))?;
            let thread_id = state
                .thread_id
                .clone()
                .ok_or_else(|| anyhow!("Codex App Server thread has not started"))?;
            (thread_id, turn_id)
        };

        self.request(
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
        )?;
        Ok(())
    }

    pub fn wait_for_exit(&mut self) -> Result<()> {
        if self.exited {
            return Ok(());
        }
        while self
            .stdout_thread
            .as_ref()
            .is_some_and(|reader| !reader.is_finished())
        {
            if self
                .shared
                .protocol_error
                .lock()
                .map(|error| error.is_some())
                .unwrap_or(true)
            {
                self.shared.terminate_process_group();
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        self.join_reader_threads()?;
        self.shared.terminate_process_group();
        let status = self.child.wait().context("waiting for Codex App Server")?;
        self.exited = true;
        self.raise_protocol_error()?;
        if !status.success() {
            return Err(anyhow!("Codex App Server exited with status {status}"));
        }
        Ok(())
    }

    /// Nonblocking process check for a resident scheduler. A completed App Server turn does not
    /// imply that the server process has exited; this reports only the child process boundary.
    pub fn try_wait_for_exit(&mut self) -> Result<Option<bool>> {
        if self.exited {
            return Ok(Some(true));
        }
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        self.shared.terminate_process_group();
        self.join_reader_threads()?;
        self.exited = true;
        self.raise_protocol_error()?;
        if !status.success() {
            bail!("Codex App Server exited with status {status}");
        }
        Ok(Some(true))
    }

    pub fn shutdown(&mut self) -> Result<()> {
        if self.exited {
            return Ok(());
        }
        self.shared.record_interrupted("Codex session shut down")?;
        self.shared.terminate_process_group();
        if self.child.try_wait()?.is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        self.join_reader_threads()?;
        self.exited = true;
        self.raise_protocol_error()
    }

    fn initialize(&mut self, cwd: &Path) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "agentic-factory",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {"experimentalApi": false},
            }),
        )?;
        self.notify("initialized", None)?;

        let result = self.request(
            "thread/start",
            json!({
                "cwd": cwd.to_string_lossy(),
                "sandbox": "read-only",
                "approvalPolicy": "never",
                "ephemeral": true,
            }),
        )?;
        let thread_id = result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Codex App Server thread/start result lacked thread.id"))?;
        self.shared.record_thread_started(thread_id)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.shared.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.shared
            .pending
            .lock()
            .map_err(|_| anyhow!("Codex request map lock was poisoned"))?
            .insert(id, sender);

        let message = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
        if let Err(error) = self.write_message(&message) {
            self.shared
                .pending
                .lock()
                .map_err(|_| anyhow!("Codex request map lock was poisoned"))?
                .remove(&id);
            return Err(error);
        }

        match receiver.recv_timeout(REQUEST_TIMEOUT) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(message)) => Err(anyhow!("Codex App Server {method} failed: {message}")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.shared
                    .pending
                    .lock()
                    .map_err(|_| anyhow!("Codex request map lock was poisoned"))?
                    .remove(&id);
                Err(anyhow!("Codex App Server {method} timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!(
                "Codex App Server exited before replying to {method}"
            )),
        }
    }

    fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let mut message = json!({"jsonrpc":"2.0", "method":method});
        if let Some(params) = params {
            message["params"] = params;
        }
        self.write_message(&message)
    }

    fn write_message(&self, message: &Value) -> Result<()> {
        let mut writer = self
            .shared
            .writer
            .lock()
            .map_err(|_| anyhow!("Codex stdin lock was poisoned"))?;
        serde_json::to_writer(&mut *writer, message)?;
        writer
            .write_all(b"\n")
            .context("writing Codex App Server request")?;
        writer.flush().context("flushing Codex App Server request")
    }

    fn join_reader_threads(&mut self) -> Result<()> {
        if let Some(reader) = self.stdout_thread.take() {
            reader
                .join()
                .map_err(|_| anyhow!("Codex App Server stdout reader panicked"))?;
        }
        if let Some(reader) = self.stderr_thread.take() {
            reader
                .join()
                .map_err(|_| anyhow!("Codex App Server stderr reader panicked"))?;
        }
        Ok(())
    }

    fn raise_protocol_error(&self) -> Result<()> {
        let error = self
            .shared
            .protocol_error
            .lock()
            .map_err(|_| anyhow!("Codex error lock was poisoned"))?
            .clone();
        if let Some(error) = error {
            bail!("Codex App Server protocol failed: {error}");
        }
        Ok(())
    }
}

impl Drop for CodexRunner {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.shutdown();
        }
    }
}

fn record_start_failure(ledger: &Ledger, session_id: SessionId, message: &str) -> Result<()> {
    let snapshot = ledger.snapshot()?;
    let terminal = snapshot
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .is_some_and(|session| {
            matches!(
                session.process_state,
                SessionProcessState::Completed
                    | SessionProcessState::Interrupted
                    | SessionProcessState::Failed
            )
        });
    if !terminal {
        ledger.append(&Event::new(
            session_id,
            EventKind::SessionFailed {
                message: RedactedOutput::new(message),
            },
        ))?;
    }
    Ok(())
}

fn resolve_codex_binary() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|directory| directory.join("codex")));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.extend([
            home.join(".local/bin/codex"),
            home.join(".volta/bin/codex"),
            home.join(".asdf/shims/codex"),
        ]);
        let nvm = home.join(".nvm/versions/node");
        if let Ok(entries) = fs::read_dir(nvm) {
            let mut node_bins = entries
                .flatten()
                .map(|entry| entry.path().join("bin/codex"))
                .collect::<Vec<_>>();
            node_bins.sort_by(|left, right| right.cmp(left));
            candidates.extend(node_bins);
        }
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
        PathBuf::from("/usr/bin/codex"),
    ]);

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| anyhow!("Codex CLI was not found on PATH or in common macOS install paths"))
}

#[cfg(test)]
mod tests {
    use super::{
        should_ignore_late_turn_notification, should_record_thread_started, ProtocolState,
    };
    use std::collections::HashSet;

    #[test]
    fn ignores_late_thread_start_after_startup_failure() {
        let state = ProtocolState {
            thread_id: None,
            active_turn: None,
            terminal_turns: HashSet::new(),
            startup_failure_recorded: true,
        };

        assert!(!should_record_thread_started(&state, "late-thread"));
    }

    #[test]
    fn ignores_late_turn_notifications_after_startup_failure() {
        let state = ProtocolState {
            thread_id: Some("thread".to_owned()),
            active_turn: None,
            terminal_turns: HashSet::new(),
            startup_failure_recorded: true,
        };

        assert!(should_ignore_late_turn_notification(&state));
    }
}

fn read_server_output<R: Read>(mut reader: R, shared: Arc<RunnerShared>) {
    let mut input = [0_u8; 8192];
    let mut pending = Vec::new();
    loop {
        let bytes_read = match reader.read(&mut input) {
            Ok(0) => {
                if !pending.is_empty() {
                    if let Err(error) = handle_json_line(&pending, &shared) {
                        shared.record_transport_failure(&error.to_string());
                        return;
                    }
                }
                shared.record_transport_eof();
                return;
            }
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                shared.record_transport_failure(&format!("reading App Server output: {error}"));
                return;
            }
        };

        let mut start = 0;
        for (index, byte) in input[..bytes_read].iter().enumerate() {
            if *byte == b'\n' {
                pending.extend_from_slice(&input[start..index]);
                if pending.last() == Some(&b'\r') {
                    pending.pop();
                }
                if pending.len() > MAX_JSON_LINE_BYTES {
                    shared.record_transport_failure("App Server JSON line exceeded 1 MiB");
                    return;
                }
                if let Err(error) = handle_json_line(&pending, &shared) {
                    shared.record_transport_failure(&error.to_string());
                    return;
                }
                pending.clear();
                start = index + 1;
            }
        }
        pending.extend_from_slice(&input[start..bytes_read]);
        if pending.len() > MAX_JSON_LINE_BYTES {
            shared.record_transport_failure("App Server JSON line exceeded 1 MiB");
            return;
        }
    }
}

fn handle_json_line(line: &[u8], shared: &Arc<RunnerShared>) -> Result<()> {
    if line.is_empty() {
        bail!("App Server emitted an empty JSON line");
    }
    let message: Value = serde_json::from_slice(line).context("parsing App Server JSON line")?;
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            return respond_method_not_found(shared, id, method);
        }
        return handle_notification(
            shared,
            method,
            message.get("params").unwrap_or(&Value::Null),
        );
    }
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        return handle_response(shared, id, &message);
    }
    bail!("App Server JSON message had neither method nor request ID");
}

fn handle_response(shared: &Arc<RunnerShared>, id: u64, message: &Value) -> Result<()> {
    let sender = shared
        .pending
        .lock()
        .map_err(|_| anyhow!("Codex request map lock was poisoned"))?
        .remove(&id);
    if let Some(sender) = sender {
        let result = if let Some(error) = message.get("error") {
            let detail = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown JSON-RPC error");
            Err(RedactedOutput::new(detail).as_str().to_owned())
        } else {
            Ok(message.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = sender.send(result);
    }
    Ok(())
}

fn handle_notification(shared: &Arc<RunnerShared>, method: &str, params: &Value) -> Result<()> {
    match method {
        "thread/started" => {
            if let Some(thread_id) = params.pointer("/thread/id").and_then(Value::as_str) {
                shared.record_thread_started(thread_id)?;
            }
        }
        "turn/started" => {
            if let Some(turn_id) = params.pointer("/turn/id").and_then(Value::as_str) {
                shared.record_turn_started(turn_id)?;
            }
        }
        "item/completed" => {
            let item = params.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        shared.append(EventKind::Output(RedactedOutput::new(text)))?;
                    }
                }
            }
        }
        "turn/completed" => {
            let turn = params.get("turn").unwrap_or(&Value::Null);
            let turn_id = turn.get("id").and_then(Value::as_str);
            let status = turn.get("status").and_then(Value::as_str);
            if let (Some(turn_id), Some(status)) = (turn_id, status) {
                match status {
                    "completed" => shared.record_turn_completed(turn_id)?,
                    "interrupted" => shared.record_turn_interrupted(turn_id)?,
                    "failed" => {
                        let message = turn
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("Codex reported a failed turn");
                        shared.record_turn_failure(turn_id, message)?;
                    }
                    // An in-progress status in a completion notification is not success.
                    _ => {}
                }
            }
        }
        "error" => {
            if params.get("willRetry").and_then(Value::as_bool) != Some(true) {
                if let Some(message) = params.pointer("/error/message").and_then(Value::as_str) {
                    shared.record_failure(message)?;
                }
            }
        }
        _ => {} // Unknown notifications are protocol data, not display output or instructions.
    }
    Ok(())
}

fn respond_method_not_found(shared: &Arc<RunnerShared>, id: u64, method: &str) -> Result<()> {
    let message = json!({
        "jsonrpc":"2.0",
        "id":id,
        "error":{"code":-32601,"message":format!("Unsupported client method: {method}")},
    });
    let mut writer = shared
        .writer
        .lock()
        .map_err(|_| anyhow!("Codex stdin lock was poisoned"))?;
    serde_json::to_writer(&mut *writer, &message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

impl RunnerShared {
    fn append(&self, kind: EventKind) -> Result<()> {
        self.ledger.append(&Event::new(self.session_id, kind))?;
        Ok(())
    }

    fn record_thread_started(&self, thread_id: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Codex runner state lock was poisoned"))?;
        if !should_record_thread_started(&state, thread_id) {
            return Ok(());
        }
        self.append(EventKind::SessionStarted {
            thread_id: thread_id.to_owned(),
        })?;
        state.thread_id = Some(thread_id.to_owned());
        Ok(())
    }

    fn record_turn_started(&self, turn_id: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Codex runner state lock was poisoned"))?;
        if should_ignore_late_turn_notification(&state)
            || state.active_turn.as_deref() == Some(turn_id)
        {
            return Ok(());
        }
        if state.active_turn.is_some() {
            bail!("Codex App Server started overlapping turns");
        }
        self.append(EventKind::TurnStarted {
            turn_id: turn_id.to_owned(),
        })?;
        state.active_turn = Some(turn_id.to_owned());
        Ok(())
    }

    fn record_turn_completed(&self, turn_id: &str) -> Result<()> {
        self.record_turn_terminal(turn_id, EventKind::TurnCompleted)
    }

    fn record_turn_interrupted(&self, turn_id: &str) -> Result<()> {
        self.record_turn_terminal(turn_id, EventKind::SessionInterrupted)
    }

    fn record_turn_failure(&self, turn_id: &str, message: &str) -> Result<()> {
        self.record_turn_terminal(
            turn_id,
            EventKind::SessionFailed {
                message: RedactedOutput::new(message),
            },
        )
    }

    fn record_turn_terminal(&self, turn_id: &str, kind: EventKind) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Codex runner state lock was poisoned"))?;
        if should_ignore_late_turn_notification(&state) {
            return Ok(());
        }
        if !state.terminal_turns.insert(turn_id.to_owned()) {
            return Ok(());
        }
        self.append(kind)?;
        if state.active_turn.as_deref() == Some(turn_id) {
            state.active_turn = None;
        }
        Ok(())
    }

    fn record_failure(&self, message: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Codex runner state lock was poisoned"))?;
        if let Some(turn_id) = state.active_turn.clone() {
            if !state.terminal_turns.insert(turn_id.clone()) {
                return Ok(());
            }
            self.append(EventKind::SessionFailed {
                message: RedactedOutput::new(message),
            })?;
            state.active_turn = None;
            return Ok(());
        }
        if state.startup_failure_recorded || !state.terminal_turns.is_empty() {
            return Ok(());
        }
        self.append(EventKind::SessionFailed {
            message: RedactedOutput::new(message),
        })?;
        state.startup_failure_recorded = true;
        Ok(())
    }

    fn record_interrupted(&self, message: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Codex runner state lock was poisoned"))?;
        if let Some(turn_id) = state.active_turn.clone() {
            if !state.terminal_turns.insert(turn_id.clone()) {
                state.active_turn = None;
                return Ok(());
            }
            self.append(EventKind::SessionInterrupted)?;
            state.active_turn = None;
        } else if state.thread_id.is_none() && !state.startup_failure_recorded {
            self.append(EventKind::SessionFailed {
                message: RedactedOutput::new(message),
            })?;
            state.startup_failure_recorded = true;
        }
        Ok(())
    }

    fn record_transport_eof(&self) {
        let active = self
            .state
            .lock()
            .map(|state| state.active_turn.is_some() || state.thread_id.is_none())
            .unwrap_or(false);
        if active {
            if let Err(error) =
                self.record_failure("Codex App Server exited before the turn completed")
            {
                self.set_protocol_error(error.to_string());
            }
        }
        self.fail_pending("Codex App Server closed its output");
        self.terminate_process_group();
    }

    fn record_transport_failure(&self, message: &str) {
        let safe = RedactedOutput::new(message);
        if let Err(error) = self.record_failure(safe.as_str()) {
            self.set_protocol_error(error.to_string());
        }
        self.set_protocol_error(safe.as_str().to_owned());
        self.fail_pending(safe.as_str());
        self.terminate_process_group();
    }

    fn fail_pending(&self, message: &str) {
        let pending = self
            .pending
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending));
        if let Ok(pending) = pending {
            for (_, sender) in pending {
                let _ = sender.send(Err(message.to_owned()));
            }
        }
    }

    fn set_protocol_error(&self, message: String) {
        if let Ok(mut error) = self.protocol_error.lock() {
            if error.is_none() {
                *error = Some(message);
            }
        }
    }

    fn terminate_process_group(&self) {
        #[cfg(unix)]
        unsafe {
            // The npm Codex launcher can exit while its native App Server and MCP helpers
            // remain alive. All descendants inherit this runner-owned process group.
            let _ = libc::kill(-self.process_group_id, libc::SIGKILL);
        }
    }
}
