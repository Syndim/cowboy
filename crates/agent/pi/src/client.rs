//! pi RPC backend client implementing `cowboy_agent_client::Client`.
//!
//! pi is driven over its JSONL RPC protocol (`pi --mode rpc`): commands with
//! correlation ids flow to stdin; responses and session events stream back on
//! stdout. A turn is complete when the `agent_settled` event arrives —
//! `agent_end` alone is not enough, since retries/compaction/steering may
//! continue after it.
//!
//! Ownership model: a background I/O task owns the [`PiProcess`], correlates
//! commands with responses, and forwards normalized session events. Session
//! establishment is deferred until the executor supplies new/load intent and
//! cwd. Persisted session references name existing Pi JSONL files; restoration
//! uses `--session`, never create-if-missing `--session-id`.

use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use cowboy_agent_client::{
    AgentAccess, AgentInfo, AgentLaunchSpec, AgentSessionDescriptor, Client, Event, ModelInfo,
    PromptContent, PromptTurnCancellation, StopReason,
};

use crate::process::PiProcess;

/// Grace period for an orderly pi exit (stdin EOF) before killing the tree.
const ORDERLY_EXIT_GRACE: Duration = Duration::from_secs(5);

/// Inactivity watchdog for prompt turns: pi must emit at least one event in
/// every window, mirroring the ACP response-timeout policy.
fn inactivity_window(response_timeout_seconds: u64) -> Duration {
    Duration::from_secs(response_timeout_seconds.max(1))
}

/// Commands the client hands to the I/O task.
#[doc(hidden)]
pub enum Command {
    Prompt {
        message: String,
        response: oneshot::Sender<anyhow::Result<Value>>,
    },
    GetState {
        response: oneshot::Sender<anyhow::Result<Value>>,
    },
    Abort {
        response: oneshot::Sender<anyhow::Result<Value>>,
    },
    GetSessionStats {
        response: oneshot::Sender<anyhow::Result<Value>>,
    },
    /// Close stdin, wait the grace window for an orderly exit, then
    /// force-kill the tree.
    Shutdown {
        grace: Duration,
        done: oneshot::Sender<()>,
    },
}

/// Events the I/O task forwards to the client.

#[derive(Debug)]
#[doc(hidden)]
pub enum TaskEvent {
    Event(Event),
    /// pi unwound its turn (including user-triggered aborts it performed
    /// itself); `aborted` mirrors the event payload.
    AgentSettled {
        #[allow(dead_code)]
        aborted: bool,
    },
    Fatal(anyhow::Error),
    Eof,
}

/// One pi RPC record received from stdout.
#[derive(Debug)]
enum Inbound {
    Response {
        id: String,
        success: bool,
        data: Value,
        error: Option<String>,
    },
    SessionEvent(Value),
}

fn parse_inbound(line: &str) -> anyhow::Result<Inbound> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| anyhow::anyhow!("pi rpc record is not valid JSON: {error}"))?;
    // pi emits `{"id":..., "type":"response", "command":..., "success":...}`
    // for command completions and typed session events otherwise.
    if value.get("type").and_then(Value::as_str) == Some("response") {
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("pi rpc response is missing an id"))?
            .to_string();
        Ok(Inbound::Response {
            id,
            success: value
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            data: value.get("data").cloned().unwrap_or(Value::Null),
            error: value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    } else {
        Ok(Inbound::SessionEvent(value))
    }
}

