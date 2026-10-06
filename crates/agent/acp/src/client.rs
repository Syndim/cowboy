use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use cowboy_agent_client::{
    AgentInfo, AgentSessionDescriptor, Event, ModelInfo, PromptContent, PromptTurnCancellation,
    StopReason,
};

use super::messages::*;
use super::transport::{Transport, TransportConfig};
use async_trait::async_trait;

const CONTINUE_PROMPT: &str = "Continue";
const DENIED_TOOL_ERROR: &str = "ACP tool use is disabled for this agent";

/// Agent tool and permission boundary. The backend must enforce its own sandbox.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AccessValue", into = "AccessValue")]
pub enum AgentAccess {
    #[default]
    Default,
    DenyAll,
    DenyEscalation {
        acp_mode: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AccessValue {
    Name(AccessName),
    Restricted(RestrictedAccess),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AccessName {
    Default,
    DenyAll,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestrictedAccess {
    mode: RestrictedMode,
    acp_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RestrictedMode {
    DenyEscalation,
}

impl TryFrom<AccessValue> for AgentAccess {
    type Error = &'static str;

    fn try_from(value: AccessValue) -> Result<Self, Self::Error> {
        match value {
            AccessValue::Name(AccessName::Default) => Ok(Self::Default),
            AccessValue::Name(AccessName::DenyAll) => Ok(Self::DenyAll),
            AccessValue::Restricted(RestrictedAccess { acp_mode, .. })
                if !acp_mode.trim().is_empty() =>
            {
                Ok(Self::DenyEscalation { acp_mode })
            }
            _ => Err("deny_escalation acp_mode must not be blank"),
        }
    }
}

impl From<AgentAccess> for AccessValue {
    fn from(access: AgentAccess) -> Self {
        match access {
            AgentAccess::Default => Self::Name(AccessName::Default),
            AgentAccess::DenyAll => Self::Name(AccessName::DenyAll),
            AgentAccess::DenyEscalation { acp_mode } => Self::Restricted(RestrictedAccess {
                mode: RestrictedMode::DenyEscalation,
                acp_mode,
            }),
        }
    }
}

impl AgentAccess {
    fn allows_tools(&self) -> bool {
        !matches!(self, Self::DenyAll)
    }

    fn denies_permissions(&self) -> bool {
        !matches!(self, Self::Default)
    }

    fn required_mode(&self) -> Option<&str> {
        match self {
            Self::DenyEscalation { acp_mode } => Some(acp_mode),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentWatchdogOptions {
    pub response_timeout_seconds: u64,
    pub cancel_timeout_seconds: u64,
    pub recovery_operation_timeout_seconds: u64,
}

impl Default for AgentWatchdogOptions {
    fn default() -> Self {
        Self {
            response_timeout_seconds: 100,
            cancel_timeout_seconds: 10,
            recovery_operation_timeout_seconds: 30,
        }
    }
}

/// ACP client — manages JSON-RPC communication with a single agent through the Transport abstraction.
///
/// The orchestrator acts as the ACP client; each agent subprocess is an ACP server.
/// Communication uses JSON-RPC 2.0, with the Transport trait abstracting the underlying I/O.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    /// Underlying transport (stdio, Zellij, etc.)
    #[serde(skip)]
    transport: Option<Box<dyn Transport>>,
    /// Transport config for reconnecting after deserialization
    transport_config: TransportConfig,
    /// JSON-RPC request ID counter (monotonically increasing)
    next_id: u64,
    /// Capabilities advertised by the agent (from the initialize response)
    pub agent_capabilities: Option<Value>,
    /// Agent information (name, version, etc.)
    pub agent_info: Option<AgentInfo>,
    /// Current ACP session ID (set by new_session / load_session)
    session_id: Option<String>,
    /// Session being loaded; not verified for prompting until load succeeds.
    #[serde(skip)]
    pending_session_id: Option<String>,
    /// Parameters needed to re-register the current session after a watchdog
    /// replaces the ACP server process.
    #[serde(skip)]
    session_load_context: Option<SessionLoadContext>,
    /// Push-back buffer for messages consumed during trailing event drain
    #[serde(skip)]
    pushback: Vec<String>,
    /// Agent-returned session descriptor captured from `session/new`,
    /// `session/load`, or post-`set_config_option` config options; never
    /// derived from the configured `ModelInfo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_descriptor: Option<AgentSessionDescriptor>,
    #[serde(default)]
    watchdog: AgentWatchdogOptions,
    #[serde(default)]
    access: AgentAccess,
    #[serde(default)]
    tool_policy_violated: bool,
    #[serde(skip)]
    policy_rejected: bool,
    #[serde(skip, default = "cleanup_verified_default")]
    cleanup_verified: bool,
    /// Workflow-agent policy; selectors and legacy clients keep it disabled.
    #[serde(skip)]
    agent_human_input: bool,
    #[serde(skip)]
    native_owner: Option<String>,
    #[serde(skip)]
    native_scope: Option<String>,
    #[serde(skip)]
    native_verified: Option<cowboy_agent_client::VerifiedProcessScope>,
    #[cfg(test)]
    #[serde(skip)]
    replacement_factory: ReplacementTransportFactory,
    #[cfg(test)]
    #[serde(skip)]
    reconnect_factory: ReplacementTransportFactory,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedTransportCreation {
    clear_env: bool,
    allowed_env: Vec<String>,
    resume_session_id: Option<String>,
}

#[cfg(test)]
fn transport_creation_records() -> &'static std::sync::Mutex<Vec<RecordedTransportCreation>> {
    static RECORDS: std::sync::OnceLock<std::sync::Mutex<Vec<RecordedTransportCreation>>> =
        std::sync::OnceLock::new();
    RECORDS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

#[cfg(test)]
fn record_transport_creation(config: &TransportConfig, resume_session_id: Option<&str>) {
    let TransportConfig::Stdio(config) = config else {
        return;
    };
    if config.command != "definitely-missing-acp-environment-test-command" {
        return;
    }
    transport_creation_records()
        .lock()
        .expect("transport creation recorder poisoned")
        .push(RecordedTransportCreation {
            clear_env: config.clear_env,
            allowed_env: config.allowed_env.clone(),
            resume_session_id: resume_session_id.map(str::to_string),
        });
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            transport: None, // transport is ephemeral, reconnect after clone
            transport_config: self.transport_config.clone(),
            next_id: self.next_id,
            agent_capabilities: self.agent_capabilities.clone(),
            agent_info: self.agent_info.clone(),
            session_id: self.session_id.clone(),
            pending_session_id: None,
            session_load_context: self.session_load_context.clone(),
            pushback: Vec::new(),
            session_descriptor: self.session_descriptor.clone(),
            watchdog: self.watchdog,
            access: self.access.clone(),
            tool_policy_violated: self.tool_policy_violated,
            policy_rejected: self.policy_rejected,
            cleanup_verified: self.cleanup_verified,
            agent_human_input: self.agent_human_input,
            // No transport or receipt is cloned, but the owner still forbids
            // an unregistered reconnect from this detached handle.
            native_owner: self.native_owner.clone(),
            native_scope: None,
            native_verified: None,
            #[cfg(test)]
            replacement_factory: ReplacementTransportFactory::default(),
            #[cfg(test)]
            reconnect_factory: ReplacementTransportFactory::default(),
        }
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("connected", &self.transport.is_some())
            .field("transport_config", &self.transport_config)
            .field("next_id", &self.next_id)
            .field("session_id", &self.session_id)
            .field("agent_info", &self.agent_info)
            .field("session_descriptor", &self.session_descriptor)
            .field("watchdog", &self.watchdog)
            .field("access", &self.access)
            .field("tool_policy_violated", &self.tool_policy_violated)
            .finish()
    }
}

#[derive(Clone, Debug)]
struct SessionLoadContext {
    cwd: String,
    mcp_servers: Vec<Value>,
}

#[derive(Debug)]
struct PromptTurnOutcome {
    stop_reason: StopReason,
    activity: PromptTurnActivity,
}

impl PromptTurnOutcome {
    fn should_continue(&self) -> bool {
        matches!(&self.stop_reason, StopReason::EndTurn) && self.activity.should_continue()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptTurnActivity {
    Empty,
    AgentProgress,
    PermissionExchange,
    Text,
}

impl PromptTurnActivity {
    fn observe_event(&mut self, event: &Event) {
        match event {
            Event::MessageChunk { .. } => *self = Self::Text,
            Event::ToolCall { .. } | Event::ThoughtChunk { .. } if matches!(self, Self::Empty) => {
                *self = Self::AgentProgress;
            }
            _ => {}
        }
    }

    fn observe_permission_request(&mut self) {
        if !matches!(self, Self::Text) {
            *self = Self::PermissionExchange;
        }
    }

    fn observe_trailing_text(&mut self, saw_text: bool) {
        if saw_text {
            *self = Self::Text;
        }
    }

    fn should_continue(self) -> bool {
        matches!(self, Self::Empty | Self::AgentProgress)
    }
}

/// Most recent interesting ACP activity observed inside a single prompt turn.
///
/// Interpretation at a watchdog inactivity deadline: a tool call still in
/// flight (`pending` / `in_progress`) means the agent is waiting on a
/// long-running job, so the conversation is alive and must not be interrupted;
/// no tool call at all, or one that already reached `completed` / `failed`
/// followed by silence, means the conversation itself is likely stuck and is
/// what the watchdog exists to recover.
#[derive(Debug, Default, Clone)]
struct LastObservedActivity {
    event_kind: Option<&'static str>,
    observed_at: Option<tokio::time::Instant>,
    tool_call: Option<ObservedToolCall>,
}

#[derive(Debug, Clone)]
struct ObservedToolCall {
    tool_call_id: String,
    title: String,
    kind: String,
    status: String,
    in_flight_since: Option<tokio::time::Instant>,
}

fn tool_call_status_is_in_flight(status: &str) -> bool {
    matches!(status, "pending" | "in_progress")
}

impl LastObservedActivity {
    fn observe_event(&mut self, event: &Event) {
        let now = tokio::time::Instant::now();
        self.event_kind = Some(event_kind(event));
        self.observed_at = Some(now);
        match event {
            Event::ToolCall {
                tool_call_id,
                title,
                kind,
                status,
            } => {
                let in_flight_since = self.carry_in_flight_since(tool_call_id, status, now);
                self.tool_call = Some(ObservedToolCall {
                    tool_call_id: tool_call_id.clone(),
                    title: title.clone(),
                    kind: kind.clone(),
                    status: status.clone(),
                    in_flight_since,
                });
            }
            Event::ToolCallUpdate {
                tool_call_id,
                status,
                ..
            } => {
                let in_flight_since = self.carry_in_flight_since(tool_call_id, status, now);
                // An update carries no title/kind, so keep the ones reported by
                // the originating tool call rather than degrading the
                // diagnostic to a bare id while progress is streaming.
                let (title, kind) = self
                    .tool_call
                    .as_ref()
                    .filter(|previous| previous.tool_call_id == *tool_call_id)
                    .map(|previous| (previous.title.clone(), previous.kind.clone()))
                    .unwrap_or_default();
                self.tool_call = Some(ObservedToolCall {
                    tool_call_id: tool_call_id.clone(),
                    title,
                    kind,
                    status: status.clone(),
                    in_flight_since,
                });
            }
            _ => {}
        }
    }

    fn carry_in_flight_since(
        &self,
        tool_call_id: &str,
        status: &str,
        now: tokio::time::Instant,
    ) -> Option<tokio::time::Instant> {
        if !tool_call_status_is_in_flight(status) {
            return None;
        }
        self.tool_call
            .as_ref()
            .filter(|previous| previous.tool_call_id == tool_call_id)
            .and_then(|previous| previous.in_flight_since)
            .or(Some(now))
    }

    fn tool_call_in_flight(&self) -> bool {
        self.tool_call
            .as_ref()
            .is_some_and(|tool_call| tool_call_status_is_in_flight(&tool_call.status))
    }

    fn last_event_kind(&self) -> Option<&'static str> {
        self.event_kind
    }

    fn last_tool_call_title(&self) -> Option<&str> {
        self.tool_call.as_ref().map(|call| call.title.as_str())
    }

    fn last_tool_call_kind(&self) -> Option<&str> {
        self.tool_call.as_ref().map(|call| call.kind.as_str())
    }

    fn last_tool_call_status(&self) -> Option<&str> {
        self.tool_call.as_ref().map(|call| call.status.as_str())
    }

    fn seconds_since_last_activity(&self) -> Option<u64> {
        self.observed_at
            .map(|observed| observed.elapsed().as_secs())
    }

    fn tool_wait_seconds(&self) -> Option<u64> {
        self.tool_call
            .as_ref()
            .and_then(|call| call.in_flight_since)
            .map(|since| since.elapsed().as_secs())
    }
}

fn prompt_content_stats(content: &[PromptContent]) -> (usize, usize) {
    let text_chars = content.iter().map(|part| part.text.chars().count()).sum();
    (content.len(), text_chars)
}

fn transport_kind(config: &TransportConfig) -> &'static str {
    match config {
        TransportConfig::Stdio(_) => "stdio",
        TransportConfig::Zellij(_) => "zellij",
        #[cfg(test)]
        TransportConfig::Mock(_) => "mock",
    }
}

fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::MessageChunk { .. } => "message_chunk",
        Event::ThoughtChunk { .. } => "thought_chunk",
        Event::ToolCall { .. } => "tool_call",
        Event::ToolCallUpdate { .. } => "tool_call_update",
        Event::Plan { .. } => "plan",
        Event::UserMessageChunk { .. } => "user_message_chunk",
        Event::Unknown { .. } => "unknown",
    }
}

fn log_acp_message(direction: &'static str, msg: &Message) {
    match msg {
        Message::Response { id, result, error } => {
            tracing::debug!(
                direction,
                kind = "response",
                id,
                has_result = result.is_some(),
                has_error = error.is_some(),
                error = ?error.as_ref().map(|value| value.to_string()),
                "ACP message parsed"
            );
        }
        Message::SessionUpdate { session_id, update } => {
            tracing::debug!(
                direction,
                kind = "session_update",
                session_id,
                update = event_kind(update),
                "ACP message parsed"
            );
        }
        Message::PermissionRequest {
            id,
            session_id,
            tool_call,
            options,
        } => {
            tracing::debug!(
                direction,
                kind = "permission_request",
                %id,
                session_id,
                tool_kind = ?tool_call.get("kind").and_then(|value| value.as_str()),
                tool_title = ?tool_call.get("title").and_then(|value| value.as_str()),
                options = options.len(),
                "ACP message parsed"
            );
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct ReplacementTransportFactory {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    outcomes: std::sync::Arc<
        parking_lot::Mutex<std::collections::VecDeque<ReplacementTransportFactoryOutcome>>,
    >,
}

#[cfg(test)]
enum ReplacementTransportFactoryOutcome {
    Ready(Box<dyn Transport>),
    Error(&'static str),
    Pending,
}

#[cfg(test)]
impl ReplacementTransportFactory {
    fn push(&self, outcome: ReplacementTransportFactoryOutcome) {
        self.outcomes.lock().push_back(outcome);
    }

    fn next(&self) -> Option<ReplacementTransportFactoryOutcome> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.outcomes.lock().pop_front()
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
impl ReplacementTransportFactoryOutcome {
    async fn into_transport(self) -> anyhow::Result<Box<dyn Transport>> {
        match self {
            Self::Ready(transport) => Ok(transport),
            Self::Error(error) => anyhow::bail!("{error}"),
            Self::Pending => std::future::pending().await,
        }
    }
}

/// Initialization failed, but its original owned process scope was reaped and inspected empty.
#[derive(Debug)]
pub struct VerifiedConnectFailure {
    source: anyhow::Error,
    shutdown: cowboy_agent_client::VerifiedClientShutdown,
}

impl VerifiedConnectFailure {
    pub fn shutdown(&self) -> &cowboy_agent_client::VerifiedClientShutdown {
        &self.shutdown
    }
}

impl std::fmt::Display for VerifiedConnectFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.source)
    }
}

impl std::error::Error for VerifiedConnectFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn cleanup_verified_default() -> bool {
    true
}

impl Client {
    /// Get a mutable reference to the existing transport (no reconnect).
    fn transport_mut(&mut self) -> anyhow::Result<&mut Box<dyn Transport>> {
        if self.tool_policy_violated {
            anyhow::bail!(DENIED_TOOL_ERROR);
        }

        self.transport
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("client transport not connected"))
    }

    /// Get a mutable reference to the transport, reconnecting if needed.
    async fn ensure_transport(&mut self) -> anyhow::Result<&mut Box<dyn Transport>> {
        if self.tool_policy_violated {
            anyhow::bail!(DENIED_TOOL_ERROR);
        }

        if self.transport.is_none() {
            tracing::info!(
                transport = transport_kind(&self.transport_config),
                session_id = ?self.session_id,
                "Reconnecting client transport"
            );
            let transport = self.create_reconnect_transport().await?;
            self.transport = Some(transport);
            self.pushback.clear();
            self.initialize_bounded("ACP lazy reconnect initialization")
                .await?;
            tracing::info!(
                transport = transport_kind(&self.transport_config),
                session_id = ?self.session_id,
                "Client transport reconnected"
            );
        }
        Ok(self.transport.as_mut().unwrap())
    }

    /// Create a transport from config, optionally with session resume.
    async fn create_transport(
        config: &TransportConfig,
        resume_session_id: Option<&str>,
        owned_scope: Option<&str>,
    ) -> anyhow::Result<Box<dyn Transport>> {
        #[cfg(test)]
        record_transport_creation(config, resume_session_id);
        match config {
            TransportConfig::Stdio(cfg) => {
                use super::transport::stdio::StdioTransport;
                let resume_arg =
                    resume_session_id.map(|session_id| format!("--resume={session_id}"));
                let additional_args = resume_arg.as_deref().into_iter().collect::<Vec<_>>();
                let transport = if let Some(scope) = owned_scope {
                    StdioTransport::connect_owned(cfg, &additional_args, scope.to_string()).await?
                } else {
                    StdioTransport::connect(cfg, &additional_args).await?
                };
                Ok(Box::new(transport) as Box<dyn Transport>)
            }

            TransportConfig::Zellij(cfg) => {
                use super::transport::zellij::ZellijTransport;
                let transport = ZellijTransport::connect(cfg, resume_session_id).await?;
                Ok(Box::new(transport) as Box<dyn Transport>)
            }
            #[cfg(test)]
            TransportConfig::Mock(cfg) => {
                Ok(Box::new(super::transport::MockTransport::new(cfg)) as Box<dyn Transport>)
            }
        }
    }

    async fn create_replacement_transport(
        &mut self,
        session_id: &str,
    ) -> anyhow::Result<Box<dyn Transport>> {
        if self.tool_policy_violated {
            anyhow::bail!(DENIED_TOOL_ERROR);
        }

        if self.native_owner.is_some() {
            anyhow::bail!("owned ACP replacement requires a new durable writer generation");
        }

        #[cfg(test)]
        if let Some(outcome) = self.replacement_factory.next() {
            return outcome.into_transport().await;
        }
        Self::create_transport(&self.transport_config, Some(session_id), None).await
    }

    async fn create_reconnect_transport(&mut self) -> anyhow::Result<Box<dyn Transport>> {
        if self.tool_policy_violated {
            anyhow::bail!(DENIED_TOOL_ERROR);
        }

        if self.native_owner.is_some() {
            anyhow::bail!("owned ACP reconnect requires a new durable writer generation");
        }

        #[cfg(test)]
        if let Some(outcome) = self.reconnect_factory.next() {
            return outcome.into_transport().await;
        }
        Self::create_transport(&self.transport_config, self.session_id.as_deref(), None).await
    }

    /// Run the ACP initialize handshake on the current transport.
    ///
    /// Uses `transport_mut()` directly instead of `send_request()` to avoid
    /// async recursion: send_request -> ensure_transport -> initialize.
    async fn initialize(&mut self) -> anyhow::Result<()> {
        let init_params = InitializeParams {
            protocol_version: 1,
            client_capabilities: build_client_capabilities(),
            client_info: ClientInfo {
                name: "cowboy",
                title: "Cowboy Orchestrator",
                version: env!("CARGO_PKG_VERSION"),
            },
        };

        // Send initialize request directly (transport must already be set)
        let id = self.next_id;
        self.next_id += 1;
        tracing::debug!(
            id,
            protocol_version = 1,
            client = "cowboy",
            "ACP initialize starting"
        );
        let request = JsonRpcRequest::new(id, "initialize", init_params);
        let line = serde_json::to_string(&request)?;
        tracing::debug!(id, method = "initialize", payload = %line, "ACP >>> request");
        self.transport_mut()?.send(&line).await?;

        // Wait for response directly
        loop {
            let msg = self.recv_message_direct().await?;
            match msg {
                Message::Response {
                    id: resp_id,
                    result,
                    error,
                } if resp_id == id => {
                    if let Some(err) = error {
                        anyhow::bail!("RPC error on 'initialize': {err}");
                    }
                    let init: InitializeResult =
                        serde_json::from_value(result.unwrap_or(serde_json::Value::Null))?;
                    self.agent_capabilities = init.agent_capabilities;
                    self.agent_info = init.agent_info;
                    tracing::info!(
                        agent = ?self.agent_info,
                        capabilities = ?self.agent_capabilities,
                        "ACP connection initialized"
                    );
                    return Ok(());
                }
                _ => {} // skip non-matching messages during init
            }
        }
    }

    async fn initialize_bounded(&mut self, operation: &'static str) -> anyhow::Result<()> {
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let initialization_error = match tokio::time::timeout(timeout, self.initialize()).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => anyhow::anyhow!("{operation} failed: {error}"),
            Err(_) => anyhow::anyhow!("{operation} timed out"),
        };

        Err(self
            .dispose_failed_initialization(initialization_error)
            .await)
    }

    async fn dispose_failed_initialization(
        &mut self,
        initialization_error: anyhow::Error,
    ) -> anyhow::Error {
        let Some(mut transport) = self.transport.take() else {
            return initialization_error;
        };
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let primary_error = initialization_error.to_string();

        let termination = async {
            if self.agent_human_input {
                transport.force_terminate_verified().await
            } else {
                transport.force_terminate().await
            }
        };
        let cleanup_error = match tokio::time::timeout(timeout, termination).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => {
                tracing::error!(
                    error = %error,
                    "Transport force termination failed after ACP initialization failure"
                );
                Some(format!("transport force termination failed: {error}"))
            }
            Err(_) => {
                tracing::error!(
                    "Transport force termination timed out after ACP initialization failure"
                );
                Some("transport force termination timed out".to_string())
            }
        };
        if cleanup_error.is_none() && self.native_owner.is_some() {
            self.native_verified = transport.verified_shutdown_scope();
        }

        drop(transport);
        if cleanup_error.is_some() {
            self.cleanup_verified = false;
        }

        match cleanup_error {
            Some(cleanup_error) => cowboy_agent_client::AgentSafetyError::CleanupUnverified(
                format!("{primary_error}; {cleanup_error}"),
            )
            .into(),
            None if self.policy_rejected => {
                cowboy_agent_client::AgentSafetyError::PolicyRejected(primary_error).into()
            }
            None => initialization_error,
        }
    }

    /// Connect to the agent with TransportConfig and complete the ACP initialize handshake.
    pub async fn connect(transport_config: TransportConfig) -> anyhow::Result<Self> {
        Self::connect_with_options(transport_config, AgentWatchdogOptions::default()).await
    }

    pub async fn connect_with_options(
        transport_config: TransportConfig,
        watchdog: AgentWatchdogOptions,
    ) -> anyhow::Result<Self> {
        Self::connect_with_options_and_access(transport_config, watchdog, AgentAccess::Default)
            .await
    }

    pub async fn connect_with_options_and_access(
        transport_config: TransportConfig,
        watchdog: AgentWatchdogOptions,
        access: AgentAccess,
    ) -> anyhow::Result<Self> {
        Self::connect_with_options_access_and_human_input(transport_config, watchdog, access, false)
            .await
    }

    pub async fn connect_with_options_access_and_human_input(
        transport_config: TransportConfig,
        watchdog: AgentWatchdogOptions,
        access: AgentAccess,
        agent_human_input: bool,
    ) -> anyhow::Result<Self> {
        let transport = Self::create_transport(&transport_config, None, None).await?;
        Self::connect_with_transport_options_access_and_human_input(
            transport,
            transport_config,
            watchdog,
            access,
            agent_human_input,
        )
        .await
    }

    /// The caller must durably register this random owner token before ACP is spawned.
    pub async fn connect_owned(
        transport_config: TransportConfig,
        watchdog: AgentWatchdogOptions,
        access: AgentAccess,
        owner_token: String,
    ) -> anyhow::Result<Self> {
        let transport = Self::create_transport(&transport_config, None, Some(&owner_token)).await?;
        let mut client = Self::uninitialized(
            transport,
            transport_config,
            watchdog,
            access,
            true,
            Some(owner_token.clone()),
        );
        if let Err(source) = client.initialize_bounded("ACP initialize").await {
            if client.cleanup_verified
                && let Some(scope) = client.native_verified.take()
                && scope.scope_id == owner_token
            {
                return Err(VerifiedConnectFailure {
                    source,
                    shutdown: cowboy_agent_client::VerifiedClientShutdown {
                        owner_token,
                        session_id: None,
                        scopes: vec![scope],
                    },
                }
                .into());
            }

            return Err(source);
        }

        Ok(client)
    }

    /// Connect using a pre-built transport (for tests or custom transports).
    pub async fn connect_with_transport(
        transport: Box<dyn Transport>,
        transport_config: TransportConfig,
    ) -> anyhow::Result<Self> {
        Self::connect_with_transport_and_options(
            transport,
            transport_config,
            AgentWatchdogOptions::default(),
        )
        .await
    }

    pub async fn connect_with_transport_and_options(
        transport: Box<dyn Transport>,
        transport_config: TransportConfig,
        watchdog: AgentWatchdogOptions,
    ) -> anyhow::Result<Self> {
        Self::connect_with_transport_and_options_and_access(
            transport,
            transport_config,
            watchdog,
            AgentAccess::Default,
        )
        .await
    }

    pub async fn connect_with_transport_and_options_and_access(
        transport: Box<dyn Transport>,
        transport_config: TransportConfig,
        watchdog: AgentWatchdogOptions,
        access: AgentAccess,
    ) -> anyhow::Result<Self> {
        Self::connect_with_transport_options_access_and_human_input(
            transport,
            transport_config,
            watchdog,
            access,
            false,
        )
        .await
    }

    pub async fn connect_with_transport_options_access_and_human_input(
        transport: Box<dyn Transport>,
        transport_config: TransportConfig,
        watchdog: AgentWatchdogOptions,
        access: AgentAccess,
        agent_human_input: bool,
    ) -> anyhow::Result<Self> {
        let mut client = Self::uninitialized(
            transport,
            transport_config,
            watchdog,
            access,
            agent_human_input,
            None,
        );
        client.initialize_bounded("ACP initialize").await?;
        Ok(client)
    }

    fn uninitialized(
        transport: Box<dyn Transport>,
        transport_config: TransportConfig,
        watchdog: AgentWatchdogOptions,
        access: AgentAccess,
        agent_human_input: bool,
        native_owner: Option<String>,
    ) -> Self {
        let native_scope = transport.scope_id().map(str::to_string);
        Self {
            transport: Some(transport),
            transport_config,
            next_id: 0,
            agent_capabilities: None,
            agent_info: None,
            session_id: None,
            pending_session_id: None,
            session_load_context: None,
            pushback: Vec::new(),
            session_descriptor: None,
            watchdog,
            access,
            tool_policy_violated: false,
            policy_rejected: false,
            cleanup_verified: true,
            agent_human_input,
            native_owner,
            native_scope,
            native_verified: None,
            #[cfg(test)]
            replacement_factory: ReplacementTransportFactory::default(),
            #[cfg(test)]
            reconnect_factory: ReplacementTransportFactory::default(),
        }
    }

    /// Set the workflow policy only before starting a backend session.
    pub fn set_agent_human_input(&mut self, enabled: bool) {
        self.agent_human_input = enabled;
    }

    #[cfg(test)]
    fn push_replacement_transport(&mut self, transport: Box<dyn Transport>) {
        self.replacement_factory
            .push(ReplacementTransportFactoryOutcome::Ready(transport));
    }

    #[cfg(test)]
    fn push_reconnect_transport(&mut self, transport: Box<dyn Transport>) {
        self.reconnect_factory
            .push(ReplacementTransportFactoryOutcome::Ready(transport));
    }

    #[cfg(test)]
    fn replacement_factory_calls(&self) -> usize {
        self.replacement_factory.calls()
    }

    #[cfg(test)]
    fn push_replacement_creation_error(&mut self, error: &'static str) {
        self.replacement_factory
            .push(ReplacementTransportFactoryOutcome::Error(error));
    }

    #[cfg(test)]
    fn push_replacement_creation_pending(&mut self) {
        self.replacement_factory
            .push(ReplacementTransportFactoryOutcome::Pending);
    }

    /// Whether the transport is connected.
    pub fn is_connected(&self) -> bool {
        self.transport.is_some()
    }

    pub fn watchdog_options(&self) -> AgentWatchdogOptions {
        self.watchdog
    }

    /// Changing access revokes any previously verified ACP session.
    pub fn set_access(&mut self, access: AgentAccess) {
        if self.access != access {
            self.session_id = None;
            self.pending_session_id = None;
            self.session_descriptor = None;
            self.session_load_context = None;
        }

        self.access = access;
    }

    fn validate_required_acp_mode(
        &self,
        mode_state: Option<&SessionModeState>,
        config_options: &[SessionConfigOption],
    ) -> anyhow::Result<()> {
        let Some(required) = self.access.required_mode() else {
            return Ok(());
        };
        if required.trim().is_empty() {
            anyhow::bail!("ACP required mode must not be blank");
        }

        let reported_state = mode_state.map(|state| state.current_mode_id.as_str());
        let mut reported_config = false;
        for option in config_options.iter().filter(|option| option.id == "mode") {
            reported_config = true;
            if option.current_value.as_str() != Some(required) {
                anyhow::bail!("ACP reported session mode differs from required mode");
            }
        }

        if reported_state.is_none() && !reported_config {
            anyhow::bail!("ACP agent did not report required session mode");
        }

        if reported_state.is_some_and(|mode| mode != required) {
            anyhow::bail!("ACP reported session mode differs from required mode");
        }

        Ok(())
    }

    /// Create a new ACP session.
    ///
    /// When a model is configured, sends `_meta.model` as a compatibility hint and
    /// applies it through ACP session config options when the agent exposes a model
    /// selector. Without a configured model, the agent keeps its own default.
    pub async fn new_session(
        &mut self,
        cwd: &str,
        mcp_servers: &[Value],
        model: Option<&ModelInfo>,
    ) -> anyhow::Result<String> {
        if self.access.required_mode().is_some() {
            self.session_id = None;
            self.pending_session_id = None;
        }

        tracing::debug!(
            cwd,
            mcp_server_count = mcp_servers.len(),
            model_id = ?model.map(|model| model.id.as_str()),
            provider = ?model.and_then(|model| model.provider.as_deref()),
            "ACP session/new starting"
        );
        let params = SessionNewParams {
            cwd: cwd.to_string(),
            mcp_servers: mcp_servers.to_vec(),
            meta: model.map(|model| SessionMeta {
                model: SessionModelMeta {
                    id: model.id.clone(),
                    provider: model.provider.clone(),
                },
            }),
        };

        let result = self.send_request("session/new", params).await?;
        let session: SessionNewResult = match serde_json::from_value(result) {
            Ok(session) => session,
            Err(error) if self.access.required_mode().is_some() => {
                self.tool_policy_violated = true;
                self.policy_rejected = true;
                self.cleanup_replacement().await;
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
        };
        if let Err(error) =
            self.validate_required_acp_mode(session.mode_state.as_ref(), &session.config_options)
        {
            self.tool_policy_violated = true;
            self.policy_rejected = true;
            self.cleanup_replacement().await;
            return Err(error);
        }
        if self.access.required_mode().is_some() {
            self.pending_session_id = Some(session.session_id.clone());
        }

        let mut descriptor_options = session.config_options.clone();
        if let Some(model) = model
            && let Some(applied_options) = self
                .apply_model_config_option(&session.session_id, &session.config_options, model)
                .await?
        {
            // A returned configOptions snapshot supersedes session/new. Do not
            // treat the earlier modeState or mode option as fresh evidence.
            if let Err(error) = self.validate_required_acp_mode(None, &applied_options) {
                self.tool_policy_violated = true;
                self.policy_rejected = true;
                self.cleanup_replacement().await;
                return Err(error);
            }

            descriptor_options = applied_options;
        }
        self.pending_session_id = None;

        self.session_descriptor = Self::descriptor_from_config_options(&descriptor_options);
        self.session_id = Some(session.session_id.clone());
        self.session_load_context = Some(SessionLoadContext {
            cwd: cwd.to_string(),
            mcp_servers: mcp_servers.to_vec(),
        });
        tracing::info!(
            session_id = %session.session_id,
            model_id = ?model.map(|model| model.id.as_str()),
            provider = ?model.and_then(|model| model.provider.as_deref()),
            "ACP session created"
        );
        Ok(session.session_id)
    }

    async fn apply_model_config_option(
        &mut self,
        session_id: &str,
        config_options: &[SessionConfigOption],
        model: &ModelInfo,
    ) -> anyhow::Result<Option<Vec<SessionConfigOption>>> {
        let model_option = config_options
            .iter()
            .find(|option| option.category.as_deref() == Some("model"))
            .or_else(|| config_options.iter().find(|option| option.id == "model"));
        let Some(model_option) = model_option else {
            tracing::debug!(
                session_id,
                model_id = %model.id,
                provider = ?model.provider,
                "ACP agent exposes no model config option; relying on session metadata"
            );
            return Ok(None);
        };

        if model_option
            .current_value
            .as_str()
            .is_some_and(|value| Self::model_value_matches(value, model))
        {
            return Ok(None);
        }

        let Some(value) = model_option
            .options
            .iter()
            .map(|option| option.value.as_str())
            .find(|value| Self::model_value_matches(value, model))
        else {
            let available = model_option
                .options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "ACP agent does not offer configured model id '{}' for provider {:?}; available values: [{}]",
                model.id,
                model.provider,
                available
            );
        };

        tracing::debug!(
            session_id,
            config_id = %model_option.id,
            model_value = value,
            "ACP session model selection starting"
        );
        let params = SetSessionConfigOptionParams {
            session_id,
            config_id: &model_option.id,
            value,
        };
        let result = self
            .send_request("session/set_config_option", params)
            .await?;
        let result: SetSessionConfigOptionResult = serde_json::from_value(result)?;
        let applied_value = result
            .config_options
            .iter()
            .find(|option| option.id == model_option.id)
            .and_then(|option| option.current_value.as_str());

        if !applied_value.is_some_and(|value| Self::model_value_matches(value, model)) {
            anyhow::bail!(
                "ACP agent did not apply configured model id '{}' for provider {:?}; reported value: {:?}",
                model.id,
                model.provider,
                applied_value
            );
        }

        tracing::info!(
            session_id,
            config_id = %model_option.id,
            model_value = applied_value,
            "ACP session model configured"
        );
        Ok(Some(result.config_options))
    }

    fn model_value_matches(value: &str, model: &ModelInfo) -> bool {
        if value == model.id {
            return true;
        }

        let Some(provider) = model.provider.as_deref() else {
            return false;
        };

        value
            .strip_prefix(provider)
            .and_then(|suffix| suffix.strip_prefix('/'))
            == Some(model.id.as_str())
    }

    /// Build an `AgentSessionDescriptor` from agent-returned config options only.
    ///
    /// Reads solely the returned `current_value`/`category`/`id` fields; the
    /// configured `ModelInfo` is never consulted, so the descriptor reflects only
    /// what the agent reported. Returns `None` when no facet is present.
    ///
    /// - Model: `category == "model"`, else `id == "model"`.
    /// - Reasoning: `category == "thought_level"` (ACP-standard reasoning
    ///   category). No backend-specific alias id is recognized without captured
    ///   evidence that it appears in real ACP `configOptions` output.
    /// - Context: semantic context ids only (`context_size`, `context_length`,
    ///   `context_window`), never a blanket `model_config` match.
    fn descriptor_from_config_options(
        config_options: &[SessionConfigOption],
    ) -> Option<AgentSessionDescriptor> {
        fn current_string(option: &SessionConfigOption) -> Option<String> {
            option.current_value.as_str().map(|value| value.to_string())
        }

        let model = config_options
            .iter()
            .find(|option| option.category.as_deref() == Some("model"))
            .or_else(|| config_options.iter().find(|option| option.id == "model"))
            .and_then(current_string);
        let reasoning = config_options
            .iter()
            .find(|option| option.category.as_deref() == Some("thought_level"))
            .and_then(current_string);
        let context = config_options
            .iter()
            .find(|option| {
                matches!(
                    option.id.as_str(),
                    "context_size" | "context_length" | "context_window"
                )
            })
            .and_then(current_string);

        if model.is_none() && reasoning.is_none() && context.is_none() {
            return None;
        }

        Some(AgentSessionDescriptor {
            model,
            context,
            reasoning,
        })
    }

    /// Return the current session ID, if any.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Send a prompt and collect all session/update events until the turn ends.
    ///
    /// Collects streaming session/update notifications and forwards them to event_handler.
    /// Permission requests are granted by default, but cancelled when tool use is denied.
    /// When the matching JSON-RPC response arrives, extracts and returns stopReason.
    ///
    /// Per the ACP spec, `end_turn` means the agent finished its response for
    /// this turn. Some agents split exploration (tool calls + thinking) and
    /// response (agent_message_chunk) into separate turns. When the agent ends
    /// a turn with `end_turn` after using tools but without producing any
    /// `agent_message_chunk` text, we automatically send a "Continue" follow-up
    /// in the same session (up to 5 times) so the agent can produce its final
    /// response.
    async fn commit_automatic_continuation(cancellation: &mut PromptTurnCancellation) -> bool {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => false,
            () = tokio::task::yield_now() => true,
        }
    }

    pub async fn prompt(
        &mut self,
        session_id: &str,
        prompt_content: Vec<PromptContent>,
        mut cancellation: PromptTurnCancellation,
        event_handler: &mut (dyn FnMut(Event) + Send),
    ) -> anyhow::Result<StopReason> {
        if self.tool_policy_violated {
            anyhow::bail!(DENIED_TOOL_ERROR);
        }
        if self.access.required_mode().is_some() {
            if self.session_id.as_deref() != Some(session_id) {
                anyhow::bail!("ACP prompt session has not passed required mode verification");
            }

            if self.transport.is_none() {
                anyhow::bail!(
                    "ACP required mode must be reverified by session/load before reconnecting"
                );
            }
        }

        const MAX_CONTINUATIONS: u32 = 5;
        let mut content = prompt_content;

        for attempt in 0..=MAX_CONTINUATIONS {
            let outcome = self
                .prompt_turn(session_id, content, &mut cancellation, event_handler)
                .await
                .map_err(|err| {
                    if self.tool_policy_violated {
                        anyhow::anyhow!(DENIED_TOOL_ERROR)
                    } else {
                        err
                    }
                })?;

            // Done if the agent produced visible text, stopped for a reason
            // other than a normal end_turn, or completed after a permission
            // exchange. A truly empty end_turn can be a backend acknowledgement
            // rather than a useful answer, even when it only emitted
            // housekeeping updates.
            if !outcome.should_continue() {
                return Ok(outcome.stop_reason);
            }

            if self.agent_human_input {
                anyhow::bail!("agent turn ended without a workflow result");
            }

            if !Self::commit_automatic_continuation(&mut cancellation).await {
                tracing::debug!(
                    session_id,
                    attempt,
                    activity = ?outcome.activity,
                    "ACP prompt cancellation won automatic continuation dispatch"
                );
                return Ok(outcome.stop_reason);
            }

            if attempt == MAX_CONTINUATIONS {
                anyhow::bail!(
                    "ACP prompt received repeated empty end_turn responses after {MAX_CONTINUATIONS} continuation prompts for session {session_id}"
                );
            }

            tracing::info!(
                session_id,
                attempt = attempt + 1,
                activity = ?outcome.activity,
                "Agent ended turn without text output, continuing"
            );
            content = vec![PromptContent::text(CONTINUE_PROMPT)];
        }

        unreachable!("prompt continuation loop always returns or errors")
    }

    async fn send_prompt_turn_cancellation(&mut self, session_id: &str) -> anyhow::Result<()> {
        self.send_notification(
            "session/cancel",
            SessionCancelParams {
                session_id: session_id.to_string(),
            },
        )
        .await
    }

    async fn dispatch_watchdog_continuation(&mut self, session_id: &str) -> anyhow::Result<u64> {
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let params = SessionPromptParams {
            session_id: session_id.to_string(),
            prompt: vec![PromptContent::text(CONTINUE_PROMPT)],
        };
        tokio::time::timeout(timeout, self.send_request_no_wait("session/prompt", params))
            .await
            .map_err(|_| anyhow::anyhow!("agent watchdog continuation dispatch timed out"))?
            .map_err(|err| anyhow::anyhow!("agent watchdog continuation dispatch failed: {err}"))
    }

    async fn reload_watchdog_replacement_session(
        &mut self,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let Some(context) = self.session_load_context.clone() else {
            if self.access.required_mode().is_some() {
                anyhow::bail!(
                    "ACP replacement cannot verify required mode without session/load context"
                );
            }

            tracing::warn!(
                session_id,
                "Agent watchdog has no session/load context; continuing legacy recovery"
            );
            return Ok(());
        };
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        tokio::time::timeout(
            timeout,
            self.load_session(session_id, &context.cwd, &context.mcp_servers),
        )
        .await
        .map_err(|_| anyhow::anyhow!("agent watchdog replacement session/load timed out"))?
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("agent watchdog replacement session/load failed: {err}"))
    }

    async fn cleanup_replacement(&mut self) {
        let Some(mut transport) = self.transport.take() else {
            return;
        };
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let termination = async {
            if self.agent_human_input {
                transport.force_terminate_verified().await
            } else {
                transport.force_terminate().await
            }
        };
        if !matches!(tokio::time::timeout(timeout, termination).await, Ok(Ok(()))) {
            self.cleanup_verified = false;
        }
    }

    async fn invalidate_session_after_failed_recovery(&mut self, session_id: &str, reason: &str) {
        self.cleanup_replacement().await;
        self.session_id = None;
        self.session_descriptor = None;
        tracing::warn!(
            event = "agent_watchdog_session_invalidated",
            session_id,
            reason,
            "Agent watchdog invalidated a session that the replacement transport could not resume"
        );
    }

    async fn hard_recover_and_continue(&mut self, session_id: &str) -> anyhow::Result<u64> {
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let Some(mut old_transport) = self.transport.take() else {
            let reason = "agent watchdog recovery found no active transport".to_string();
            self.invalidate_session_after_failed_recovery(session_id, &reason)
                .await;
            return Err(anyhow::anyhow!(reason));
        };

        match tokio::time::timeout(timeout, old_transport.force_terminate()).await {
            Ok(Ok(())) => {
                tracing::warn!(
                    event = "agent_watchdog_force_terminated",
                    session_id,
                    "Agent watchdog force-terminated the unresponsive transport"
                );
            }
            Ok(Err(err)) => {
                tracing::error!(
                    event = "agent_watchdog_recovery_failed",
                    session_id,
                    error = %err,
                    "Agent watchdog force termination failed"
                );
                let reason = format!("agent watchdog force termination failed: {err}");
                self.cleanup_verified = false;
                drop(old_transport);
                self.invalidate_session_after_failed_recovery(session_id, &reason)
                    .await;
                return Err(anyhow::anyhow!(reason));
            }
            Err(_) => {
                tracing::error!(
                    event = "agent_watchdog_recovery_failed",
                    session_id,
                    "Agent watchdog force termination timed out"
                );
                let reason = "agent watchdog force termination timed out".to_string();
                self.cleanup_verified = false;
                drop(old_transport);
                self.invalidate_session_after_failed_recovery(session_id, &reason)
                    .await;
                return Err(anyhow::anyhow!(reason));
            }
        }
        drop(old_transport);

        let replacement = match tokio::time::timeout(
            timeout,
            self.create_replacement_transport(session_id),
        )
        .await
        {
            Ok(Ok(replacement)) => replacement,
            Ok(Err(err)) => {
                if err
                    .chain()
                    .any(|cause| cause.is::<cowboy_agent_client::AgentSafetyError>())
                {
                    self.cleanup_verified = false;
                }
                let reason = format!("agent watchdog replacement transport creation failed: {err}");
                self.invalidate_session_after_failed_recovery(session_id, &reason)
                    .await;
                return Err(anyhow::anyhow!(reason));
            }
            Err(_) => {
                self.cleanup_verified = false;
                let reason = "agent watchdog replacement transport creation timed out".to_string();
                self.invalidate_session_after_failed_recovery(session_id, &reason)
                    .await;
                return Err(anyhow::anyhow!(reason));
            }
        };
        self.transport = Some(replacement);
        self.pushback.clear();

        if let Err(err) = self
            .initialize_bounded("agent watchdog replacement initialization")
            .await
        {
            let reason = format!("agent watchdog replacement initialization failed: {err}");
            self.invalidate_session_after_failed_recovery(session_id, &reason)
                .await;
            return Err(anyhow::anyhow!(reason));
        }

        if let Err(err) = self.reload_watchdog_replacement_session(session_id).await {
            let reason = err.to_string();
            self.invalidate_session_after_failed_recovery(session_id, &reason)
                .await;
            return Err(anyhow::anyhow!(reason));
        }
        tracing::warn!(
            event = "agent_watchdog_transport_resumed",
            session_id,
            "Agent watchdog resumed the session on a replacement transport"
        );
        match self.dispatch_watchdog_continuation(session_id).await {
            Ok(id) => Ok(id),
            Err(err) => {
                let reason = err.to_string();
                self.invalidate_session_after_failed_recovery(session_id, &reason)
                    .await;
                Err(anyhow::anyhow!(reason))
            }
        }
    }

    /// Execute a single prompt turn and report why it stopped plus which
    /// response/progress signals were observed during the turn.
    async fn prompt_turn(
        &mut self,
        session_id: &str,
        prompt_content: Vec<PromptContent>,
        cancellation: &mut PromptTurnCancellation,
        event_handler: &mut (dyn FnMut(Event) + Send),
    ) -> anyhow::Result<PromptTurnOutcome> {
        let (content_count, prompt_chars) = prompt_content_stats(&prompt_content);
        tracing::debug!(
            session_id,
            content_count,
            prompt_chars,
            "ACP prompt turn starting"
        );
        let params = SessionPromptParams {
            session_id: session_id.to_string(),
            prompt: prompt_content,
        };
        let mut id = self.send_request_no_wait("session/prompt", params).await?;
        tracing::debug!(
            session_id,
            id,
            content_count,
            prompt_chars,
            "ACP prompt sent"
        );

        let mut activity = PromptTurnActivity::Empty;
        let mut last_activity = LastObservedActivity::default();
        let mut watchdog_soft_recoveries: u32 = 0;
        let mut external_cancellation_sent = false;
        let mut deferred_updates_after_external_cancellation = 0usize;
        let mut replacement_continuation_active = false;
        let mut response_deadline_at = tokio::time::Instant::now()
            + Duration::from_secs(self.watchdog.response_timeout_seconds);
        let stop_reason = 'monitor: loop {
            let response_deadline = tokio::time::sleep_until(response_deadline_at);
            tokio::pin!(response_deadline);
            enum WaitOutcome {
                Message(anyhow::Result<Message>),
                ExternalCancellation,
                WatchdogTimeout,
            }
            let outcome = if external_cancellation_sent {
                WaitOutcome::Message(self.recv_message_raw().await)
            } else if self.access.denies_permissions() {
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => WaitOutcome::ExternalCancellation,
                    () = &mut response_deadline => WaitOutcome::WatchdogTimeout,
                    message = self.recv_message_raw() => WaitOutcome::Message(message),
                }
            } else {
                tokio::select! {
                    biased;
                    message = self.recv_message_raw() => WaitOutcome::Message(message),
                    () = cancellation.cancelled() => WaitOutcome::ExternalCancellation,
                    () = &mut response_deadline => WaitOutcome::WatchdogTimeout,
                }
            };

            if let WaitOutcome::Message(Ok(ref message)) = outcome {
                self.reject_foreign_session_message(message, session_id)
                    .await?;
                if self.deny_permission_request(message).await? {
                    continue 'monitor;
                }
                self.reject_forbidden_message(message).await?;
            }
            response_deadline_at = tokio::time::Instant::now()
                + Duration::from_secs(self.watchdog.response_timeout_seconds);

            let msg = match outcome {
                WaitOutcome::Message(message) => match message {
                    Ok(message) => message,
                    Err(err) => {
                        if self.tool_policy_violated {
                            anyhow::bail!(DENIED_TOOL_ERROR);
                        }

                        if self.agent_human_input {
                            return Err(err);
                        }

                        if external_cancellation_sent {
                            return Err(err);
                        }
                        if replacement_continuation_active {
                            let reason =
                                format!("agent watchdog replacement continuation failed: {err}");
                            self.invalidate_session_after_failed_recovery(session_id, &reason)
                                .await;
                            return Err(anyhow::anyhow!("{reason}"));
                        }
                        tracing::warn!(
                            event = "agent_watchdog_recovery_failed",
                            session_id,
                            error = %err,
                            "Agent watchdog observed an unusable ACP stream"
                        );
                        id = self.hard_recover_and_continue(session_id).await.map_err(
                            |recovery| {
                                anyhow::anyhow!("{err}; agent watchdog recovery failed: {recovery}")
                            },
                        )?;
                        replacement_continuation_active = true;
                        continue;
                    }
                },
                WaitOutcome::ExternalCancellation => {
                    self.send_prompt_turn_cancellation(session_id).await?;
                    tracing::debug!(session_id, id, "ACP prompt turn cancellation sent");
                    external_cancellation_sent = true;
                    continue;
                }
                WaitOutcome::WatchdogTimeout => {
                    replacement_continuation_active = false;
                    if self.agent_human_input {
                        // A bounded cancellation is advisory; the action runner
                        // must still verify scoped process-tree termination.
                        let _ = tokio::time::timeout(
                            Duration::from_secs(self.watchdog.cancel_timeout_seconds),
                            self.send_prompt_turn_cancellation(session_id),
                        )
                        .await;
                        anyhow::bail!("agent response timed out");
                    }

                    if last_activity.tool_call_in_flight() {
                        // A tool call is still running, so the conversation is
                        // alive and the agent is waiting on it. Cancelling here
                        // would kill real work and restart the same wait, so
                        // restart the watchdog instead. Deciding that a tool
                        // call is stuck, and aborting it, is the agent's
                        // responsibility rather than Cowboy's.
                        tracing::warn!(
                            event = "agent_watchdog_tool_wait",
                            session_id,
                            id,
                            timeout_seconds = self.watchdog.response_timeout_seconds,
                            last_event_kind = last_activity.last_event_kind(),
                            last_tool_call_title = last_activity.last_tool_call_title(),
                            last_tool_call_kind = last_activity.last_tool_call_kind(),
                            last_tool_call_status = last_activity.last_tool_call_status(),
                            seconds_since_last_activity =
                                last_activity.seconds_since_last_activity(),
                            waited_seconds = last_activity.tool_wait_seconds(),
                            "Agent watchdog restarted while a tool call is in flight"
                        );
                        continue 'monitor;
                    }
                    tracing::warn!(
                        event = "agent_watchdog_timeout",
                        session_id,
                        id,
                        timeout_seconds = self.watchdog.response_timeout_seconds,
                        last_event_kind = last_activity.last_event_kind(),
                        last_tool_call_title = last_activity.last_tool_call_title(),
                        last_tool_call_kind = last_activity.last_tool_call_kind(),
                        last_tool_call_status = last_activity.last_tool_call_status(),
                        seconds_since_last_activity = last_activity.seconds_since_last_activity(),
                        "Agent watchdog detected response inactivity"
                    );
                    if let Err(err) = self.send_prompt_turn_cancellation(session_id).await {
                        tracing::warn!(
                            event = "agent_watchdog_recovery_failed",
                            session_id,
                            error = %err,
                            "Agent watchdog cancel notification failed"
                        );
                        id = self.hard_recover_and_continue(session_id).await?;
                        replacement_continuation_active = true;
                        continue;
                    }
                    tracing::warn!(
                        event = "agent_watchdog_cancel_sent",
                        session_id,
                        id,
                        "Agent watchdog sent session/cancel"
                    );

                    let cancel_deadline = tokio::time::sleep(Duration::from_secs(
                        self.watchdog.cancel_timeout_seconds,
                    ));
                    tokio::pin!(cancel_deadline);
                    loop {
                        enum CancelGraceOutcome {
                            Message(anyhow::Result<Message>),
                            ExternalCancellation,
                            Timeout,
                        }
                        let outcome = if external_cancellation_sent {
                            CancelGraceOutcome::Message(self.recv_message_raw().await)
                        } else if self.access.denies_permissions() {
                            tokio::select! {
                                biased;
                                () = cancellation.cancelled() => CancelGraceOutcome::ExternalCancellation,
                                () = &mut cancel_deadline => CancelGraceOutcome::Timeout,
                                message = self.recv_message_raw() => CancelGraceOutcome::Message(message),
                            }
                        } else {
                            tokio::select! {
                                biased;
                                message = self.recv_message_raw() => CancelGraceOutcome::Message(message),
                                () = cancellation.cancelled() => CancelGraceOutcome::ExternalCancellation,
                                () = &mut cancel_deadline => CancelGraceOutcome::Timeout,
                            }
                        };
                        let message = match outcome {
                            CancelGraceOutcome::Message(message) => message,
                            CancelGraceOutcome::ExternalCancellation => {
                                external_cancellation_sent = true;
                                continue;
                            }
                            CancelGraceOutcome::Timeout => {
                                if external_cancellation_sent {
                                    continue 'monitor;
                                }
                                id = self.hard_recover_and_continue(session_id).await?;
                                replacement_continuation_active = true;
                                response_deadline_at = tokio::time::Instant::now()
                                    + Duration::from_secs(self.watchdog.response_timeout_seconds);
                                continue 'monitor;
                            }
                        };
                        if let Ok(ref message) = message {
                            self.reject_foreign_session_message(message, session_id)
                                .await?;
                            self.reject_forbidden_message(message).await?;
                            if self.deny_permission_request(message).await? {
                                continue;
                            }
                        }
                        let message = match message {
                            Ok(message) => message,
                            Err(err) => {
                                if self.tool_policy_violated {
                                    anyhow::bail!(DENIED_TOOL_ERROR);
                                }

                                if external_cancellation_sent {
                                    return Err(err);
                                }
                                id = self.hard_recover_and_continue(session_id).await?;
                                replacement_continuation_active = true;
                                response_deadline_at = tokio::time::Instant::now()
                                    + Duration::from_secs(self.watchdog.response_timeout_seconds);
                                continue 'monitor;
                            }
                        };
                        match message {
                            Message::SessionUpdate { update, .. } => {
                                activity.observe_event(&update);
                                last_activity.observe_event(&update);
                                event_handler(update);
                            }
                            Message::PermissionRequest {
                                id: req_id,
                                session_id: permission_session_id,
                                tool_call,
                                options: _,
                            } => {
                                tracing::debug!(
                                    session_id = %permission_session_id,
                                    request_id = %req_id,
                                    tool_kind = ?tool_call.get("kind").and_then(|value| value.as_str()),
                                    "ACP permission request cancelled during watchdog grace"
                                );
                                self.send_rpc_response(&req_id, PermissionOutcome::cancelled())
                                    .await?;
                            }
                            Message::Response {
                                id: resp_id,
                                result,
                                error,
                            } if resp_id == id => {
                                if let Some(err) = error {
                                    if external_cancellation_sent {
                                        anyhow::bail!("Agent error: {err}");
                                    }
                                    id = self.hard_recover_and_continue(session_id).await?;
                                    replacement_continuation_active = true;
                                    response_deadline_at = tokio::time::Instant::now()
                                        + Duration::from_secs(
                                            self.watchdog.response_timeout_seconds,
                                        );
                                    continue 'monitor;
                                }
                                let result: SessionPromptResult =
                                    serde_json::from_value(result.unwrap_or(Value::Null))
                                        .unwrap_or(SessionPromptResult { stop_reason: None });
                                let stop_reason = result.stop_reason.unwrap_or(StopReason::EndTurn);
                                // Any terminal prompt response observed inside
                                // the grace window acknowledges the
                                // `session/cancel` Cowboy just sent, whatever
                                // stop reason the backend chose to report. It
                                // is a truncated turn, not a completed reply,
                                // so it must never be surfaced to the caller as
                                // one.
                                if external_cancellation_sent {
                                    break 'monitor StopReason::Cancelled;
                                }
                                watchdog_soft_recoveries += 1;
                                tracing::warn!(
                                    event = "agent_watchdog_soft_recovered",
                                    session_id,
                                    stop_reason = ?stop_reason,
                                    soft_recoveries = watchdog_soft_recoveries,
                                    last_event_kind = last_activity.last_event_kind(),
                                    last_tool_call_title = last_activity.last_tool_call_title(),
                                    last_tool_call_kind = last_activity.last_tool_call_kind(),
                                    last_tool_call_status = last_activity.last_tool_call_status(),
                                    seconds_since_last_activity =
                                        last_activity.seconds_since_last_activity(),
                                    "Agent watchdog cancellation completed; continuing session"
                                );
                                id = self.dispatch_watchdog_continuation(session_id).await?;
                                response_deadline_at = tokio::time::Instant::now()
                                    + Duration::from_secs(self.watchdog.response_timeout_seconds);
                                continue 'monitor;
                            }
                            _ => {}
                        }
                    }
                }
            };

            let matching_prompt_response = matches!(
                &msg,
                Message::Response { id: response_id, .. } if *response_id == id
            );
            if !external_cancellation_sent
                && !matching_prompt_response
                && cancellation.try_cancelled()
            {
                let defer_for_buffered_completion = matches!(&msg, Message::SessionUpdate { .. })
                    && deferred_updates_after_external_cancellation < 1;
                if defer_for_buffered_completion {
                    deferred_updates_after_external_cancellation += 1;
                } else {
                    self.send_prompt_turn_cancellation(session_id).await?;
                    external_cancellation_sent = true;
                    tracing::debug!(session_id, id, "ACP prompt turn cancellation sent");
                }
            }

            match msg {
                Message::SessionUpdate { update, .. } => {
                    activity.observe_event(&update);
                    last_activity.observe_event(&update);
                    event_handler(update);
                }
                Message::PermissionRequest {
                    id: req_id,
                    session_id: permission_session_id,
                    tool_call,
                    options,
                } => {
                    activity.observe_permission_request();
                    let outcome = if external_cancellation_sent {
                        PermissionOutcome::cancelled()
                    } else {
                        PermissionOutcome::allow_from_options(&options)
                    };
                    tracing::debug!(
                        session_id = %permission_session_id,
                        request_id = %req_id,
                        tool_kind = ?tool_call.get("kind").and_then(|value| value.as_str()),
                        tool_title = ?tool_call.get("title").and_then(|value| value.as_str()),
                        options = options.len(),
                        outcome = ?outcome,
                        "ACP permission request answered"
                    );
                    self.send_rpc_response(&req_id, outcome).await?;
                }
                Message::Response {
                    id: resp_id,
                    result,
                    error,
                } if resp_id == id => {
                    if let Some(err) = error {
                        if replacement_continuation_active {
                            let reason =
                                format!("agent watchdog replacement continuation RPC error: {err}");
                            self.invalidate_session_after_failed_recovery(session_id, &reason)
                                .await;
                            anyhow::bail!("{reason}");
                        }
                        tracing::warn!(session_id, id = resp_id, error = %err, "ACP prompt response error");
                        anyhow::bail!("Agent error: {err}");
                    }
                    let prompt_result: SessionPromptResult =
                        serde_json::from_value(result.unwrap_or(Value::Null))
                            .unwrap_or(SessionPromptResult { stop_reason: None });
                    let stop_reason = prompt_result.stop_reason.unwrap_or(StopReason::EndTurn);
                    break stop_reason;
                }
                _ => {}
            }
        };
        if self.tool_policy_violated {
            anyhow::bail!(DENIED_TOOL_ERROR);
        }

