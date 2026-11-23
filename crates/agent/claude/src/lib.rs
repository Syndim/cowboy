mod wire;

use anyhow::{Result, bail, ensure};
use async_trait::async_trait;
use cowboy_agent_client::{
    AgentAccess, AgentInfo, AgentLaunchSpec, AgentSessionDescriptor, BackendConnector, Client,
    Event, ModelInfo, PromptContent, PromptTurnCancellation, ResolvedBackend, StopReason,
};
use serde_json::{Value, json};
use std::{collections::HashSet, fmt, time::Duration};
use tokio::time::{Instant, timeout, timeout_at};
use uuid::Uuid;
use wire::Wire;

#[derive(Debug, Default)]
pub struct ClaudeCodeBackendConnector;

#[async_trait]
impl BackendConnector for ClaudeCodeBackendConnector {
    fn kind(&self) -> &str {
        "claude-code"
    }

    async fn connect(&self, spec: &AgentLaunchSpec) -> Result<ResolvedBackend> {
        ensure!(
            spec.startup_readiness.is_none(),
            "Claude Code does not support startup readiness logs"
        );
        ensure!(
            !matches!(spec.access, AgentAccess::DenyEscalation { .. }),
            "Claude Code does not support ACP mode verification"
        );
        ensure!(
            spec.watchdog.response_timeout_seconds > 0
                && spec.watchdog.cancel_timeout_seconds > 0
                && spec.watchdog.recovery_operation_timeout_seconds > 0,
            "Claude Code timeouts must be positive"
        );
        ensure!(
            spec.args.is_empty(),
            "Claude Code manages its launch arguments; set args = []"
        );
        Ok(ResolvedBackend {
            client: Box::new(ClaudeClient {
                spec: spec.clone(),
                wire: None,
                session: None,
                descriptor: None,
                info: AgentInfo {
                    name: "claude-code".into(),
                    title: None,
                    version: None,
                },
            }),
            model: spec.model.clone(),
            agent: spec.agent.clone(),
        })
    }
}

struct ClaudeClient {
    spec: AgentLaunchSpec,
    wire: Option<Wire>,
    session: Option<String>,
    descriptor: Option<AgentSessionDescriptor>,
    info: AgentInfo,
}

impl fmt::Debug for ClaudeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeClient")
            .field("connected", &self.wire.is_some())
            .finish_non_exhaustive()
    }
}

impl ClaudeClient {
    fn setup_timeout(&self) -> Duration {
        Duration::from_secs(self.spec.watchdog.recovery_operation_timeout_seconds)
    }

    async fn establish(
        &mut self,
        cwd: &str,
        session: &str,
        resume: bool,
        model: Option<&ModelInfo>,
        mcp: &[Value],
    ) -> Result<()> {
        ensure!(
            mcp.is_empty(),
            "Claude Code does not support Cowboy-provided MCP servers"
        );
        ensure!(
            std::path::Path::new(cwd).is_absolute(),
            "Claude Code requires an absolute working directory"
        );
        ensure!(
            Uuid::parse_str(session).is_ok(),
            "Claude Code requires a UUID session ID"
        );
        self.close().await?;
        let mut args = Vec::new();
        args.extend(
            [
                "-p",
                "--input-format=stream-json",
                "--output-format=stream-json",
                "--verbose",
                "--include-partial-messages",
                "--safe-mode",
                "--setting-sources=",
                "--strict-mcp-config",
                "--permission-mode=default",
                "--permission-prompt-tool=stdio",
            ]
            .map(str::to_owned),
        );
        if matches!(self.spec.access, AgentAccess::DenyAll) {
            args.push("--tools=".into());
        }
        args.push(format!(
            "{}={session}",
            if resume { "--resume" } else { "--session-id" }
        ));
        if let Some(model) = model.or(self.spec.model.as_ref()) {
            ensure!(
                model
                    .provider
                    .as_deref()
                    .is_none_or(|provider| provider == "anthropic"),
                "Claude Code supports only the anthropic model provider"
            );
            args.push(format!("--model={}", model.id));
        }
        let mut wire = Wire::spawn(&self.spec.command, &args, &self.spec.allowed_env, cwd).await?;
        let ready = timeout(self.setup_timeout(), async {
            wire.send(&control("initialize", json!({"subtype":"initialize","hooks":null}))).await?;
            loop {
                let frame = wire.recv().await?;
                if reply(&frame, "initialize")? {
                    let metadata = &frame["response"]["response"];
                    if let Some(mode) = metadata["current_permission_mode"].as_str() {
                        ensure!(mode == "default", "Claude Code did not select the expected permission mode");
                    }
                    return Ok(metadata["claude_code_version"].as_str().map(str::to_owned));
                }
                if frame["type"] == "result" { bail!("Claude Code initialization failed; check the CLI installation and credentials"); }
                reject_inbound(&mut wire, &frame).await?;
            }
        }).await.unwrap_or_else(|_| Err(anyhow::anyhow!("Claude Code initialization timed out")));
        self.info.version = match ready {
            Ok(version) => version,
            Err(error) => {
                let _ = wire.close().await;
                return Err(error);
            }
        };
        self.wire = Some(wire);
        self.session = Some(session.to_owned());
        Ok(())
    }