/// Build the agent-returned session descriptor from a `get_state` reply.
/// Only values pi reports are captured, never the configured model.
fn descriptor_from_state(state: &Value) -> AgentSessionDescriptor {
    AgentSessionDescriptor {
        model: state
            .get("model")
            .and_then(|model| model.get("id").and_then(Value::as_str).map(str::to_string))
            .or_else(|| {
                state
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
        context: None,
        reasoning: state
            .get("thinkingLevel")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn build_launch_args(
    base: &[String],
    session_id: &str,
    restore: bool,
    model: Option<&ModelInfo>,
) -> Vec<String> {
    let mut args = base.to_vec();
    if !args.windows(2).any(|pair| pair == ["--mode", "rpc"]) {
        args.push("--mode".to_string());
        args.push("rpc".to_string());
    }
    args.push(if restore { "--session" } else { "--session-id" }.to_string());
    args.push(session_id.to_string());
    if let Some(model) = model {
        let id = match &model.provider {
            Some(provider) => format!("{provider}/{}", model.id),
            None => model.id.clone(),
        };
        args.push("--model".to_string());
        args.push(id);
    }
    args
}

/// Process launch parameters kept for respawning on `load_session`.
#[derive(Clone)]
struct ProcessConfigBuilder {
    command: String,
    args: Vec<String>,
    allowed_env: Vec<String>,
    model: Option<ModelInfo>,
}

impl ProcessConfigBuilder {
    fn config(
        &self,
        session_id: &str,
        restore: bool,
        model: Option<&ModelInfo>,
    ) -> crate::process::PiProcessConfig {
        crate::process::PiProcessConfig {
            command: self.command.clone(),
            args: build_launch_args(&self.args, session_id, restore, model),
            clear_env: true,
            allowed_env: self.allowed_env.clone(),
            env: Vec::new(),
        }
    }
}

/// Connected pi RPC backend for one role session.
pub struct PiRpcClient {
    command_tx: Option<mpsc::Sender<Command>>,
    io_task: Option<tokio::task::JoinHandle<()>>,
    events: mpsc::UnboundedReceiver<TaskEvent>,
    // Opaque persisted session reference: Pi's explicit session file, not a
    // create-if-missing UUID. None until new/load establishment succeeds.
    session_id: Option<String>,
    requested_id: Option<String>,
    closed: bool,
    process_config: ProcessConfigBuilder,
    descriptor: Option<AgentSessionDescriptor>,
    agent_info: AgentInfo,
    watchdog_seconds: u64,
    operation_timeout: Duration,
    cancel_timeout: Duration,
}

impl Drop for PiRpcClient {
    fn drop(&mut self) {
        if let Some(task) = self.io_task.take() {
            task.abort();
        }
    }
}

impl std::fmt::Debug for PiRpcClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PiRpcClient")
            .field("session_id", &self.session_id)
            .field("connected", &self.is_connected())
            .finish_non_exhaustive()
    }
}

impl PiRpcClient {
    /// Prepare a backend handle. Session establishment owns process launch so
    /// the executor can select new versus saved history and supply its cwd.
    /// The optional id is a seed for a new session, never proof of restoration.
    pub async fn connect(spec: &AgentLaunchSpec, session_id: Option<&str>) -> anyhow::Result<Self> {
        if spec.access != AgentAccess::Default {
            anyhow::bail!("pi rpc does not support restricted access settings");
        }
        if spec.startup_readiness.is_some() {
            anyhow::bail!("pi rpc does not support ACP startup-readiness settings");
        }
        if spec.args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "--session"
                    | "--session-id"
                    | "--no-session"
                    | "--continue"
                    | "-c"
                    | "--resume"
                    | "-r"
                    | "--fork"
            )
        }) {
            anyhow::bail!("pi rpc session selection is owned by Cowboy");
        }
        let (_, events) = mpsc::unbounded_channel();
        Ok(Self {
            command_tx: None,
            io_task: None,
            events,
            session_id: None,
            requested_id: session_id.map(str::to_owned),
            closed: false,
            process_config: ProcessConfigBuilder {
                command: spec.command.clone(),
                args: spec.args.clone(),
                allowed_env: spec.allowed_env.clone(),
                model: spec.model.clone(),
            },
            descriptor: None,
            agent_info: AgentInfo {
                name: "pi".to_string(),
                title: None,
                version: None,
            },
            watchdog_seconds: spec.watchdog.response_timeout_seconds,
            operation_timeout: Duration::from_secs(
                spec.watchdog.recovery_operation_timeout_seconds.max(1),
            ),
            cancel_timeout: Duration::from_secs(spec.watchdog.cancel_timeout_seconds.max(1)),
        })
    }

    async fn send_command(
        &self,
        command: impl FnOnce(oneshot::Sender<anyhow::Result<Value>>) -> Command,
    ) -> anyhow::Result<Value> {
        let tx = self
            .command_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("pi rpc session is not established"))?;
        let (response_tx, response_rx) = oneshot::channel();
        tokio::time::timeout(self.operation_timeout, async {
            tx.send(command(response_tx))
                .await
                .map_err(|_| anyhow::anyhow!("pi rpc task has shut down"))?;
            response_rx
                .await
                .map_err(|_| anyhow::anyhow!("pi rpc task dropped the command"))?
        })
        .await
        .map_err(|_| anyhow::anyhow!("pi rpc command response deadline exceeded"))?
    }

    async fn stop_process(&mut self) {
        self.stop_process_with_grace(ORDERLY_EXIT_GRACE).await;
    }

    async fn stop_process_with_grace(&mut self, grace: Duration) {
        self.session_id = None;
        self.descriptor = None;
        if let Some(tx) = self.command_tx.take() {
            let _ = tokio::time::timeout(grace + Duration::from_secs(1), shutdown_task(&tx, grace))
                .await;
        }
        if let Some(task) = self.io_task.as_mut()
            && tokio::time::timeout(Duration::from_secs(1), &mut *task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        self.io_task.take();
        self.drain_events();
    }

    async fn start_process(
        &mut self,
        session: &str,
        restore: bool,
        cwd: &Path,
        model: Option<&ModelInfo>,
    ) -> anyhow::Result<Value> {
        if self.closed {
            anyhow::bail!("pi rpc client is closed");
        }
        self.stop_process().await;
        let (tx, events, task) = spawn_io_task(
            self.process_config.config(session, restore, model),
            self.watchdog_seconds,
            cwd,
        )
        .await?;
        self.command_tx = Some(tx);
        self.io_task = Some(task);
        self.events = events;
        let state = self
            .send_command(|response| Command::GetState { response })
            .await;
        if state.is_err() {
            self.stop_process().await;
        }
        state
    }

    fn returned_reference(state: &Value, expected_id: &str) -> anyhow::Result<String> {
        if state.get("sessionId").and_then(Value::as_str) != Some(expected_id) {
            anyhow::bail!("pi rpc returned a different session identity");
        }
        let file = state
            .get("sessionFile")
            .and_then(Value::as_str)
            .filter(|value| !value.chars().any(char::is_control))
            .ok_or_else(|| {
                anyhow::anyhow!("pi rpc did not return a persistent session reference")
            })?;
        if !Path::new(file).is_absolute() {
            anyhow::bail!("pi rpc session reference is not absolute");
        }
        Ok(file.to_owned())
    }

    /// Drain leftover events, discarding them (used across restarts).
    fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            drop(event);
        }
    }
}