        if matches!(stop_reason, StopReason::Cancelled)
            && self.access.allows_tools()
            && !self.access.denies_permissions()
        {
            tracing::debug!(
                session_id,
                id,
                activity = ?activity,
                "ACP cancelled prompt turn completed"
            );
            return Ok(PromptTurnOutcome {
                stop_reason,
                activity,
            });
        }

        // Drain trailing session/update events that some agents stream around or
        // after the prompt Response. Whenever the turn produced no visible text
        // yet, wait generously: some backends (e.g. Oh My Pi) may acknowledge the
        // prompt before they start streaming, and first-token latency can be
        // several seconds, so a short window would silently drop the whole reply.
        let drain_ms = if matches!(stop_reason, StopReason::Cancelled)
            || matches!(activity, PromptTurnActivity::Text)
        {
            500
        } else {
            15_000
        };
        let saw_trailing_text = self
            .drain_trailing_events(session_id, event_handler, drain_ms)
            .await?;
        if self.tool_policy_violated {
            anyhow::bail!(DENIED_TOOL_ERROR);
        }

        activity.observe_trailing_text(saw_trailing_text);

        tracing::debug!(
            session_id,
            id,
            stop_reason = ?stop_reason,
            activity = ?activity,
            trailing_text = saw_trailing_text,
            "ACP prompt turn completed"
        );
        Ok(PromptTurnOutcome {
            stop_reason,
            activity,
        })
    }

    /// Drain any session/update events that arrive shortly after a prompt
    /// Response. Returns whether any visible `agent_message_chunk` text was seen,
    /// so the caller can tell a genuinely empty turn from one whose reply only
    /// streamed after the Response.
    ///
    /// Some agents send `agent_message_chunk` events after (or instead of before)
    /// the JSON-RPC Response — e.g. when a tool call fails and the agent falls
    /// back to text, or when the backend acks the prompt before streaming. We
    /// drain these with a timeout so they reach the handler and do not leak into
    /// the next prompt call.
    async fn drain_trailing_events(
        &mut self,
        active_session_id: &str,
        event_handler: &mut (dyn FnMut(Event) + Send),
        initial_timeout_ms: u64,
    ) -> anyhow::Result<bool> {
        use tokio::time::{Duration, timeout};

        let mut drain_timeout = Duration::from_millis(initial_timeout_ms);
        let restricted_deadline = (self.access.denies_permissions())
            .then(|| tokio::time::Instant::now() + Duration::from_millis(initial_timeout_ms));
        let mut saw_text = false;

        if self.transport.is_none() {
            return Ok(saw_text);
        }
        let mut pushback_line = None;

        loop {
            if restricted_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                break;
            }

            let line = {
                let transport = self.transport_mut()?;
                let remaining = restricted_deadline
                    .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()))
                    .unwrap_or(drain_timeout);
                match timeout(drain_timeout.min(remaining), transport.recv()).await {
                    Ok(Ok(Some(line))) => line,
                    Ok(Err(error)) => {
                        if self.access.denies_permissions() || !self.access.allows_tools() {
                            self.tool_policy_violated = true;
                            self.policy_rejected = true;
                            self.cleanup_replacement().await;
                            return Err(error);
                        }
                        break;
                    }
                    // EOF after a completed response is normal for some backends.
                    Ok(Ok(None)) | Err(_) => break,
                }
            };
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            let json = match serde_json::from_str::<Value>(&line) {
                Ok(json) => json,
                Err(error) if self.access.denies_permissions() || !self.access.allows_tools() => {
                    self.tool_policy_violated = true;
                    self.policy_rejected = true;
                    self.cleanup_replacement().await;
                    anyhow::bail!("invalid ACP trailing message: {error}");
                }
                Err(_) => continue,
            };
            self.validate_incoming_mode_update(&json).await?;
            self.reject_invalid_permission_request_id(&json).await?;

            if let Some(msg) = parse_acp_message(&json) {
                self.reject_foreign_session_message(&msg, active_session_id)
                    .await?;

                self.reject_forbidden_message(&msg).await?;
                if self.deny_permission_request(&msg).await? {
                    drain_timeout = Duration::from_millis(200);
                    continue;
                }
                tracing::debug!(payload = %line, "ACP <<< trailing");
                match msg {
                    Message::SessionUpdate { update, .. } => {
                        if matches!(update, Event::MessageChunk { .. }) {
                            saw_text = true;
                        }
                        event_handler(update);
                        // Keep draining — more events may follow
                        drain_timeout = Duration::from_millis(200);
                        continue;
                    }
                    _ => {
                        // Non-update message — push back for next recv_message
                        pushback_line = Some(line);
                        break;
                    }
                }
            }
        }

        if let Some(line) = pushback_line {
            self.pushback.push(line);
        }

        Ok(saw_text)
    }

    /// Check whether the agent supports session/load, based on agentCapabilities from initialize.
    pub fn supports_load_session(&self) -> bool {
        self.agent_capabilities
            .as_ref()
            .and_then(|caps| caps.get("loadSession"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Resume an existing session with ACP session/load.
    ///
    /// The agent replays the full conversation history through session/update notifications;
    /// once the result arrives, session/prompt can continue.
    pub async fn load_session(
        &mut self,
        session_id: &str,
        cwd: &str,
        mcp_servers: &[Value],
    ) -> anyhow::Result<Vec<Event>> {
        if self.tool_policy_violated {
            anyhow::bail!(DENIED_TOOL_ERROR);
        }

        if self.access.required_mode().is_some() {
            self.session_id = None;
            self.pending_session_id = None;
        }

        if !self.supports_load_session() {
            tracing::warn!(session_id, "ACP session/load unsupported by agent");
            anyhow::bail!("Agent does not support loadSession capability");
        }
        if self.access.required_mode().is_some() {
            self.pending_session_id = Some(session_id.to_string());
        }

        tracing::debug!(
            session_id,
            cwd,
            mcp_server_count = mcp_servers.len(),
            "ACP session/load starting"
        );
        let params = SessionLoadParams {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            mcp_servers: mcp_servers.to_vec(),
        };
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let id = match tokio::time::timeout_at(
            deadline,
            self.send_request_no_wait("session/load", params),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                self.tool_policy_violated = true;
                self.cleanup_replacement().await;
                anyhow::bail!("ACP session/load timed out; transport terminated");
            }
        };

        let mut history = Vec::new();
        loop {
            let msg =
                match tokio::time::timeout_at(deadline, self.recv_message_for_session(session_id))
                    .await
                {
                    Ok(result) => result?,
                    Err(_) => {
                        self.tool_policy_violated = true;
                        self.cleanup_replacement().await;
                        anyhow::bail!("ACP session/load timed out; transport terminated");
                    }
                };
            match msg {
                Message::SessionUpdate { update, .. } => {
                    tracing::trace!(
                        session_id,
                        update = event_kind(&update),
                        "ACP session/load replay event"
                    );
                    history.push(update);
                }
                Message::Response {
                    id: resp_id,
                    result,
                    error,
                } if resp_id == id => {
                    if let Some(err) = error {
                        tracing::warn!(session_id, id = resp_id, error = %err, "ACP session/load response error");
                        anyhow::bail!("session/load error: {err}");
                    }
                    let result = match result {
                        Some(Value::Null) | None => Ok(SessionLoadResult::default()),
                        Some(result) => serde_json::from_value(result),
                    };
                    let result = match result {
                        Ok(result) => result,
                        Err(error) if self.access.required_mode().is_some() => {
                            self.tool_policy_violated = true;
                            self.policy_rejected = true;
                            self.cleanup_replacement().await;
                            return Err(error.into());
                        }
                        Err(error) => return Err(error.into()),
                    };
                    if let Err(error) = self.validate_required_acp_mode(
                        result.mode_state.as_ref(),
                        &result.config_options,
                    ) {
                        self.tool_policy_violated = true;
                        self.policy_rejected = true;
                        self.cleanup_replacement().await;
                        return Err(error);
                    }

                    self.session_descriptor =
                        Self::descriptor_from_config_options(&result.config_options);
                    self.session_id = Some(session_id.to_string());
                    self.pending_session_id = None;
                    self.session_load_context = Some(SessionLoadContext {
                        cwd: cwd.to_string(),
                        mcp_servers: mcp_servers.to_vec(),
                    });
                    tracing::info!(
                        session_id,
                        history_events = history.len(),
                        "ACP session loaded"
                    );
                    return Ok(history);
                }
                _ => {} // Ignore non-matching messages during session/load replay.
            }
        }
    }

    /// Send a JSON-RPC request and wait for its response.
    async fn send_request<P: Serialize>(
        &mut self,
        method: &'static str,
        params: P,
    ) -> anyhow::Result<Value> {
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let id = match tokio::time::timeout_at(deadline, self.send_request_no_wait(method, params))
            .await
        {
            Ok(result) => result?,
            Err(_) => {
                self.tool_policy_violated = true;
                self.cleanup_replacement().await;
                anyhow::bail!("ACP {method} timed out; transport terminated");
            }
        };

        loop {
            let msg = match tokio::time::timeout_at(deadline, self.recv_message()).await {
                Ok(result) => result?,
                Err(_) => {
                    self.tool_policy_violated = true;
                    self.cleanup_replacement().await;
                    anyhow::bail!("ACP {method} timed out; transport terminated");
                }
            };
            match msg {
                Message::Response {
                    id: resp_id,
                    result,
                    error,
                } if resp_id == id => {
                    if let Some(err) = error {
                        anyhow::bail!("RPC error on '{}': {err}", method);
                    }
                    return Ok(result.unwrap_or(Value::Null));
                }
                _ => {} // Skip non-matching messages while waiting for this response.
            }
        }
    }

    /// Send a JSON-RPC request without waiting for a response, returning the request ID.
    async fn send_request_no_wait<P: Serialize>(
        &mut self,
        method: &'static str,
        params: P,
    ) -> anyhow::Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let request = JsonRpcRequest::new(id, method, params);
        let line = serde_json::to_string(&request)?;
        tracing::debug!(id, method, payload = %line, "ACP >>> request");
        self.ensure_transport().await?.send(&line).await?;
        Ok(id)
    }

    /// Send a JSON-RPC notification with no request id or response.
    async fn send_notification<P: Serialize>(
        &mut self,
        method: &'static str,
        params: P,
    ) -> anyhow::Result<()> {
        let notification = JsonRpcNotification::new(method, params);
        let line = serde_json::to_string(&notification)?;
        tracing::debug!(method, payload = %line, "ACP >>> notification");
        self.ensure_transport().await?.send(&line).await?;
        Ok(())
    }

    /// Send a JSON-RPC response to an agent request, such as a permission request.
    async fn send_rpc_response<R: Serialize>(
        &mut self,
        id: &JsonRpcId,
        result: R,
    ) -> anyhow::Result<()> {
        let response = JsonRpcResponse::new(id, result);
        let line = serde_json::to_string(&response)?;
        tracing::debug!(%id, payload = %line, "ACP >>> response");
        self.ensure_transport().await?.send(&line).await?;
        Ok(())
    }

    /// A restricted agent may keep working after a cancelled permission request.
    /// Never reconnect to send the denial: an uncertain send stops this transport.
    async fn deny_permission_request(&mut self, msg: &Message) -> anyhow::Result<bool> {
        let Message::PermissionRequest { id, .. } = msg else {
            return Ok(false);
        };

        if !matches!(self.access, AgentAccess::DenyEscalation { .. }) {
            return Ok(false);
        }

        let response = JsonRpcResponse::new(id, PermissionOutcome::cancelled());
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let sent = async {
            let line = serde_json::to_string(&response)?;
            self.transport_mut()?.send(&line).await
        };
        if !matches!(tokio::time::timeout(timeout, sent).await, Ok(Ok(()))) {
            self.tool_policy_violated = true;
            self.policy_rejected = true;
            self.cleanup_replacement().await;
            anyhow::bail!("ACP permission denial could not be sent; transport terminated");
        }

        Ok(true)
    }

    async fn reject_foreign_session_message(
        &mut self,
        msg: &Message,
        active_session_id: &str,
    ) -> anyhow::Result<()> {
        if matches!(self.access, AgentAccess::Default) {
            return Ok(());
        }

        let observed = match msg {
            Message::PermissionRequest { session_id, .. }
            | Message::SessionUpdate { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        };
        if observed.is_some_and(|id| id != active_session_id) {
            self.tool_policy_violated = true;
            self.policy_rejected = true;
            self.cleanup_replacement().await;
            anyhow::bail!("ACP restricted session mismatch; transport terminated");
        }

        if let (
            Some(required),
            Message::SessionUpdate {
                update: Event::Unknown { raw, .. },
                ..
            },
        ) = (self.access.required_mode(), msg)
        {
            let reported = raw
                .get("currentModeId")
                .and_then(Value::as_str)
                .or_else(|| {
                    raw.get("modeState")
                        .and_then(|value| value.get("currentModeId"))
                        .and_then(Value::as_str)
                });
            let changed_options = raw.get("configOptions").and_then(Value::as_array);
            if reported.is_some_and(|mode| mode != required)
                || changed_options
                    .into_iter()
                    .flatten()
                    .filter(|option| option.get("id").and_then(Value::as_str) == Some("mode"))
                    .any(|option| {
                        option.get("currentValue").and_then(Value::as_str) != Some(required)
                    })
            {
                self.tool_policy_violated = true;
                self.policy_rejected = true;
                self.cleanup_replacement().await;
                anyhow::bail!("ACP reported session mode differs from required mode");
            }
        }

        Ok(())
    }

    /// Reject forbidden tool activity before any caller can forward it or
    /// accept a successful response. Permission requests receive an ACP
    /// cancelled response, even during replay, cancellation grace and drain.
    async fn reject_forbidden_message(&mut self, msg: &Message) -> anyhow::Result<()> {
        if self.access.allows_tools() {
            return Ok(());
        }

        match msg {
            Message::PermissionRequest { id, .. } => {
                self.tool_policy_violated = true;
                self.policy_rejected = true;
                let response = JsonRpcResponse::new(id, PermissionOutcome::cancelled());
                let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
                let sent = async {
                    let line = serde_json::to_string(&response)?;
                    self.transport
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("Agent connection closed unexpectedly"))?
                        .send(&line)
                        .await
                };
                if !matches!(tokio::time::timeout(timeout, sent,).await, Ok(Ok(()))) {
                    tracing::warn!("ACP tool denial could not be sent; terminating transport");
                    self.cleanup_replacement().await;
                }

                anyhow::bail!(DENIED_TOOL_ERROR);
            }
            Message::SessionUpdate {
                update: Event::ToolCall { .. } | Event::ToolCallUpdate { .. },
                ..
            } => {
                self.tool_policy_violated = true;
                self.policy_rejected = true;
                self.cleanup_replacement().await;
                anyhow::bail!(DENIED_TOOL_ERROR);
            }
            _ => Ok(()),
        }
    }

    async fn reject_invalid_permission_request_id(&mut self, json: &Value) -> anyhow::Result<()> {
        if (!self.access.allows_tools() || self.access.denies_permissions())
            && has_invalid_permission_request_id(json)
        {
            self.tool_policy_violated = true;
            self.policy_rejected = true;
            self.cleanup_replacement().await;
            anyhow::bail!("ACP permission request has an invalid id; transport terminated");
        }

        Ok(())
    }

    async fn validate_incoming_mode_update(&mut self, json: &Value) -> anyhow::Result<()> {
        let Some(required) = self.access.required_mode() else {
            return Ok(());
        };
        if json.get("method").and_then(Value::as_str) != Some("session/update") {
            return Ok(());
        }

        let params = json.get("params");
        let update = params.and_then(|params| params.get("update"));
        let kind = update
            .and_then(|update| update.get("sessionUpdate"))
            .and_then(Value::as_str);
        let mode = update.and_then(|update| update.get("currentModeId"));
        let state = update.and_then(|update| update.get("modeState"));
        let state_mode = state.and_then(|state| state.get("currentModeId"));
        let options = update.and_then(|update| update.get("configOptions"));
        let mode_options = options
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|option| option.get("id").and_then(Value::as_str) == Some("mode"))
            .collect::<Vec<_>>();
        let is_mode_update = matches!(
            kind,
            Some("current_mode_update" | "mode_update" | "config_option_update")
        ) || mode.is_some()
            || state.is_some()
            || !mode_options.is_empty();
        let expected = self
            .pending_session_id
            .as_deref()
            .or(self.session_id.as_deref());
        let observed = params
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_str);
        let invalid = kind.is_none()
            || options.is_some_and(|value| !value.is_array())
            || (is_mode_update
                && (expected.is_none()
                    || observed != expected
                    || mode.is_some_and(|value| value.as_str() != Some(required))
                    || state
                        .is_some_and(|_| state_mode.and_then(Value::as_str) != Some(required))
                    || mode_options.iter().any(|option| {
                        option.get("currentValue").and_then(Value::as_str) != Some(required)
                    })
                    || (mode.is_none() && state.is_none() && mode_options.is_empty())));
        if invalid {
            self.tool_policy_violated = true;
            self.policy_rejected = true;
            self.cleanup_replacement().await;
            anyhow::bail!(
                "ACP mode update could not confirm required session mode; transport terminated"
            );
        }

        Ok(())
    }

    async fn parse_incoming_json(&mut self, line: &str) -> anyhow::Result<Value> {
        let json = match serde_json::from_str(line) {
            Ok(json) => json,
            Err(err) if !self.access.allows_tools() || self.access.denies_permissions() => {
                self.tool_policy_violated = true;
                self.policy_rejected = true;
                self.cleanup_replacement().await;
                anyhow::bail!(
                    "Malformed ACP JSON under restricted policy: {err}; transport terminated"
                );
            }
            Err(err) => anyhow::bail!("Failed to parse JSON from agent: {err}\nLine: {line}"),
        };
        self.reject_invalid_permission_request_id(&json).await?;
        self.validate_incoming_mode_update(&json).await?;
        Ok(json)
    }

    /// Receive and parse the next ACP message from the transport.
    ///
    /// Keeps reading until it gets one parseable ACP message.
    /// Skips empty lines and unrecognized message formats.
    async fn recv_message(&mut self) -> anyhow::Result<Message> {
        loop {
            let msg = self.recv_message_raw().await?;
            self.reject_forbidden_message(&msg).await?;
            if !self.deny_permission_request(&msg).await? {
                return Ok(msg);
            }
        }
    }

    async fn recv_message_for_session(&mut self, session_id: &str) -> anyhow::Result<Message> {
        loop {
            let msg = self.recv_message_raw().await?;
            self.reject_foreign_session_message(&msg, session_id)
                .await?;
            self.reject_forbidden_message(&msg).await?;
            if !self.deny_permission_request(&msg).await? {
                return Ok(msg);
            }
        }
    }

    async fn recv_message_raw(&mut self) -> anyhow::Result<Message> {
        loop {
            // Check pushback buffer first
            let line = if let Some(pushed) = self.pushback.pop() {
                pushed
            } else {
                self.ensure_transport()
                    .await?
                    .recv()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Agent connection closed unexpectedly"))?
            };

            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let json = self.parse_incoming_json(&line).await?;

            if let Some(msg) = parse_acp_message(&json) {
                if self.access.allows_tools() {
                    tracing::debug!(payload = %line, "ACP <<< received");
                    log_acp_message("inbound", &msg);
                }
                return Ok(msg);
            }
            if self.access.allows_tools() {
                tracing::warn!(payload = %line, "ACP <<< skipping unrecognized message");
            }
        }
    }

    /// Receive next ACP message using transport_mut (no reconnect).
    /// Used by initialize() to avoid async recursion.
    async fn recv_message_direct(&mut self) -> anyhow::Result<Message> {
        loop {
            let line = if let Some(pushed) = self.pushback.pop() {
                pushed
            } else {
                self.transport_mut()?
                    .recv()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Agent connection closed unexpectedly"))?
            };

            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let json = self.parse_incoming_json(&line).await?;

            if let Some(msg) = parse_acp_message(&json) {
                self.reject_forbidden_message(&msg).await?;
                if self.deny_permission_request(&msg).await? {
                    continue;
                }
                tracing::debug!(payload = %line, "ACP <<< received");
                log_acp_message("inbound_direct", &msg);
                return Ok(msg);
            }
            tracing::warn!(payload = %line, "ACP <<< skipping unrecognized message");
        }
    }

    async fn terminate_for_human_input(&mut self) -> anyhow::Result<()> {
        if !self.cleanup_verified {
            anyhow::bail!("prior ACP process cleanup was not verified");
        }

        let Some(mut transport) = self.transport.take() else {
            return Ok(());
        };
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        match tokio::time::timeout(timeout, transport.force_terminate_verified()).await {
            Ok(Ok(())) => {
                self.native_verified = transport.verified_shutdown_scope();
                Ok(())
            }
            _ => {
                self.cleanup_verified = false;
                anyhow::bail!("ACP process cleanup was not verified")
            }
        }
    }

    pub async fn close_verified(
        &mut self,
        owner_token: &str,
    ) -> anyhow::Result<cowboy_agent_client::VerifiedClientShutdown> {
        if self.native_owner.as_deref() != Some(owner_token)
            || self.native_scope.as_deref() != Some(owner_token)
            || !self.cleanup_verified
        {
            anyhow::bail!("ACP owned process identity or prior cleanup is unverified");
        }

        self.terminate_for_human_input().await?;
        let scope = self
            .native_verified
            .as_ref()
            .filter(|scope| scope.scope_id == owner_token)
            .ok_or_else(|| anyhow::anyhow!("ACP owned process scope has no verified exit"))?;
        Ok(cowboy_agent_client::VerifiedClientShutdown {
            owner_token: owner_token.to_string(),
            session_id: self.session_id.clone(),
            scopes: vec![scope.clone()],
        })
    }

    /// Close the connection.
    pub async fn close(&mut self) -> anyhow::Result<()> {
        tracing::debug!(
            transport = transport_kind(&self.transport_config),
            session_id = ?self.session_id,
            connected = self.transport.is_some(),
            "Closing ACP client"
        );
        if self.native_owner.is_some() && self.transport.is_some() {
            self.cleanup_verified = false;
        }

        if let Some(ref mut t) = self.transport {
            t.close().await?;
        }
        self.transport = None;
        tracing::debug!(session_id = ?self.session_id, "ACP client closed");
        Ok(())
    }
}