    async fn run_turn(
        &mut self,
        session: &str,
        content: Vec<PromptContent>,
        cancellation: &mut PromptTurnCancellation,
        handler: &mut (dyn FnMut(Event) + Send),
    ) -> Result<StopReason> {
        let inactivity = Duration::from_secs(self.spec.watchdog.response_timeout_seconds);
        let cancel_timeout = Duration::from_secs(self.spec.watchdog.cancel_timeout_seconds);
        let deny_all = matches!(self.spec.access, AgentAccess::DenyAll);
        let wire = self
            .wire
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Claude Code is disconnected"))?;
        send_within(wire, inactivity, &json!({"type":"user","session_id":session,"parent_tool_use_id":null,"message":{"role":"user","content":content}})).await?;
        let mut cancel_deadline = None;
        let mut acknowledged = false;
        let mut finished = false;
        let mut deadline = Instant::now() + inactivity;
        let mut turn = Turn::default();
        loop {
            ensure!(
                Instant::now() < cancel_deadline.unwrap_or(deadline),
                "Claude Code turn timed out"
            );
            let received = if let Some(deadline) = cancel_deadline {
                timeout_at(deadline, wire.recv()).await
            } else {
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        let end = Instant::now() + cancel_timeout;
                        send_within(wire, cancel_timeout, &control("interrupt", json!({"subtype":"interrupt"}))).await?;
                        cancel_deadline = Some(end);
                        continue;
                    }
                    frame = timeout_at(deadline, wire.recv()) => frame,
                }
            };
            let frame = received.map_err(|_| anyhow::anyhow!("Claude Code turn timed out"))??;
            if frame["type"] == "control_request" {
                let end = cancel_deadline.unwrap_or(deadline);
                timeout_at(end, reject_inbound(wire, &frame))
                    .await
                    .map_err(|_| anyhow::anyhow!("Claude Code permission reply timed out"))??;
                continue;
            }
            if cancel_deadline.is_some() && reply(&frame, "interrupt")? {
                acknowledged = true;
            }
            if let Some(observed) = frame["session_id"].as_str() {
                ensure!(
                    observed == session,
                    "Claude Code returned an unexpected session ID"
                );
            }
            if frame["type"] == "system" && frame["subtype"] == "init" {
                self.descriptor = frame["model"].as_str().map(|model| AgentSessionDescriptor {
                    model: Some(model.into()),
                    ..Default::default()
                });
            }
            if frame["type"] == "result" {
                if cancel_deadline.is_some() {
                    finished = true;
                } else {
                    ensure!(
                        frame["session_id"] == session,
                        "Claude Code result is missing the expected session ID"
                    );
                    if frame["subtype"] == "error_max_turns" {
                        return Ok(StopReason::MaxTurnRequests);
                    }
                    ensure!(
                        frame["is_error"] == false && frame["subtype"] == "success",
                        "Claude Code turn failed; check authentication, model availability, and saved session state"
                    );
                    return Ok(StopReason::EndTurn);
                }
            }
            if acknowledged && finished {
                return Ok(StopReason::Cancelled);
            }
            if turn.project(&frame, deny_all, handler)? {
                deadline = Instant::now() + inactivity;
            }
        }
    }
}

#[async_trait]
impl Client for ClaudeClient {
    fn is_connected(&self) -> bool {
        self.wire.is_some()
    }
    fn agent_info(&self) -> Option<&AgentInfo> {
        Some(&self.info)
    }
    fn session_id(&self) -> Option<&str> {
        self.session.as_deref()
    }
    fn session_descriptor(&self) -> Option<&AgentSessionDescriptor> {
        self.descriptor.as_ref()
    }
    fn supports_load_session(&self) -> bool {
        true
    }
    async fn new_session(
        &mut self,
        cwd: &str,
        mcp: &[Value],
        model: Option<&ModelInfo>,
    ) -> Result<String> {
        let session = Uuid::new_v4().to_string();
        self.establish(cwd, &session, false, model, mcp).await?;
        Ok(session)
    }
    async fn load_session(
        &mut self,
        session: &str,
        cwd: &str,
        mcp: &[Value],
    ) -> Result<Vec<Event>> {
        self.establish(cwd, session, true, None, mcp).await?;
        Ok(vec![])
    }
    async fn prompt(
        &mut self,
        session: &str,
        content: Vec<PromptContent>,
        mut cancellation: PromptTurnCancellation,
        handler: &mut (dyn FnMut(Event) + Send),
    ) -> Result<StopReason> {
        ensure!(
            self.session_id() == Some(session),
            "Claude Code session is not open"
        );
        ensure!(
            !content.is_empty() && content.iter().all(|block| block.content_type == "text"),
            "Claude Code requires text prompt content"
        );
        if cancellation.try_cancelled() {
            return Ok(StopReason::Cancelled);
        }
        let outcome = self
            .run_turn(session, content, &mut cancellation, handler)
            .await;
        if outcome.is_err() {
            let _ = self.close().await;
        }
        outcome
    }
    async fn close(&mut self) -> Result<()> {
        self.session = None;
        self.descriptor = None;
        if let Some(mut wire) = self.wire.take() {
            wire.close().await?;
        }
        Ok(())
    }
}

