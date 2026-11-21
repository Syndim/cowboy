use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use anyhow::{Context, bail};
use async_trait::async_trait;
use copilot_sdk::{
    Client as SdkClient, ClientMode, CreateSessionOptions, ResumeSessionOptions,
    handler::{ExitPlanModeHandler, ExitPlanModeResult},
    session::Session,
    types::{ExitPlanModeData, PermissionMode, RequestId, SessionEvent, SessionId},
};
use cowboy_agent_client::{
    AgentAccess, AgentInfo, AgentLaunchSpec, AgentSessionDescriptor, AgentWatchdogOptions,
    BackendConnector, Client, Event, ModelInfo, PromptContent, PromptTurnCancellation,
    ResolvedBackend, StopReason,
};
use serde_json::{Value, json};
use tokio::process::{Child, Command};

#[derive(Default)]
pub struct CopilotSdkBackendConnector {
    processes: Arc<Mutex<HashMap<u32, Weak<ProcessGuard>>>>,
}

#[async_trait]
impl BackendConnector for CopilotSdkBackendConnector {
    fn kind(&self) -> &str {
        "copilot-sdk"
    }

    async fn connect(&self, spec: &AgentLaunchSpec) -> anyhow::Result<ResolvedBackend> {
        validate_spec(spec)?;
        Ok(ResolvedBackend {
            client: Box::new(SdkBackendClient {
                spec: spec.clone(),
                processes: self.processes.clone(),
                runtime: None,
                session: None,
                closed: false,
            }),
            model: spec.model.clone(),
            agent: spec.agent.clone(),
        })
    }

    async fn terminate_all_agents(&self, timeout: Duration) -> usize {
        let processes: Vec<_> = {
            let mut registry = self.processes.lock().unwrap_or_else(|e| e.into_inner());
            registry.retain(|_, process| process.strong_count() > 0);
            registry.values().filter_map(Weak::upgrade).collect()
        };
        let deadline = tokio::time::Instant::now() + timeout;
        let signalled: Vec<_> = processes
            .into_iter()
            .filter(|process| process.terminate())
            .collect();
        let mut count = 0;
        for process in signalled {
            if tokio::time::timeout_at(deadline, process.wait_for_exit()).await == Ok(true) {
                count += 1;
            }
        }
        count
    }
}

fn validate_spec(spec: &AgentLaunchSpec) -> anyhow::Result<()> {
    if spec.backend != "copilot-sdk" {
        bail!("incorrect backend kind");
    }
    let command = std::path::Path::new(&spec.command);
    if !command.is_absolute()
        || command
            .file_name()
            .is_some_and(|name| name == "copilot" || name == "copilot.exe")
    {
        bail!(
            "copilot-sdk requires an absolute path to the compatible copilot-runtime wrapper, not the Copilot CLI"
        );
    }
    if !spec.args.is_empty() {
        bail!("copilot-sdk does not support configured arguments; use args = []");
    }
    if spec.startup_readiness.is_some() {
        bail!("copilot-sdk does not support startup readiness probes");
    }
    if !matches!(spec.access, AgentAccess::Default) {
        bail!("copilot-sdk does not support this access policy");
    }
    validate_model(spec.model.as_ref())?;
    if spec.watchdog.response_timeout_seconds == 0
        || spec.watchdog.cancel_timeout_seconds == 0
        || spec.watchdog.recovery_operation_timeout_seconds == 0
    {
        bail!("copilot-sdk watchdog timeouts must be nonzero");
    }
    if !cfg!(unix) {
        bail!("copilot-sdk process isolation is currently supported only on Unix");
    }
    Ok(())
}

fn validate_model(model: Option<&ModelInfo>) -> anyhow::Result<()> {
    if let Some(model) = model {
        if model.id.trim().is_empty() {
            bail!("copilot-sdk model ID must not be empty");
        }
        if model.provider.is_some() {
            bail!("copilot-sdk does not support model provider selection");
        }
    }
    Ok(())
}