#[async_trait]
impl cowboy_agent_client::Client for Client {
    fn is_connected(&self) -> bool {
        Client::is_connected(self)
    }

    fn agent_info(&self) -> Option<&AgentInfo> {
        self.agent_info.as_ref()
    }

    fn session_descriptor(&self) -> Option<&AgentSessionDescriptor> {
        self.session_descriptor.as_ref()
    }

    fn session_id(&self) -> Option<&str> {
        Client::session_id(self)
    }

    async fn new_session(
        &mut self,
        cwd: &str,
        mcp_servers: &[Value],
        model: Option<&ModelInfo>,
    ) -> anyhow::Result<String> {
        Client::new_session(self, cwd, mcp_servers, model).await
    }

    fn supports_load_session(&self) -> bool {
        Client::supports_load_session(self)
    }

    async fn load_session(
        &mut self,
        session_id: &str,
        cwd: &str,
        mcp_servers: &[Value],
    ) -> anyhow::Result<Vec<Event>> {
        Client::load_session(self, session_id, cwd, mcp_servers).await
    }

    async fn prompt(
        &mut self,
        session_id: &str,
        prompt_content: Vec<PromptContent>,
        cancellation: PromptTurnCancellation,
        event_handler: &mut (dyn FnMut(Event) + Send),
    ) -> anyhow::Result<StopReason> {
        Client::prompt(
            self,
            session_id,
            prompt_content,
            cancellation,
            event_handler,
        )
        .await
    }