fn control(id: &str, request: Value) -> Value {
    json!({"type":"control_request","request_id":id,"request":request})
}

fn reply(frame: &Value, id: &str) -> Result<bool> {
    if frame["type"] != "control_response" || frame["response"]["request_id"] != id {
        return Ok(false);
    }
    ensure!(
        frame["response"]["subtype"] == "success",
        "Claude Code rejected the {id} control request"
    );
    Ok(true)
}

async fn reject_inbound(wire: &mut Wire, frame: &Value) -> Result<()> {
    if frame["type"] != "control_request" {
        return Ok(());
    }
    let id = frame["request_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Claude Code control request has no valid identifier"))?;
    if frame["request"]["subtype"] == "can_use_tool" {
        wire.send(&json!({"type":"control_response","response":{"request_id":id,"subtype":"success","response":{"behavior":"deny","message":"Cowboy does not approve tool requests"}}})).await
    } else {
        wire.send(&json!({"type":"control_response","response":{"request_id":id,"subtype":"error","error":"Unsupported Claude Code control request"}})).await
    }
}

async fn send_within(wire: &mut Wire, duration: Duration, value: &Value) -> Result<()> {
    timeout(duration, wire.send(value))
        .await
        .map_err(|_| anyhow::anyhow!("Claude Code write timed out"))?
}

#[derive(Default)]
struct Turn {
    streamed_text: bool,
    tools: HashSet<String>,
}

impl Turn {
    fn project(
        &mut self,
        frame: &Value,
        deny_all: bool,
        emit: &mut (dyn FnMut(Event) + Send),
    ) -> Result<bool> {
        let kind = frame["type"].as_str();
        match kind {
            Some("stream_event") => {
                let event = &frame["event"];
                if event["type"] == "message_start" {
                    self.streamed_text = false;
                }
                if event["type"] == "content_block_start" {
                    self.tool(&event["content_block"], deny_all, emit)?;
                }
                for block in [&event["content_block"], &event["delta"]] {
                    if let Some(text) = block["text"].as_str().filter(|text| !text.is_empty()) {
                        self.streamed_text = true;
                        emit(Event::MessageChunk {
                            content: json!({"type":"text","text":text}),
                        });
                    }
                }
                Ok(true)
            }
            Some("assistant" | "user") => {
                for block in frame["message"]["content"].as_array().into_iter().flatten() {
                    self.tool(block, deny_all, emit)?;
                    match block["type"].as_str() {
                        Some("text") if kind == Some("assistant") && !self.streamed_text => {
                            emit(Event::MessageChunk {
                                content: block.clone(),
                            });
                        }
                        Some("tool_result") => {
                            ensure!(
                                !deny_all,
                                "Claude Code reported tool activity with deny_all enabled"
                            );
                            if let Some(id) = block["tool_use_id"].as_str() {
                                emit(Event::ToolCallUpdate { tool_call_id: id.into(), status: if block["is_error"] == true { "failed" } else { "completed" }.into(), content: block.get("content").cloned().map(|content| if content.is_string() { json!([{"type":"content","content":{"type":"text","text":content}}]) } else { content }) });
                            }
                        }
                        _ => {}
                    }
                }
                if kind == Some("assistant") {
                    self.streamed_text = false;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn tool(
        &mut self,
        block: &Value,
        deny_all: bool,
        emit: &mut (dyn FnMut(Event) + Send),
    ) -> Result<()> {
        if block["type"] == "tool_use" {
            ensure!(
                !deny_all,
                "Claude Code reported tool activity with deny_all enabled"
            );
            if let Some(id) = block["id"].as_str()
                && self.tools.insert(id.to_owned())
            {
                emit(Event::ToolCall {
                    tool_call_id: id.into(),
                    title: block["name"].as_str().unwrap_or("Claude tool").into(),
                    kind: "other".into(),
                    status: "in_progress".into(),
                });
            }
        }
        Ok(())
    }
}