async fn spawn_io_task(
    config: crate::process::PiProcessConfig,
    watchdog_seconds: u64,
    cwd: &Path,
) -> anyhow::Result<(
    mpsc::Sender<Command>,
    mpsc::UnboundedReceiver<TaskEvent>,
    tokio::task::JoinHandle<()>,
)> {
    let process = PiProcess::spawn_at(&config, Some(cwd)).await?;
    let (command_tx, command_rx) = mpsc::channel(16);
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(io_loop(process, command_rx, event_tx, watchdog_seconds));
    Ok((command_tx, event_rx, task))
}

/// Drive subprocess I/O until EOF, failure, or shutdown.
pub async fn io_loop(
    mut process: PiProcess,
    mut command_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::UnboundedSender<TaskEvent>,
    _watchdog_seconds: u64,
) {
    let mut responders: HashMap<String, oneshot::Sender<anyhow::Result<Value>>> = HashMap::new();
    let mut next_request_id = 0_u64;

    loop {
        tokio::select! {
            command = command_rx.recv() => {
                match command {
                    Some(Command::Shutdown { grace, done }) => {
                        // Orderly exit: close stdin, wait the grace window,
                        // then force-kill the tree.
                        process.close_stdin();
                        if !process.wait_for_exit(grace).await {
                            process.kill().await;
                        }
                        let _ = done.send(());
                        break;
                    }
                    Some(command) => {
                        next_request_id += 1;
                        let id = format!("cowboy-{next_request_id}");
                        let (record, response) = match command {
                            Command::Prompt { message, response } => (
                                serde_json::json!({"id": id, "type": "prompt", "message": message}).to_string(),
                                response,
                            ),
                            Command::GetState { response } => (
                                serde_json::json!({"id": id, "type": "get_state"}).to_string(),
                                response,
                            ),
                            Command::Abort { response } => (
                                serde_json::json!({"id": id, "type": "abort"}).to_string(),
                                response,
                            ),
                            Command::GetSessionStats { response } => (
                                serde_json::json!({"id":id,"type":"get_session_stats"}).to_string(), response,
                            ),
                            Command::Shutdown { .. } => unreachable!("handled above"),
                        };
                        responders.insert(id, response);
                        if let Err(error) = process.send_line(&record).await {
                            fail_all(&mut responders, &event_tx, format!("pi rpc write failed: {error}")).await;
                            break;
                        }
                    }
                    None => break,
                }
            }
            line = process.recv_line() => {
                match line {
                    Ok(Some(line)) => {
                        match parse_inbound(&line) {
                            Ok(Inbound::Response { id, success, data, error }) => {
                                match responders.remove(&id) {
                                    Some(responder) => {
                                        let result = if success {
                                            Ok(data)
                                        } else {
                                            Err(anyhow::anyhow!(error.unwrap_or_else(|| "pi rpc command failed".to_string())))
                                        };
                                        let _ = responder.send(result);
                                    }
                                    None => {
                                        // pi echoes the request id; an id with
                                        // no pending responder means protocol
                                        // drift — surface it instead of
                                        // dropping it silently.
                                        let _ = event_tx.send(TaskEvent::Fatal(anyhow::anyhow!(
                                            "pi rpc response references unknown id {id:?}"
                                        )));
                                    }
                                }
                            }
                            Ok(Inbound::SessionEvent(event)) => {
                                if event.get("type").and_then(Value::as_str) == Some("agent_settled") {
                                    let aborted = event.get("aborted").and_then(Value::as_bool).unwrap_or(false);
                                    let _ = event_tx.send(TaskEvent::AgentSettled { aborted });
                                } else if let Some(mapped) = map_session_event(&event) {
                                    let _ = event_tx.send(mapped);
                                }
                            }
                            Err(error) => {
                                let _ = event_tx.send(TaskEvent::Fatal(anyhow::anyhow!(
                                    "pi rpc record parse failed: {error}"
                                )));
                            }
                        }
                    }
                    Ok(None) => {
                        let _ = event_tx.send(TaskEvent::Eof);
                        fail_all(&mut responders, &event_tx, "pi rpc subprocess exited".to_string()).await;
                        break;
                    }
                    Err(error) => {
                        fail_all(&mut responders, &event_tx, format!("pi rpc read failed: {error}")).await;
                        break;
                    }
                }
            }
        }
    }
}

