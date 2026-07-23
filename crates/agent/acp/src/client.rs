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
    /// Push-back buffer for messages consumed during trailing event drain
    #[serde(skip)]
    pushback: Vec<String>,
    /// Agent-returned session descriptor captured from `session/new` (or the
    /// post-`set_config_option`) config options; never derived from the
    /// configured `ModelInfo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_descriptor: Option<AgentSessionDescriptor>,
    #[serde(default)]
    watchdog: AgentWatchdogOptions,
    #[cfg(test)]
    #[serde(skip)]
    replacement_factory: ReplacementTransportFactory,
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
            pushback: Vec::new(),
            session_descriptor: self.session_descriptor.clone(),
            watchdog: self.watchdog,
            #[cfg(test)]
            replacement_factory: ReplacementTransportFactory::default(),
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
            .finish()
    }
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
                id,
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

impl Client {
    /// Get a mutable reference to the existing transport (no reconnect).
    fn transport_mut(&mut self) -> anyhow::Result<&mut Box<dyn Transport>> {
        self.transport
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("client transport not connected"))
    }

    /// Get a mutable reference to the transport, reconnecting if needed.
    async fn ensure_transport(&mut self) -> anyhow::Result<&mut Box<dyn Transport>> {
        if self.transport.is_none() {
            tracing::info!(
                transport = transport_kind(&self.transport_config),
                session_id = ?self.session_id,
                "Reconnecting client transport"
            );
            let transport =
                Self::create_transport(&self.transport_config, self.session_id.as_deref()).await?;
            self.transport = Some(transport);
            self.pushback.clear();
            self.initialize().await?;
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
    ) -> anyhow::Result<Box<dyn Transport>> {
        match config {
            TransportConfig::Stdio(cfg) => {
                use super::transport::stdio::StdioTransport;
                let resume_arg =
                    resume_session_id.map(|session_id| format!("--resume={session_id}"));
                let additional_args = resume_arg.as_deref().into_iter().collect::<Vec<_>>();
                let transport = StdioTransport::connect(cfg, &additional_args).await?;
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
        #[cfg(test)]
        if let Some(outcome) = self.replacement_factory.next() {
            return outcome.into_transport().await;
        }
        Self::create_transport(&self.transport_config, Some(session_id)).await
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

    /// Connect to the agent with TransportConfig and complete the ACP initialize handshake.
    pub async fn connect(transport_config: TransportConfig) -> anyhow::Result<Self> {
        Self::connect_with_options(transport_config, AgentWatchdogOptions::default()).await
    }

    pub async fn connect_with_options(
        transport_config: TransportConfig,
        watchdog: AgentWatchdogOptions,
    ) -> anyhow::Result<Self> {
        let transport = Self::create_transport(&transport_config, None).await?;
        Self::connect_with_transport_and_options(transport, transport_config, watchdog).await
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
        let mut client = Self {
            transport: Some(transport),
            transport_config,
            next_id: 0,
            agent_capabilities: None,
            agent_info: None,
            session_id: None,
            pushback: Vec::new(),
            session_descriptor: None,
            watchdog,
            #[cfg(test)]
            replacement_factory: ReplacementTransportFactory::default(),
        };

        client.initialize().await?;
        Ok(client)
    }

    #[cfg(test)]
    fn push_replacement_transport(&mut self, transport: Box<dyn Transport>) {
        self.replacement_factory
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
        let session: SessionNewResult = serde_json::from_value(result)?;
        let mut descriptor_options = session.config_options.clone();
        if let Some(model) = model
            && let Some(applied_options) = self
                .apply_model_config_option(&session.session_id, &session.config_options, model)
                .await?
        {
            descriptor_options = applied_options;
        }
        self.session_descriptor = Self::descriptor_from_config_options(&descriptor_options);
        self.session_id = Some(session.session_id.clone());
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
    /// Automatically grants agent permission requests; executor and reviewer roles both have full permissions.
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
        const MAX_CONTINUATIONS: u32 = 5;
        let mut content = prompt_content;

        for attempt in 0..=MAX_CONTINUATIONS {
            let outcome = self
                .prompt_turn(session_id, content, &mut cancellation, event_handler)
                .await?;

            // Done if the agent produced visible text, stopped for a reason
            // other than a normal end_turn, or completed after a permission
            // exchange. A truly empty end_turn can be a backend acknowledgement
            // rather than a useful answer, even when it only emitted
            // housekeeping updates.
            if !outcome.should_continue() {
                return Ok(outcome.stop_reason);
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

    async fn cleanup_replacement(&mut self) {
        let Some(mut transport) = self.transport.take() else {
            return;
        };
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let _ = tokio::time::timeout(timeout, transport.force_terminate()).await;
    }

    async fn hard_recover_and_continue(&mut self, session_id: &str) -> anyhow::Result<u64> {
        let timeout = Duration::from_secs(self.watchdog.recovery_operation_timeout_seconds);
        let Some(mut old_transport) = self.transport.take() else {
            anyhow::bail!("agent watchdog recovery found no active transport");
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
                anyhow::bail!("agent watchdog force termination failed: {err}");
            }
            Err(_) => {
                tracing::error!(
                    event = "agent_watchdog_recovery_failed",
                    session_id,
                    "Agent watchdog force termination timed out"
                );
                anyhow::bail!("agent watchdog force termination timed out");
            }
        }
        drop(old_transport);

        let replacement =
            tokio::time::timeout(timeout, self.create_replacement_transport(session_id))
                .await
                .map_err(|_| {
                    anyhow::anyhow!("agent watchdog replacement transport creation timed out")
                })?
                .map_err(|err| {
                    anyhow::anyhow!("agent watchdog replacement transport creation failed: {err}")
                })?;
        self.transport = Some(replacement);
        self.pushback.clear();

        match tokio::time::timeout(timeout, self.initialize()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                self.cleanup_replacement().await;
                anyhow::bail!("agent watchdog replacement initialization failed: {err}");
            }
            Err(_) => {
                self.cleanup_replacement().await;
                anyhow::bail!("agent watchdog replacement initialization timed out");
            }
        }

        tracing::warn!(
            event = "agent_watchdog_transport_resumed",
            session_id,
            "Agent watchdog resumed the session on a replacement transport"
        );
        match self.dispatch_watchdog_continuation(session_id).await {
            Ok(id) => Ok(id),
            Err(err) => {
                self.cleanup_replacement().await;
                Err(err)
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
        let mut external_cancellation_sent = false;
        let mut deferred_updates_after_external_cancellation = 0usize;
        let mut replacement_continuation_active = false;
        let stop_reason = 'monitor: loop {
            let response_deadline =
                tokio::time::sleep(Duration::from_secs(self.watchdog.response_timeout_seconds));
            tokio::pin!(response_deadline);
            enum WaitOutcome {
                Message(anyhow::Result<Message>),
                ExternalCancellation,
                WatchdogTimeout,
            }
            let outcome = if external_cancellation_sent {
                WaitOutcome::Message(self.recv_message().await)
            } else {
                tokio::select! {
                    biased;
                    message = self.recv_message() => WaitOutcome::Message(message),
                    () = cancellation.cancelled() => WaitOutcome::ExternalCancellation,
                    () = &mut response_deadline => WaitOutcome::WatchdogTimeout,
                }
            };

            let msg = match outcome {
                WaitOutcome::Message(message) => match message {
                    Ok(message) => message,
                    Err(err) => {
                        if external_cancellation_sent {
                            return Err(err);
                        }
                        if replacement_continuation_active {
                            self.cleanup_replacement().await;
                            return Err(anyhow::anyhow!(
                                "agent watchdog replacement continuation failed: {err}"
                            ));
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
                    tracing::warn!(
                        event = "agent_watchdog_timeout",
                        session_id,
                        id,
                        timeout_seconds = self.watchdog.response_timeout_seconds,
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
                            CancelGraceOutcome::Message(self.recv_message().await)
                        } else {
                            tokio::select! {
                                biased;
                                message = self.recv_message() => CancelGraceOutcome::Message(message),
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
                                continue 'monitor;
                            }
                        };
                        let message = match message {
                            Ok(message) => message,
                            Err(err) => {
                                if external_cancellation_sent {
                                    return Err(err);
                                }
                                id = self.hard_recover_and_continue(session_id).await?;
                                replacement_continuation_active = true;
                                continue 'monitor;
                            }
                        };
                        match message {
                            Message::SessionUpdate { update, .. } => {
                                activity.observe_event(&update);
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
                                    request_id = req_id,
                                    tool_kind = ?tool_call.get("kind").and_then(|value| value.as_str()),
                                    "ACP permission request cancelled during watchdog grace"
                                );
                                self.send_rpc_response(req_id, PermissionOutcome::cancelled())
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
                                    continue 'monitor;
                                }
                                let result: SessionPromptResult =
                                    serde_json::from_value(result.unwrap_or(Value::Null))
                                        .unwrap_or(SessionPromptResult { stop_reason: None });
                                let stop_reason = result.stop_reason.unwrap_or(StopReason::EndTurn);
                                if matches!(stop_reason, StopReason::Cancelled) {
                                    if external_cancellation_sent {
                                        break 'monitor StopReason::Cancelled;
                                    }
                                    tracing::warn!(
                                        event = "agent_watchdog_soft_recovered",
                                        session_id,
                                        "Agent watchdog cancellation completed; continuing session"
                                    );
                                    id = self.dispatch_watchdog_continuation(session_id).await?;
                                    continue 'monitor;
                                }
                                break 'monitor stop_reason;
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
                        request_id = req_id,
                        tool_kind = ?tool_call.get("kind").and_then(|value| value.as_str()),
                        tool_title = ?tool_call.get("title").and_then(|value| value.as_str()),
                        options = options.len(),
                        outcome = ?outcome,
                        "ACP permission request answered"
                    );
                    self.send_rpc_response(req_id, outcome).await?;
                }
                Message::Response {
                    id: resp_id,
                    result,
                    error,
                } if resp_id == id => {
                    if let Some(err) = error {
                        if replacement_continuation_active {
                            self.cleanup_replacement().await;
                            anyhow::bail!(
                                "agent watchdog replacement continuation RPC error: {err}"
                            );
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

        if matches!(stop_reason, StopReason::Cancelled) {
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
        let drain_ms = if matches!(activity, PromptTurnActivity::Text) {
            500
        } else {
            15_000
        };
        let saw_trailing_text = self.drain_trailing_events(event_handler, drain_ms).await;
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
        event_handler: &mut (dyn FnMut(Event) + Send),
        initial_timeout_ms: u64,
    ) -> bool {
        use tokio::time::{Duration, timeout};

        let mut drain_timeout = Duration::from_millis(initial_timeout_ms);
        let mut saw_text = false;

        let Some(transport) = self.transport.as_mut() else {
            return saw_text;
        };
        let mut pushback_line = None;

        while let Ok(Ok(Some(line))) = timeout(drain_timeout, transport.recv()).await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            tracing::debug!(payload = %line, "ACP <<< trailing");
            if let Ok(json) = serde_json::from_str::<Value>(&line)
                && let Some(msg) = parse_acp_message(&json)
            {
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

        saw_text
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
        if !self.supports_load_session() {
            tracing::warn!(session_id, "ACP session/load unsupported by agent");
            anyhow::bail!("Agent does not support loadSession capability");
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
        let id = self.send_request_no_wait("session/load", params).await?;

        let mut history = Vec::new();
        loop {
            let msg = self.recv_message().await?;
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
                    id: resp_id, error, ..
                } if resp_id == id => {
                    if let Some(err) = error {
                        tracing::warn!(session_id, id = resp_id, error = %err, "ACP session/load response error");
                        anyhow::bail!("session/load error: {err}");
                    }
                    self.session_id = Some(session_id.to_string());
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
        let id = self.send_request_no_wait(method, params).await?;

        loop {
            let msg = self.recv_message().await?;
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
    async fn send_rpc_response<R: Serialize>(&mut self, id: u64, result: R) -> anyhow::Result<()> {
        let response = JsonRpcResponse::new(id, result);
        let line = serde_json::to_string(&response)?;
        tracing::debug!(id, payload = %line, "ACP >>> response");
        self.ensure_transport().await?.send(&line).await?;
        Ok(())
    }

    /// Receive and parse the next ACP message from the transport.
    ///
    /// Keeps reading until it gets one parseable ACP message.
    /// Skips empty lines and unrecognized message formats.
    async fn recv_message(&mut self) -> anyhow::Result<Message> {
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

            let json: Value = serde_json::from_str(&line).map_err(|e| {
                anyhow::anyhow!("Failed to parse JSON from agent: {e}\nLine: {line}")
            })?;

            if let Some(msg) = parse_acp_message(&json) {
                tracing::debug!(payload = %line, "ACP <<< received");
                log_acp_message("inbound", &msg);
                return Ok(msg);
            }
            tracing::warn!(payload = %line, "ACP <<< skipping unrecognized message");
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

            let json: Value = serde_json::from_str(&line).map_err(|e| {
                anyhow::anyhow!("Failed to parse JSON from agent: {e}\nLine: {line}")
            })?;

            if let Some(msg) = parse_acp_message(&json) {
                tracing::debug!(payload = %line, "ACP <<< received");
                log_acp_message("inbound_direct", &msg);
                return Ok(msg);
            }
            tracing::warn!(payload = %line, "ACP <<< skipping unrecognized message");
        }
    }

    /// Close the connection.
    pub async fn close(&mut self) -> anyhow::Result<()> {
        tracing::debug!(
            transport = transport_kind(&self.transport_config),
            session_id = ?self.session_id,
            connected = self.transport.is_some(),
            "Closing ACP client"
        );
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
            env: vec![],
        })
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
        let continuation = next_outgoing(&mut replacement_out_rx).await;
        assert_eq!(continuation["id"], 3);
        assert_eq!(continuation["params"]["sessionId"], "sess_1");
        assert_eq!(continuation["params"]["prompt"][0]["text"], CONTINUE_PROMPT);
        replacement_in_tx
            .send(text_chunk_update("sess_1", "recovered"))
            .unwrap();
        replacement_in_tx
            .send(prompt_response(3, "end_turn"))
            .unwrap();

        let (client, result) = prompt.await.unwrap();
        assert!(matches!(result.unwrap(), StopReason::EndTurn));
        assert_eq!(initial_counters.force_terminated.load(Ordering::SeqCst), 1);
        assert_eq!(client.replacement_factory_calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_hard_replacement_rpc_error_disposes_transport() {
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
        let continuation = next_outgoing(&mut replacement_out_rx).await;
        replacement_in_tx
            .send(rpc_error(
                continuation["id"].as_u64().unwrap(),
                -32000,
                "replacement failed",
            ))
            .unwrap();

        let (client, result) = prompt.await.unwrap();
        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("replacement continuation RPC error")
        );
        assert_eq!(
            replacement_counters.force_terminated.load(Ordering::SeqCst),
            1
        );
        assert_eq!(client.replacement_factory_calls(), 1);
        assert!(!client.is_connected());
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
        incoming_tx.send(prompt_response(1, "end_turn")).unwrap();

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
}