    fn human_input_failure_allowed(&self) -> bool {
        !self.policy_rejected && self.cleanup_verified
    }

    async fn terminate_for_human_input(&mut self) -> anyhow::Result<()> {
        Client::terminate_for_human_input(self).await
    }

    async fn close_verified(
        &mut self,
        owner_token: &str,
    ) -> anyhow::Result<cowboy_agent_client::VerifiedClientShutdown> {
        Client::close_verified(self, owner_token).await
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        Client::close(self).await
    }
}

/// Client capabilities declared by the orchestrator during ACP initialize.
///
/// Cowboy currently observes agent tool progress via `session/update`, but it does
/// not implement ACP's inbound `fs/*` or `terminal/*` client methods. Do not
/// advertise those capabilities until handlers exist; otherwise agents may route
/// reads, writes, or command execution through RPC methods Cowboy cannot answer.
fn build_client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        fs: FsCapabilities {
            read_text_file: false,
            write_text_file: false,
        },
        terminal: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use serde_json::Value;
    use std::future::poll_fn;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};

    fn dummy_transport_config() -> TransportConfig {
        TransportConfig::Stdio(crate::transport::StdioConfig {
            command: "mock".into(),
            args: vec![],
            clear_env: false,
            allowed_env: vec![],
            env: vec![],
        })
    }

    fn sanitized_missing_transport_config() -> TransportConfig {
        TransportConfig::Stdio(crate::transport::StdioConfig {
            command: "definitely-missing-acp-environment-test-command".into(),
            args: vec![],
            clear_env: true,
            allowed_env: vec!["GLOBAL".to_string(), "ROLE".to_string()],
            env: vec![],
        })
    }

    fn reset_transport_creation_records() {
        transport_creation_records()
            .lock()
            .expect("transport creation recorder poisoned")
            .clear();
    }

    fn transport_creation_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[tokio::test]
    async fn lazy_reconnect_reuses_original_environment_policy() {
        let _guard = transport_creation_test_lock().lock().await;
        reset_transport_creation_records();
        let config = sanitized_missing_transport_config();

        assert!(Client::create_transport(&config, None, None).await.is_err());
        assert!(
            Client::create_transport(&config, Some("session-lazy"), None)
                .await
                .is_err()
        );

        assert_eq!(
            *transport_creation_records()
                .lock()
                .expect("transport creation recorder poisoned"),
            [
                RecordedTransportCreation {
                    clear_env: true,
                    allowed_env: vec!["GLOBAL".to_string(), "ROLE".to_string()],
                    resume_session_id: None,
                },
                RecordedTransportCreation {
                    clear_env: true,
                    allowed_env: vec!["GLOBAL".to_string(), "ROLE".to_string()],
                    resume_session_id: Some("session-lazy".to_string()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn hard_recovery_reuses_original_environment_policy() {
        let _guard = transport_creation_test_lock().lock().await;
        reset_transport_creation_records();
        let config = sanitized_missing_transport_config();

        assert!(
            Client::create_transport(&config, Some("session-hard"), None)
                .await
                .is_err()
        );

        assert_eq!(
            *transport_creation_records()
                .lock()
                .expect("transport creation recorder poisoned"),
            [RecordedTransportCreation {
                clear_env: true,
                allowed_env: vec!["GLOBAL".to_string(), "ROLE".to_string()],
                resume_session_id: Some("session-hard".to_string()),
            }]
        );
    }

    #[derive(Default)]
    struct ControlledTransportCounters {
        received: AtomicUsize,
        received_at_cancel: AtomicUsize,
        force_terminated: AtomicUsize,
    }

    #[derive(Default)]
    struct ScriptedTransportCounters {
        sends: AtomicUsize,
        force_terminated: AtomicUsize,
        dropped: AtomicUsize,
    }

    enum ScriptedReceive {
        Message(String),
        Error(&'static str),
        Eof,
        Pending,
    }

    enum ScriptedOperation {
        Ready,
        Error(&'static str),
        Pending,
    }

    struct ScriptedTransport {
        incoming: std::collections::VecDeque<ScriptedReceive>,
        fail_send_at: Option<(usize, ScriptedOperation)>,
        force: ScriptedOperation,
        counters: Arc<ScriptedTransportCounters>,
    }

    impl ScriptedTransport {
        fn new(incoming: Vec<ScriptedReceive>, counters: Arc<ScriptedTransportCounters>) -> Self {
            Self {
                incoming: incoming.into(),
                fail_send_at: None,
                force: ScriptedOperation::Ready,
                counters,
            }
        }

        fn send_action(mut self, index: usize, action: ScriptedOperation) -> Self {
            self.fail_send_at = Some((index, action));
            self
        }

        fn force_action(mut self, action: ScriptedOperation) -> Self {
            self.force = action;
            self
        }
    }

    impl Drop for ScriptedTransport {
        fn drop(&mut self) {
            self.counters.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl Transport for ScriptedTransport {
        async fn send(&mut self, _message: &str) -> anyhow::Result<()> {
            let index = self.counters.sends.fetch_add(1, Ordering::SeqCst);
            if let Some((failure_index, action)) = &self.fail_send_at
                && index == *failure_index
            {
                return match action {
                    ScriptedOperation::Ready => Ok(()),
                    ScriptedOperation::Error(error) => anyhow::bail!("{error}"),
                    ScriptedOperation::Pending => std::future::pending().await,
                };
            }
            Ok(())
        }

        async fn recv(&mut self) -> anyhow::Result<Option<String>> {
            match self
                .incoming
                .pop_front()
                .unwrap_or(ScriptedReceive::Pending)
            {
                ScriptedReceive::Message(message) => Ok(Some(message)),
                ScriptedReceive::Error(error) => anyhow::bail!("{error}"),
                ScriptedReceive::Eof => Ok(None),
                ScriptedReceive::Pending => std::future::pending().await,
            }
        }

        async fn close(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        async fn force_terminate(&mut self) -> anyhow::Result<()> {
            self.counters
                .force_terminated
                .fetch_add(1, Ordering::SeqCst);
            match &self.force {
                ScriptedOperation::Ready => Ok(()),
                ScriptedOperation::Error(error) => anyhow::bail!("{error}"),
                ScriptedOperation::Pending => std::future::pending().await,
            }
        }
    }

    struct ControlledTransport {
        incoming: mpsc::UnboundedReceiver<String>,
        outgoing: mpsc::UnboundedSender<String>,
        counters: Option<Arc<ControlledTransportCounters>>,
    }

    #[async_trait]
    impl Transport for ControlledTransport {
        async fn send(&mut self, message: &str) -> anyhow::Result<()> {
            if serde_json::from_str::<Value>(message)
                .ok()
                .and_then(|message| message.get("method").cloned())
                .is_some_and(|method| method == "session/cancel")
                && let Some(counters) = &self.counters
            {
                counters
                    .received_at_cancel
                    .store(counters.received.load(Ordering::SeqCst), Ordering::SeqCst);
            }
            self.outgoing
                .send(message.to_string())
                .map_err(|_| anyhow::anyhow!("controlled outgoing channel closed"))
        }

        async fn recv(&mut self) -> anyhow::Result<Option<String>> {
            let message = self.incoming.recv().await;
            if message.is_some()
                && let Some(counters) = &self.counters
            {
                counters.received.fetch_add(1, Ordering::SeqCst);
            }
            Ok(message)
        }

        async fn close(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        async fn force_terminate(&mut self) -> anyhow::Result<()> {
            if let Some(counters) = &self.counters {
                counters.force_terminated.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    async fn next_outgoing(receiver: &mut mpsc::UnboundedReceiver<String>) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("client should send the next ACP message")
            .expect("controlled outgoing channel should remain open");
        serde_json::from_str(&message).unwrap()
    }

    fn test_watchdog() -> AgentWatchdogOptions {
        AgentWatchdogOptions {
            response_timeout_seconds: 1,
            cancel_timeout_seconds: 2,
            recovery_operation_timeout_seconds: 3,
        }
    }

    fn tool_call_started(
        session_id: &str,
        tool_call_id: &str,
        title: &str,
        kind: &str,
        status: &str,
    ) -> String {
        session_update(
            session_id,
            serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": tool_call_id,
                "title": title,
                "kind": kind,
                "status": status
            }),
        )
    }

    fn tool_call_progress(session_id: &str, tool_call_id: &str, status: &str) -> String {
        session_update(
            session_id,
            serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_call_id,
                "status": status
            }),
        )
    }

    /// Connect a watchdog-configured client over a `ControlledTransport` and
    /// drain the `initialize` request so tests start at the first prompt.
    async fn controlled_watchdog_client() -> (
        Client,
        mpsc::UnboundedSender<String>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let client = Client::connect_with_transport_and_options(
            Box::new(ControlledTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
                counters: None,
            }),
            dummy_transport_config(),
            test_watchdog(),
        )
        .await
        .unwrap();
        let _initialize = next_outgoing(&mut outgoing_rx).await;
        (client, incoming_tx, outgoing_rx)
    }

    async fn scripted_client(
        initial: ScriptedTransport,
    ) -> (Client, Arc<ScriptedTransportCounters>) {
        let counters = initial.counters.clone();
        let client = Client::connect_with_transport_and_options(
            Box::new(initial),
            dummy_transport_config(),
            test_watchdog(),
        )
        .await
        .unwrap();
        (client, counters)
    }

    #[test]
    fn access_round_trips_and_rejects_invalid_combinations() {
        for (value, access) in [
            (serde_json::json!("default"), AgentAccess::Default),
            (serde_json::json!("deny_all"), AgentAccess::DenyAll),
            (
                serde_json::json!({"mode":"deny_escalation","acp_mode":"read-only"}),
                AgentAccess::DenyEscalation {
                    acp_mode: "read-only".into(),
                },
            ),
        ] {
            assert_eq!(
                serde_json::from_value::<AgentAccess>(value.clone()).unwrap(),
                access
            );
            assert_eq!(serde_json::to_value(access).unwrap(), value);
        }

        for value in [
            serde_json::json!("deny_escalation"),
            serde_json::json!({"mode":"deny_escalation"}),
            serde_json::json!({"mode":"deny_escalation","acp_mode":" "}),
            serde_json::json!({"mode":"deny_all","acp_mode":"read-only"}),
            serde_json::json!({"mode":"deny_escalation","acp_mode":"read-only","extra":true}),
        ] {
            assert!(serde_json::from_value::<AgentAccess>(value).is_err());
        }

        let mut old_client = serde_json::to_value(Client {
            transport: None,
            transport_config: dummy_transport_config(),
            next_id: 0,
            agent_capabilities: None,
            agent_info: None,
            session_id: None,
            session_load_context: None,
            pushback: Vec::new(),
            session_descriptor: None,
            pending_session_id: None,
            watchdog: AgentWatchdogOptions::default(),
            access: AgentAccess::Default,
            tool_policy_violated: false,
            policy_rejected: false,
            cleanup_verified: true,
            agent_human_input: false,
            native_owner: None,
            native_scope: None,
            native_verified: None,
            replacement_factory: ReplacementTransportFactory::default(),
            reconnect_factory: ReplacementTransportFactory::default(),
        })
        .unwrap();
        old_client["allow_tools"] = serde_json::json!(false);
        assert!(serde_json::from_value::<Client>(old_client).is_err());
    }

    #[tokio::test]
    async fn test_connect_and_initialize() {
        let transport = MockTransport::new(vec![&init_response(0)]);
        let outgoing = transport.outgoing();

        let client = Client::connect_with_transport(Box::new(transport), dummy_transport_config())
            .await
            .unwrap();

        // Verify agent info was parsed
        let info = client.agent_info.as_ref().unwrap();
        assert_eq!(info.name, "mock-agent");
        assert_eq!(info.version.as_deref(), Some("1.0"));

        // Verify loadSession capability detected
        assert!(client.supports_load_session());

        // Verify the initialize request was sent correctly
        let sent = outgoing.lock();
        assert_eq!(sent.len(), 1);
        let req: Value = serde_json::from_str(&sent[0]).unwrap();
        assert_eq!(req["method"], "initialize");
        assert_eq!(req["params"]["protocolVersion"], 1);
        assert_eq!(req["params"]["clientInfo"]["name"], "cowboy");
        assert!(
            !req["params"]["clientCapabilities"]["terminal"]
                .as_bool()
                .unwrap()
        );
        assert!(
            !req["params"]["clientCapabilities"]["fs"]["readTextFile"]
                .as_bool()
                .unwrap()
        );
        assert!(
            !req["params"]["clientCapabilities"]["fs"]["writeTextFile"]
                .as_bool()
                .unwrap()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn connect_times_out_when_initialize_never_responds() {
        let counters = Arc::new(ScriptedTransportCounters::default());
        let transport =
            ScriptedTransport::new(vec![ScriptedReceive::Pending], Arc::clone(&counters));
        let watchdog = AgentWatchdogOptions {
            response_timeout_seconds: 100,
            cancel_timeout_seconds: 10,
            recovery_operation_timeout_seconds: 1,
        };

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            Client::connect_with_transport_and_options(
                Box::new(transport),
                dummy_transport_config(),
                watchdog,
            ),
        )
        .await
        .expect("ACP initialize should honor the recovery operation timeout");
        let error = result.expect_err("a missing initialize response should fail the connection");

        assert!(
            error.to_string().contains("initialize"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn initialize_timeout_disposes_transport() {
        let counters = Arc::new(ScriptedTransportCounters::default());
        let transport =
            ScriptedTransport::new(vec![ScriptedReceive::Pending], Arc::clone(&counters));
        let watchdog = AgentWatchdogOptions {
            response_timeout_seconds: 100,
            cancel_timeout_seconds: 10,
            recovery_operation_timeout_seconds: 1,
        };

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            Client::connect_with_transport_and_options(
                Box::new(transport),
                dummy_transport_config(),
                watchdog,
            ),
        )
        .await
        .expect("ACP initialize and cleanup should honor the recovery operation timeout");
        let error = result.expect_err("a missing initialize response should fail the connection");

        assert_eq!(error.to_string(), "ACP initialize timed out");
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(counters.dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn initialize_timeout_preserves_force_termination_error() {
        let counters = Arc::new(ScriptedTransportCounters::default());
        let transport =
            ScriptedTransport::new(vec![ScriptedReceive::Pending], Arc::clone(&counters))
                .force_action(ScriptedOperation::Error("cleanup failed"));
        let watchdog = AgentWatchdogOptions {
            response_timeout_seconds: 100,
            cancel_timeout_seconds: 10,
            recovery_operation_timeout_seconds: 1,
        };

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            Client::connect_with_transport_and_options(
                Box::new(transport),
                dummy_transport_config(),
                watchdog,
            ),
        )
        .await
        .expect("initialization and cleanup should be bounded");
        let error = result.expect_err("a missing initialize response should fail the connection");

        assert_eq!(
            error.to_string(),
            "ACP initialize timed out; transport force termination failed: cleanup failed"
        );
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(counters.dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn initialize_timeout_preserves_force_termination_timeout() {
        let counters = Arc::new(ScriptedTransportCounters::default());
        let transport =
            ScriptedTransport::new(vec![ScriptedReceive::Pending], Arc::clone(&counters))
                .force_action(ScriptedOperation::Pending);
        let watchdog = AgentWatchdogOptions {
            response_timeout_seconds: 100,
            cancel_timeout_seconds: 10,
            recovery_operation_timeout_seconds: 1,
        };

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            Client::connect_with_transport_and_options(
                Box::new(transport),
                dummy_transport_config(),
                watchdog,
            ),
        )
        .await
        .expect("initialization and cleanup should be bounded");
        let error = result.expect_err("a missing initialize response should fail the connection");

        assert_eq!(
            error.to_string(),
            "ACP initialize timed out; transport force termination timed out"
        );
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(counters.dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn lazy_reconnect_times_out_and_disposes_transport() {
        let transport = MockTransport::new(vec![&init_response(0)]);
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        client.transport = None;

        let counters = Arc::new(ScriptedTransportCounters::default());
        client.push_reconnect_transport(Box::new(ScriptedTransport::new(
            vec![ScriptedReceive::Pending],
            Arc::clone(&counters),
        )));
        client.watchdog.recovery_operation_timeout_seconds = 1;

        let result = tokio::time::timeout(Duration::from_secs(3), client.ensure_transport())
            .await
            .expect("lazy reconnect initialization and cleanup should be bounded");
        let error = match result {
            Ok(_) => panic!("a missing reconnect initialize response should fail"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "ACP lazy reconnect initialization timed out"
        );
        assert_eq!(counters.sends.load(Ordering::SeqCst), 1);
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(counters.dropped.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn test_connect_without_load_session() {
        let resp = rpc_response(
            0,
            serde_json::json!({
                "agentCapabilities": {},
                "agentInfo": {"name": "simple-agent"}
            }),
        );
        let transport = MockTransport::new(vec![&resp]);

        let client = Client::connect_with_transport(Box::new(transport), dummy_transport_config())
            .await
            .unwrap();

        assert!(!client.supports_load_session());
    }

    #[tokio::test]
    async fn test_connect_init_error() {
        let resp = rpc_error(0, -1, "unsupported protocol version");
        let transport = MockTransport::new(vec![&resp]);

        let result =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config()).await;
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("RPC error"));
    }

    #[tokio::test]
    async fn test_new_session() {
        let init_resp = init_response(0);
        let sess_resp = session_new_response(1, "sess_123");
        let transport = MockTransport::new(vec![&init_resp, &sess_resp]);
        let outgoing = transport.outgoing();

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let model = ModelInfo {
            id: "sonnet".into(),

            provider: Some("anthropic".into()),
        };

        let session_id = client
            .new_session("/project", &[], Some(&model))
            .await
            .unwrap();
        assert_eq!(session_id, "sess_123");

        // Verify session/new request
        let sent = outgoing.lock();
        let req: Value = serde_json::from_str(&sent[1]).unwrap();
        assert_eq!(req["method"], "session/new");
        assert_eq!(req["params"]["cwd"], "/project");
        assert_eq!(req["params"]["_meta"]["model"]["id"], "sonnet");
        assert_eq!(req["params"]["_meta"]["model"]["provider"], "anthropic");
    }

    #[tokio::test]
    async fn test_new_session_without_model_skips_acp_model_configuration() {
        let init_resp = init_response(0);
        let sess_resp = rpc_response(
            1,
            serde_json::json!({
                "sessionId": "sess_123",
                "configOptions": [{
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "agent-default",
                    "options": [{"value": "other-model", "name": "Other Model"}]
                }]
            }),
        );
        let transport = MockTransport::new(vec![&init_resp, &sess_resp]);
        let outgoing = transport.outgoing();

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let session_id = client.new_session("/project", &[], None).await.unwrap();

        assert_eq!(session_id, "sess_123");
        let sent = outgoing.lock();
        assert_eq!(sent.len(), 2);
        let request: Value = serde_json::from_str(&sent[1]).unwrap();
        assert!(request["params"].get("_meta").is_none());
    }

    #[tokio::test]
    async fn required_mode_checks_new_session_before_prompt() {
        for (reported, accepted) in [
            (
                serde_json::json!({"modeState":{"currentModeId":"read-only"}}),
                true,
            ),
            (
                serde_json::json!({"configOptions":[{"id":"mode","currentValue":"read-only"}]}),
                true,
            ),
            (
                serde_json::json!({"modeState":{"currentModeId":"read-only"},"configOptions":[{"id":"mode","currentValue":"agent"}]}),
                false,
            ),
            (
                serde_json::json!({"modeState":{"currentModeId":"agent"}}),
                false,
            ),
            (serde_json::json!({}), false),
        ] {
            let init = init_response(0);
            let mut result = reported;
            result["sessionId"] = serde_json::json!("sess_1");
            let session = rpc_response(1, result);
            let transport = MockTransport::new(vec![&init, &session]);
            let mut client =
                Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                    .await
                    .unwrap();
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            let response = client.new_session("/tmp", &[], None).await;
            assert_eq!(response.is_ok(), accepted);
            if !accepted {
                assert!(!client.is_connected());
                assert!(client.tool_policy_violated);
                assert!(
                    client
                        .prompt(
                            "sess_1",
                            vec![PromptContent::text("work")],
                            PromptTurnCancellation::disabled(),
                            &mut |_| {}
                        )
                        .await
                        .is_err()
                );
            }
            assert_eq!(client.clone().access.required_mode(), Some("read-only"));
            let restored: Client =
                serde_json::from_value(serde_json::to_value(&client).unwrap()).unwrap();
            assert_eq!(restored.access.required_mode(), Some("read-only"));
        }
    }

    #[tokio::test]
    async fn changing_required_mode_revokes_prior_session_proof() {
        let init = init_response(0);
        let unrestricted = rpc_response(1, serde_json::json!({"sessionId":"sess_A"}));
        let verified = rpc_response(
            2,
            serde_json::json!({"sessionId":"sess_B","modeState":{"currentModeId":"read-only"}}),
        );
        let transport = MockTransport::new(vec![&init, &unrestricted, &verified]);
        let outgoing = transport.outgoing();
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        client.new_session("/tmp", &[], None).await.unwrap();
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        assert_eq!(client.session_id(), None);
        assert!(
            client
                .prompt(
                    "sess_A",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {}
                )
                .await
                .is_err()
        );
        assert_eq!(outgoing.lock().len(), 2);
        client.new_session("/tmp", &[], None).await.unwrap();
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        assert_eq!(
            client.session_id(),
            Some("sess_B"),
            "same requirement preserves proof"
        );
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "other".into(),
        });
        assert_eq!(client.session_id(), None);
        assert!(
            client
                .clone()
                .prompt(
                    "sess_B",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {}
                )
                .await
                .is_err()
        );
        assert_eq!(outgoing.lock().len(), 3);
    }

    #[tokio::test]
    async fn required_mode_prompt_requires_verified_id_and_live_transport() {
        let init = init_response(0);
        let session = rpc_response(
            1,
            serde_json::json!({"sessionId":"sess_A","modeState":{"currentModeId":"read-only"}}),
        );
        let transport = MockTransport::new(vec![&init, &session]);
        let outgoing = transport.outgoing();
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        assert_eq!(
            client.new_session("/tmp", &[], None).await.unwrap(),
            "sess_A"
        );
        assert!(
            client
                .prompt(
                    "sess_B",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {}
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("not passed")
        );
        assert_eq!(
            outgoing.lock().len(),
            2,
            "mismatched session must not send prompt"
        );
        let mut cloned = client.clone();
        assert!(
            cloned
                .prompt(
                    "sess_A",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {}
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("reverified")
        );
        assert!(!cloned.is_connected());
        let mut restored: Client =
            serde_json::from_value(serde_json::to_value(&client).unwrap()).unwrap();
        assert!(
            restored
                .prompt(
                    "sess_A",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {}
                )
                .await
                .is_err()
        );
        assert!(!restored.is_connected());
    }

    #[tokio::test]
    async fn required_mode_rechecks_model_option_response() {
        let init = init_response(0);
        let session = rpc_response(
            1,
            serde_json::json!({
                "sessionId":"sess_A",
                "modeState":{"currentModeId":"read-only"},
                "configOptions":[{"id":"model","category":"model","currentValue":"old","options":[{"value":"new"}]},
                                 {"id":"mode","currentValue":"read-only"}]
            }),
        );
        let changed = rpc_response(
            2,
            serde_json::json!({"configOptions":[
                {"id":"model","currentValue":"new"}, {"id":"mode","currentValue":"agent"}
            ]}),
        );
        let transport = MockTransport::new(vec![&init, &session, &changed]);
        let outgoing = transport.outgoing();
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        let model = ModelInfo {
            id: "new".into(),
            provider: None,
        };
        assert!(
            client
                .new_session("/tmp", &[], Some(&model))
                .await
                .unwrap_err()
                .to_string()
                .contains("required mode")
        );
        assert!(client.tool_policy_violated);
        assert!(!client.is_connected());
        assert_eq!(
            outgoing.lock().len(),
            3,
            "no prompt may follow model mode drift"
        );
    }

    #[tokio::test]
    async fn malformed_mode_notification_during_model_selection_fails_closed() {
        let init = init_response(0);
        let session = rpc_response(
            1,
            serde_json::json!({
                "sessionId": "sess_A", "configOptions": [
                    {"id":"model","category":"model","currentValue":"old","options":[{"value":"new"}]},
                    {"id":"mode","currentValue":"read-only"}
                ]
            }),
        );
        let malformed = session_update(
            "sess_A",
            serde_json::json!({"sessionUpdate":"current_mode_update"}),
        );
        let model_only = rpc_response(
            2,
            serde_json::json!({"configOptions":[{"id":"model","currentValue":"new"}]}),
        );
        let transport = MockTransport::new(vec![&init, &session, &malformed, &model_only]);
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        let model = ModelInfo {
            id: "new".into(),
            provider: None,
        };
        assert!(client.new_session("/tmp", &[], Some(&model)).await.is_err());
        assert!(client.tool_policy_violated);
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn model_selection_requires_mode_in_returned_full_snapshot() {
        for (mode, accepted) in [(None, false), (Some("read-only"), true)] {
            let init = init_response(0);
            let session = rpc_response(
                1,
                serde_json::json!({
                    "sessionId": "sess_A",
                    "modeState": {"currentModeId": "read-only"},
                    "configOptions": [
                        {"id":"model","category":"model","currentValue":"old","options":[{"value":"new"}]},
                        {"id":"mode","currentValue":"read-only"}
                    ]
                }),
            );
            let mut options = vec![serde_json::json!({"id":"model","currentValue":"new"})];
            if let Some(mode) = mode {
                options.push(serde_json::json!({"id":"mode","currentValue":mode}));
            }
            let changed = rpc_response(2, serde_json::json!({"configOptions":options}));
            let transport = MockTransport::new(vec![&init, &session, &changed]);
            let outgoing = transport.outgoing();
            let mut client =
                Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                    .await
                    .unwrap();
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            let model = ModelInfo {
                id: "new".into(),
                provider: None,
            };
            let result = client.new_session("/tmp", &[], Some(&model)).await;
            assert_eq!(result.is_ok(), accepted);
            if !accepted {
                assert!(client.tool_policy_violated);
                assert!(!client.is_connected());
                assert!(
                    client
                        .prompt(
                            "sess_A",
                            vec![PromptContent::text("work")],
                            PromptTurnCancellation::disabled(),
                            &mut |_| {}
                        )
                        .await
                        .is_err()
                );
            }
            assert_eq!(outgoing.lock().len(), 3, "no prompt after missing mode");
        }
    }

    #[tokio::test]
    async fn model_selection_rejects_mode_change_before_model_only_response() {
        for reported_mode in ["agent", "read-only"] {
            let init = init_response(0);
            let session = rpc_response(
                1,
                serde_json::json!({
                    "sessionId": "sess_A",
                    "configOptions": [
                        {"id": "model", "category": "model", "currentValue": "old", "options": [{"value": "new"}]},
                        {"id": "mode", "currentValue": "read-only"}
                    ]
                }),
            );
            let notification = session_update(
                "sess_A",
                serde_json::json!({
                    "sessionUpdate": "current_mode_update", "currentModeId": reported_mode
                }),
            );
            let changed = rpc_response(
                2,
                serde_json::json!({
                    "configOptions": [{"id": "model", "currentValue": "new"}]
                }),
            );
            let transport = MockTransport::new(vec![&init, &session, &notification, &changed]);
            let outgoing = transport.outgoing();
            let mut client =
                Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                    .await
                    .unwrap();
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            let model = ModelInfo {
                id: "new".into(),
                provider: None,
            };
            let result = client.new_session("/tmp", &[], Some(&model)).await;
            if reported_mode == "agent" {
                assert!(result.is_err());
                assert!(client.tool_policy_violated);
                assert!(!client.is_connected());
            } else {
                assert!(result.is_err(), "model-only snapshot omits required mode");
                assert!(client.tool_policy_violated);
                assert!(!client.is_connected());
            }
            assert_eq!(
                outgoing.lock().len(),
                3,
                "no prompt may be sent during setup"
            );
        }
    }

    #[tokio::test]
    async fn test_new_session_sets_qualified_model_config_option() {
        let init_resp = init_response(0);
        let sess_resp = rpc_response(
            1,
            serde_json::json!({
                "sessionId": "sess_123",
                "configOptions": [{
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "github-copilot/gpt-5.5",
                    "options": [{
                        "value": "github-copilot/claude-opus-4.8",
                        "name": "Claude Opus 4.8"
                    }]
                }]
            }),
        );
        let set_resp = rpc_response(
            2,
            serde_json::json!({
                "configOptions": [{
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "github-copilot/claude-opus-4.8",
                    "options": [{
                        "value": "github-copilot/claude-opus-4.8",
                        "name": "Claude Opus 4.8"
                    }]
                }]
            }),
        );
        let transport = MockTransport::new(vec![&init_resp, &sess_resp, &set_resp]);
        let outgoing = transport.outgoing();

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        let model = ModelInfo {
            id: "claude-opus-4.8".into(),
            provider: Some("github-copilot".into()),
        };

        let session_id = client
            .new_session("/project", &[], Some(&model))
            .await
            .unwrap();

        assert_eq!(session_id, "sess_123");
        let sent = outgoing.lock();
        assert_eq!(sent.len(), 3);
        let request: Value = serde_json::from_str(&sent[2]).unwrap();
        assert_eq!(request["method"], "session/set_config_option");
        assert_eq!(request["params"]["sessionId"], "sess_123");
        assert_eq!(request["params"]["configId"], "model");
        assert_eq!(request["params"]["value"], "github-copilot/claude-opus-4.8");
    }

    #[tokio::test]
    async fn test_new_session_sets_unqualified_model_config_option() {
        let init_resp = init_response(0);
        let sess_resp = rpc_response(
            1,
            serde_json::json!({
                "sessionId": "sess_123",
                "configOptions": [{
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "claude-sonnet-5",
                    "options": [{"value": "gpt-5.6-sol", "name": "GPT-5.6 Sol"}]
                }]
            }),
        );
        let set_resp = rpc_response(
            2,
            serde_json::json!({
                "configOptions": [{
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "gpt-5.6-sol",
                    "options": [{"value": "gpt-5.6-sol", "name": "GPT-5.6 Sol"}]
                }]
            }),
        );
        let transport = MockTransport::new(vec![&init_resp, &sess_resp, &set_resp]);
        let outgoing = transport.outgoing();

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        let model = ModelInfo {
            id: "gpt-5.6-sol".into(),
            provider: Some("github-copilot".into()),
        };

        client
            .new_session("/project", &[], Some(&model))
            .await
            .unwrap();

        let sent = outgoing.lock();
        assert_eq!(sent.len(), 3);
        let request: Value = serde_json::from_str(&sent[2]).unwrap();
        assert_eq!(request["params"]["value"], "gpt-5.6-sol");
    }

    #[test]
    fn descriptor_from_config_options_reads_returned_values_only() {
        let options: Vec<SessionConfigOption> = serde_json::from_value(serde_json::json!([
            {
                "id": "model",
                "category": "model",
                "currentValue": "github-copilot/gpt-5.6-sol",
                "options": [{"value": "github-copilot/gpt-5.6-sol"}]
            },
            {
                "id": "thought_level",
                "category": "thought_level",
                "currentValue": "high",
                "options": [{"value": "high"}]
            },
            {
                "id": "context_size",
                "category": "model_config",
                "currentValue": "1m",
                "options": [{"value": "1m"}]
            }
        ]))
        .unwrap();

        let descriptor = Client::descriptor_from_config_options(&options).unwrap();
        assert_eq!(
            descriptor.model.as_deref(),
            Some("github-copilot/gpt-5.6-sol")
        );
        assert_eq!(descriptor.reasoning.as_deref(), Some("high"));
        assert_eq!(descriptor.context.as_deref(), Some("1m"));
    }

    #[test]
    fn descriptor_from_config_options_ignores_non_context_model_config() {
        // A `model_config` option that is NOT a semantic context id (e.g. speed
        // mode) must never be taken as context.
        let options: Vec<SessionConfigOption> = serde_json::from_value(serde_json::json!([
            {
                "id": "model",
                "category": "model",
                "currentValue": "gpt-5.6-sol",
                "options": [{"value": "gpt-5.6-sol"}]
            },
            {
                "id": "speed_mode",
                "category": "model_config",
                "currentValue": "fast",
                "options": [{"value": "fast"}]
            }
        ]))
        .unwrap();

        let descriptor = Client::descriptor_from_config_options(&options).unwrap();
        assert_eq!(descriptor.model.as_deref(), Some("gpt-5.6-sol"));
        assert!(descriptor.context.is_none());
        assert!(descriptor.reasoning.is_none());
    }

    #[test]
    fn descriptor_from_config_options_uses_id_model_fallback() {
        let options: Vec<SessionConfigOption> = serde_json::from_value(serde_json::json!([
            {
                "id": "model",
                "currentValue": "gpt-5.6-sol",
                "options": [{"value": "gpt-5.6-sol"}]
            }
        ]))
        .unwrap();

        let descriptor = Client::descriptor_from_config_options(&options).unwrap();
        assert_eq!(descriptor.model.as_deref(), Some("gpt-5.6-sol"));
    }

    #[test]
    fn descriptor_from_config_options_none_when_empty() {
        assert!(Client::descriptor_from_config_options(&[]).is_none());

        let unrelated: Vec<SessionConfigOption> = serde_json::from_value(serde_json::json!([
            {"id": "speed_mode", "category": "model_config", "currentValue": "fast", "options": []}
        ]))
        .unwrap();
        assert!(Client::descriptor_from_config_options(&unrelated).is_none());
    }

    #[tokio::test]
    async fn new_session_captures_descriptor_case_a_no_configured_model() {
        // Case A: no model configured, so model-selection enforcement never runs.
        let init_resp = init_response(0);
        let sess_resp = rpc_response(
            1,
            serde_json::json!({
                "sessionId": "sess_123",
                "configOptions": [
                    {"id": "model", "category": "model", "currentValue": "gpt-5.6-sol", "options": []},
                    {"id": "context_size", "category": "model_config", "currentValue": "1m", "options": []},
                    {"id": "thought_level", "category": "thought_level", "currentValue": "high", "options": []}
                ]
            }),
        );
        let transport = MockTransport::new(vec![&init_resp, &sess_resp]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        client.new_session("/project", &[], None).await.unwrap();

        let descriptor = cowboy_agent_client::Client::session_descriptor(&client).unwrap();
        assert_eq!(descriptor.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(descriptor.context.as_deref(), Some("1m"));
        assert_eq!(descriptor.reasoning.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn new_session_captures_descriptor_case_b_exact_id_ignores_sentinel_provider() {
        // Case B: configured id equals the returned unqualified model id, but the
        // configured provider is a distinct sentinel. Selection succeeds through
        // the exact-id branch (`value == model.id`), so the sentinel provider is
        // never consulted and never enters the descriptor.
        let init_resp = init_response(0);
        let sess_resp = rpc_response(
            1,
            serde_json::json!({
                "sessionId": "sess_123",
                "configOptions": [
                    {"id": "model", "category": "model", "currentValue": "gpt-5.6-sol", "options": [{"value": "gpt-5.6-sol"}]},
                    {"id": "thought_level", "category": "thought_level", "currentValue": "high", "options": []}
                ]
            }),
        );
        let transport = MockTransport::new(vec![&init_resp, &sess_resp]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        let model = ModelInfo {
            id: "gpt-5.6-sol".into(),
            provider: Some("SENTINEL-PROVIDER".into()),
        };

        client
            .new_session("/project", &[], Some(&model))
            .await
            .unwrap();

        let descriptor = cowboy_agent_client::Client::session_descriptor(&client).unwrap();
        assert_eq!(descriptor.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(descriptor.reasoning.as_deref(), Some("high"));
        let rendered = format!("{descriptor:?}");
        assert!(
            !rendered.contains("SENTINEL-PROVIDER"),
            "sentinel provider leaked into descriptor: {rendered}"
        );
    }

    #[tokio::test]
    async fn test_new_session_rejects_unavailable_configured_model() {
        let init_resp = init_response(0);
        let sess_resp = rpc_response(
            1,
            serde_json::json!({
                "sessionId": "sess_123",
                "configOptions": [{
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "gpt-5.5",
                    "options": [{"value": "gpt-5.5", "name": "GPT-5.5"}]
                }]
            }),
        );
        let transport = MockTransport::new(vec![&init_resp, &sess_resp]);
        let outgoing = transport.outgoing();

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        let model = ModelInfo {
            id: "gpt-5.6-sol".into(),
            provider: Some("github-copilot".into()),
        };

        let error = client
            .new_session("/project", &[], Some(&model))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("does not offer configured model")
        );
        assert_eq!(outgoing.lock().len(), 2);
    }

    #[tokio::test]
    async fn test_prompt_with_streaming_updates() {
        let init_resp = init_response(0);
        let update1 = text_chunk_update("sess_1", "Hello ");
        let update2 = text_chunk_update("sess_1", "world!");
        let pr = prompt_response(1, "end_turn");

        let transport = MockTransport::new(vec![&init_resp, &update1, &update2, &pr]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let mut updates = Vec::new();
        let stop = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("say hello")],
                PromptTurnCancellation::disabled(),
                &mut |update| updates.push(update),
            )
            .await
            .unwrap();

        assert!(matches!(stop, StopReason::EndTurn));
        assert_eq!(updates.len(), 2);
        assert!(matches!(&updates[0], Event::MessageChunk { .. }));
        assert!(matches!(&updates[1], Event::MessageChunk { .. }));
    }

    #[tokio::test]
    async fn test_prompt_captures_text_streamed_after_response() {
        // Some backends (e.g. Oh My Pi) ack the prompt — sending the Response —
        // before they stream the reply. The drain must capture that trailing
        // text and mark the turn as having produced text, so the continuation
        // logic does not fire a spurious "Continue" follow-up after a thought.
        let init_resp = init_response(0);
        let thought = session_update(
            "sess_1",
            serde_json::json!({
                "sessionUpdate": "agent_thought_chunk",
                "content": {"text": "thinking"}
            }),
        );
        let pr = prompt_response(1, "end_turn");
        let late_text = text_chunk_update("sess_1", "late reply");

        let transport = MockTransport::new(vec![&init_resp, &thought, &pr, &late_text]);
        let outgoing = transport.outgoing();

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let mut updates = Vec::new();
        let stop = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("hi")],
                PromptTurnCancellation::disabled(),
                &mut |update| updates.push(update),
            )
            .await
            .unwrap();

        assert!(matches!(stop, StopReason::EndTurn));
        // The text streamed after the Response still reached the handler.
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, Event::MessageChunk { .. })),
            "trailing agent_message_chunk should be delivered"
        );
        // Only init + the single session/prompt were sent — no Continue follow-up.
        let sent = outgoing.lock();
        assert_eq!(sent.len(), 2, "should not send a Continue follow-up");
    }

    #[tokio::test]
    async fn test_prompt_continues_empty_end_turn_without_progress() {
        // OMP can occasionally finish a turn with only housekeeping updates and
        // no visible text. Treat that as an empty acknowledgement and ask it to
        // continue instead of returning a blank prompt to the caller.
        let init_resp = init_response(0);
        let empty_turn = prompt_response(1, "end_turn");
        let continued_turn = prompt_response(2, "end_turn");
        let continued_text = text_chunk_update("sess_1", "continued reply");

        let transport = MockTransport::new(vec![
            &init_resp,
            &empty_turn,
            &continued_turn,
            &continued_text,
        ]);
        let outgoing = transport.outgoing();

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let mut updates = Vec::new();
        let stop = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("hi")],
                PromptTurnCancellation::disabled(),
                &mut |update| updates.push(update),
            )
            .await
            .unwrap();

        assert!(matches!(stop, StopReason::EndTurn));
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, Event::MessageChunk { .. })),
            "continued turn should deliver agent_message_chunk text"
        );

        let sent = outgoing.lock();
        assert_eq!(sent.len(), 3, "init + original prompt + Continue");
        let continue_req: Value = serde_json::from_str(&sent[2]).unwrap();
        assert_eq!(continue_req["method"], "session/prompt");
        assert_eq!(continue_req["params"]["prompt"][0]["text"], "Continue");
    }

    #[tokio::test]
    async fn test_prompt_errors_after_repeated_empty_end_turns() {
        // The live PID 87922 hang reproduced as OMP returning only housekeeping
        // updates plus end_turn for the original selector prompt and every
        // automatic "Continue". Once a continuation also produces no text,
        // Cowboy should surface the empty backend response instead of treating
        // repeated blank turns as a successful prompt.
        let init_resp = init_response(0);
        let empty_1 = prompt_response(1, "end_turn");
        let empty_2 = prompt_response(2, "end_turn");
        let empty_3 = prompt_response(3, "end_turn");
        let empty_4 = prompt_response(4, "end_turn");
        let empty_5 = prompt_response(5, "end_turn");
        let empty_6 = prompt_response(6, "end_turn");
        let transport = MockTransport::new(vec![
            &init_resp, &empty_1, &empty_2, &empty_3, &empty_4, &empty_5, &empty_6,
        ]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let result = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("select workflow")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await;

        assert!(
            result.is_err(),
            "repeated empty ACP end_turn responses should fail instead of returning success: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_prompt_auto_grants_permission() {
        let init_resp = init_response(0);
        let perm_req = permission_request(100, "sess_1", "write_file");
        let pr = prompt_response(1, "end_turn");

        let transport = MockTransport::new(vec![&init_resp, &perm_req, &pr]);
        let outgoing = transport.outgoing();

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let stop = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("write a file")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap();

        assert!(matches!(stop, StopReason::EndTurn));

        // Verify permission was granted (3 outgoing: init + prompt + permission response)
        let sent = outgoing.lock();
        assert_eq!(sent.len(), 3);
        let perm_resp: Value = serde_json::from_str(&sent[2]).unwrap();
        assert_eq!(perm_resp["id"], 100);
        assert_eq!(perm_resp["result"]["outcome"]["outcome"], "selected");
        assert_eq!(perm_resp["result"]["outcome"]["optionId"], "allow-once");
    }

    #[tokio::test]
    async fn string_permission_request_id_is_preserved_when_granted() {
        let init = init_response(0);
        let permission = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "permission/grant",
            "method": "session/request_permission",
            "params": {
                "sessionId": "sess_1",
                "options": [{ "kind": "allow_once", "optionId": "allow-once" }]
            }
        })
        .to_string();
        let response = prompt_response(1, "end_turn");
        let transport = MockTransport::new(vec![&init, &permission, &response]);
        let outgoing = transport.outgoing();
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        assert!(matches!(
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
                .unwrap(),
            StopReason::EndTurn
        ));
        let sent = outgoing.lock();
        let permission_response: Value = serde_json::from_str(&sent[2]).unwrap();
        assert_eq!(permission_response["id"], "permission/grant");
        assert_eq!(
            permission_response["result"]["outcome"]["optionId"],
            "allow-once"
        );
    }

    #[tokio::test]
    async fn direct_receive_cancels_string_permission_id_without_options() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        incoming
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "permission/setup",
                    "method": "session/request_permission",
                    "params": { "sessionId": "sess_1" }
                })
                .to_string(),
            )
            .unwrap();
        incoming.send(rpc_response(42, Value::Null)).unwrap();

        assert!(matches!(
            client.recv_message_direct().await.unwrap(),
            Message::Response { id: 42, .. }
        ));
        let denied = next_outgoing(&mut outgoing).await;
        assert_eq!(denied["id"], "permission/setup");
        assert_eq!(denied["result"]["outcome"]["outcome"], "cancelled");
        assert!(!client.tool_policy_violated);
    }

    #[tokio::test]
    async fn invalid_permission_ids_poison_and_terminate_restricted_transports() {
        for deny_permissions in [true, false] {
            for direct in [true, false] {
                for id in [
                    None,
                    Some(Value::Null),
                    Some(serde_json::json!(-1)),
                    Some(serde_json::json!(1.5)),
                    Some(serde_json::json!({ "unexpected": "id" })),
                ] {
                    let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
                    if deny_permissions {
                        client.set_access(AgentAccess::DenyEscalation {
                            acp_mode: "read-only".into(),
                        });
                    } else {
                        client.set_access(AgentAccess::DenyAll);
                    }
                    let mut request = serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "session/request_permission",
                        "params": { "sessionId": "sess_1" }
                    });
                    if let Some(id) = &id {
                        request["id"] = id.clone();
                    }
                    incoming.send(request.to_string()).unwrap();

                    let received = if direct {
                        client.recv_message_direct().await
                    } else {
                        client.recv_message_raw().await
                    };
                    assert!(
                        received.unwrap_err().to_string().contains("invalid id"),
                        "direct={direct}, deny_permissions={deny_permissions}, id={id:?}"
                    );
                    assert!(client.tool_policy_violated);
                    assert!(client.transport.is_none());
                    assert!(outgoing.try_recv().is_err());
                }
            }
        }
    }

    #[tokio::test]
    async fn malformed_json_terminates_restricted_receive_paths() {
        for direct in [true, false] {
            let (mut client, incoming, _) = controlled_watchdog_client().await;
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            incoming
                .send("{\"method\":\"session/request_permission\",\"id\":".into())
                .unwrap();

            let received = if direct {
                client.recv_message_direct().await
            } else {
                client.recv_message_raw().await
            };
            assert!(
                received
                    .unwrap_err()
                    .to_string()
                    .contains("Malformed ACP JSON")
            );
            assert!(client.tool_policy_violated);
            assert!(client.transport.is_none());
        }
    }

    #[tokio::test]
    async fn denied_permission_does_not_block_tools_or_poison_session() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        let task = tokio::spawn(async move {
            let mut events = Vec::new();
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |event| events.push(event),
                )
                .await;
            (client, result, events)
        });

        assert_eq!(
            next_outgoing(&mut outgoing).await["method"],
            "session/prompt"
        );
        incoming
            .send(tool_call_started(
                "sess_1", "call_1", "ordinary", "execute", "pending",
            ))
            .unwrap();
        incoming
            .send(permission_request(101, "sess_1", "escalation"))
            .unwrap();
        let denied = next_outgoing(&mut outgoing).await;
        assert_eq!(denied["id"], 101);
        assert_eq!(denied["result"]["outcome"]["outcome"], "cancelled");
        incoming
            .send(tool_call_progress("sess_1", "call_1", "completed"))
            .unwrap();
        incoming.send(text_chunk_update("sess_1", "done")).unwrap();
        incoming.send(prompt_response(1, "end_turn")).unwrap();
        let (client, result, events) = task.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ToolCall { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ToolCallUpdate { .. }))
        );
        assert!(!client.tool_policy_violated);
        assert_eq!(client.clone().access.required_mode(), Some("read-only"));
        let restored: Client =
            serde_json::from_value(serde_json::to_value(&client).unwrap()).unwrap();
        assert_eq!(restored.access.required_mode(), Some("read-only"));
    }

    #[tokio::test]
    async fn denied_permission_with_missing_options_still_cancels() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });

        let _prompt = next_outgoing(&mut outgoing).await;
        incoming.send(serde_json::json!({"jsonrpc":"2.0","id":102,"method":"session/request_permission","params":{"sessionId":"sess_1","options":"invalid"}}).to_string()).unwrap();
        assert_eq!(
            next_outgoing(&mut outgoing).await["result"]["outcome"]["outcome"],
            "cancelled"
        );
        incoming
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming.send(prompt_response(1, "end_turn")).unwrap();
        assert!(matches!(task.await.unwrap().unwrap(), StopReason::EndTurn));
    }

    #[tokio::test]
    async fn restricted_receive_cancels_string_permission_id_without_options() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        incoming
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "permission/prompt",
                    "method": "session/request_permission",
                    "params": { "sessionId": "sess_1" }
                })
                .to_string(),
            )
            .unwrap();
        incoming.send(rpc_response(42, Value::Null)).unwrap();

        assert!(matches!(
            client.recv_message().await.unwrap(),
            Message::Response { id: 42, .. }
        ));
        let denied = next_outgoing(&mut outgoing).await;
        assert_eq!(denied["id"], "permission/prompt");
        assert_eq!(denied["result"]["outcome"]["outcome"], "cancelled");
        assert!(!client.tool_policy_violated);
    }

    #[tokio::test]
    async fn default_allow_skips_unreplyable_permission_requests() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        incoming
            .send(
                serde_json::json!({ "method": "session/request_permission", "id": null })
                    .to_string(),
            )
            .unwrap();
        incoming.send(rpc_response(42, Value::Null)).unwrap();

        assert!(matches!(
            client.recv_message().await.unwrap(),
            Message::Response { id: 42, .. }
        ));
        assert!(!client.tool_policy_violated);
        assert!(client.transport.is_some());
        assert!(outgoing.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn denied_requests_cannot_extend_session_setup_deadlines() {
        for load in [false, true] {
            let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            let task = tokio::spawn(async move {
                let result = if load {
                    client.load_session("sess_1", "/tmp", &[]).await.map(|_| ())
                } else {
                    client.new_session("/tmp", &[], None).await.map(|_| ())
                };
                (client, result)
            });
            let request = next_outgoing(&mut outgoing).await;
            assert_eq!(
                request["method"],
                if load { "session/load" } else { "session/new" }
            );
            for id in 300..305 {
                tokio::time::advance(Duration::from_millis(500)).await;
                incoming
                    .send(permission_request(id, "sess_1", "escalation"))
                    .unwrap();
                assert_eq!(
                    next_outgoing(&mut outgoing).await["result"]["outcome"]["outcome"],
                    "cancelled"
                );
            }
            tokio::time::advance(Duration::from_millis(600)).await;
            let (client, result) = task.await.unwrap();
            assert!(result.unwrap_err().to_string().contains("timed out"));
            assert!(!client.is_connected());
            assert!(client.tool_policy_violated);
        }
    }

    #[tokio::test]
    async fn restricted_permission_is_denied_during_session_setup() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        let task = tokio::spawn(async move { client.new_session("/tmp", &[], None).await });
        let request = next_outgoing(&mut outgoing).await;
        assert_eq!(request["method"], "session/new");
        incoming
            .send(permission_request(103, "sess_1", "setup escalation"))
            .unwrap();
        assert_eq!(
            next_outgoing(&mut outgoing).await["result"]["outcome"]["outcome"],
            "cancelled"
        );
        incoming.send(serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":{"sessionId":"sess_1","configOptions":[{"id":"mode","currentValue":"read-only"}]}}).to_string()).unwrap();
        assert_eq!(task.await.unwrap().unwrap(), "sess_1");
    }

    #[tokio::test]
    async fn restricted_permission_is_denied_during_trailing_drain() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        let _prompt = next_outgoing(&mut outgoing).await;
        incoming
            .send(text_chunk_update("sess_1", "answer"))
            .unwrap();
        incoming.send(prompt_response(1, "end_turn")).unwrap();
        incoming
            .send(permission_request(104, "sess_1", "trailing escalation"))
            .unwrap();
        assert_eq!(
            next_outgoing(&mut outgoing).await["result"]["outcome"]["outcome"],
            "cancelled"
        );
        assert!(matches!(task.await.unwrap().unwrap(), StopReason::EndTurn));
    }

    #[tokio::test(start_paused = true)]
    async fn restricted_denial_send_failure_terminates_and_poisoned_client_cannot_reconnect() {
        for action in [
            ScriptedOperation::Pending,
            ScriptedOperation::Error("send failed"),
        ] {
            let counters = Arc::new(ScriptedTransportCounters::default());
            let transport = ScriptedTransport::new(
                vec![
                    ScriptedReceive::Message(init_response(0)),
                    ScriptedReceive::Message(permission_request(100, "sess_1", "escalation")),
                ],
                counters.clone(),
            )
            .send_action(2, action)
            .force_action(ScriptedOperation::Error("termination failed"));
            let (mut client, _) = scripted_client(transport).await;
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            client.session_id = Some("sess_1".into());
            let error = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("denial") || error.to_string().contains("tool"));
            assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
            assert!(client.transport.is_none());
            assert!(client.tool_policy_violated);
            assert_denied_prompt(
                client
                    .prompt(
                        "sess_1",
                        vec![PromptContent::text("again")],
                        PromptTurnCancellation::disabled(),
                        &mut |_| {},
                    )
                    .await,
            );
            assert_denied_prompt(
                client
                    .new_session("/tmp", &[], None)
                    .await
                    .map(|_| StopReason::EndTurn),
            );
            assert_denied_prompt(
                client
                    .load_session("sess_1", "/tmp", &[])
                    .await
                    .map(|_| StopReason::EndTurn),
            );
            assert_denied_prompt(client.ensure_transport().await.map(|_| StopReason::EndTurn));
            assert_eq!(counters.sends.load(Ordering::SeqCst), 3);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn deny_all_never_completes_when_denial_send_is_blocked() {
        for case in [
            "external",
            "watchdog_grace",
            "trailing_response",
            "send_error",
            "terminate_pending",
        ] {
            let counters = Arc::new(ScriptedTransportCounters::default());
            let mut incoming = vec![ScriptedReceive::Message(init_response(0))];
            if case == "watchdog_grace" {
                incoming.push(ScriptedReceive::Pending);
            }
            if case == "trailing_response" {
                incoming.push(ScriptedReceive::Message(text_chunk_update(
                    "sess_1", "answer",
                )));
                incoming.push(ScriptedReceive::Message(prompt_response(1, "end_turn")));
            }
            incoming.push(ScriptedReceive::Message(permission_request(
                100,
                "sess_1",
                "private tool",
            )));
            if case != "trailing_response" {
                incoming.push(ScriptedReceive::Message(prompt_response(1, "end_turn")));
            }
            let denial_send = if case == "watchdog_grace" { 3 } else { 2 };
            let send_action = if case == "send_error" {
                ScriptedOperation::Error("private send failure")
            } else {
                ScriptedOperation::Pending
            };
            let mut transport = ScriptedTransport::new(incoming, counters.clone())
                .send_action(denial_send, send_action);
            if case == "terminate_pending" {
                transport = transport.force_action(ScriptedOperation::Pending);
            }
            let (mut client, _) = scripted_client(transport).await;
            client.set_access(AgentAccess::DenyAll);
            let cancellation = if case == "external" {
                PromptTurnCancellation::from_future(async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                })
            } else {
                PromptTurnCancellation::disabled()
            };
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("answer")],
                    cancellation,
                    &mut |_| {},
                )
                .await;
            assert_denied_prompt(result);
            assert_eq!(
                counters.force_terminated.load(Ordering::SeqCst),
                1,
                "{case}"
            );
            assert!(client.transport.is_none(), "{case}");
        }
    }

    fn assert_denied_prompt(result: anyhow::Result<StopReason>) {
        assert_eq!(result.unwrap_err().to_string(), DENIED_TOOL_ERROR);
    }

    #[tokio::test]
    async fn deny_all_cancels_permission_and_poisoned_client_cannot_prompt_again() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyAll);
        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("write a file")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });

        assert_eq!(
            next_outgoing(&mut outgoing_rx).await["method"],
            "session/prompt"
        );
        incoming_tx
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "permission/forbidden",
                    "method": "session/request_permission",
                    "params": { "sessionId": "sess_1", "toolCall": { "title": "private-tool-title" } }
                })
                .to_string(),
            )
            .unwrap();
        let response = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(response["id"], "permission/forbidden");
        assert_eq!(response["result"]["outcome"]["outcome"], "cancelled");
        assert!(response["result"]["outcome"].get("optionId").is_none());

        let (mut client, result) = prompt.await.unwrap();
        assert_denied_prompt(result);
        assert_denied_prompt(
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("retry")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await,
        );
        assert!(
            outgoing_rx.try_recv().is_err(),
            "no retry may be dispatched"
        );

        let mut cloned = client.clone();
        assert_denied_prompt(
            cloned
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("retry")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await,
        );
        let persisted = serde_json::to_value(&client).unwrap();
        let mut restored: Client = serde_json::from_value(persisted).unwrap();
        restored.set_access(AgentAccess::Default);
        assert_denied_prompt(
            restored
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("retry")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await,
        );
    }

    #[tokio::test]
    async fn deny_all_rejects_unprompted_tool_events_without_forwarding_them() {
        for forbidden in [
            tool_call_started(
                "sess_1",
                "call_1",
                "secret tool title",
                "execute",
                "pending",
            ),
            tool_call_progress("sess_1", "call_1", "completed"),
        ] {
            let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
            client.set_access(AgentAccess::DenyAll);
            incoming_tx.send(forbidden).unwrap();
            incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
            let prompt = tokio::spawn(async move {
                let mut events = Vec::new();
                let result = client
                    .prompt(
                        "sess_1",
                        vec![PromptContent::text("answer without tools")],
                        PromptTurnCancellation::disabled(),
                        &mut |event| events.push(event),
                    )
                    .await;
                (result, events)
            });

            assert_eq!(
                next_outgoing(&mut outgoing_rx).await["method"],
                "session/prompt"
            );
            let (result, events) = prompt.await.unwrap();
            assert_denied_prompt(result);
            assert!(events.is_empty(), "forbidden events must not be forwarded");
        }
    }

    #[tokio::test]
    async fn foreign_or_missing_session_activity_cannot_keep_restricted_prompt_alive() {
        for message in [
            tool_call_started("foreign", "call_1", "fake", "execute", "pending"),
            tool_call_started("", "call_1", "fake", "execute", "pending"),
            text_chunk_update("foreign", "leak"),
            text_chunk_update("", "leak"),
            permission_request(501, "foreign", "escalation"),
            permission_request(502, "", "escalation"),
        ] {
            let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            client.session_id = Some("sess_1".into());
            let task = tokio::spawn(async move {
                let result = client
                    .prompt(
                        "sess_1",
                        vec![PromptContent::text("work")],
                        PromptTurnCancellation::disabled(),
                        &mut |_| {},
                    )
                    .await;
                (client, result)
            });
            let _prompt = next_outgoing(&mut outgoing).await;
            incoming.send(message).unwrap();
            let (client, result) = task.await.unwrap();
            assert_denied_prompt(result);
            assert!(client.tool_policy_violated);
            assert!(!client.is_connected());
            assert!(
                outgoing.try_recv().is_err(),
                "foreign request cannot receive a grant"
            );
        }
    }

    #[tokio::test]
    async fn observed_mode_change_aborts_required_session_before_forwarding() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        let task = tokio::spawn(async move {
            let mut events = Vec::new();
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |event| events.push(event),
                )
                .await;
            (client, result, events)
        });
        let _prompt = next_outgoing(&mut outgoing).await;
        incoming
            .send(session_update(
                "sess_1",
                serde_json::json!({"sessionUpdate":"current_mode_update","currentModeId":"agent"}),
            ))
            .unwrap();
        let (client, result, events) = task.await.unwrap();
        assert_denied_prompt(result);
        assert!(events.is_empty());
        assert!(client.tool_policy_violated);
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn model_updates_do_not_invalidate_required_mode_but_full_snapshots_do() {
        for (update, accepted) in [
            (
                serde_json::json!({"sessionUpdate":"current_model_update","currentModelId":"new"}),
                true,
            ),
            (
                serde_json::json!({"sessionUpdate":"config_option_update","configOptions":[{"id":"model","currentValue":"new"}]}),
                false,
            ),
            (
                serde_json::json!({"sessionUpdate":"config_option_update","configOptions":[{"id":"model","currentValue":"new"},{"id":"mode","currentValue":"read-only"}]}),
                true,
            ),
            (
                serde_json::json!({"sessionUpdate":"config_option_update","configOptions":[{"id":"mode","currentValue":"agent"}]}),
                false,
            ),
        ] {
            let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            client.session_id = Some("sess_1".into());
            let task = tokio::spawn(async move {
                let result = client
                    .prompt(
                        "sess_1",
                        vec![PromptContent::text("work")],
                        PromptTurnCancellation::disabled(),
                        &mut |_| {},
                    )
                    .await;
                (client, result)
            });
            let _prompt = next_outgoing(&mut outgoing).await;
            incoming.send(session_update("sess_1", update)).unwrap();
            if accepted {
                incoming.send(text_chunk_update("sess_1", "ok")).unwrap();
                incoming.send(prompt_response(1, "end_turn")).unwrap();
            }
            let (client, result) = task.await.unwrap();
            assert_eq!(result.is_ok(), accepted);
            assert_eq!(client.tool_policy_violated, !accepted);
            assert_eq!(client.is_connected(), accepted);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn forbidden_tool_event_terminates_even_when_force_termination_fails() {
        for event in [
            tool_call_started("sess_1", "call_1", "forbidden", "execute", "pending"),
            tool_call_progress("sess_1", "call_1", "completed"),
        ] {
            for force in [
                ScriptedOperation::Error("cannot terminate"),
                ScriptedOperation::Pending,
            ] {
                let counters = Arc::new(ScriptedTransportCounters::default());
                let transport = ScriptedTransport::new(
                    vec![
                        ScriptedReceive::Message(init_response(0)),
                        ScriptedReceive::Message(event.clone()),
                    ],
                    counters.clone(),
                )
                .force_action(force);
                let (mut client, _) = scripted_client(transport).await;
                client.set_access(AgentAccess::DenyAll);
                assert_denied_prompt(
                    client
                        .prompt(
                            "sess_1",
                            vec![PromptContent::text("work")],
                            PromptTurnCancellation::disabled(),
                            &mut |_| {},
                        )
                        .await,
                );
                assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
                assert!(client.transport.is_none());
                assert!(client.tool_policy_violated);
                assert_denied_prompt(
                    client
                        .new_session("/tmp", &[], None)
                        .await
                        .map(|_| StopReason::EndTurn),
                );
                assert_eq!(counters.sends.load(Ordering::SeqCst), 2);
            }
        }
    }

    #[tokio::test]
    async fn deny_all_still_completes_tool_free_prompts() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyAll);
        let prompt = tokio::spawn(async move {
            let mut events = Vec::new();
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("answer without tools")],
                    PromptTurnCancellation::disabled(),
                    &mut |event| events.push(event),
                )
                .await;
            (result, events)
        });

        assert_eq!(
            next_outgoing(&mut outgoing_rx).await["method"],
            "session/prompt"
        );
        incoming_tx
            .send(text_chunk_update("sess_1", "safe answer"))
            .unwrap();
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        let (result, events) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::MessageChunk { .. }));
    }

    #[tokio::test]
    async fn deny_all_rejects_tool_updates_trailing_successful_responses() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyAll);
        let prompt = tokio::spawn(async move {
            let mut events = Vec::new();
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("answer")],
                    PromptTurnCancellation::disabled(),
                    &mut |event| events.push(event),
                )
                .await;
            (result, events)
        });

        assert_eq!(
            next_outgoing(&mut outgoing_rx).await["method"],
            "session/prompt"
        );
        incoming_tx
            .send(text_chunk_update("sess_1", "ordinary text"))
            .unwrap();
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        incoming_tx
            .send(tool_call_progress("sess_1", "secret-id", "completed"))
            .unwrap();
        let (result, events) = prompt.await.unwrap();
        assert_denied_prompt(result);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::MessageChunk { .. }));
    }

    #[tokio::test]
    async fn deny_all_cancels_permission_after_prompt_response() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyAll);
        let prompt = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("answer")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });

        assert_eq!(
            next_outgoing(&mut outgoing_rx).await["method"],
            "session/prompt"
        );
        incoming_tx
            .send(text_chunk_update("sess_1", "apparently complete"))
            .unwrap();
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        incoming_tx
            .send(permission_request(100, "sess_1", "secret tool"))
            .unwrap();
        let denial = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(denial["id"], 100);
        assert_eq!(denial["result"]["outcome"]["outcome"], "cancelled");
        assert_denied_prompt(prompt.await.unwrap());
    }

    #[tokio::test]
    async fn restricted_trailing_transport_error_terminates_instead_of_succeeding() {
        let counters = Arc::new(ScriptedTransportCounters::default());
        let transport = ScriptedTransport::new(
            vec![
                ScriptedReceive::Message(init_response(0)),
                ScriptedReceive::Message(text_chunk_update("sess_1", "answer")),
                ScriptedReceive::Message(prompt_response(1, "end_turn")),
                ScriptedReceive::Error("stream failed"),
            ],
            counters.clone(),
        );
        let (mut client, _) = scripted_client(transport).await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        let error = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("work")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("tool"));
        assert!(client.tool_policy_violated);
        assert!(!client.is_connected());
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn restricted_cancelled_turn_denies_trailing_request() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        let _prompt = next_outgoing(&mut outgoing).await;
        incoming.send(prompt_response(1, "cancelled")).unwrap();
        incoming
            .send(permission_request(107, "sess_1", "late escalation"))
            .unwrap();
        let denied = next_outgoing(&mut outgoing).await;
        assert_eq!(denied["id"], 107);
        assert_eq!(denied["result"]["outcome"]["outcome"], "cancelled");
        assert!(matches!(
            task.await.unwrap().unwrap(),
            StopReason::Cancelled
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn denied_requests_do_not_defer_watchdog_cancellation() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        let _prompt = next_outgoing(&mut outgoing).await;
        for id in 110..115 {
            tokio::time::advance(Duration::from_millis(190)).await;
            incoming
                .send(permission_request(id, "sess_1", "escalation"))
                .unwrap();
            assert_eq!(next_outgoing(&mut outgoing).await["id"], id);
        }
        tokio::time::advance(Duration::from_millis(50)).await;
        assert_eq!(
            next_outgoing(&mut outgoing).await["method"],
            "session/cancel"
        );
        incoming.send(prompt_response(1, "cancelled")).unwrap();
        assert_eq!(
            next_outgoing(&mut outgoing).await["method"],
            "session/prompt"
        );
        incoming
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming.send(prompt_response(2, "end_turn")).unwrap();
        assert!(matches!(task.await.unwrap().unwrap(), StopReason::EndTurn));
    }

    #[tokio::test(start_paused = true)]
    async fn external_cancellation_wins_ready_denial_flood() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::from_future(async move {
                        let _ = cancel_rx.await;
                    }),
                    &mut |_| {},
                )
                .await
        });
        let _prompt = next_outgoing(&mut outgoing).await;
        cancel_tx.send(()).unwrap();
        for id in 200..300 {
            incoming
                .send(permission_request(id, "sess_1", "escalation"))
                .unwrap();
        }
        assert_eq!(
            next_outgoing(&mut outgoing).await["method"],
            "session/cancel"
        );
        incoming.send(prompt_response(1, "cancelled")).unwrap();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            StopReason::Cancelled
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn restricted_permission_during_watchdog_grace_is_cancelled_without_poisoning() {
        let (mut client, incoming, mut outgoing) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        let task = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        assert_eq!(
            next_outgoing(&mut outgoing).await["method"],
            "session/prompt"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            next_outgoing(&mut outgoing).await["method"],
            "session/cancel"
        );
        incoming
            .send(permission_request(106, "sess_1", "grace escalation"))
            .unwrap();
        assert_eq!(
            next_outgoing(&mut outgoing).await["result"]["outcome"]["outcome"],
            "cancelled"
        );
        incoming.send(prompt_response(1, "cancelled")).unwrap();
        assert_eq!(
            next_outgoing(&mut outgoing).await["method"],
            "session/prompt"
        );
        incoming
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming.send(prompt_response(2, "end_turn")).unwrap();
        let (client, result) = task.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert!(!client.tool_policy_violated);
    }

    #[tokio::test(start_paused = true)]
    async fn deny_all_catches_permission_and_tool_during_watchdog_grace() {
        for permission in [false, true] {
            let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
            client.set_access(AgentAccess::DenyAll);
            let prompt = tokio::spawn(async move {
                let mut events = Vec::new();
                let result = client
                    .prompt(
                        "sess_1",
                        vec![PromptContent::text("answer")],
                        PromptTurnCancellation::disabled(),
                        &mut |event| events.push(event),
                    )
                    .await;
                (result, events)
            });
            assert_eq!(
                next_outgoing(&mut outgoing_rx).await["method"],
                "session/prompt"
            );
            tokio::time::advance(Duration::from_secs(1)).await;
            assert_eq!(
                next_outgoing(&mut outgoing_rx).await["method"],
                "session/cancel"
            );
            if permission {
                incoming_tx
                    .send(permission_request(100, "sess_1", "secret tool"))
                    .unwrap();
                let denial = next_outgoing(&mut outgoing_rx).await;
                assert_eq!(denial["id"], 100);
                assert_eq!(denial["result"]["outcome"]["outcome"], "cancelled");
            } else {
                incoming_tx
                    .send(tool_call_started(
                        "sess_1",
                        "call_1",
                        "secret tool",
                        "execute",
                        "pending",
                    ))
                    .unwrap();
            }
            let (result, events) = prompt.await.unwrap();
            assert_denied_prompt(result);
            assert!(events.is_empty());
            assert!(
                outgoing_rx.try_recv().is_err(),
                "no continuation after violation"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn deny_all_catches_tool_use_on_automatic_continuation() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        client.set_access(AgentAccess::DenyAll);
        let prompt = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("answer")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        assert_eq!(
            next_outgoing(&mut outgoing_rx).await["method"],
            "session/prompt"
        );
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(15)).await;
        let continuation = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(continuation["method"], "session/prompt");
        assert_eq!(continuation["params"]["prompt"][0]["text"], CONTINUE_PROMPT);
        incoming_tx
            .send(tool_call_progress("sess_1", "call_1", "completed"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();
        assert_denied_prompt(prompt.await.unwrap());
    }

    #[tokio::test]
    async fn cancelled_prompt_sends_session_cancel_notification_and_reuses_session() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: None,
        };
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        let initialize = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(initialize["method"], "initialize");

        let (cancel_tx, cancel_rx) = oneshot::channel();
        let active_prompt = tokio::spawn(async move {
            let stop_reason = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("initial")],
                    PromptTurnCancellation::from_future(async move {
                        let _ = cancel_rx.await;
                    }),
                    &mut |_| {},
                )
                .await;
            (client, stop_reason)
        });

        let initial_prompt = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(initial_prompt["method"], "session/prompt");
        assert_eq!(initial_prompt["params"]["sessionId"], "sess_1");
        cancel_tx.send(()).unwrap();

        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        assert_eq!(cancel["params"]["sessionId"], "sess_1");
        assert!(cancel.get("id").is_none());

        incoming_tx
            .send(permission_request(100, "sess_1", "write_file"))
            .unwrap();
        let permission = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(permission["id"], 100);
        assert_eq!(permission["result"]["outcome"]["outcome"], "cancelled");
        assert!(permission["result"]["outcome"].get("optionId").is_none());

        incoming_tx.send(prompt_response(1, "cancelled")).unwrap();
        let (mut client, cancelled) = active_prompt.await.unwrap();
        assert!(matches!(cancelled.unwrap(), StopReason::Cancelled));

        incoming_tx
            .send(text_chunk_update("sess_1", "replacement response"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();
        let replacement_stop = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("replacement")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap();
        assert!(matches!(replacement_stop, StopReason::EndTurn));
        let replacement = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(replacement["method"], "session/prompt");
        assert_eq!(replacement["params"]["sessionId"], "sess_1");

        let mut outgoing = vec![initialize, initial_prompt, cancel, permission, replacement];
        while let Ok(message) = outgoing_rx.try_recv() {
            outgoing.push(serde_json::from_str(&message).unwrap());
        }
        assert_eq!(
            outgoing
                .iter()
                .filter(|message| message["method"] == "session/cancel")
                .count(),
            1
        );
        assert!(
            outgoing
                .iter()
                .all(|message| message["method"] != "session/close")
        );
        let prompts = outgoing
            .iter()
            .filter(|message| message["method"] == "session/prompt")
            .collect::<Vec<_>>();
        assert_eq!(prompts.len(), 2, "cancelled turn must not send Continue");
        assert_eq!(prompts[1]["params"]["prompt"][0]["text"], "replacement");
    }

    #[tokio::test]
    async fn sustained_updates_emit_bounded_cancel_and_cancel_later_permissions() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        let counters = Arc::new(ControlledTransportCounters::default());
        incoming_tx.send(init_response(0)).unwrap();
        let transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: Some(counters.clone()),
        };
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        let initialize = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(initialize["method"], "initialize");
        counters.received.store(0, Ordering::SeqCst);

        for index in 0..8 {
            incoming_tx
                .send(session_update(
                    "sess_1",
                    serde_json::json!({
                        "sessionUpdate": "agent_thought_chunk",
                        "content": {"text": format!("update-{index}")}
                    }),
                ))
                .unwrap();
        }
        let active_prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("initial")],
                    PromptTurnCancellation::from_future(async {}),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });

        let initial_prompt = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(initial_prompt["method"], "session/prompt");
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        assert_eq!(
            counters.received_at_cancel.load(Ordering::SeqCst),
            2,
            "cancellation should be sent after at most one deferred update"
        );

        incoming_tx
            .send(permission_request(100, "sess_1", "write_file"))
            .unwrap();
        incoming_tx.send(prompt_response(1, "cancelled")).unwrap();
        let permission = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(permission["id"], 100);
        assert_eq!(permission["result"]["outcome"]["outcome"], "cancelled");
        assert!(permission["result"]["outcome"].get("optionId").is_none());

        let (_client, result) = active_prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::Cancelled));
        let outgoing = [initialize, initial_prompt, cancel, permission];
        assert_eq!(
            outgoing
                .iter()
                .filter(|message| message["method"] == "session/cancel")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn completed_prompt_wins_ready_cancellation_and_allows_serial_replacement() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: None,
        };
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        let initialize = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(initialize["method"], "initialize");

        incoming_tx
            .send(session_update(
                "sess_1",
                serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tool-1",
                    "title": "Completed action",
                    "kind": "other",
                    "status": "completed"
                }),
            ))
            .unwrap();
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        incoming_tx
            .send(rpc_response(999, serde_json::json!({})))
            .unwrap();
        let active_prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("initial")],
                    PromptTurnCancellation::from_future(async {}),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let first_prompt = next_outgoing(&mut outgoing_rx).await;
        let (mut client, initial) = tokio::time::timeout(Duration::from_secs(1), active_prompt)
            .await
            .expect("completed prompt should return before automatic continuation")
            .unwrap();
        assert!(matches!(initial.unwrap(), StopReason::EndTurn));

        incoming_tx
            .send(text_chunk_update("sess_1", "replacement response"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();
        let replacement = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("accepted follow-up")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap();
        assert!(matches!(replacement, StopReason::EndTurn));
        let second_prompt = next_outgoing(&mut outgoing_rx).await;

        let mut outgoing = vec![initialize, first_prompt, second_prompt];
        while let Ok(message) = outgoing_rx.try_recv() {
            outgoing.push(serde_json::from_str(&message).unwrap());
        }
        assert!(
            outgoing
                .iter()
                .all(|message| message["method"] != "session/cancel")
        );
        let prompts = outgoing
            .iter()
            .filter(|message| message["method"] == "session/prompt")
            .collect::<Vec<_>>();
        assert_eq!(
            prompts.len(),
            2,
            "accepted follow-up must be the next prompt"
        );
        assert_eq!(prompts[0]["params"]["prompt"][0]["text"], "initial");
        assert_eq!(
            prompts[1]["params"]["prompt"][0]["text"],
            "accepted follow-up"
        );
        assert!(
            prompts
                .iter()
                .all(|prompt| prompt["params"]["prompt"][0]["text"] != "Continue")
        );
    }

    #[tokio::test]
    async fn cancellation_wins_automatic_continuation_dispatch_boundary() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: None,
        };
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        let initialize = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(initialize["method"], "initialize");

        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        incoming_tx
            .send(rpc_response(999, serde_json::json!({})))
            .unwrap();
        let (boundary_tx, boundary_rx) = oneshot::channel();
        let active_prompt = tokio::spawn(async move {
            let mut polls = 0;
            let mut boundary_tx = Some(boundary_tx);
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("initial")],
                    PromptTurnCancellation::from_future(poll_fn(move |_context| {
                        polls += 1;
                        if polls == 1 {
                            return Poll::Pending;
                        }
                        if let Some(boundary_tx) = boundary_tx.take() {
                            boundary_tx
                                .send(())
                                .expect("boundary receiver should remain open");
                        }
                        Poll::Ready(())
                    })),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let first_prompt = next_outgoing(&mut outgoing_rx).await;
        tokio::time::timeout(Duration::from_secs(1), boundary_rx)
            .await
            .expect("continuation dispatch boundary should be reached")
            .expect("boundary sender should remain open");
        let (mut client, initial) = tokio::time::timeout(Duration::from_secs(1), active_prompt)
            .await
            .expect("cancellation should win before Continue is committed")
            .unwrap();
        assert!(matches!(initial.unwrap(), StopReason::EndTurn));

        incoming_tx
            .send(text_chunk_update("sess_1", "replacement response"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();
        let replacement = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("accepted follow-up")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap();
        assert!(matches!(replacement, StopReason::EndTurn));
        let second_prompt = next_outgoing(&mut outgoing_rx).await;

        let mut outgoing = vec![initialize, first_prompt, second_prompt];
        while let Ok(message) = outgoing_rx.try_recv() {
            outgoing.push(serde_json::from_str(&message).unwrap());
        }
        assert!(
            outgoing
                .iter()
                .all(|message| message["method"] != "session/cancel")
        );
        let prompts = outgoing
            .iter()
            .filter(|message| message["method"] == "session/prompt")
            .collect::<Vec<_>>();
        assert_eq!(
            prompts.len(),
            2,
            "accepted follow-up must be the next prompt"
        );
        assert_eq!(prompts[0]["params"]["prompt"][0]["text"], "initial");
        assert_eq!(
            prompts[1]["params"]["prompt"][0]["text"],
            "accepted follow-up"
        );
        assert!(
            prompts
                .iter()
                .all(|prompt| prompt["params"]["prompt"][0]["text"] != "Continue")
        );
    }

    #[tokio::test]
    async fn test_prompt_agent_error() {
        let init_resp = init_response(0);
        let error_resp = rpc_error(1, -32000, "context window exceeded");
        let transport = MockTransport::new(vec![&init_resp, &error_resp]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let result = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("test")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Agent error"));
    }

    #[tokio::test]
    async fn test_prompt_max_tokens_stop() {
        let init_resp = init_response(0);
        let pr = prompt_response(1, "max_tokens");
        let transport = MockTransport::new(vec![&init_resp, &pr]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let stop = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("test")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap();

        assert!(matches!(stop, StopReason::MaxTokens));
    }

    #[tokio::test]
    async fn test_prompt_connection_closed() {
        let init_resp = init_response(0);
        // No prompt response — transport returns None (EOF)
        let transport = MockTransport::new(vec![&init_resp]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let result = client
            .prompt(
                "sess_1",
                vec![PromptContent::text("test")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await;

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("closed unexpectedly")
        );
    }

    #[tokio::test]
    async fn test_load_session() {
        let init_resp = init_response(0);
        let history1 = session_update(
            "sess_old",
            serde_json::json!({
                "sessionUpdate": "user_message_chunk",
                "content": {"text": "original prompt"}
            }),
        );
        let history2 = session_update(
            "sess_old",
            serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"text": "original response"}
            }),
        );
        let load_resp = rpc_response(1, Value::Null);

        let transport = MockTransport::new(vec![&init_resp, &history1, &history2, &load_resp]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let history = client
            .load_session("sess_old", "/project", &[])
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert!(matches!(&history[0], Event::UserMessageChunk { .. }));
        assert!(matches!(&history[1], Event::MessageChunk { .. }));
    }

    #[tokio::test]
    async fn restricted_permission_during_session_replay_preserves_history() {
        let init = init_response(0);
        let replay = text_chunk_update("sess_old", "history");
        let escalation = permission_request(105, "sess_old", "replay escalation");
        let loaded = rpc_response(
            1,
            serde_json::json!({"configOptions":[{"id":"mode","currentValue":"read-only"}]}),
        );
        let transport = MockTransport::new(vec![&init, &replay, &escalation, &loaded]);
        let outgoing = transport.outgoing();
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        let history = client
            .load_session("sess_old", "/project", &[])
            .await
            .unwrap();
        assert!(matches!(history.as_slice(), [Event::MessageChunk { .. }]));
        let sent = outgoing.lock();
        let denied: Value = serde_json::from_str(&sent[2]).unwrap();
        assert_eq!(denied["id"], 105);
        assert_eq!(denied["result"]["outcome"]["outcome"], "cancelled");
    }

    #[tokio::test]
    async fn required_mode_rejects_drift_during_session_replay() {
        let init = init_response(0);
        let drift = session_update(
            "sess_B",
            serde_json::json!({"sessionUpdate":"current_mode_update","currentModeId":"agent"}),
        );
        let loaded = rpc_response(
            1,
            serde_json::json!({"configOptions":[{"id":"mode","currentValue":"read-only"}]}),
        );
        let transport = MockTransport::new(vec![&init, &drift, &loaded]);
        let outgoing = transport.outgoing();
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        assert!(client.load_session("sess_B", "/tmp", &[]).await.is_err());
        assert!(client.tool_policy_violated);
        assert!(!client.is_connected());
        assert_eq!(
            outgoing.lock().len(),
            2,
            "no prompt after replay mode drift"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn failed_load_revokes_previously_verified_session() {
        for timeout in [false, true] {
            let counters = Arc::new(ScriptedTransportCounters::default());
            let mut incoming = vec![
                ScriptedReceive::Message(init_response(0)),
                ScriptedReceive::Message(rpc_response(
                    1,
                    serde_json::json!({
                        "sessionId": "sess_A", "configOptions": [{"id": "mode", "currentValue": "read-only"}]
                    }),
                )),
            ];
            if !timeout {
                incoming.push(ScriptedReceive::Message(rpc_response(
                    2,
                    serde_json::json!("malformed"),
                )));
            }
            let (mut client, _) =
                scripted_client(ScriptedTransport::new(incoming, counters.clone())).await;
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            client.new_session("/tmp", &[], None).await.unwrap();
            assert!(client.load_session("sess_B", "/tmp", &[]).await.is_err());
            assert_ne!(client.session_id.as_deref(), Some("sess_A"));
            let sends = counters.sends.load(Ordering::SeqCst);
            assert!(
                client
                    .prompt(
                        "sess_A",
                        vec![PromptContent::text("work")],
                        PromptTurnCancellation::disabled(),
                        &mut |_| {}
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                counters.sends.load(Ordering::SeqCst),
                sends,
                "stale session cannot prompt"
            );
        }
    }

    #[tokio::test]
    async fn malformed_mode_state_terminates_new_or_loaded_session() {
        for load in [false, true] {
            let init = init_response(0);
            let mut messages = vec![init];
            if load {
                messages.push(rpc_response(1, serde_json::json!({"sessionId":"sess_A","configOptions":[{"id":"mode","currentValue":"read-only"}]})));
                messages.push(rpc_response(
                    2,
                    serde_json::json!({"modeState":{"currentModeId":7}}),
                ));
            } else {
                messages.push(rpc_response(
                    1,
                    serde_json::json!({"sessionId":"sess_A","modeState":{"currentModeId":7}}),
                ));
            }
            let refs = messages.iter().map(String::as_str).collect::<Vec<_>>();
            let transport = MockTransport::new(refs);
            let mut client =
                Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                    .await
                    .unwrap();
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            let result = if load {
                client.new_session("/tmp", &[], None).await.unwrap();
                client.load_session("sess_B", "/tmp", &[]).await.map(|_| ())
            } else {
                client.new_session("/tmp", &[], None).await.map(|_| ())
            };
            assert!(result.is_err());
            assert!(client.tool_policy_violated);
            assert!(!client.is_connected());
            assert_eq!(client.session_id(), None);
        }
    }

    #[tokio::test]
    async fn unsupported_load_revokes_prior_verified_session() {
        let init = rpc_response(
            0,
            serde_json::json!({"protocolVersion":1,"agentCapabilities":{"loadSession":false}}),
        );
        let session = rpc_response(
            1,
            serde_json::json!({"sessionId":"sess_A","modeState":{"currentModeId":"read-only"}}),
        );
        let transport = MockTransport::new(vec![&init, &session]);
        let outgoing = transport.outgoing();
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.new_session("/tmp", &[], None).await.unwrap();
        assert!(client.load_session("sess_B", "/tmp", &[]).await.is_err());
        assert_eq!(client.session_id(), None);
        assert!(
            client
                .prompt(
                    "sess_A",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {}
                )
                .await
                .is_err()
        );
        assert_eq!(outgoing.lock().len(), 2);
    }

    #[tokio::test]
    async fn required_mode_checks_loaded_session_before_use() {
        for (mode, accepted) in [
            (Some("read-only"), true),
            (Some("agent"), false),
            (None, false),
        ] {
            let init = init_response(0);
            let result = mode.map_or_else(
                || serde_json::json!({}),
                |value| serde_json::json!({"configOptions":[{"id":"mode","currentValue":value}]}),
            );
            let loaded = rpc_response(1, result);
            let transport = MockTransport::new(vec![&init, &loaded]);
            let mut client =
                Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                    .await
                    .unwrap();
            client.set_access(AgentAccess::DenyEscalation {
                acp_mode: "read-only".into(),
            });
            let result = client.load_session("sess_1", "/tmp", &[]).await;
            assert_eq!(result.is_ok(), accepted);
            if !accepted {
                assert!(!client.is_connected());
                assert!(client.tool_policy_violated);
            }
        }
    }

    #[tokio::test]
    async fn load_session_captures_descriptor_from_returned_config_options() {
        let init_resp = init_response(0);
        let load_resp = rpc_response(
            1,
            serde_json::json!({
                "configOptions": [
                    {
                        "id": "model",
                        "category": "model",
                        "currentValue": "github-copilot/gpt-5.6-sol",
                        "options": [{"value": "github-copilot/gpt-5.6-sol"}]
                    },
                    {
                        "id": "thought_level",
                        "category": "thought_level",
                        "currentValue": "high",
                        "options": [{"value": "high"}]
                    }
                ]
            }),
        );
        let transport = MockTransport::new(vec![&init_resp, &load_resp]);
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        client
            .load_session("sess_old", "/project", &[])
            .await
            .unwrap();

        let descriptor = cowboy_agent_client::Client::session_descriptor(&client)
            .expect("session/load config options should restore the model descriptor");
        assert_eq!(
            descriptor.model.as_deref(),
            Some("github-copilot/gpt-5.6-sol")
        );
        assert_eq!(descriptor.reasoning.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn load_session_clears_descriptor_for_descriptorless_results() {
        let responses = [
            serde_json::json!({"jsonrpc": "2.0", "id": 1}).to_string(),
            rpc_response(1, Value::Null),
            rpc_response(1, serde_json::json!({})),
            rpc_response(1, serde_json::json!({"configOptions": []})),
            rpc_response(
                1,
                serde_json::json!({
                    "configOptions": [{
                        "id": "theme",
                        "category": "display",
                        "currentValue": "dark",
                        "options": [{"value": "dark"}]
                    }]
                }),
            ),
        ];

        for load_resp in responses {
            let init_resp = init_response(0);
            let transport = MockTransport::new(vec![&init_resp, &load_resp]);
            let mut client =
                Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                    .await
                    .unwrap();
            client.session_descriptor = Some(AgentSessionDescriptor {
                model: Some("stale-model".into()),
                context: None,
                reasoning: None,
            });

            client
                .load_session("sess_old", "/project", &[])
                .await
                .unwrap();

            assert!(
                cowboy_agent_client::Client::session_descriptor(&client).is_none(),
                "descriptorless session/load result should clear stale descriptor: {load_resp}"
            );
            assert_eq!(client.session_id(), Some("sess_old"));
        }
    }

    #[tokio::test]
    async fn load_session_rejects_malformed_non_null_result() {
        let init_resp = init_response(0);
        let load_resp = rpc_response(1, serde_json::json!("malformed"));
        let transport = MockTransport::new(vec![&init_resp, &load_resp]);
        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let error = client
            .load_session("sess_old", "/project", &[])
            .await
            .unwrap_err();

        assert!(error.to_string().contains("invalid type"));
        assert_eq!(client.session_id(), None);
    }

    #[tokio::test]
    async fn test_load_session_not_supported() {
        let resp = rpc_response(
            0,
            serde_json::json!({
                "agentCapabilities": {},
                "agentInfo": {"name": "no-load-agent"}
            }),
        );
        let transport = MockTransport::new(vec![&resp]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let result = client.load_session("sess_old", "/project", &[]).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("does not support loadSession")
        );
    }

    #[tokio::test]
    async fn test_recv_skips_empty_lines_and_unknown_messages() {
        let init_resp = init_response(0);
        let unknown = r#"{"some":"unrelated","data":true}"#;
        let sess_resp = session_new_response(1, "sess_1");

        let transport = MockTransport::new(vec![
            &init_resp, "",      // empty line — should be skipped
            unknown, // unknown format — should be skipped
            &sess_resp,
        ]);

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let model = ModelInfo {
            id: "test".into(),

            provider: None,
        };

        // Should succeed despite garbage in the stream
        let sid = client
            .new_session("/project", &[], Some(&model))
            .await
            .unwrap();
        assert_eq!(sid, "sess_1");
    }

    #[tokio::test]
    async fn test_request_id_increments() {
        let init_resp = init_response(0);
        let resp1 = session_new_response(1, "s1");
        let resp2 = session_new_response(2, "s2");

        let transport = MockTransport::new(vec![&init_resp, &resp1, &resp2]);
        let outgoing = transport.outgoing();

        let mut client =
            Client::connect_with_transport(Box::new(transport), dummy_transport_config())
                .await
                .unwrap();

        let model = ModelInfo {
            id: "test".into(),

            provider: None,
        };

        client.new_session("/a", &[], Some(&model)).await.unwrap();
        client.new_session("/b", &[], Some(&model)).await.unwrap();

        // IDs should be 0 (init), 1, 2
        let sent = outgoing.lock();
        let req0: Value = serde_json::from_str(&sent[0]).unwrap();
        let req1: Value = serde_json::from_str(&sent[1]).unwrap();
        let req2: Value = serde_json::from_str(&sent[2]).unwrap();
        assert_eq!(req0["id"], 0);
        assert_eq!(req1["id"], 1);
        assert_eq!(req2["id"], 2);
    }

    #[test]
    fn watchdog_options_retain_defaults_and_explicit_values() {
        assert_eq!(
            AgentWatchdogOptions::default(),
            AgentWatchdogOptions {
                response_timeout_seconds: 100,
                cancel_timeout_seconds: 10,
                recovery_operation_timeout_seconds: 30,
            }
        );
        let explicit = AgentWatchdogOptions {
            response_timeout_seconds: 1,
            cancel_timeout_seconds: 2,
            recovery_operation_timeout_seconds: 3,
        };
        assert_eq!(explicit.response_timeout_seconds, 1);
        assert_eq!(explicit.cancel_timeout_seconds, 2);
        assert_eq!(explicit.recovery_operation_timeout_seconds, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn replacement_transport_is_consumed_only_after_forced_reconnect() {
        let initial_counters = Arc::new(ScriptedTransportCounters::default());
        let initial = ScriptedTransport::new(
            vec![ScriptedReceive::Message(init_response(0))],
            initial_counters,
        );
        let (mut client, _) = scripted_client(initial).await;
        assert_eq!(client.replacement_factory_calls(), 0);

        let replacement_counters = Arc::new(ScriptedTransportCounters::default());
        client.push_replacement_transport(Box::new(ScriptedTransport::new(
            vec![ScriptedReceive::Message(init_response(1))],
            replacement_counters,
        )));
        assert_eq!(client.replacement_factory_calls(), 0);

        let continuation_id = client.hard_recover_and_continue("sess_1").await.unwrap();

        assert_eq!(continuation_id, 2);
        assert_eq!(client.replacement_factory_calls(), 1);
        assert!(client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_soft_cancel_continues_same_session() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: None,
        };
        let mut client = Client::connect_with_transport_and_options(
            Box::new(transport),
            dummy_transport_config(),
            AgentWatchdogOptions {
                response_timeout_seconds: 1,
                cancel_timeout_seconds: 2,
                recovery_operation_timeout_seconds: 3,
            },
        )
        .await
        .unwrap();
        let _initialize = next_outgoing(&mut outgoing_rx).await;

        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let initial = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(initial["id"], 1);

        tokio::time::advance(Duration::from_secs(1)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(1, "cancelled")).unwrap();

        let continuation = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(continuation["method"], "session/prompt");
        assert_eq!(continuation["params"]["sessionId"], "sess_1");
        assert_eq!(continuation["params"]["prompt"][0]["text"], CONTINUE_PROMPT);
        incoming_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_cancel_acknowledged_with_end_turn_recovers_instead_of_truncating() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: None,
        };
        let mut client = Client::connect_with_transport_and_options(
            Box::new(transport),
            dummy_transport_config(),
            AgentWatchdogOptions {
                response_timeout_seconds: 1,
                cancel_timeout_seconds: 2,
                recovery_operation_timeout_seconds: 3,
            },
        )
        .await
        .unwrap();
        let _initialize = next_outgoing(&mut outgoing_rx).await;

        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let initial = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(initial["id"], 1);

        // The agent is busy waiting on long-running external work and streams
        // nothing, so the watchdog fires and cancels the turn.
        tokio::time::advance(Duration::from_secs(1)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");

        // The backend acknowledges the watchdog cancel by ending the turn with a
        // truncated notice and `end_turn` rather than reporting `cancelled`.
        incoming_tx
            .send(text_chunk_update(
                "sess_1",
                "Info: Operation cancelled by user",
            ))
            .unwrap();
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();

        // A turn that Cowboy itself cancelled must still be recovered on the
        // same session. Surfacing the truncated reply as an ordinary completed
        // turn makes it indistinguishable from a real reply, so the caller burns
        // a retry without ever giving the agent more time.
        let continuation = tokio::time::timeout(Duration::from_secs(5), outgoing_rx.recv()).await;
        let Ok(Some(continuation)) = continuation else {
            let (_client, result) = prompt.await.unwrap();
            panic!(
                "a watchdog-cancelled turn must be recovered on the same session, but the client \
sent no continuation after cancelling and returned {result:?} with the truncated reply"
            );
        };
        let continuation: Value = serde_json::from_str(&continuation).unwrap();
        assert_eq!(continuation["method"], "session/prompt");
        assert_eq!(continuation["params"]["sessionId"], "sess_1");
        assert_eq!(continuation["params"]["prompt"][0]["text"], CONTINUE_PROMPT);
        incoming_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_restart_resumes_same_session() {
        let initial_counters = Arc::new(ControlledTransportCounters::default());
        let replacement_counters = Arc::new(ControlledTransportCounters::default());
        let (initial_in_tx, initial_in_rx) = mpsc::unbounded_channel();
        let (initial_out_tx, mut initial_out_rx) = mpsc::unbounded_channel();
        initial_in_tx.send(init_response(0)).unwrap();
        let initial = ControlledTransport {
            incoming: initial_in_rx,
            outgoing: initial_out_tx,
            counters: Some(initial_counters.clone()),
        };
        let mut client = Client::connect_with_transport_and_options(
            Box::new(initial),
            dummy_transport_config(),
            AgentWatchdogOptions {
                response_timeout_seconds: 1,
                cancel_timeout_seconds: 2,
                recovery_operation_timeout_seconds: 3,
            },
        )
        .await
        .unwrap();
        client.session_id = Some("sess_1".to_string());
        client.session_load_context = Some(SessionLoadContext {
            cwd: "/project".to_string(),
            mcp_servers: Vec::new(),
        });
        let _initialize = next_outgoing(&mut initial_out_rx).await;

        let (replacement_in_tx, replacement_in_rx) = mpsc::unbounded_channel();
        let (replacement_out_tx, mut replacement_out_rx) = mpsc::unbounded_channel();
        replacement_in_tx.send(init_response(2)).unwrap();
        client.push_replacement_transport(Box::new(ControlledTransport {
            incoming: replacement_in_rx,
            outgoing: replacement_out_tx,
            counters: Some(replacement_counters),
        }));

        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _initial_prompt = next_outgoing(&mut initial_out_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let cancel = next_outgoing(&mut initial_out_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        tokio::time::advance(Duration::from_secs(1)).await;
        initial_in_tx
            .send(text_chunk_update("sess_1", "late activity"))
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;

        let replacement_initialize = next_outgoing(&mut replacement_out_rx).await;
        assert_eq!(replacement_initialize["method"], "initialize");
        let load = next_outgoing(&mut replacement_out_rx).await;
        assert_eq!(load["method"], "session/load");
        assert_eq!(load["params"]["sessionId"], "sess_1");
        assert_eq!(load["params"]["cwd"], "/project");
        replacement_in_tx
            .send(rpc_response(
                load["id"].as_u64().unwrap(),
                serde_json::json!({}),
            ))
            .unwrap();
        let continuation = next_outgoing(&mut replacement_out_rx).await;
        assert_eq!(continuation["params"]["sessionId"], "sess_1");
        assert_eq!(continuation["params"]["prompt"][0]["text"], CONTINUE_PROMPT);
        replacement_in_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        replacement_in_tx
            .send(prompt_response(
                continuation["id"].as_u64().unwrap(),
                "end_turn",
            ))
            .unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(initial_counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(client.replacement_factory_calls(), 1);
        assert_eq!(client.session_id(), Some("sess_1"));
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_replacement_load_missing_session_invalidates_client_session() {
        let replacement_counters = Arc::new(ControlledTransportCounters::default());
        let (initial_in_tx, initial_in_rx) = mpsc::unbounded_channel();
        let (initial_out_tx, mut initial_out_rx) = mpsc::unbounded_channel();
        initial_in_tx.send(init_response(0)).unwrap();
        let initial = ControlledTransport {
            incoming: initial_in_rx,
            outgoing: initial_out_tx,
            counters: None,
        };
        let mut client = Client::connect_with_transport_and_options(
            Box::new(initial),
            dummy_transport_config(),
            AgentWatchdogOptions {
                response_timeout_seconds: 1,
                cancel_timeout_seconds: 2,
                recovery_operation_timeout_seconds: 3,
            },
        )
        .await
        .unwrap();
        client.session_id = Some("sess_1".to_string());
        client.session_load_context = Some(SessionLoadContext {
            cwd: "/project".to_string(),
            mcp_servers: Vec::new(),
        });
        let _initialize = next_outgoing(&mut initial_out_rx).await;
        let (replacement_in_tx, replacement_in_rx) = mpsc::unbounded_channel();
        let (replacement_out_tx, mut replacement_out_rx) = mpsc::unbounded_channel();
        replacement_in_tx.send(init_response(2)).unwrap();
        client.push_replacement_transport(Box::new(ControlledTransport {
            incoming: replacement_in_rx,
            outgoing: replacement_out_tx,
            counters: Some(replacement_counters.clone()),
        }));

        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _initial_prompt = next_outgoing(&mut initial_out_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let _cancel = next_outgoing(&mut initial_out_rx).await;
        tokio::time::advance(Duration::from_secs(2)).await;
        let _replacement_initialize = next_outgoing(&mut replacement_out_rx).await;
        let load = next_outgoing(&mut replacement_out_rx).await;
        assert_eq!(load["method"], "session/load");
        replacement_in_tx
            .send(rpc_error(
                load["id"].as_u64().unwrap(),
                -32602,
                "Session sess_1 not found",
            ))
            .unwrap();

        let (client, result) = prompt.await.unwrap();
        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("replacement session/load failed")
        );
        assert_eq!(
            replacement_counters.force_terminated.load(Ordering::SeqCst),
            1
        );
        assert_eq!(client.replacement_factory_calls(), 1);
        assert!(!client.is_connected());
        assert_eq!(client.session_id(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn external_cancellation_wins_simultaneous_watchdog_deadline() {
        let counters = Arc::new(ControlledTransportCounters::default());
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: Some(counters.clone()),
        };
        let mut client = Client::connect_with_transport_and_options(
            Box::new(transport),
            dummy_transport_config(),
            AgentWatchdogOptions {
                response_timeout_seconds: 1,
                cancel_timeout_seconds: 2,
                recovery_operation_timeout_seconds: 3,
            },
        )
        .await
        .unwrap();
        let _initialize = next_outgoing(&mut outgoing_rx).await;

        let prompt = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::from_future(tokio::time::sleep(Duration::from_secs(1))),
                    &mut |_| {},
                )
                .await
        });
        let _initial_prompt = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(1, "cancelled")).unwrap();

        assert!(matches!(
            prompt.await.unwrap().unwrap(),
            StopReason::Cancelled
        ));
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 0);
        assert!(outgoing_rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn external_cancellation_during_watchdog_grace_suppresses_continuation() {
        let counters = Arc::new(ControlledTransportCounters::default());
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: Some(counters.clone()),
        };
        let mut client = Client::connect_with_transport_and_options(
            Box::new(transport),
            dummy_transport_config(),
            AgentWatchdogOptions {
                response_timeout_seconds: 1,
                cancel_timeout_seconds: 2,
                recovery_operation_timeout_seconds: 3,
            },
        )
        .await
        .unwrap();
        let _initialize = next_outgoing(&mut outgoing_rx).await;
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let prompt = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::from_future(async move {
                        let _ = cancel_rx.await;
                    }),
                    &mut |_| {},
                )
                .await
        });
        let _initial_prompt = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");

        cancel_tx.send(()).unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 0);
        assert!(outgoing_rx.try_recv().is_err());
        incoming_tx.send(prompt_response(1, "cancelled")).unwrap();

        assert!(matches!(
            prompt.await.unwrap().unwrap(),
            StopReason::Cancelled
        ));
        assert!(outgoing_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn force_terminate_disposes_controlled_transport() {
        let counters = Arc::new(ControlledTransportCounters::default());
        let (_incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, _outgoing_rx) = mpsc::unbounded_channel();
        let mut transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: Some(counters.clone()),
        };

        transport.force_terminate().await.unwrap();

        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_soft_parsed_activity_resets_inactivity_deadline() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let transport = ControlledTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: None,
        };
        let mut client = Client::connect_with_transport_and_options(
            Box::new(transport),
            dummy_transport_config(),
            AgentWatchdogOptions {
                response_timeout_seconds: 1,
                cancel_timeout_seconds: 2,
                recovery_operation_timeout_seconds: 3,
            },
        )
        .await
        .unwrap();
        let _initialize = next_outgoing(&mut outgoing_rx).await;
        let prompt = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        let _initial = next_outgoing(&mut outgoing_rx).await;

        tokio::time::advance(Duration::from_millis(900)).await;
        incoming_tx
            .send(text_chunk_update("sess_1", "activity"))
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(900)).await;
        assert!(outgoing_rx.try_recv().is_err());
        tokio::time::advance(Duration::from_millis(100)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        // Acknowledging the watchdog cancel — with any stop reason — recovers
        // the turn on the same session, so the continuation must be completed
        // before the turn reports its own stop reason.
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        let continuation = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(continuation["method"], "session/prompt");
        incoming_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();

        assert!(matches!(
            prompt.await.unwrap().unwrap(),
            StopReason::EndTurn
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_soft_normal_completion_wins_ready_timeout_without_cancel() {
        let counters = Arc::new(ControlledTransportCounters::default());
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let mut client = Client::connect_with_transport_and_options(
            Box::new(ControlledTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
                counters: Some(counters.clone()),
            }),
            dummy_transport_config(),
            test_watchdog(),
        )
        .await
        .unwrap();
        let _ = next_outgoing(&mut outgoing_rx).await;
        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _ = next_outgoing(&mut outgoing_rx).await;
        incoming_tx
            .send(text_chunk_update("sess_1", "completed"))
            .unwrap();
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 0);
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 0);
        assert!(outgoing_rx.try_recv().is_err());
    }

    async fn run_cancel_grace_escalation(message: String) -> (Client, anyhow::Result<StopReason>) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let mut client = Client::connect_with_transport_and_options(
            Box::new(ControlledTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
                counters: None,
            }),
            dummy_transport_config(),
            test_watchdog(),
        )
        .await
        .unwrap();
        let _ = next_outgoing(&mut outgoing_rx).await;
        client.push_replacement_transport(Box::new(ScriptedTransport::new(
            vec![
                ScriptedReceive::Message(init_response(2)),
                ScriptedReceive::Message(text_chunk_update("sess_1", "recovered")),
                ScriptedReceive::Message(prompt_response(3, "end_turn")),
            ],
            Arc::new(ScriptedTransportCounters::default()),
        )));
        let task = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _ = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let _ = next_outgoing(&mut outgoing_rx).await;
        incoming_tx.send(message).unwrap();
        task.await.unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_soft_prompt_rpc_error_during_cancel_grace_escalates() {
        let (client, result) =
            run_cancel_grace_escalation(rpc_error(1, -32000, "prompt failed")).await;
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_soft_malformed_json_during_cancel_grace_escalates() {
        let (client, result) = run_cancel_grace_escalation("{malformed".to_string()).await;
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_soft_external_cancellation_before_timeout_sends_no_continuation() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let mut client = Client::connect_with_transport_and_options(
            Box::new(ControlledTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
                counters: None,
            }),
            dummy_transport_config(),
            test_watchdog(),
        )
        .await
        .unwrap();
        let _ = next_outgoing(&mut outgoing_rx).await;
        let task = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::from_future(tokio::time::sleep(Duration::from_millis(
                        500,
                    ))),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _ = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_millis(500)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(1, "cancelled")).unwrap();

        let (client, result) = task.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::Cancelled));
        assert_eq!(client.replacement_factory_calls(), 0);
        assert!(outgoing_rx.try_recv().is_err());
    }

    async fn direct_hard_client(
        force: ScriptedOperation,
    ) -> (Client, Arc<ScriptedTransportCounters>) {
        let counters = Arc::new(ScriptedTransportCounters::default());
        let transport = ScriptedTransport::new(
            vec![ScriptedReceive::Message(init_response(0))],
            counters.clone(),
        )
        .force_action(force);
        scripted_client(transport).await
    }

    fn replacement_with(
        incoming: Vec<ScriptedReceive>,
        counters: Arc<ScriptedTransportCounters>,
    ) -> ScriptedTransport {
        ScriptedTransport::new(incoming, counters)
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_replacement_revalidates_required_mode_before_continuation() {
        let (mut client, _) = direct_hard_client(ScriptedOperation::Ready).await;
        client.set_access(AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        });
        client.session_id = Some("sess_1".into());
        client.session_load_context = Some(SessionLoadContext {
            cwd: "/tmp".into(),
            mcp_servers: Vec::new(),
        });
        let counters = Arc::new(ScriptedTransportCounters::default());
        let replacement = replacement_with(
            vec![
                ScriptedReceive::Message(init_response(1)),
                ScriptedReceive::Message(rpc_response(
                    2,
                    serde_json::json!({"configOptions":[{"id":"mode","currentValue":"agent"}]}),
                )),
            ],
            counters.clone(),
        );
        client.push_replacement_transport(Box::new(replacement));
        let error = client
            .hard_recover_and_continue("sess_1")
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("replacement session/load failed"),
            "{error}"
        );
        assert_eq!(
            counters.sends.load(Ordering::SeqCst),
            2,
            "no continuation may be sent"
        );
        assert!(!client.is_connected());
        assert!(client.tool_policy_violated);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_force_termination_error_prevents_replacement() {
        let (mut client, counters) =
            direct_hard_client(ScriptedOperation::Error("terminate failed")).await;
        client.push_replacement_creation_error("must not be consumed");

        let error = client
            .hard_recover_and_continue("sess_1")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("force termination failed"));
        assert_eq!(client.replacement_factory_calls(), 0);
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_force_termination_timeout_prevents_replacement() {
        let (mut client, counters) = direct_hard_client(ScriptedOperation::Pending).await;
        client.push_replacement_creation_error("must not be consumed");
        let error = {
            let future = client.hard_recover_and_continue("sess_1");
            tokio::pin!(future);
            tokio::time::advance(Duration::from_secs(3)).await;
            future.await.unwrap_err()
        };

        assert!(error.to_string().contains("force termination timed out"));
        assert_eq!(client.replacement_factory_calls(), 0);
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_replacement_creation_error_leaves_no_transport() {
        let (mut client, _) = direct_hard_client(ScriptedOperation::Ready).await;
        client.push_replacement_creation_error("creation failed");

        let error = client
            .hard_recover_and_continue("sess_1")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("transport creation failed"));
        assert_eq!(client.replacement_factory_calls(), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_replacement_creation_timeout_leaves_no_transport() {
        let (mut client, _) = direct_hard_client(ScriptedOperation::Ready).await;
        client.push_replacement_creation_pending();
        let error = {
            let future = client.hard_recover_and_continue("sess_1");
            tokio::pin!(future);
            tokio::time::advance(Duration::from_secs(3)).await;
            future.await.unwrap_err()
        };

        assert!(error.to_string().contains("transport creation timed out"));
        assert_eq!(client.replacement_factory_calls(), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_initialization_rpc_error_disposes_replacement() {
        let (mut client, _) = direct_hard_client(ScriptedOperation::Ready).await;
        let counters = Arc::new(ScriptedTransportCounters::default());
        client.push_replacement_transport(Box::new(replacement_with(
            vec![ScriptedReceive::Message(rpc_error(
                1,
                -32000,
                "initialize failed",
            ))],
            counters.clone(),
        )));

        let error = client
            .hard_recover_and_continue("sess_1")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("initialization failed"));
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(counters.dropped.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_initialization_timeout_disposes_replacement() {
        let (mut client, _) = direct_hard_client(ScriptedOperation::Ready).await;
        let counters = Arc::new(ScriptedTransportCounters::default());
        client.push_replacement_transport(Box::new(replacement_with(
            vec![ScriptedReceive::Pending],
            counters.clone(),
        )));
        let error = {
            let future = client.hard_recover_and_continue("sess_1");
            tokio::pin!(future);
            tokio::time::advance(Duration::from_secs(3)).await;
            future.await.unwrap_err()
        };

        assert!(error.to_string().contains("initialization timed out"));
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(counters.dropped.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_continuation_send_error_disposes_replacement() {
        let (mut client, _) = direct_hard_client(ScriptedOperation::Ready).await;
        let counters = Arc::new(ScriptedTransportCounters::default());
        let replacement = replacement_with(
            vec![ScriptedReceive::Message(init_response(1))],
            counters.clone(),
        )
        .send_action(1, ScriptedOperation::Error("continuation send failed"));
        client.push_replacement_transport(Box::new(replacement));

        let error = client
            .hard_recover_and_continue("sess_1")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("continuation dispatch failed"));
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(counters.dropped.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_continuation_dispatch_timeout_disposes_replacement() {
        let (mut client, _) = direct_hard_client(ScriptedOperation::Ready).await;
        let counters = Arc::new(ScriptedTransportCounters::default());
        let replacement = replacement_with(
            vec![ScriptedReceive::Message(init_response(1))],
            counters.clone(),
        )
        .send_action(1, ScriptedOperation::Pending);
        client.push_replacement_transport(Box::new(replacement));
        let error = {
            let future = client.hard_recover_and_continue("sess_1");
            tokio::pin!(future);
            tokio::time::advance(Duration::from_secs(3)).await;
            future.await.unwrap_err()
        };

        assert!(
            error
                .to_string()
                .contains("continuation dispatch timed out")
        );
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(counters.dropped.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    async fn run_replacement_stream_failure(
        failure: ScriptedReceive,
    ) -> (Client, anyhow::Error, Arc<ScriptedTransportCounters>) {
        let initial_counters = Arc::new(ScriptedTransportCounters::default());
        let initial = ScriptedTransport::new(
            vec![
                ScriptedReceive::Message(init_response(0)),
                ScriptedReceive::Pending,
            ],
            initial_counters,
        );
        let (mut client, _) = scripted_client(initial).await;
        let replacement_counters = Arc::new(ScriptedTransportCounters::default());
        client.push_replacement_transport(Box::new(replacement_with(
            vec![ScriptedReceive::Message(init_response(2)), failure],
            replacement_counters.clone(),
        )));
        let task = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        tokio::time::advance(Duration::from_secs(3)).await;
        let (client, result) = task.await.unwrap();
        (client, result.unwrap_err(), replacement_counters)
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_replacement_malformed_json_disposes_transport() {
        let (client, error, counters) =
            run_replacement_stream_failure(ScriptedReceive::Message("{malformed".to_string()))
                .await;
        assert!(
            error
                .to_string()
                .contains("replacement continuation failed")
        );
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_replacement_eof_disposes_transport() {
        let (client, error, counters) = run_replacement_stream_failure(ScriptedReceive::Eof).await;
        assert!(
            error
                .to_string()
                .contains("replacement continuation failed")
        );
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_cleanup_timeout_drops_replacement_transport() {
        let (mut client, _) = direct_hard_client(ScriptedOperation::Ready).await;
        let counters = Arc::new(ScriptedTransportCounters::default());
        let replacement = replacement_with(
            vec![ScriptedReceive::Message(rpc_error(
                1,
                -32000,
                "initialize failed",
            ))],
            counters.clone(),
        )
        .force_action(ScriptedOperation::Pending);
        client.push_replacement_transport(Box::new(replacement));
        let error = {
            let future = client.hard_recover_and_continue("sess_1");
            tokio::pin!(future);
            tokio::time::advance(Duration::from_secs(3)).await;
            future.await.unwrap_err()
        };

        assert!(error.to_string().contains("initialization failed"));
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(counters.dropped.load(Ordering::SeqCst), 1);
        assert!(!client.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_fixed_cancel_grace_ignores_activity() {
        let initial_counters = Arc::new(ControlledTransportCounters::default());
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let mut client = Client::connect_with_transport_and_options(
            Box::new(ControlledTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
                counters: Some(initial_counters.clone()),
            }),
            dummy_transport_config(),
            test_watchdog(),
        )
        .await
        .unwrap();
        let _ = next_outgoing(&mut outgoing_rx).await;
        client.push_replacement_transport(Box::new(ScriptedTransport::new(
            vec![
                ScriptedReceive::Message(init_response(2)),
                ScriptedReceive::Message(text_chunk_update("sess_1", "recovered")),
                ScriptedReceive::Message(prompt_response(3, "end_turn")),
            ],
            Arc::new(ScriptedTransportCounters::default()),
        )));
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        let _ = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let _ = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        incoming_tx
            .send(text_chunk_update("sess_1", "late activity"))
            .unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;

        assert!(matches!(task.await.unwrap().unwrap(), StopReason::EndTurn));
        assert_eq!(initial_counters.force_terminated.load(Ordering::SeqCst), 1);
    }

    async fn run_original_stream_failure(failure: ScriptedReceive) -> StopReason {
        let counters = Arc::new(ScriptedTransportCounters::default());
        let initial = ScriptedTransport::new(
            vec![ScriptedReceive::Message(init_response(0)), failure],
            counters,
        );
        let (mut client, _) = scripted_client(initial).await;
        client.push_replacement_transport(Box::new(ScriptedTransport::new(
            vec![
                ScriptedReceive::Message(init_response(2)),
                ScriptedReceive::Message(text_chunk_update("sess_1", "recovered")),
                ScriptedReceive::Message(prompt_response(3, "end_turn")),
            ],
            Arc::new(ScriptedTransportCounters::default()),
        )));
        client
            .prompt(
                "sess_1",
                vec![PromptContent::text("work")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_notification_write_failure_uses_hard_recovery() {
        let counters = Arc::new(ScriptedTransportCounters::default());
        let initial = ScriptedTransport::new(
            vec![
                ScriptedReceive::Message(init_response(0)),
                ScriptedReceive::Pending,
            ],
            counters.clone(),
        )
        .send_action(2, ScriptedOperation::Error("cancel write failed"));
        let (mut client, _) = scripted_client(initial).await;
        client.push_replacement_transport(Box::new(ScriptedTransport::new(
            vec![
                ScriptedReceive::Message(init_response(2)),
                ScriptedReceive::Message(text_chunk_update("sess_1", "recovered")),
                ScriptedReceive::Message(prompt_response(3, "end_turn")),
            ],
            Arc::new(ScriptedTransportCounters::default()),
        )));
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;

        assert!(matches!(task.await.unwrap().unwrap(), StopReason::EndTurn));
        assert_eq!(counters.force_terminated.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_eof_during_prompt_uses_hard_recovery() {
        assert!(matches!(
            run_original_stream_failure(ScriptedReceive::Eof).await,
            StopReason::EndTurn
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_eof_during_cancel_grace_uses_hard_recovery() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let mut client = Client::connect_with_transport_and_options(
            Box::new(ControlledTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
                counters: None,
            }),
            dummy_transport_config(),
            test_watchdog(),
        )
        .await
        .unwrap();
        let _ = next_outgoing(&mut outgoing_rx).await;
        client.push_replacement_transport(Box::new(ScriptedTransport::new(
            vec![
                ScriptedReceive::Message(init_response(2)),
                ScriptedReceive::Message(text_chunk_update("sess_1", "recovered")),
                ScriptedReceive::Message(prompt_response(3, "end_turn")),
            ],
            Arc::new(ScriptedTransportCounters::default()),
        )));
        let task = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _ = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let _ = next_outgoing(&mut outgoing_rx).await;
        drop(incoming_tx);
        let (client, result) = task.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_valid_unrecognized_json_does_not_reset_deadline() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let mut client = Client::connect_with_transport_and_options(
            Box::new(ControlledTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
                counters: None,
            }),
            dummy_transport_config(),
            test_watchdog(),
        )
        .await
        .unwrap();
        let _ = next_outgoing(&mut outgoing_rx).await;
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        let _ = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_millis(900)).await;
        incoming_tx
            .send(r#"{"jsonrpc":"2.0","method":"unknown"}"#.to_string())
            .unwrap();
        tokio::time::advance(Duration::from_millis(100)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        incoming_tx
            .send(text_chunk_update("sess_1", "completed"))
            .unwrap();
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        let continuation = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(continuation["method"], "session/prompt");
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();
        assert!(matches!(task.await.unwrap().unwrap(), StopReason::EndTurn));
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_malformed_json_during_prompt_uses_hard_recovery() {
        assert!(matches!(
            run_original_stream_failure(ScriptedReceive::Message("{malformed".to_string())).await,
            StopReason::EndTurn
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_second_stall_after_soft_recovery_is_monitored() {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
        incoming_tx.send(init_response(0)).unwrap();
        let mut client = Client::connect_with_transport_and_options(
            Box::new(ControlledTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
                counters: None,
            }),
            dummy_transport_config(),
            test_watchdog(),
        )
        .await
        .unwrap();
        let _ = next_outgoing(&mut outgoing_rx).await;
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        let _ = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let first_cancel = next_outgoing(&mut outgoing_rx).await;
        incoming_tx.send(prompt_response(1, "cancelled")).unwrap();
        let _first_continue = next_outgoing(&mut outgoing_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let second_cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(first_cancel["method"], "session/cancel");
        assert_eq!(second_cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(2, "cancelled")).unwrap();
        let _second_continue = next_outgoing(&mut outgoing_rx).await;
        incoming_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming_tx.send(prompt_response(3, "end_turn")).unwrap();
        assert!(matches!(task.await.unwrap().unwrap(), StopReason::EndTurn));
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_second_stall_after_hard_recovery_is_monitored() {
        let initial_counters = Arc::new(ScriptedTransportCounters::default());
        let initial = ScriptedTransport::new(
            vec![
                ScriptedReceive::Message(init_response(0)),
                ScriptedReceive::Pending,
            ],
            initial_counters,
        );
        let (mut client, _) = scripted_client(initial).await;
        let (replacement_in_tx, replacement_in_rx) = mpsc::unbounded_channel();
        let (replacement_out_tx, mut replacement_out_rx) = mpsc::unbounded_channel();
        replacement_in_tx.send(init_response(2)).unwrap();
        client.push_replacement_transport(Box::new(ControlledTransport {
            incoming: replacement_in_rx,
            outgoing: replacement_out_tx,
            counters: None,
        }));
        let task = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        let _initialize = next_outgoing(&mut replacement_out_rx).await;
        let _first_continue = next_outgoing(&mut replacement_out_rx).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let second_cancel = next_outgoing(&mut replacement_out_rx).await;
        assert_eq!(second_cancel["method"], "session/cancel");
        replacement_in_tx
            .send(prompt_response(3, "cancelled"))
            .unwrap();
        let _second_continue = next_outgoing(&mut replacement_out_rx).await;
        replacement_in_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        replacement_in_tx
            .send(prompt_response(4, "end_turn"))
            .unwrap();
        assert!(matches!(task.await.unwrap().unwrap(), StopReason::EndTurn));
    }

    fn chunk_text(event: &Event) -> Option<String> {
        match event {
            Event::MessageChunk { content } => content
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_cancel_acknowledged_with_end_turn_forwards_truncated_and_continued_text() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_texts = Arc::clone(&captured);
        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |event| {
                        if let Some(text) = chunk_text(&event) {
                            handler_texts.lock().unwrap().push(text);
                        }
                    },
                )
                .await;
            (client, result)
        });
        let initial = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(initial["id"], 1);

        incoming_tx
            .send(text_chunk_update("sess_1", "before stall"))
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");

        incoming_tx
            .send(text_chunk_update(
                "sess_1",
                "Info: Operation cancelled by user",
            ))
            .unwrap();
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        let continuation = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(continuation["params"]["prompt"][0]["text"], CONTINUE_PROMPT);
        incoming_tx
            .send(text_chunk_update("sess_1", "after continuation"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 0);
        assert_eq!(
            captured.lock().unwrap().as_slice(),
            [
                "before stall".to_string(),
                "Info: Operation cancelled by user".to_string(),
                "after continuation".to_string(),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_cancel_acknowledged_with_non_cancelled_stop_reason_recovers() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _initial = next_outgoing(&mut outgoing_rx).await;

        tokio::time::advance(Duration::from_secs(1)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(1, "max_tokens")).unwrap();

        let continuation = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(continuation["method"], "session/prompt");
        assert_eq!(continuation["params"]["sessionId"], "sess_1");
        assert_eq!(continuation["params"]["prompt"][0]["text"], CONTINUE_PROMPT);
        incoming_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_external_cancellation_during_grace_with_end_turn_ack_returns_cancelled() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::from_future(async move {
                        let _ = cancel_rx.await;
                    }),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _initial = next_outgoing(&mut outgoing_rx).await;

        tokio::time::advance(Duration::from_secs(1)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");

        // The user cancels while the watchdog is still waiting for the backend
        // to acknowledge, and the backend then answers with `end_turn`.
        cancel_tx.send(()).unwrap();
        tokio::task::yield_now().await;
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::Cancelled));
        assert_eq!(client.replacement_factory_calls(), 0);
        assert!(outgoing_rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_soft_recovery_count_increments_across_repeated_stalls() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _initial = next_outgoing(&mut outgoing_rx).await;

        tokio::time::advance(Duration::from_secs(1)).await;
        let first_cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(first_cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        let first_continue = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(first_continue["params"]["sessionId"], "sess_1");
        assert_eq!(
            first_continue["params"]["prompt"][0]["text"],
            CONTINUE_PROMPT
        );

        tokio::time::advance(Duration::from_secs(1)).await;
        let second_cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(second_cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();
        let second_continue = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(second_continue["params"]["sessionId"], "sess_1");
        assert_eq!(
            second_continue["params"]["prompt"][0]["text"],
            CONTINUE_PROMPT
        );

        incoming_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming_tx.send(prompt_response(3, "end_turn")).unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 0);
        assert!(outgoing_rx.try_recv().is_err());
    }

    #[test]
    fn watchdog_last_activity_tracks_in_flight_tool_call() {
        let mut activity = LastObservedActivity::default();
        assert!(!activity.tool_call_in_flight());

        activity.observe_event(&Event::ToolCall {
            tool_call_id: "call_1".into(),
            title: "Run integration suite".into(),
            kind: "execute".into(),
            status: "in_progress".into(),
        });
        assert_eq!(activity.last_event_kind(), Some("tool_call"));
        assert_eq!(
            activity.last_tool_call_title(),
            Some("Run integration suite")
        );
        assert_eq!(activity.last_tool_call_kind(), Some("execute"));
        assert_eq!(activity.last_tool_call_status(), Some("in_progress"));
        assert!(activity.tool_call_in_flight());

        activity.observe_event(&Event::ToolCallUpdate {
            tool_call_id: "call_1".into(),
            status: "completed".into(),
            content: None,
        });
        assert_eq!(activity.last_event_kind(), Some("tool_call_update"));
        assert_eq!(activity.last_tool_call_status(), Some("completed"));
        assert_eq!(
            activity.last_tool_call_title(),
            Some("Run integration suite")
        );
        assert_eq!(activity.last_tool_call_kind(), Some("execute"));
        assert!(!activity.tool_call_in_flight());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_defers_cancel_while_tool_call_in_flight() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        let prompt = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        let _initial = next_outgoing(&mut outgoing_rx).await;

        incoming_tx
            .send(tool_call_started(
                "sess_1",
                "call_1",
                "Run integration suite",
                "execute",
                "in_progress",
            ))
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;

        assert!(outgoing_rx.try_recv().is_err());
        prompt.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_tool_wait_restarts_indefinitely_while_tool_in_flight() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        let prompt = tokio::spawn(async move {
            client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await
        });
        let _initial = next_outgoing(&mut outgoing_rx).await;

        incoming_tx
            .send(tool_call_started(
                "sess_1",
                "call_1",
                "Run integration suite",
                "execute",
                "in_progress",
            ))
            .unwrap();
        tokio::task::yield_now().await;
        for _ in 0..10 {
            tokio::time::advance(Duration::from_secs(1)).await;
            assert!(outgoing_rx.try_recv().is_err());
        }

        prompt.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_cancels_after_in_flight_tool_call_completes() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _initial = next_outgoing(&mut outgoing_rx).await;

        incoming_tx
            .send(tool_call_started(
                "sess_1",
                "call_1",
                "Run integration suite",
                "execute",
                "in_progress",
            ))
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(outgoing_rx.try_recv().is_err());

        incoming_tx
            .send(tool_call_progress("sess_1", "call_1", "completed"))
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;

        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        let continuation = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(continuation["params"]["prompt"][0]["text"], CONTINUE_PROMPT);
        incoming_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 0);
        assert!(outgoing_rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_cancels_immediately_when_tool_call_completed() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _initial = next_outgoing(&mut outgoing_rx).await;

        incoming_tx
            .send(tool_call_started(
                "sess_1",
                "call_1",
                "Run integration suite",
                "execute",
                "completed",
            ))
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;

        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();
        let continuation = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(continuation["params"]["prompt"][0]["text"], CONTINUE_PROMPT);
        incoming_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        incoming_tx.send(prompt_response(2, "end_turn")).unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(client.replacement_factory_calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_external_cancellation_during_tool_wait_returns_cancelled() {
        let (mut client, incoming_tx, mut outgoing_rx) = controlled_watchdog_client().await;
        let prompt = tokio::spawn(async move {
            let result = client
                .prompt(
                    "sess_1",
                    vec![PromptContent::text("work")],
                    PromptTurnCancellation::from_future(tokio::time::sleep(Duration::from_millis(
                        4500,
                    ))),
                    &mut |_| {},
                )
                .await;
            (client, result)
        });
        let _initial = next_outgoing(&mut outgoing_rx).await;

        incoming_tx
            .send(tool_call_started(
                "sess_1",
                "call_1",
                "Run integration suite",
                "execute",
                "in_progress",
            ))
            .unwrap();
        tokio::task::yield_now().await;
        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(1)).await;
            assert!(outgoing_rx.try_recv().is_err());
        }

        tokio::time::advance(Duration::from_millis(500)).await;
        let cancel = next_outgoing(&mut outgoing_rx).await;
        assert_eq!(cancel["method"], "session/cancel");
        incoming_tx.send(prompt_response(1, "cancelled")).unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::Cancelled));
        assert_eq!(client.replacement_factory_calls(), 0);
        assert!(outgoing_rx.try_recv().is_err());
    }
}