async fn fail_all(
    responders: &mut HashMap<String, oneshot::Sender<anyhow::Result<Value>>>,
    event_tx: &mpsc::UnboundedSender<TaskEvent>,
    reason: String,
) {
    let _ = event_tx.send(TaskEvent::Fatal(anyhow::anyhow!("{reason}")));
    for (_, responder) in responders.drain() {
        let _ = responder.send(Err(anyhow::anyhow!("{reason}")));
    }
}

/// Map one pi session event into the normalized Cowboy event stream.
fn map_session_event(event: &Value) -> Option<TaskEvent> {
    let kind = event.get("type").and_then(Value::as_str)?;
    match kind {
        "message_update" => {
            let update = event.get("assistantMessageEvent")?;
            match update.get("type").and_then(Value::as_str)? {
                "text_delta" => Some(TaskEvent::Event(Event::MessageChunk {
                    content: serde_json::json!({
                        "text": update.get("delta").and_then(Value::as_str).unwrap_or_default()
                    }),
                })),
                "thinking_delta" => Some(TaskEvent::Event(Event::ThoughtChunk {
                    content: serde_json::json!({
                        "text": update.get("delta").and_then(Value::as_str).unwrap_or_default()
                    }),
                })),
                _ => None,
            }
        }
        "tool_execution_start" => Some(TaskEvent::Event(Event::ToolCall {
            tool_call_id: event
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: event
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            kind: "execute".to_string(),
            status: "in_progress".to_string(),
        })),
        "tool_execution_update" | "tool_execution_end" => {
            Some(TaskEvent::Event(Event::ToolCallUpdate {
                tool_call_id: event
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                status: if kind == "tool_execution_end" {
                    if event
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        "error".to_string()
                    } else {
                        "completed".to_string()
                    }
                } else {
                    "in_progress".to_string()
                },
                content: event
                    .get("partialResult")
                    .or_else(|| event.get("result"))
                    .cloned(),
            }))
        }
        // Lifecycle noise the executor does not consume.
        "agent_start"
        | "turn_start"
        | "turn_end"
        | "agent_end"
        | "queue_update"
        | "entry_appended"
        | "session_info_changed"
        | "thinking_level_changed"
        | "compaction_start"
        | "compaction_end"
        | "auto_retry_start"
        | "auto_retry_end"
        | "auto_retry_scheduled"
        | "summarization_retry_scheduled"
        | "summarization_retry_attempt_start"
        | "summarization_retry_finished"
        | "bash_execution_update" => None,
        other => Some(TaskEvent::Event(Event::Unknown {
            session_update: other.to_string(),
            raw: event.clone(),
        })),
    }
}

