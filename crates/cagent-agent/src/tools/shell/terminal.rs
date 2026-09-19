use super::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TerminalId(uuid::Uuid);

impl TerminalId {
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl Default for TerminalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TerminalId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for TerminalId {
    type Err = uuid::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Running,
    Terminating,
    Exited,
    Killed,
    TimedOut,
    Orphaned,
}

impl TerminalStatus {
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Terminating)
    }

    #[must_use]
    pub const fn is_final(self) -> bool {
        !self.is_active()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalStarted {
    pub id: TerminalId,
    pub owner: crate::ConversationId,
    pub status: TerminalStatus,
    pub backend: BashBackendInfo,
}

/// Durable, frontend-neutral state for one supervised background terminal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalSnapshot {
    pub id: TerminalId,
    pub owner: crate::ConversationId,
    /// The delegated run that launched this terminal, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_run_id: Option<crate::AgentRunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_node_id: Option<crate::NodeId>,
    /// Whether the command was positively classified as read-safe before it
    /// started. `None` is retained for terminals created by older versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_safe: Option<bool>,
    pub command: String,
    pub status: TerminalStatus,
    pub created_at: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub exit_code: Option<i32>,
    pub output_base: u64,
    pub output_cursor: u64,
    pub output_bytes: u64,
    pub discarded_bytes: u64,
    pub truncated: bool,
    /// Retained terminal text including ANSI SGR styling. Frontends must parse
    /// this value as terminal data and must never write it directly to a TTY.
    #[serde(default)]
    pub ansi_output: String,
    /// Control-sequence-free output for compact display, tools, and the model.
    pub output: String,
}

impl TerminalSnapshot {
    /// Returns the compact terminal state suitable for a live transcript.
    ///
    /// The supervisor and store retain the complete ring buffer. Session
    /// snapshots deliberately expose only a bounded, head/tail preview; a
    /// frontend can obtain the complete plain and ANSI output with
    /// [`TerminalOutputRequest`].
    #[must_use]
    pub fn preview(&self) -> Self {
        let output = output_preview(&self.output);
        // A head/tail excerpt can split an escape sequence, so it is not safe
        // terminal data. Expanded terminal views fetch the retained ANSI text
        // explicitly instead.
        Self {
            id: self.id,
            owner: self.owner,
            owner_agent_run_id: self.owner_agent_run_id,
            tool_call_node_id: self.tool_call_node_id,
            read_safe: self.read_safe,
            command: self.command.clone(),
            status: self.status,
            created_at: self.created_at.clone(),
            started_at: self.started_at.clone(),
            completed_at: self.completed_at.clone(),
            exit_code: self.exit_code,
            output_base: self.output_base,
            output_cursor: self.output_cursor,
            output_bytes: self.output_bytes,
            discarded_bytes: self.discarded_bytes,
            truncated: self.truncated,
            ansi_output: output.clone(),
            output,
        }
    }
}