struct ProcessGuard {
    #[cfg(unix)]
    pid: u32,
    terminated: std::sync::atomic::AtomicBool,
    child: Arc<tokio::sync::Mutex<Child>>,
}
impl ProcessGuard {
    async fn wait_for_exit(&self) -> bool {
        if self.child.lock().await.wait().await.is_err() {
            return false;
        }
        loop {
            #[cfg(unix)]
            if unsafe { libc::kill(-(self.pid as i32), 0) } != 0 {
                return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn terminate(&self) -> bool {
        if self
            .terminated
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return false;
        }
        #[cfg(unix)]
        {
            unsafe { libc::kill(-(self.pid as i32), libc::SIGKILL) == 0 }
        }
        #[cfg(not(unix))]
        false
    }
}
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

struct Runtime {
    sdk: SdkClient,
    child: Arc<tokio::sync::Mutex<Child>>,
    guard: Arc<ProcessGuard>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.sdk.force_stop();
        self.guard.terminate();
    }
}

struct OperationGuard {
    sdk: SdkClient,
    process: Arc<ProcessGuard>,
    armed: bool,
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.sdk.force_stop();
            self.process.terminate();
        }
    }
}

struct SdkBackendClient {
    spec: AgentLaunchSpec,
    processes: Arc<Mutex<HashMap<u32, Weak<ProcessGuard>>>>,
    runtime: Option<Runtime>,
    session: Option<Session>,
    closed: bool,
}