#[async_trait]
impl Client for PiRpcClient {
    fn is_connected(&self) -> bool {
        !self.closed && self.command_tx.as_ref().is_none_or(|tx| !tx.is_closed())
    }

    fn agent_info(&self) -> Option<&AgentInfo> {
        Some(&self.agent_info)
    }

    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn session_descriptor(&self) -> Option<&AgentSessionDescriptor> {
        self.descriptor.as_ref()
    }

    async fn new_session(
        &mut self,
        cwd: &str,
        mcp_servers: &[Value],
        model: Option<&ModelInfo>,
    ) -> anyhow::Result<String> {
        if !mcp_servers.is_empty() {
            anyhow::bail!("pi rpc does not support session MCP configuration");
        }
        let cwd = std::fs::canonicalize(cwd)?;
        let id = self
            .requested_id
            .take()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let model = model.cloned().or_else(|| self.process_config.model.clone());
        let state = self.start_process(&id, false, &cwd, model.as_ref()).await?;
        let reference = match Self::returned_reference(&state, &id) {
            Ok(reference) => reference,
            Err(error) => {
                self.stop_process().await;
                return Err(error);
            }
        };
        self.descriptor = Some(descriptor_from_state(&state));
        self.session_id = Some(reference.clone());
        Ok(reference)
    }

    fn supports_load_session(&self) -> bool {
        true
    }