/// Produces the fixed-size transcript preview used for active and historical
/// terminal cards. It never clones the complete output into a frontend state.
#[must_use]
pub fn output_preview(output: &str) -> String {
    const EDGE_LINES: usize = 2;
    let head = output.lines().take(EDGE_LINES).collect::<Vec<_>>();
    let tail = output.lines().rev().take(EDGE_LINES).collect::<Vec<_>>();
    let lines = output.lines().count();
    if lines <= EDGE_LINES * 2 {
        return output.to_owned();
    }
    let mut preview = head.join("\n");
    preview.push_str(&format!("\n… {} lines omitted …\n", lines - EDGE_LINES * 2));
    preview.push_str(&tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
    preview
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // Started snapshots are emitted immediately on the supervisor channel.
pub(crate) enum TerminalSupervisorEvent {
    /// The process was spawned. Publish it immediately so transcript activity
    /// exists even when the command has not produced output yet.
    Started(TerminalSnapshot),
    /// Output arrived. The runtime snapshots this terminal at its coalesced
    /// checkpoint instead of materializing every intermediate buffer.
    Updated(TerminalId),
    /// The process exited and its output was drained. The runtime constructs
    /// the final snapshot so output-sized allocations do not remain attached
    /// to the short-lived PTY reader thread's allocator arena.
    Completed(TerminalId),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalOutputRequest {
    pub id: TerminalId,
    #[serde(default)]
    pub cursor: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalOutput {
    pub id: TerminalId,
    pub owner: crate::ConversationId,
    pub output: String,
    /// Retained terminal data including ANSI SGR styling. Consumers must parse
    /// this as terminal data and must never write it directly to a TTY.
    #[serde(default)]
    pub ansi_output: String,
    pub cursor: u64,
    pub lost_output: bool,
    pub status: TerminalStatus,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalWriteRequest {
    pub id: TerminalId,
    pub data: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalKillRequest {
    pub id: TerminalId,
    #[serde(default)]
    pub force: bool,
}

#[derive(Default)]
struct TerminalBuffer {
    bytes: VecDeque<u8>,
    base: u64,
    end: u64,
    update_pending: bool,
    status: Option<TerminalStatus>,
    requested_final_status: Option<TerminalStatus>,
    completed_at: Option<String>,
    child_exited: bool,
    output_drained: bool,
    exit_code: Option<i32>,
}

struct TerminalProcess {
    owner: crate::ConversationId,
    owner_agent_run_id: Option<crate::AgentRunId>,
    tool_call_node_id: Option<crate::NodeId>,
    read_safe: bool,
    detached: bool,
    command: String,
    created_at: String,
    buffer: Arc<Mutex<TerminalBuffer>>,
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    completed: CancellationToken,
    #[cfg(unix)]
    process_group: Option<i32>,
}

impl Drop for TerminalProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(process_group),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        #[cfg(windows)]
        if let Ok(child) = self.child.get_mut() {
            let _ = child.kill();
        }
    }
}

#[derive(Clone)]
pub struct TerminalSupervisor {
    backend: BashBackend,
    workspace: Arc<std::sync::RwLock<PathBuf>>,
    credential_variables: Vec<String>,
    protected_credential_variables: Vec<String>,
    cap: usize,
    default_timeout_seconds: u64,
    terminal_mode: crate::config::TerminalMode,
    termination_grace: Duration,
    terminals: Arc<Mutex<BTreeMap<TerminalId, Arc<TerminalProcess>>>>,
    events: tokio::sync::mpsc::Sender<TerminalSupervisorEvent>,
}

impl std::fmt::Debug for TerminalSupervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalSupervisor")
            .field("workspace", &self.workspace.read().ok().as_deref())
            .finish_non_exhaustive()
    }
}

struct SpawnedPty {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn std::io::Read + Send>,
    writer: Option<Box<dyn std::io::Write + Send>>,
    #[cfg(unix)]
    process_group: Option<i32>,
}

fn spawn_pty(
    backend: &BashBackend,
    credential_variables: &[String],
    protected_credential_variables: &[String],
    terminal_mode: crate::config::TerminalMode,
    cwd: &Path,
    request: &BashRequest,
) -> Result<SpawnedPty, ShellError> {
    let pair = portable_pty::native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| ShellError::Pty(error.to_string()))?;
    let child = pair
        .slave
        .spawn_command(pty_command(
            backend,
            credential_variables,
            protected_credential_variables,
            terminal_mode,
            cwd,
            request,
        ))
        .map_err(|error| ShellError::Pty(error.to_string()))?;
    drop(pair.slave);
    #[cfg(unix)]
    let process_group = pair
        .master
        .process_group_leader()
        .or_else(|| child.process_id().and_then(|pid| i32::try_from(pid).ok()));
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| ShellError::Pty(error.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| ShellError::Pty(error.to_string()))?;
    drop(pair.master);
    Ok(SpawnedPty {
        child,
        reader,
        writer: Some(writer),
        #[cfg(unix)]
        process_group,
    })
}

fn pty_command(
    backend: &BashBackend,
    credential_variables: &[String],
    protected_credential_variables: &[String],
    terminal_mode: crate::config::TerminalMode,
    cwd: &Path,
    request: &BashRequest,
) -> CommandBuilder {
    let mut command = CommandBuilder::new(&backend.executable);
    configure_backend_pty_command(&mut command, backend, cwd, &request.command);
    for variable in credential_variables {
        if !request.forward_env.contains(variable)
            || protected_credential_variables.contains(variable)
        {
            command.env_remove(variable);
        }
    }
    for variable in protected_credential_variables {
        command.env_remove(variable);
    }
    for (name, value) in &request.env {
        if !protected_credential_variables.contains(name) {
            command.env(name, value);
        }
    }
    command.env("PAGER", "cat");
    command.env("JJ_PAGER", "cat");
    command.env("GIT_PAGER", "cat");
    command.env("GH_PAGER", "cat");
    if terminal_mode == crate::config::TerminalMode::Dumb {
        command.env("TERM", "dumb");
    }
    command
}

impl TerminalSupervisor {
    #[must_use]
    pub(crate) fn new(
        executor: &ShellExecutor,
    ) -> (Self, tokio::sync::mpsc::Receiver<TerminalSupervisorEvent>) {
        let (events, receiver) = tokio::sync::mpsc::channel(1024);
        (
            Self {
                backend: executor.backend.clone(),
                workspace: Arc::new(std::sync::RwLock::new(executor.workspace.clone())),
                credential_variables: executor.credential_variables.clone(),
                protected_credential_variables: executor.protected_credential_variables.clone(),
                cap: executor.terminal_buffer_bytes(),
                default_timeout_seconds: executor.default_timeout_seconds(),
                terminal_mode: executor.terminal_mode(),
                termination_grace: Duration::from_secs(5),
                terminals: Arc::new(Mutex::new(BTreeMap::new())),
                events,
            },
            receiver,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_termination_grace(mut self, grace: Duration) -> Self {
        self.termination_grace = grace;
        self
    }

    /// Starts a background Bash request under a PTY.
    ///
    /// # Errors
    /// Returns an error for an invalid request or PTY/process setup failure.
    ///
    /// # Panics
    /// Panics if an internal terminal-state lock was poisoned by another panic.
    #[allow(clippy::too_many_lines)]
    pub fn start(
        &self,
        owner: crate::ConversationId,
        tool_call_node_id: crate::NodeId,
        request: &BashRequest,
    ) -> Result<TerminalStarted, ShellError> {
        self.start_classified(owner, tool_call_node_id, request, false)
    }

    pub fn start_classified(
        &self,
        owner: crate::ConversationId,
        tool_call_node_id: crate::NodeId,
        request: &BashRequest,
        read_safe: bool,
    ) -> Result<TerminalStarted, ShellError> {
        self.start_owned(owner, Some(tool_call_node_id), None, request, read_safe)
    }

    /// Starts a terminal and records the delegated run that owns it.
    pub fn start_owned(
        &self,
        owner: crate::ConversationId,
        tool_call_node_id: Option<crate::NodeId>,
        owner_agent_run_id: Option<crate::AgentRunId>,
        request: &BashRequest,
        read_safe: bool,
    ) -> Result<TerminalStarted, ShellError> {
        self.start_with_origin(
            owner,
            tool_call_node_id,
            owner_agent_run_id,
            request,
            read_safe,
            false,
            None,
        )
    }

    pub fn start_detached(
        &self,
        owner: crate::ConversationId,
        anchor_node_id: crate::NodeId,
        request: &BashRequest,
    ) -> Result<TerminalStarted, ShellError> {
        self.start_with_origin(
            owner,
            Some(anchor_node_id),
            None,
            request,
            false,
            true,
            Some(anchor_node_id),
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn start_with_origin(
        &self,
        owner: crate::ConversationId,
        tool_call_node_id: Option<crate::NodeId>,
        owner_agent_run_id: Option<crate::AgentRunId>,
        request: &BashRequest,
        read_safe: bool,
        detached: bool,
        _anchor_node_id: Option<crate::NodeId>,
    ) -> Result<TerminalStarted, ShellError> {
        validate_bash_request(request)?;
        let workspace = self
            .workspace
            .read()
            .expect("terminal workspace lock poisoned")
            .clone();
        let cwd = effective_cwd(&workspace, request.cwd.as_deref());
        let SpawnedPty {
            child,
            mut reader,
            writer: Some(writer),
            #[cfg(unix)]
            process_group,
        } = spawn_pty(
            &self.backend,
            &self.credential_variables,
            &self.protected_credential_variables,
            self.terminal_mode,
            &cwd,
            request,
        )?
        else {
            unreachable!("background PTYs always expose interactive input");
        };
        let id = TerminalId::new();
        let created_at = terminal_now();
        let buffer = Arc::new(Mutex::new(TerminalBuffer {
            status: Some(TerminalStatus::Running),
            ..TerminalBuffer::default()
        }));
        let process = Arc::new(TerminalProcess {
            owner,
            owner_agent_run_id,
            tool_call_node_id,
            read_safe,
            detached,
            command: request.command.clone(),
            created_at,
            buffer: buffer.clone(),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            completed: CancellationToken::new(),
            #[cfg(unix)]
            process_group,
        });
        self.terminals
            .lock()
            .expect("terminal map lock poisoned")
            .insert(id, process.clone());
        let cap = self.cap;
        let events = self.events.clone();
        let process_for_thread = process.clone();
        std::thread::Builder::new()
            .name(format!("cagent-terminal-{id}"))
            .spawn(move || {
                let mut chunk = [0_u8; 16 * 1024];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            let mut state = buffer.lock().expect("terminal buffer lock poisoned");
                            state.end = state
                                .end
                                .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
                            state.bytes.extend(&chunk[..count]);
                            while state.bytes.len() > cap {
                                state.bytes.pop_front();
                                state.base = state.base.saturating_add(1);
                            }
                            let notify = !state.update_pending;
                            state.update_pending = true;
                            drop(state);
                            if notify {
                                let _ = events.blocking_send(TerminalSupervisorEvent::Updated(id));
                            }
                        }
                    }
                }
                buffer
                    .lock()
                    .expect("terminal buffer lock poisoned")
                    .output_drained = true;
                loop {
                    refresh_terminal_status(&process_for_thread);
                    let finished = process_for_thread
                        .buffer
                        .lock()
                        .expect("terminal buffer lock poisoned")
                        .status
                        .is_some_and(TerminalStatus::is_final);
                    if finished {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                let _ = events.blocking_send(TerminalSupervisorEvent::Completed(id));
                process_for_thread.completed.cancel();
            })
            .map_err(ShellError::Io)?;
        let _ = self
            .events
            .try_send(TerminalSupervisorEvent::Started(terminal_snapshot(
                id, &process,
            )));
        if let Some(seconds) = Some(request.timeout.unwrap_or(self.default_timeout_seconds)) {
            let supervisor = self.clone();
            let completed = process.completed.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = completed.cancelled() => {}
                    () = tokio::time::sleep(Duration::from_secs(seconds)) => {
                        if supervisor
                            .snapshot(id)
                            .ok()
                            .is_some_and(|snapshot| snapshot.status.is_active())
                        {
                            let _ = supervisor.timeout(id);
                        }
                    }
                }
            });
        }
        Ok(TerminalStarted {
            id,
            owner,
            status: TerminalStatus::Running,
            backend: BashBackendInfo {
                kind: self.backend.kind,
                executable: self.backend.executable.clone(),
                host_cwd: cwd.clone(),
                shell_cwd: shell_path(self.backend.kind, &cwd),
            },
        })
    }

    pub(crate) fn set_workspace(&self, workspace: PathBuf) {
        if let Ok(mut current) = self.workspace.write() {
            *current = workspace;
        }
    }

    /// Waits for a terminal without taking ownership of its durable mailbox.
    /// Cancellation requests graceful termination and returns immediately;
    /// the supervisor finishes termination and persists the final state.
    pub async fn wait(
        &self,
        id: TerminalId,
        cancellation: &CancellationToken,
    ) -> Result<TerminalSnapshot, ShellError> {
        loop {
            let snapshot = self.snapshot(id)?;
            if snapshot.status.is_final() {
                return Ok(snapshot);
            }
            tokio::select! {
                () = cancellation.cancelled() => {
                    self.request_kill(&TerminalKillRequest { id, force: false })?;
                    return Err(ShellError::Cancelled);
                },
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    }

    pub(crate) fn snapshot(&self, id: TerminalId) -> Result<TerminalSnapshot, ShellError> {
        let process = self.process(id)?;
        refresh_terminal_status(&process);
        Ok(terminal_snapshot(id, &process))
    }

    /// Allows the next PTY read to enqueue another coalesced update event.
    pub(crate) fn acknowledge_update(&self, id: TerminalId) -> Result<(), ShellError> {
        let process = self.process(id)?;
        process
            .buffer
            .lock()
            .expect("terminal buffer lock poisoned")
            .update_pending = false;
        Ok(())
    }

    /// Evicts a completed process after its final snapshot has been persisted.
    /// Active terminals remain owned by the supervisor.
    pub(crate) fn remove_completed(&self, id: TerminalId) {
        let mut terminals = self.terminals.lock().expect("terminal map lock poisoned");
        let removable = terminals.get(&id).is_some_and(|process| {
            refresh_terminal_status(process);
            process
                .buffer
                .lock()
                .expect("terminal buffer lock poisoned")
                .status
                .is_some_and(TerminalStatus::is_final)
        });
        if removable {
            terminals.remove(&id);
        }
    }

    /// Reads terminal output after an optional monotonic cursor.
    ///
    /// # Errors
    /// Returns an error when the terminal ID is unknown.
    ///
    /// # Panics
    /// Panics if an internal terminal-state lock was poisoned by another panic.
    pub fn output(&self, request: &TerminalOutputRequest) -> Result<TerminalOutput, ShellError> {
        let process = self.process(request.id)?;
        refresh_terminal_status(&process);
        let state = process
            .buffer
            .lock()
            .expect("terminal buffer lock poisoned");
        let requested = request.cursor.unwrap_or(state.base);
        let start = requested.max(state.base).min(state.end);
        let skip = usize::try_from(start.saturating_sub(state.base)).unwrap_or(usize::MAX);
        let bytes = state.bytes.iter().skip(skip).copied().collect::<Vec<_>>();
        Ok(TerminalOutput {
            id: request.id,
            owner: process.owner,
            output: escape_process_output(&String::from_utf8_lossy(&bytes)),
            ansi_output: String::from_utf8_lossy(&bytes).into_owned(),
            cursor: state.end,
            lost_output: requested < state.base,
            status: state.status.unwrap_or(TerminalStatus::Orphaned),
            exit_code: state.exit_code,
            started_at: process.created_at.clone(),
            completed_at: state.completed_at.clone(),
        })
    }

    /// Writes text or terminal control sequences to a terminal.
    ///
    /// # Errors
    /// Returns an error for an unknown terminal, invalid input, or PTY I/O failure.
    ///
    /// # Panics
    /// Panics if an internal terminal-state lock was poisoned by another panic.
    pub fn write(&self, request: &TerminalWriteRequest) -> Result<usize, ShellError> {
        let process = self.process(request.id)?;
        let mut writer = process
            .writer
            .lock()
            .expect("terminal writer lock poisoned");
        writer
            .write_all(request.data.as_bytes())
            .and_then(|()| writer.flush())
            .map_err(ShellError::Io)?;
        Ok(request.data.len())
    }

    /// Gracefully or forcibly terminates a terminal process group.
    ///
    /// # Errors
    /// Returns an error for an unknown terminal or failed process signal.
    ///
    /// # Panics
    /// Panics if an internal terminal-state lock was poisoned by another panic.
    pub async fn kill(&self, request: &TerminalKillRequest) -> Result<TerminalStatus, ShellError> {
        self.request_kill(request)?;
        Ok(self.wait_for_final(request.id).await?.status)
    }

    /// Requests termination without waiting for the process to settle.
    pub(crate) fn request_kill(
        &self,
        request: &TerminalKillRequest,
    ) -> Result<TerminalStatus, ShellError> {
        self.request_termination(request.id, TerminalStatus::Killed, request.force)
    }

    fn timeout(&self, id: TerminalId) -> Result<(), ShellError> {
        self.request_termination(id, TerminalStatus::TimedOut, true)?;
        Ok(())
    }

    fn request_termination(
        &self,
        id: TerminalId,
        requested_final_status: TerminalStatus,
        force: bool,
    ) -> Result<TerminalStatus, ShellError> {
        let process = self.process(id)?;
        refresh_terminal_status(&process);
        let (first_request, start_grace) = {
            let mut state = process
                .buffer
                .lock()
                .expect("terminal buffer lock poisoned");
            let status = state.status.unwrap_or(TerminalStatus::Orphaned);
            if status.is_final() {
                return Ok(status);
            }
            let first_request = status == TerminalStatus::Running;
            if first_request {
                state.status = Some(TerminalStatus::Terminating);
                state.requested_final_status = Some(requested_final_status);
            }
            (first_request, first_request && !force)
        };

        if !first_request && !force {
            return Ok(TerminalStatus::Terminating);
        }

        if let Err(error) = signal_terminal(&process, force) {
            let mut state = process
                .buffer
                .lock()
                .expect("terminal buffer lock poisoned");
            if first_request {
                state.status = Some(TerminalStatus::Running);
                state.requested_final_status = None;
            }
            return Err(error);
        }
        let _ = self.events.try_send(TerminalSupervisorEvent::Updated(id));

        if start_grace {
            let supervisor = self.clone();
            let grace = self.termination_grace;
            std::thread::Builder::new()
                .name(format!("cagent-terminal-stop-{id}"))
                .spawn(move || {
                    std::thread::sleep(grace);
                    if supervisor
                        .snapshot(id)
                        .ok()
                        .is_some_and(|snapshot| snapshot.status.is_active())
                        && let Ok(process) = supervisor.process(id)
                    {
                        let _ = signal_terminal(&process, true);
                    }
                })
                .map_err(ShellError::Io)?;
        }
        Ok(TerminalStatus::Terminating)
    }

    async fn wait_for_final(&self, id: TerminalId) -> Result<TerminalSnapshot, ShellError> {
        loop {
            let snapshot = self.snapshot(id)?;
            if snapshot.status.is_final() {
                return Ok(snapshot);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[must_use]
    /// Returns `(running, inactive)` terminal counts.
    ///
    /// # Panics
    /// Panics if an internal terminal-state lock was poisoned by another panic.
    pub fn counts(&self) -> (usize, usize) {
        let terminals = self.terminals.lock().expect("terminal map lock poisoned");
        let running = terminals
            .values()
            .filter(|process| {
                refresh_terminal_status(process);
                process
                    .buffer
                    .lock()
                    .expect("terminal buffer lock poisoned")
                    .status
                    .is_some_and(TerminalStatus::is_active)
            })
            .count();
        (running, terminals.len().saturating_sub(running))
    }

    /// Terminates all supervised terminals.
    ///
    /// # Panics
    /// Panics if an internal terminal-state lock was poisoned by another panic.
    pub async fn terminate_all(&self, force: bool) {
        let ids = self
            .terminals
            .lock()
            .expect("terminal map lock poisoned")
            .keys()
            .copied()
            .collect::<Vec<_>>();
        futures_util::future::join_all(ids.into_iter().map(|id| {
            let supervisor = self.clone();
            async move {
                let _ = supervisor.kill(&TerminalKillRequest { id, force }).await;
            }
        }))
        .await;
    }

    fn process(&self, id: TerminalId) -> Result<Arc<TerminalProcess>, ShellError> {
        self.terminals
            .lock()
            .expect("terminal map lock poisoned")
            .get(&id)
            .cloned()
            .ok_or(ShellError::UnknownTerminal(id))
    }
}

fn effective_cwd(workspace: &Path, requested: Option<&Path>) -> PathBuf {
    let requested = requested.unwrap_or(workspace);
    if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    }
}

fn signal_terminal(process: &TerminalProcess, force: bool) -> Result<(), ShellError> {
    #[cfg(unix)]
    {
        let Some(process_group) = process.process_group else {
            return Err(ShellError::Pty(
                "supervised terminal has no process group".into(),
            ));
        };
        let signal = if force {
            nix::sys::signal::Signal::SIGKILL
        } else {
            nix::sys::signal::Signal::SIGTERM
        };
        match nix::sys::signal::killpg(nix::unistd::Pid::from_raw(process_group), signal) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(error) => Err(ShellError::Pty(error.to_string())),
        }
    }
    #[cfg(windows)]
    {
        if force {
            let process_id = process
                .child
                .lock()
                .expect("terminal child lock poisoned")
                .process_id();
            if let Some(process_id) = process_id {
                let status = std::process::Command::new("taskkill")
                    .args(["/PID", &process_id.to_string(), "/T", "/F"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                if status.is_ok_and(|status| status.success()) {
                    return Ok(());
                }
            }
            let mut child = process.child.lock().expect("terminal child lock poisoned");
            child
                .kill()
                .map_err(|error| ShellError::Pty(error.to_string()))
        } else {
            let mut writer = process
                .writer
                .lock()
                .expect("terminal writer lock poisoned");
            writer
                .write_all(&[3])
                .and_then(|()| writer.flush())
                .map_err(ShellError::Io)
        }
    }
}

fn refresh_terminal_status(process: &TerminalProcess) {
    let mut child = process.child.lock().expect("terminal child lock poisoned");
    if let Ok(Some(status)) = child.try_wait() {
        let mut state = process
            .buffer
            .lock()
            .expect("terminal buffer lock poisoned");
        state.child_exited = true;
        state.exit_code = i32::try_from(status.exit_code()).ok();
    }
    drop(child);
    let mut state = process
        .buffer
        .lock()
        .expect("terminal buffer lock poisoned");
    if state.child_exited
        && state.output_drained
        && state.status.is_some_and(TerminalStatus::is_active)
    {
        state.status = Some(
            state
                .requested_final_status
                .unwrap_or(TerminalStatus::Exited),
        );
        state.completed_at.get_or_insert_with(terminal_now);
    }
}

fn terminal_snapshot(id: TerminalId, process: &TerminalProcess) -> TerminalSnapshot {
    let state = process
        .buffer
        .lock()
        .expect("terminal buffer lock poisoned");
    let ansi_output =
        String::from_utf8_lossy(&state.bytes.iter().copied().collect::<Vec<_>>()).into_owned();
    let output = escape_process_output(&ansi_output);
    let status = state.status.unwrap_or(TerminalStatus::Orphaned);
    TerminalSnapshot {
        id,
        owner: process.owner,
        owner_agent_run_id: process.owner_agent_run_id,
        tool_call_node_id: process.tool_call_node_id,
        read_safe: (!process.detached).then_some(process.read_safe),
        command: process.command.clone(),
        status,
        created_at: process.created_at.clone(),
        started_at: process.created_at.clone(),
        completed_at: state.completed_at.clone(),
        exit_code: state.exit_code,
        output_base: state.base,
        output_cursor: state.end,
        output_bytes: state.end,
        discarded_bytes: state.base,
        truncated: state.base != 0,
        ansi_output,
        output,
    }
}

fn terminal_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

pub(super) fn escape_process_output(value: &str) -> String {
    #[derive(Clone, Copy)]
    enum AnsiState {
        Text,
        Escape,
        EscapeIntermediate,
        Csi,
        Osc,
        OscEscape,
        ControlString,
        ControlStringEscape,
    }

    let mut escaped = String::with_capacity(value.len());
    let mut state = AnsiState::Text;
    let mut previous_carriage_return = false;
    for character in value.chars() {
        state = match state {
            AnsiState::Text => match character {
                '\x1b' => AnsiState::Escape,
                '\u{9b}' => AnsiState::Csi,
                '\u{9d}' => AnsiState::Osc,
                '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => AnsiState::ControlString,
                _ => {
                    append_process_character(
                        &mut escaped,
                        character,
                        &mut previous_carriage_return,
                    );
                    AnsiState::Text
                }
            },
            AnsiState::Escape => match character {
                '[' => AnsiState::Csi,
                ']' => AnsiState::Osc,
                'P' | 'X' | '^' | '_' => AnsiState::ControlString,
                '\x20'..='\x2f' => AnsiState::EscapeIntermediate,
                _ => AnsiState::Text,
            },
            AnsiState::EscapeIntermediate => {
                if ('\x30'..='\x7e').contains(&character) {
                    AnsiState::Text
                } else {
                    AnsiState::EscapeIntermediate
                }
            }
            AnsiState::Csi => {
                if ('\x40'..='\x7e').contains(&character) || character == '\u{9c}' {
                    AnsiState::Text
                } else {
                    AnsiState::Csi
                }
            }
            AnsiState::Osc => match character {
                '\x07' | '\u{9c}' => AnsiState::Text,
                '\x1b' => AnsiState::OscEscape,
                _ => AnsiState::Osc,
            },
            AnsiState::OscEscape => {
                if character == '\\' {
                    AnsiState::Text
                } else if character == '\x1b' {
                    AnsiState::OscEscape
                } else {
                    AnsiState::Osc
                }
            }
            AnsiState::ControlString => match character {
                '\u{9c}' => AnsiState::Text,
                '\x1b' => AnsiState::ControlStringEscape,
                _ => AnsiState::ControlString,
            },
            AnsiState::ControlStringEscape => {
                if character == '\\' {
                    AnsiState::Text
                } else if character == '\x1b' {
                    AnsiState::ControlStringEscape
                } else {
                    AnsiState::ControlString
                }
            }
        };
    }
    escaped
}

fn append_process_character(
    output: &mut String,
    character: char,
    previous_carriage_return: &mut bool,
) {
    match character {
        '\r' => {
            output.push('\n');
            *previous_carriage_return = true;
        }
        '\n' => {
            if !*previous_carriage_return {
                output.push('\n');
            }
            *previous_carriage_return = false;
        }
        '\t' => {
            output.push(character);
            *previous_carriage_return = false;
        }
        _ if !character.is_control() => {
            output.push(character);
            *previous_carriage_return = false;
        }
        _ => {
            use std::fmt::Write as _;
            let _ = write!(output, "\\u{{{:x}}}", u32::from(character));
            *previous_carriage_return = false;
        }
    }
}

#[cfg(test)]
mod terminal_output_tests {
    use super::{effective_cwd, escape_process_output, output_preview};
    use std::path::Path;

    #[test]
    fn relative_cwd_is_anchored_to_the_workspace() {
        assert_eq!(
            effective_cwd(Path::new("/workspace"), Some(Path::new("crates/agent"))),
            Path::new("/workspace/crates/agent")
        );
        assert_eq!(
            effective_cwd(Path::new("/workspace"), Some(Path::new("/tmp/project"))),
            Path::new("/tmp/project")
        );
    }

    #[test]
    fn strips_ansi_csi_osc_and_control_strings() {
        let value = concat!(
            "\x1b[31mred\x1b[0m ",
            "\x1b]8;;https://example.test\x07link\x1b]8;;\x07 ",
            "\x1bPignored\x1b\\plain\0"
        );
        assert_eq!(escape_process_output(value), "red link plain\\u{0}");
    }

    #[test]
    fn normalizes_terminal_carriage_returns_for_safe_output() {
        assert_eq!(
            escape_process_output("first\rsecond\r\nthird"),
            "first\nsecond\nthird"
        );
    }

    #[test]
    fn preview_keeps_head_and_tail_without_cloning_the_middle() {
        assert_eq!(
            output_preview("one\ntwo\nthree\nfour\nfive\n"),
            "one\ntwo\n… 1 lines omitted …\nfour\nfive"
        );
    }
}