impl std::fmt::Debug for SdkBackendClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SdkBackendClient")
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl SdkBackendClient {
    async fn start(&mut self, cwd: &str) -> anyhow::Result<()> {
        if self.closed {
            bail!("copilot-sdk client is closed");
        }
        if self.runtime.is_some() {
            bail!("copilot-sdk session already exists");
        }
        let cwd = PathBuf::from(cwd);
        let mut command = Command::new(&self.spec.command);
        command
            .args([
                "--server",
                "--stdio",
                "--no-auto-update",
                "--log-level",
                "none",
            ])
            .current_dir(&cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for name in &self.spec.allowed_env {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .context("failed to start copilot-sdk runtime")?;
        let pid = child
            .id()
            .context("copilot-sdk runtime has no process ID")?;
        let stdin = child
            .stdin
            .take()
            .context("copilot-sdk runtime stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("copilot-sdk runtime stdout unavailable")?;
        let child = Arc::new(tokio::sync::Mutex::new(child));
        let guard = Arc::new(ProcessGuard {
            #[cfg(unix)]
            pid,
            terminated: false.into(),
            child: child.clone(),
        });
        self.processes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pid, Arc::downgrade(&guard));
        let sdk = SdkClient::from_streams(ClientMode::CodingAgent, stdout, stdin, cwd)
            .map_err(|_| anyhow::anyhow!("failed to initialize copilot-sdk transport"))?;
        self.runtime = Some(Runtime { sdk, child, guard });
        let mut operation = self.operation_guard();
        let sdk = &self.runtime.as_ref().unwrap().sdk;
        let timeout = Duration::from_secs(self.spec.watchdog.recovery_operation_timeout_seconds);
        let result = tokio::time::timeout(timeout, sdk.verify_protocol_version()).await;
        operation.armed = false;
        match result {
            Ok(Ok(())) => Ok(()),
            _ => {
                self.runtime = None;
                bail!("copilot-sdk runtime handshake failed or timed out")
            }
        }
    }

    fn operation_guard(&self) -> OperationGuard {
        let runtime = self.runtime.as_ref().unwrap();
        OperationGuard {
            sdk: runtime.sdk.clone(),
            process: runtime.guard.clone(),
            armed: true,
        }
    }

    fn fail_closed(&mut self) {
        self.session = None;
        self.runtime = None;
        self.closed = true;
    }
}

struct RejectExitPlan;

#[async_trait]
impl ExitPlanModeHandler for RejectExitPlan {
    async fn handle(
        &self,
        _: SessionId,
        _: Option<RequestId>,
        _: ExitPlanModeData,
    ) -> ExitPlanModeResult {
        ExitPlanModeResult {
            approved: false,
            selected_action: None,
            feedback: None,
        }
    }
}

fn is_root(event: &SessionEvent) -> bool {
    event.agent_id.as_deref().is_none_or(str::is_empty)
}

fn map_event(event: &SessionEvent) -> Option<Event> {
    if !is_root(event) {
        return None;
    }
    let data = &event.data;
    let text = |name: &str| data.get(name).and_then(Value::as_str);
    match event.event_type.as_str() {
        "assistant.message_delta" => Some(Event::MessageChunk {
            content: json!({"type":"text", "text":text("deltaContent")?}),
        }),
        "assistant.reasoning_delta" => Some(Event::ThoughtChunk {
            content: json!({"type":"text", "text":text("deltaContent")?}),
        }),
        "tool.execution_start" => Some(Event::ToolCall {
            tool_call_id: text("toolCallId")?.to_owned(),
            title: text("toolTitle")
                .or_else(|| text("toolName"))
                .unwrap_or("tool")
                .to_owned(),
            kind: text("toolName").unwrap_or("tool").to_owned(),
            status: "in_progress".to_owned(),
        }),
        "tool.execution_progress" => Some(Event::ToolCallUpdate {
            tool_call_id: text("toolCallId")?.to_owned(),
            status: "in_progress".to_owned(),
            content: Some(json!({"type":"text", "text":text("progressMessage")?})),
        }),
        "tool.execution_complete" => Some(Event::ToolCallUpdate {
            tool_call_id: text("toolCallId")?.to_owned(),
            status: if data.get("success").and_then(Value::as_bool) == Some(true) {
                "completed"
            } else {
                "failed"
            }
            .to_owned(),
            content: None,
        }),
        _ => None,
    }
}

#[async_trait]
impl Client for SdkBackendClient {
    fn is_connected(&self) -> bool {
        !self.closed
            && self.runtime.as_ref().is_none_or(|runtime| {
                !runtime
                    .guard
                    .terminated
                    .load(std::sync::atomic::Ordering::SeqCst)
            })
    }
    fn agent_info(&self) -> Option<&AgentInfo> {
        None
    }
    fn session_id(&self) -> Option<&str> {
        self.session.as_ref().map(|session| session.id().as_ref())
    }
    fn session_descriptor(&self) -> Option<&AgentSessionDescriptor> {
        None
    }
    fn supports_load_session(&self) -> bool {
        true
    }

    async fn new_session(
        &mut self,
        cwd: &str,
        mcp_servers: &[Value],
        model: Option<&ModelInfo>,
    ) -> anyhow::Result<String> {
        if !mcp_servers.is_empty() {
            bail!("copilot-sdk does not support MCP configuration");
        }
        validate_model(model)?;
        self.start(cwd).await?;
        let mut options = CreateSessionOptions::default()
            .with_working_directory(cwd)
            .with_events(|e| e.with_streaming(true))
            .with_permissions(|p| p.deny_all_permissions().with_mode(PermissionMode::Manual))
            .with_interaction(|i| i.with_on_exit_plan_mode_request(Arc::new(RejectExitPlan)));
        options.enable_config_discovery = Some(false);
        if let Some(model) = model {
            options.model = Some(model.id.clone());
        }
        let mut guard = self.operation_guard();
        let sdk = &self.runtime.as_ref().unwrap().sdk;
        let result = match sdk.prepare_session(options) {
            Ok(prepared) => start_prepared(prepared, self.spec.watchdog).await,
            Err(_) => Err(anyhow::anyhow!("copilot-sdk session preparation failed")),
        };
        guard.armed = false;
        match result {
            Ok((session, _)) => {
                let id = session.id().to_string();
                self.session = Some(session);
                Ok(id)
            }
            _ => {
                self.fail_closed();
                bail!("copilot-sdk session creation failed or timed out")
            }
        }
    }

    async fn load_session(
        &mut self,
        session_id: &str,
        cwd: &str,
        mcp_servers: &[Value],
    ) -> anyhow::Result<Vec<Event>> {
        if !mcp_servers.is_empty() {
            bail!("copilot-sdk does not support MCP configuration");
        }
        if session_id.is_empty() {
            bail!("copilot-sdk session ID must not be empty");
        }
        self.start(cwd).await?;
        let mut options = ResumeSessionOptions::new(session_id.into())
            .with_continue_pending_work(false)
            .with_suppress_resume_event(true)
            .with_working_directory(cwd)
            .with_events(|e| e.with_streaming(true))
            .with_permissions(|p| p.deny_all_permissions().with_mode(PermissionMode::Manual))
            .with_interaction(|i| i.with_on_exit_plan_mode_request(Arc::new(RejectExitPlan)));
        options.enable_config_discovery = Some(false);
        if let Some(model) = &self.spec.model {
            options.model = Some(model.id.clone());
        }
        let mut guard = self.operation_guard();
        let sdk = &self.runtime.as_ref().unwrap().sdk;
        let result = match sdk.prepare_resume_session(options) {
            Ok(prepared) => start_prepared(prepared, self.spec.watchdog).await,
            Err(_) => Err(anyhow::anyhow!("copilot-sdk session preparation failed")),
        };
        guard.armed = false;
        match result {
            Ok((session, events)) if session.id().as_ref() == session_id => {
                self.session = Some(session);
                Ok(events)
            }
            _ => {
                self.fail_closed();
                bail!("copilot-sdk session reload failed or timed out")
            }
        }
    }

    async fn prompt(
        &mut self,
        session_id: &str,
        prompt_content: Vec<PromptContent>,
        mut cancellation: PromptTurnCancellation,
        event_handler: &mut (dyn FnMut(Event) + Send),
    ) -> anyhow::Result<StopReason> {
        if !self.is_connected() {
            bail!("copilot-sdk client is closed");
        }
        if self.session_id() != Some(session_id) {
            bail!("copilot-sdk session ID mismatch");
        }
        if prompt_content
            .iter()
            .any(|content| content.content_type != "text")
        {
            bail!("copilot-sdk supports text prompts only");
        }
        let text = prompt_content
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
            .join("\n");
        let mut guard = self.operation_guard();
        let session = self.session.as_ref().unwrap();
        let result = run_turn(
            session,
            text,
            &mut cancellation,
            event_handler,
            self.spec.watchdog,
        )
        .await;
        guard.armed = false;
        if result.is_err() {
            self.fail_closed();
        }
        result
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        self.closed = true;
        let mut operation = self.runtime.as_ref().map(|_| self.operation_guard());
        let mut shutdown_failed = false;
        if let Some(session) = self.session.take()
            && !matches!(
                tokio::time::timeout(
                    Duration::from_secs(self.spec.watchdog.recovery_operation_timeout_seconds),
                    session.disconnect()
                )
                .await,
                Ok(Ok(()))
            )
        {
            shutdown_failed = true;
        }
        if let Some(runtime) = self.runtime.take() {
            let timeout =
                Duration::from_secs(self.spec.watchdog.recovery_operation_timeout_seconds);
            if !matches!(
                tokio::time::timeout(timeout, runtime.sdk.rpc().runtime().shutdown()).await,
                Ok(Ok(()))
            ) {
                shutdown_failed = true;
            }
            runtime.sdk.force_stop();
            if !matches!(
                tokio::time::timeout(timeout, async { runtime.child.lock().await.wait().await })
                    .await,
                Ok(Ok(_))
            ) {
                shutdown_failed = true;
            }
        }
        if let Some(operation) = &mut operation {
            operation.armed = false;
        }
        if shutdown_failed {
            bail!("copilot-sdk runtime shutdown failed or timed out");
        }
        Ok(())
    }
}

fn record_startup_event(event: SessionEvent, events: &mut Vec<Event>) -> anyhow::Result<()> {
    if is_root(&event)
        && (event.event_type == "session.error"
            || (event.event_type == "session.warning"
                && event.data.get("warningType").and_then(Value::as_str) == Some("policy_blocked")))
    {
        bail!("copilot-sdk session startup reported an error");
    }
    if let Some(mapped) = map_event(&event) {
        if events.len() >= 4096 {
            bail!("copilot-sdk startup emitted too many events");
        }
        events.push(mapped);
    }
    Ok(())
}

async fn start_prepared(
    prepared: copilot_sdk::session::PreparedSession,
    watchdog: AgentWatchdogOptions,
) -> anyhow::Result<(Session, Vec<Event>)> {
    let mut subscription = prepared.subscribe();
    let start = prepared.start();
    tokio::pin!(start);
    let deadline = tokio::time::sleep(Duration::from_secs(
        watchdog.recovery_operation_timeout_seconds,
    ));
    tokio::pin!(deadline);
    let mut events = Vec::new();
    loop {
        tokio::select! {
            result = &mut start => {
                let session = result.map_err(|_| anyhow::anyhow!("copilot-sdk session startup failed"))?;
                if !session.permission_mode_result().is_some_and(|result| result.success && result.mode == PermissionMode::Manual) {
                    bail!("copilot-sdk runtime did not confirm manual permission mode");
                }
                while let Ok(event) = tokio::time::timeout(Duration::ZERO, subscription.recv()).await {
                    record_startup_event(event.map_err(|_| anyhow::anyhow!("copilot-sdk startup event stream closed or lagged"))?, &mut events)?;
                }
                return Ok((session, events));
            }
            event = subscription.recv() => {
                record_startup_event(event.map_err(|_| anyhow::anyhow!("copilot-sdk startup event stream closed or lagged"))?, &mut events)?;
            }
            _ = &mut deadline => bail!("copilot-sdk session startup timed out"),
        }
    }
}

#[derive(Default)]
struct TurnCorrelation {
    started: bool,
    completion: Option<StopReason>,
}

impl TurnCorrelation {
    fn observe(&mut self, event: &SessionEvent, id: &str) -> anyhow::Result<()> {
        if !is_root(event) || self.completion.is_some() {
            return Ok(());
        }
        let text = |name| event.data.get(name).and_then(Value::as_str);
        match event.event_type.as_str() {
            "user.message" if text("messageId") == Some(id) => self.started = true,
            "assistant.message" => {
                let origin = text("originatingMessageId");
                if origin == Some(id) {
                    self.started = true;
                }
            }
            "session.error"
                if text("originatingMessageId")
                    .filter(|s| !s.is_empty())
                    .is_none_or(|origin| origin == id) =>
            {
                bail!("copilot-sdk session reported an error")
            }
            "session.warning" if text("warningType") == Some("policy_blocked") => {
                bail!("copilot-sdk request blocked by policy")
            }
            "session.idle" if self.started && text("mode") != Some("autopilot") => {
                self.completion = Some(
                    if event.data.get("aborted").and_then(Value::as_bool) == Some(true) {
                        StopReason::Cancelled
                    } else {
                        StopReason::EndTurn
                    },
                );
            }
            "session.autopilot_stopped" if text("requestId") == Some(id) => {
                self.completion = Some(match text("reason") {
                    Some("aborted") => StopReason::Cancelled,
                    Some("error") | None => {
                        bail!("copilot-sdk autopilot did not complete successfully")
                    }
                    _ => StopReason::EndTurn,
                });
            }
            _ => {}
        }
        Ok(())
    }
}

fn emit_turn_event(
    event: &SessionEvent,
    message_id: Option<&str>,
    streamed_messages: &mut HashSet<String>,
    handler: &mut (dyn FnMut(Event) + Send),
) {
    if event.event_type.starts_with("assistant.")
        && event
            .data
            .get("originatingMessageId")
            .and_then(Value::as_str)
            .filter(|origin| !origin.is_empty())
            .is_some_and(|origin| Some(origin) != message_id)
    {
        return;
    }
    if is_root(event)
        && event.event_type == "assistant.message_delta"
        && let Some(id) = event.data.get("messageId").and_then(Value::as_str)
    {
        streamed_messages.insert(id.to_owned());
    }
    if is_root(event) && event.event_type == "assistant.message" {
        let id = event.data.get("messageId").and_then(Value::as_str);
        if id.is_none_or(|id| !streamed_messages.contains(id))
            && let Some(text) = event.data.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            handler(Event::MessageChunk {
                content: json!({"type":"text", "text":text}),
            });
        }
    }
    if let Some(mapped) = map_event(event) {
        handler(mapped);
    }
}

async fn run_turn(
    session: &Session,
    text: String,
    cancellation: &mut PromptTurnCancellation,
    handler: &mut (dyn FnMut(Event) + Send),
    watchdog: AgentWatchdogOptions,
) -> anyhow::Result<StopReason> {
    if cancellation.try_cancelled() {
        return Ok(StopReason::Cancelled);
    }
    let mut events = session.subscribe();
    let send = session.send(text);
    tokio::pin!(send);
    let response_timeout = Duration::from_secs(watchdog.response_timeout_seconds);
    let deadline = tokio::time::sleep(response_timeout);
    tokio::pin!(deadline);
    let mut message_id = None::<String>;
    let mut pending = Vec::new();
    let mut correlation = TurnCorrelation::default();
    let mut cancelled = false;
    let mut cancellation_deadline = None;
    let mut abort = None;
    let mut abort_finished = false;
    let mut streamed_messages = HashSet::new();
    let mut active_tools = HashSet::new();
    loop {
        if cancellation_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            bail!("copilot-sdk cancellation timed out");
        }
        if (!cancelled || abort.is_none())
            && let Some(result) = correlation.completion.take()
        {
            return Ok(result);
        }
        if cancelled && message_id.is_some() && abort.is_none() && !abort_finished {
            abort = Some(Box::pin(session.abort()));
        }
        tokio::select! {
            response = &mut send, if message_id.is_none() => {
                let id = response.map_err(|_| anyhow::anyhow!("copilot-sdk prompt failed"))?;
                if id.is_empty() { bail!("copilot-sdk prompt returned an empty message ID"); }
                for (event, deferred) in pending.drain(..) {
                    correlation.observe(&event, &id)?;
                    if deferred { emit_turn_event(&event, Some(&id), &mut streamed_messages, handler); }
                }
                message_id = Some(id);
            }
            event = events.recv() => {
                let event = event.map_err(|_| anyhow::anyhow!("copilot-sdk event stream closed or lagged"))?;
                if !cancelled { deadline.as_mut().reset(tokio::time::Instant::now() + response_timeout); }
                if is_root(&event) && event.event_type == "session.warning" && event.data.get("warningType").and_then(Value::as_str) == Some("policy_blocked") {
                    bail!("copilot-sdk request blocked by policy");
                }
                let deferred = message_id.is_none() && (event.event_type == "assistant.message"
                    || (event.event_type.starts_with("assistant.")
                        && event.data.get("originatingMessageId").and_then(Value::as_str).is_some_and(|origin| !origin.is_empty())));
                if let Some(id) = &message_id { correlation.observe(&event, id)?; }
                else {
                    if pending.len() >= 4096 { bail!("copilot-sdk prompt emitted too many events before acknowledgement"); }
                    pending.push((event.clone(), deferred));
                }
                if let Some(id) = event.data.get("toolCallId").and_then(Value::as_str) {
                    let key = (event.agent_id.clone(), id.to_owned());
                    match event.event_type.as_str() {
                        "tool.execution_start" => { active_tools.insert(key); }
                        "tool.execution_complete" => { active_tools.remove(&key); }
                        _ => {}
                    }
                }
                if !deferred { emit_turn_event(&event, message_id.as_deref(), &mut streamed_messages, handler); }
            }
            _ = cancellation.cancelled(), if !cancelled => {
                cancelled = true;
                let until = tokio::time::Instant::now() + Duration::from_secs(watchdog.cancel_timeout_seconds);
                cancellation_deadline = Some(until);
                deadline.as_mut().reset(until);
            }
            result = async { abort.as_mut().unwrap().await }, if abort.is_some() => {
                result.map_err(|_| anyhow::anyhow!("copilot-sdk abort failed"))?;
                abort = None;
                abort_finished = true;
            }
            _ = &mut deadline => {
                if !cancelled && !active_tools.is_empty() {
                    deadline.as_mut().reset(tokio::time::Instant::now() + response_timeout);
                } else if cancelled { bail!("copilot-sdk cancellation timed out"); }
                else { bail!("copilot-sdk prompt timed out"); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, data: Value, agent_id: Option<&str>) -> SessionEvent {
        serde_json::from_value(json!({"id":"e","timestamp":"2026-01-01T00:00:00Z","parentId":null,"type":kind,"data":data,"agentId":agent_id})).unwrap()
    }

    #[test]
    fn correlation_replays_pre_ack_events_in_order() {
        let pending = [
            event("session.idle", json!({}), None),
            event("user.message", json!({"messageId":"current"}), Some("")),
            event(
                "assistant.message",
                json!({"originatingMessageId":"older"}),
                None,
            ),
            event("session.idle", json!({"mode":"autopilot"}), None),
            event("assistant.message", json!({}), None),
            event("session.idle", json!({}), Some("child")),
            event("session.idle", json!({}), Some("")),
        ];
        let mut correlation = TurnCorrelation::default();
        for (index, event) in pending.iter().enumerate() {
            correlation.observe(event, "current").unwrap();
            assert_eq!(correlation.completion.is_some(), index == pending.len() - 1);
        }
        assert!(matches!(correlation.completion, Some(StopReason::EndTurn)));
    }

    #[test]
    fn autopilot_and_policy_correlation() {
        let mut correlation = TurnCorrelation::default();
        correlation
            .observe(
                &event(
                    "session.autopilot_stopped",
                    json!({"requestId":"old","reason":"task_complete"}),
                    None,
                ),
                "current",
            )
            .unwrap();
        assert!(correlation.completion.is_none());
        correlation
            .observe(
                &event(
                    "session.autopilot_stopped",
                    json!({"requestId":"current","reason":"task_complete"}),
                    None,
                ),
                "current",
            )
            .unwrap();
        assert!(matches!(correlation.completion, Some(StopReason::EndTurn)));
        let mut correlation = TurnCorrelation::default();
        assert!(
            correlation
                .observe(
                    &event(
                        "session.warning",
                        json!({"warningType":"policy_blocked"}),
                        None
                    ),
                    "current"
                )
                .is_err()
        );
    }
}