    async fn load_session(
        &mut self,
        session_id: &str,
        cwd: &str,
        mcp_servers: &[Value],
    ) -> anyhow::Result<Vec<Event>> {
        if !mcp_servers.is_empty() {
            anyhow::bail!("pi rpc does not support session MCP configuration");
        }
        // A saved opaque reference names an EXISTING file. Never use
        // --session-id for restoration: Pi creates that UUID when missing.
        let path = Path::new(session_id);
        if !path.is_absolute() {
            anyhow::bail!(
                "pi rpc restoration requires an existing absolute session-file reference"
            );
        }
        let path = path.canonicalize()?;
        if !path.is_file() {
            anyhow::bail!("pi rpc saved session is not a regular file");
        }
        let mut header = Vec::new();
        std::io::BufReader::new(std::fs::File::open(&path)?)
            .take(16_384)
            .read_until(b'\n', &mut header)?;
        let header: Value = serde_json::from_slice(&header)?;
        if header.get("type").and_then(Value::as_str) != Some("session") {
            anyhow::bail!("pi rpc saved session header is invalid");
        }
        let id = header
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("pi rpc saved session identity is missing"))?;
        let reference = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("pi rpc session reference is not UTF-8"))?
            .to_owned();
        let cwd = std::fs::canonicalize(cwd)?;
        let stored_cwd = header
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("pi rpc saved session cwd is missing"))?;
        if std::fs::canonicalize(stored_cwd)? != cwd {
            anyhow::bail!("pi rpc cannot restore history into a different session cwd");
        }
        let state = self.start_process(&reference, true, &cwd, None).await?;
        let verified = async {
            let returned = Self::returned_reference(&state, id)?;
            if Path::new(&returned).canonicalize()? != path {
                anyhow::bail!("pi rpc restored a different session file");
            }
            let stats = self
                .send_command(|response| Command::GetSessionStats { response })
                .await?;
            if stats.get("sessionId").and_then(Value::as_str) != Some(id)
                || stats
                    .get("totalMessages")
                    .and_then(Value::as_u64)
                    .is_none_or(|count| count == 0)
            {
                anyhow::bail!("pi rpc did not restore saved session history");
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = verified {
            self.stop_process().await;
            return Err(error);
        }
        self.descriptor = Some(descriptor_from_state(&state));
        // The executor retains its saved opaque reference verbatim. Validate
        // canonical paths without changing the reference used by its prompt.
        self.session_id = Some(session_id.to_owned());
        Ok(Vec::new())
    }

    async fn prompt(
        &mut self,
        session_id: &str,
        prompt_content: Vec<PromptContent>,
        cancellation: PromptTurnCancellation,
        event_handler: &mut (dyn FnMut(Event) + Send),
    ) -> anyhow::Result<StopReason> {
        if self.session_id.as_deref() != Some(session_id) {
            anyhow::bail!("pi rpc prompt does not match an established session");
        }
        let message = prompt_content
            .into_iter()
            .map(|content| content.text)
            .collect::<Vec<_>>()
            .join("\n");

        let disposition = match self
            .send_command(|response| Command::Prompt { message, response })
            .await
        {
            Ok(value) => value,
            Err(error) => {
                self.stop_process_with_grace(Duration::ZERO).await;
                self.closed = true;
                return Err(error);
            }
        };
        // `handled` means pi consumed the message without starting a run
        // (e.g. queued into an active turn) — nothing to wait for.
        if disposition.get("disposition").and_then(Value::as_str) == Some("handled") {
            return Ok(StopReason::EndTurn);
        }

        let mut cancellation = cancellation;
        let mut cancelled = false;
        let mut cancel_deadline = None;
        let mut fatal: Option<anyhow::Error> = None;
        let window = inactivity_window(self.watchdog_seconds);
        loop {
            tokio::select! {
                _ = cancellation.cancelled(), if !cancelled => {
                    cancelled = true;
                    cancel_deadline = Some(tokio::time::Instant::now() + self.cancel_timeout);
                    let aborted = tokio::time::timeout(self.cancel_timeout,
                        self.send_command(|response| Command::Abort { response })).await;
                    if !matches!(aborted, Ok(Ok(_))) {
                        self.stop_process_with_grace(Duration::ZERO).await;
                        self.closed = true;
                        anyhow::bail!("pi rpc abort acknowledgement deadline exceeded");
                    }
                }
                _ = async {
                    if let Some(deadline) = cancel_deadline {
                        tokio::time::sleep_until(deadline).await;
                    } else { std::future::pending::<()>().await; }
                } => {
                    self.stop_process_with_grace(Duration::ZERO).await;
                    self.closed = true;
                    anyhow::bail!("pi rpc cancellation did not settle before its deadline");
                }
                _ = tokio::time::sleep(window), if !cancelled => {
                    // Inactivity watchdog: an agent that emits nothing for a
                    // full window is treated as stuck. The executor's retry
                    // policy owns recovery; surface a clear failure.
                    self.stop_process_with_grace(Duration::ZERO).await;
                    self.closed = true;
                    return Err(anyhow::anyhow!(
                        "pi rpc turn exceeded {}s without any event",
                        self.watchdog_seconds.max(1)
                    ));
                }
                event = self.events.recv() => {
                    match event {
                        Some(TaskEvent::Event(event)) => event_handler(event),
                        Some(TaskEvent::AgentSettled { .. }) => {
                            return Ok(if cancelled {
                                StopReason::Cancelled
                            } else {
                                StopReason::EndTurn
                            });
                        }
                        Some(TaskEvent::Fatal(error)) => {
                            fatal = Some(error);
                        }
                        Some(TaskEvent::Eof) => {
                            return Err(anyhow::anyhow!(
                                "pi rpc subprocess exited before the turn settled"
                            ));
                        }
                        None => return Err(anyhow::anyhow!("pi rpc event stream closed")),
                    }
                    if let Some(error) = fatal.take() {
                        self.stop_process_with_grace(Duration::ZERO).await;
                        self.closed = true;
                        return Err(error);
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        self.stop_process().await;
        self.closed = true;
        Ok(())
    }
}

async fn shutdown_task(command_tx: &mpsc::Sender<Command>, grace: Duration) {
    let (done_tx, done_rx) = oneshot::channel();
    if command_tx
        .send(Command::Shutdown {
            grace,
            done: done_tx,
        })
        .await
        .is_ok()
    {
        let _ = done_rx.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowboy_agent_client::AgentWatchdogOptions;

    fn spec(command: &str) -> AgentLaunchSpec {
        AgentLaunchSpec {
            agent: "pi".to_string(),
            backend: "pi-rpc".to_string(),
            command: command.to_string(),
            args: Vec::new(),
            allowed_env: Vec::new(),
            access: cowboy_agent_client::AgentAccess::default(),
            watchdog: AgentWatchdogOptions::default(),
            startup_readiness: None,
            model: None,
        }
    }

    #[test]
    fn launch_args_add_rpc_mode_session_and_model() {
        let args = build_launch_args(
            &["--no-session".to_string()],
            "abc-123",
            false,
            Some(&ModelInfo {
                id: "claude-sonnet-4.5".to_string(),
                provider: Some("anthropic".to_string()),
            }),
        );
        assert!(
            args.windows(2).any(|pair| pair == ["--mode", "rpc"]),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--session-id", "abc-123"]),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "anthropic/claude-sonnet-4.5"]),
            "{args:?}"
        );
    }

    #[test]
    fn launch_args_do_not_duplicate_rpc_mode() {
        let args = build_launch_args(
            &["--mode".to_string(), "rpc".to_string()],
            "s1",
            false,
            None,
        );
        assert_eq!(args, ["--mode", "rpc", "--session-id", "s1"]);
    }

    #[test]
    fn parse_inbound_routes_responses_and_events() {
        let response = parse_inbound(
            r#"{"id":"cowboy-1","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}"#,
        )
        .unwrap();
        match response {
            Inbound::Response { id, success, .. } => {
                assert_eq!(id, "cowboy-1");
                assert!(success);
            }
            Inbound::SessionEvent(_) => panic!("expected response"),
        }

        let event = parse_inbound(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hi"}}"#,
        )
        .unwrap();
        assert!(matches!(event, Inbound::SessionEvent(_)));

        let malformed = parse_inbound("not json at all").unwrap_err();
        assert!(
            malformed.to_string().contains("not valid JSON"),
            "{malformed}"
        );
    }

    #[test]
    fn descriptor_from_state_captures_only_agent_values() {
        let descriptor = descriptor_from_state(&serde_json::json!({
            "model": {"id": "stub-model"},
            "thinkingLevel": "high",
            "sessionFile": "/tmp/session.jsonl"
        }));
        assert_eq!(descriptor.model.as_deref(), Some("stub-model"));
        assert_eq!(descriptor.reasoning.as_deref(), Some("high"));
        assert!(descriptor.context.is_none());

        let empty = descriptor_from_state(&serde_json::json!({}));
        assert_eq!(empty, AgentSessionDescriptor::default());
    }

    #[test]
    fn map_session_event_normalizes_deltas_and_tools() {
        let text = map_session_event(&serde_json::json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_delta", "delta": "hello "}
        }))
        .unwrap();
        match text {
            TaskEvent::Event(Event::MessageChunk { content }) => {
                assert_eq!(content["text"], "hello ");
            }
            _ => panic!("expected message chunk"),
        }

        let thinking = map_session_event(&serde_json::json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "thinking_delta", "delta": "hmm"}
        }))
        .unwrap();
        assert!(matches!(
            thinking,
            TaskEvent::Event(Event::ThoughtChunk { .. })
        ));

        let tool = map_session_event(&serde_json::json!({
            "type": "tool_execution_start",
            "toolCallId": "call_1",
            "toolName": "bash"
        }))
        .unwrap();
        match tool {
            TaskEvent::Event(Event::ToolCall {
                tool_call_id,
                title,
                status,
                ..
            }) => {
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(title, "bash");
                assert_eq!(status, "in_progress");
            }
            _ => panic!("expected tool call"),
        }

        let tool_end = map_session_event(&serde_json::json!({
            "type": "tool_execution_end",
            "toolCallId": "call_1",
            "isError": true
        }))
        .unwrap();
        match tool_end {
            TaskEvent::Event(Event::ToolCallUpdate { status, .. }) => {
                assert_eq!(status, "error");
            }
            _ => panic!("expected tool update"),
        }

        let partial = map_session_event(&serde_json::json!({
            "type": "tool_execution_update",
            "toolCallId": "call_1",
            "partialResult": {"content": [{"type": "text", "text": "part"}]}
        }))
        .unwrap();
        match partial {
            TaskEvent::Event(Event::ToolCallUpdate {
                status, content, ..
            }) => {
                assert_eq!(status, "in_progress");
                assert!(content.is_some());
            }
            _ => panic!("expected tool update"),
        }
    }

    #[test]
    fn map_session_event_ignores_noise_and_wraps_unknowns() {
        for kind in [
            "agent_start",
            "agent_end",
            "turn_start",
            "turn_end",
            "compaction_start",
            "auto_retry_start",
        ] {
            assert!(
                map_session_event(&serde_json::json!({ "type": kind })).is_none(),
                "{kind} should be ignored"
            );
        }
        let unknown =
            map_session_event(&serde_json::json!({ "type": "future_event", "x": 1 })).unwrap();
        match unknown {
            TaskEvent::Event(Event::Unknown { session_update, .. }) => {
                assert_eq!(session_update, "future_event");
            }
            _ => panic!("expected unknown"),
        }
    }

    #[tokio::test]
    async fn session_establishment_reports_missing_binary() {
        let mut client =
            PiRpcClient::connect(&spec("definitely-not-pi-binary-xyz"), Some("session-1"))
                .await
                .unwrap();
        let error = client.new_session(".", &[], None).await.unwrap_err();
        assert!(
            error.to_string().contains("definitely-not-pi-binary-xyz"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn shutdown_task_terminates_stub_process() {
        use crate::process::PiProcessConfig;

        // `cat` echoes JSONL back and never exits on its own, proving the
        // shutdown path terminates a live process.
        let config = PiProcessConfig {
            command: "/bin/cat".to_string(),
            args: Vec::new(),
            clear_env: false,
            allowed_env: Vec::new(),
            env: Vec::new(),
        };
        let process = PiProcess::spawn(&config).await.unwrap();
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(io_loop(process, command_rx, event_tx, 60));

        shutdown_task(&command_tx, ORDERLY_EXIT_GRACE).await;
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("io task must finish after shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn stub_pi_process_answers_prompt_and_settles() {
        use crate::process::PiProcessConfig;

        // Shell stub emulating pi: echoes a response record per input record,
        // then streams message updates and agent_settled.
        let script = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'); printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"started\"}}"
  printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"---\nstatus: success\nsummary: ok\n---\ndone"}}'
  printf '%s\n' '{"type":"agent_settled","aborted":false}'
done
"#;
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("pi-stub.sh");
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script_path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script_path, permissions).unwrap();
        }

        let config = PiProcessConfig {
            command: script_path.to_string_lossy().to_string(),
            args: Vec::new(),
            clear_env: false,
            allowed_env: Vec::new(),
            env: Vec::new(),
        };
        let process = PiProcess::spawn(&config).await.unwrap();
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        tokio::spawn(io_loop(process, command_rx, event_tx, 60));

        let (response_tx, response_rx) = oneshot::channel();
        command_tx
            .send(Command::Prompt {
                message: "work".to_string(),
                response: response_tx,
            })
            .await
            .unwrap();
        let disposition = response_rx.await.unwrap().unwrap();
        assert_eq!(disposition["disposition"], "started");

        let mut chunks = Vec::new();
        let mut settled = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                TaskEvent::Event(Event::MessageChunk { content }) => {
                    chunks.push(content["text"].as_str().unwrap().to_string())
                }
                TaskEvent::AgentSettled { .. } => {
                    settled = true;
                    break;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(settled);
        assert!(chunks[0].contains("status: success"), "{chunks:?}");

        shutdown_task(&command_tx, ORDERLY_EXIT_GRACE).await;
    }

    #[tokio::test]
    async fn get_state_probe_reaches_stub() {
        use crate::process::PiProcessConfig;

        let script = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"get_state"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'); printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"model\":{\"id\":\"stub-model\"},\"thinkingLevel\":\"high\"}}"
      ;;
  esac
done
"#;
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("pi-state.sh");
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script_path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script_path, permissions).unwrap();
        }

        let config = PiProcessConfig {
            command: script_path.to_string_lossy().to_string(),
            args: Vec::new(),
            clear_env: false,
            allowed_env: Vec::new(),
            env: Vec::new(),
        };
        let process = PiProcess::spawn(&config).await.unwrap();
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        tokio::spawn(io_loop(process, command_rx, event_tx, 60));

        let (response_tx, response_rx) = oneshot::channel();
        command_tx
            .send(Command::GetState {
                response: response_tx,
            })
            .await
            .unwrap();
        let state = response_rx.await.unwrap().unwrap();
        let descriptor = descriptor_from_state(&state);
        assert_eq!(descriptor.model.as_deref(), Some("stub-model"));
        assert_eq!(descriptor.reasoning.as_deref(), Some("high"));

        shutdown_task(&command_tx, ORDERLY_EXIT_GRACE).await;
    }
}
