use serde::{Deserialize, Serialize};
use serde_json::Value;

use cowboy_agent_client::{
    AgentInfo, Event, ModelInfo, PromptContent, PromptTurnCancellation, StopReason,
};

use super::messages::*;
use super::transport::{Transport, TransportConfig};
use async_trait::async_trait;

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
        let transport = Self::create_transport(&transport_config, None).await?;
        Self::connect_with_transport(transport, transport_config).await
    }

    /// Connect using a pre-built transport (for tests or custom transports).
    pub async fn connect_with_transport(
        transport: Box<dyn Transport>,
        transport_config: TransportConfig,
    ) -> anyhow::Result<Self> {
        let mut client = Self {
            transport: Some(transport),
            transport_config,
            next_id: 0,
            agent_capabilities: None,
            agent_info: None,
            session_id: None,
            pushback: Vec::new(),
        };

        client.initialize().await?;
        Ok(client)
    }

    /// Whether the transport is connected.
    pub fn is_connected(&self) -> bool {
        self.transport.is_some()
    }

    /// Create a new ACP session.
    ///
    /// Passes model configuration to the agent through the ACP `_meta` extension field.
    /// ACP-aware agents read model settings from `_meta.model`.
    /// Agents that do not support it ignore the `_meta` field, as allowed by the ACP spec.
    pub async fn new_session(
        &mut self,
        cwd: &str,
        mcp_servers: &[Value],
        model: &ModelInfo,
    ) -> anyhow::Result<String> {
        tracing::debug!(
            cwd,
            mcp_server_count = mcp_servers.len(),
            model_id = %model.id,
            provider = ?model.provider,
            "ACP session/new starting"
        );
        let params = SessionNewParams {
            cwd: cwd.to_string(),
            mcp_servers: mcp_servers.to_vec(),
            meta: SessionMeta {
                model: SessionModelMeta {
                    id: model.id.clone(),
                    provider: model.provider.clone(),
                },
            },
        };

        let result = self.send_request("session/new", params).await?;
        let session: SessionNewResult = serde_json::from_value(result)?;
        self.session_id = Some(session.session_id.clone());
        tracing::info!(
            session_id = %session.session_id,
            model_id = %model.id,
            provider = ?model.provider,
            "ACP session created"
        );
        Ok(session.session_id)
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
            content = vec![PromptContent::text("Continue")];
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
        let id = self.send_request_no_wait("session/prompt", params).await?;
        tracing::debug!(
            session_id,
            id,
            content_count,
            prompt_chars,
            "ACP prompt sent"
        );

        const MAX_DEFERRED_UPDATES_AFTER_CANCELLATION: usize = 1;
        let mut activity = PromptTurnActivity::Empty;
        let mut cancellation_sent = false;
        let mut deferred_updates_after_cancellation = 0;

        let stop_reason = loop {
            let msg = if cancellation_sent {
                self.recv_message().await?
            } else {
                let message = tokio::select! {
                    biased;
                    message = self.recv_message() => Some(message?),
                    () = cancellation.cancelled() => None,
                };
                let Some(message) = message else {
                    self.send_prompt_turn_cancellation(session_id).await?;
                    cancellation_sent = true;
                    tracing::debug!(session_id, id, "ACP prompt turn cancellation sent");
                    continue;
                };
                message
            };

            let matching_prompt_response = matches!(
                &msg,
                Message::Response { id: response_id, .. } if *response_id == id
            );
            let cancellation_ready =
                !cancellation_sent && !matching_prompt_response && cancellation.try_cancelled();
            if cancellation_ready {
                let defer_for_buffered_completion = matches!(&msg, Message::SessionUpdate { .. })
                    && deferred_updates_after_cancellation
                        < MAX_DEFERRED_UPDATES_AFTER_CANCELLATION;
                if defer_for_buffered_completion {
                    deferred_updates_after_cancellation += 1;
                } else {
                    self.send_prompt_turn_cancellation(session_id).await?;
                    cancellation_sent = true;
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
                    let outcome = if cancellation_sent {
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
                        cancellation_sent,
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
                        tracing::warn!(session_id, id = resp_id, error = %err, "ACP prompt response error");
                        anyhow::bail!("Agent error: {err}");
                    }
                    let prompt_result: SessionPromptResult =
                        serde_json::from_value(result.unwrap_or(Value::Null))
                            .unwrap_or(SessionPromptResult { stop_reason: None });
                    let stop_reason = prompt_result.stop_reason.unwrap_or(StopReason::EndTurn);
                    if cancellation_sent && !matches!(stop_reason, StopReason::Cancelled) {
                        tracing::debug!(
                            session_id,
                            id = resp_id,
                            stop_reason = ?stop_reason,
                            "ACP prompt completed before cancellation took effect"
                        );
                    }
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

    fn session_id(&self) -> Option<&str> {
        Client::session_id(self)
    }

    async fn new_session(
        &mut self,
        cwd: &str,
        mcp_servers: &[Value],
        model: &ModelInfo,
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
    }

    async fn next_outgoing(receiver: &mut mpsc::UnboundedReceiver<String>) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("client should send the next ACP message")
            .expect("controlled outgoing channel should remain open");
        serde_json::from_str(&message).unwrap()
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

        let session_id = client.new_session("/project", &[], &model).await.unwrap();
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
        let sid = client.new_session("/project", &[], &model).await.unwrap();
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

        client.new_session("/a", &[], &model).await.unwrap();
        client.new_session("/b", &[], &model).await.unwrap();

        // IDs should be 0 (init), 1, 2
        let sent = outgoing.lock();
        let req0: Value = serde_json::from_str(&sent[0]).unwrap();
        let req1: Value = serde_json::from_str(&sent[1]).unwrap();
        let req2: Value = serde_json::from_str(&sent[2]).unwrap();
        assert_eq!(req0["id"], 0);
        assert_eq!(req1["id"], 1);
        assert_eq!(req2["id"], 2);
    }
}
