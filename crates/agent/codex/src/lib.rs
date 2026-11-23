//! Codex app-server JSON-RPC backend.
mod process;

use async_trait::async_trait;
use cowboy_agent_client::{
    AgentAccess, AgentInfo, AgentLaunchSpec, AgentSessionDescriptor, BackendConnector, Client,
    Event, ModelInfo, PromptContent, PromptTurnCancellation, ResolvedBackend, StopReason,
};
use process::{CodexProcess, CodexProcessConfig};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Debug, Default)]
pub struct CodexBackendConnector;

#[async_trait]
impl BackendConnector for CodexBackendConnector {
    fn kind(&self) -> &str {
        "codex-app-server"
    }
    async fn connect(&self, spec: &AgentLaunchSpec) -> anyhow::Result<ResolvedBackend> {
        anyhow::ensure!(
            spec.backend == self.kind(),
            "wrong backend for Codex connector"
        );
        anyhow::ensure!(
            spec.access == AgentAccess::Default,
            "Codex app-server supports only default access; restricted Cowboy access modes cannot be enforced"
        );
        anyhow::ensure!(
            spec.startup_readiness.is_none(),
            "Codex app-server does not support ACP startup readiness"
        );
        Ok(ResolvedBackend {
            client: Box::new(CodexClient::new(spec.clone())),
            model: spec.model.clone(),
            agent: spec.agent.clone(),
        })
    }
}

struct CodexClient {
    spec: AgentLaunchSpec,
    process: Option<CodexProcess>,
    session: Option<String>,
    descriptor: Option<AgentSessionDescriptor>,
    info: AgentInfo,
    next_id: u64,
    closed: bool,
}
impl std::fmt::Debug for CodexClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexClient")
            .field("session", &self.session)
            .field("connected", &self.process.is_some())
            .finish()
    }
}
impl CodexClient {
    fn new(spec: AgentLaunchSpec) -> Self {
        Self {
            spec,
            process: None,
            session: None,
            descriptor: None,
            info: AgentInfo {
                name: "codex".into(),
                title: None,
                version: None,
            },
            next_id: 0,
            closed: false,
        }
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(self.spec.watchdog.recovery_operation_timeout_seconds.max(1))
    }
    async fn send(&mut self, value: &Value) -> anyhow::Result<()> {
        let line = serde_json::to_string(value)?;
        self.process
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Codex process is not running"))?
            .send_line(&line)
            .await
    }
    async fn request(&mut self, method: &str, params: Value) -> anyhow::Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"id": id, "method": method, "params": params}))
            .await?;
        Ok(id)
    }
    async fn recv(&mut self) -> anyhow::Result<Value> {
        let line = self
            .process
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Codex process is not running"))?
            .recv_line()
            .await?
            .ok_or_else(|| anyhow::anyhow!("Codex app-server closed its output"))?;
        serde_json::from_str(&line)
            .map_err(|_| anyhow::anyhow!("Codex app-server sent invalid JSON"))
    }
    async fn server_request(&mut self, msg: &Value) -> anyhow::Result<bool> {
        let (Some(id), Some(method)) = (msg.get("id"), msg.get("method").and_then(Value::as_str))
        else {
            return Ok(false);
        };
        let reply = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                json!({"id": id, "result": {"decision": "decline"}})
            }
            _ => {
                json!({"id": id, "error": {"code": -32601, "message": "Unsupported server request"}})
            }
        };
        self.send(&reply).await?;
        Ok(true)
    }
    async fn response(&mut self, id: u64) -> anyhow::Result<Value> {
        loop {
            let msg = self.recv().await?;
            if self.server_request(&msg).await? {
                continue;
            }
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                anyhow::ensure!(
                    msg.get("error").is_none(),
                    "Codex app-server request failed (code {})",
                    msg.pointer("/error/code")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                );
                return msg
                    .get("result")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Codex response has no result"));
            }
        }
    }
    async fn operation(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let deadline = self.timeout();
        tokio::time::timeout(deadline, async {
            let id = self.request(method, params).await?;
            self.response(id).await
        })
        .await
        .map_err(|_| anyhow::anyhow!("Codex app-server operation timed out"))?
    }
    async fn establish(
        &mut self,
        cwd: &str,
        session: Option<&str>,
        model: Option<&ModelInfo>,
        mcp: &[Value],
    ) -> anyhow::Result<String> {
        anyhow::ensure!(!self.closed, "Codex client is closed");
        anyhow::ensure!(
            mcp.is_empty(),
            "Codex app-server does not support per-session MCP server configuration"
        );
        if let Some(mut old) = self.process.take() {
            old.kill().await;
        }
        self.session = None;
        self.descriptor = None;
        let cwd = std::fs::canonicalize(cwd)?;
        let cwd_text = cwd
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Codex working directory is not UTF-8"))?;
        let config = CodexProcessConfig {
            command: self.spec.command.clone(),
            args: self.spec.args.clone(),
            clear_env: true,
            allowed_env: self.spec.allowed_env.clone(),
            env: vec![],
        };
        self.process = Some(CodexProcess::spawn_at(&config, Some(&cwd)).await?);
        let result = async {
            self.operation("initialize", json!({"clientInfo":{"name":"cowboy","title":"Cowboy","version":env!("CARGO_PKG_VERSION")},"capabilities":null})).await?;
            self.send(&json!({"method":"initialized"})).await?;
            let mut params = json!({"cwd":cwd_text,"approvalPolicy":"on-request","approvalsReviewer":"user","sandbox":"workspace-write"});
            if let Some(model) = model {
                params["model"] = json!(model.id);
                if let Some(provider) = &model.provider { params["modelProvider"] = json!(provider); }
            }
            let method = if let Some(id) = session { params["threadId"] = json!(id); "thread/resume" } else { "thread/start" };
            let result = self.operation(method, params).await?;
            anyhow::ensure!(matches!(result.pointer("/sandbox/type").and_then(Value::as_str), Some("workspaceWrite" | "readOnly")), "Codex did not enforce a restricted sandbox");
            anyhow::ensure!(result.get("approvalPolicy").and_then(Value::as_str) == Some("on-request"), "Codex did not enforce the requested approval policy");
            anyhow::ensure!(result.get("approvalsReviewer").and_then(Value::as_str) == Some("user"), "Codex did not route approvals to Cowboy");
            let id = result.pointer("/thread/id").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("Codex thread response lacks an id"))?.to_owned();
            if let Some(expected) = session { anyhow::ensure!(id == expected, "Codex resumed a different thread"); }
            self.descriptor = Some(AgentSessionDescriptor { model: result.get("model").and_then(Value::as_str).map(str::to_owned), context: None, reasoning: result.get("reasoningEffort").and_then(Value::as_str).map(str::to_owned) });
            self.session = Some(id.clone());
            Ok(id)
        }.await;
        if result.is_err()
            && let Some(mut process) = self.process.take()
        {
            process.kill().await;
        }
        result
    }
}

#[async_trait]
impl Client for CodexClient {
    fn is_connected(&self) -> bool {
        self.process.is_some() && self.session.is_some()
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
    ) -> anyhow::Result<String> {
        self.establish(cwd, None, model, mcp).await
    }
    async fn load_session(
        &mut self,
        session: &str,
        cwd: &str,
        mcp: &[Value],
    ) -> anyhow::Result<Vec<Event>> {
        let model = self.spec.model.clone();
        self.establish(cwd, Some(session), model.as_ref(), mcp)
            .await?;
        Ok(vec![])
    }
    async fn prompt(
        &mut self,
        session: &str,
        content: Vec<PromptContent>,
        mut cancellation: PromptTurnCancellation,
        handler: &mut (dyn FnMut(Event) + Send),
    ) -> anyhow::Result<StopReason> {
        anyhow::ensure!(
            self.session.as_deref() == Some(session),
            "Codex session does not match"
        );
        anyhow::ensure!(
            content.iter().all(|c| c.content_type == "text"),
            "Codex backend supports only text prompts"
        );
        if cancellation.try_cancelled() {
            return Ok(StopReason::Cancelled);
        }
        let input: Vec<_> = content
            .into_iter()
            .map(|c| json!({"type":"text","text":c.text,"text_elements":[]}))
            .collect();
        let result = self
            .run_turn(session, input, &mut cancellation, handler)
            .await;
        if result.is_err() {
            if let Some(mut p) = self.process.take() {
                p.kill().await;
            }
            self.session = None;
            self.descriptor = None;
        }
        result
    }
    async fn close(&mut self) -> anyhow::Result<()> {
        self.closed = true;
        self.session = None;
        self.descriptor = None;
        if let Some(mut p) = self.process.take() {
            p.kill().await;
        }
        Ok(())
    }
}

impl CodexClient {
    async fn run_turn(
        &mut self,
        session: &str,
        input: Vec<Value>,
        cancellation: &mut PromptTurnCancellation,
        handler: &mut (dyn FnMut(Event) + Send),
    ) -> anyhow::Result<StopReason> {
        let start_id = self
            .request("turn/start", json!({"threadId":session,"input":input}))
            .await?;
        let mut turn_id: Option<String> = None;
        let mut pending = Vec::new();
        let mut cancelled = false;
        let mut interrupt_id = None;
        let mut deadline = tokio::time::Instant::now()
            + Duration::from_secs(self.spec.watchdog.response_timeout_seconds.max(1));
        loop {
            tokio::select! {
                _ = cancellation.cancelled(), if !cancelled => {
                    cancelled = true;
                    deadline = tokio::time::Instant::now() + Duration::from_secs(self.spec.watchdog.cancel_timeout_seconds.max(1));
                    if let Some(turn) = &turn_id { interrupt_id = Some(self.request("turn/interrupt", json!({"threadId":session,"turnId":turn})).await?); }
                }
                _ = tokio::time::sleep_until(deadline) => { anyhow::bail!("Codex turn {} timed out", if cancelled {"cancellation"} else {"response"}); }
                msg = self.recv() => {
                    let msg = msg?;
                    if self.server_request(&msg).await? { continue; }
                    if msg.get("id").and_then(Value::as_u64) == Some(start_id) {
                        anyhow::ensure!(msg.get("error").is_none(), "Codex turn/start failed (code {})", msg.pointer("/error/code").and_then(Value::as_i64).unwrap_or(0));
                        let id = msg.pointer("/result/turn/id").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("Codex turn/start response lacks an id"))?.to_owned();
                        turn_id = Some(id.clone());
                        if cancelled { interrupt_id = Some(self.request("turn/interrupt", json!({"threadId":session,"turnId":id})).await?); }
                        for event in pending.drain(..) {
                            if let Some(stop) = project(&event, session, &id, handler)? { return Ok(stop); }
                        }
                    } else if interrupt_id.is_some() && msg.get("id").and_then(Value::as_u64) == interrupt_id {
                        anyhow::ensure!(msg.get("error").is_none(), "Codex turn/interrupt failed");
                    } else if msg.get("method").is_some() {
                        if let Some(id) = &turn_id {
                            if let Some(stop) = project(&msg, session, id, handler)? { return Ok(stop); }
                        } else { pending.push(msg); }
                    }
                    if !cancelled { deadline = tokio::time::Instant::now() + Duration::from_secs(self.spec.watchdog.response_timeout_seconds.max(1)); }
                }
            }
        }
    }
}

fn project(
    msg: &Value,
    session: &str,
    turn: &str,
    handler: &mut (dyn FnMut(Event) + Send),
) -> anyhow::Result<Option<StopReason>> {
    let p = &msg["params"];
    if p.get("threadId").and_then(Value::as_str) != Some(session)
        || p.get("turnId")
            .and_then(Value::as_str)
            .or_else(|| p.pointer("/turn/id").and_then(Value::as_str))
            != Some(turn)
    {
        return Ok(None);
    }
    match msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "item/agentMessage/delta" => {
            if let Some(text) = p.get("delta").and_then(Value::as_str) {
                handler(Event::MessageChunk {
                    content: json!({"type":"text","text":text}),
                });
            }
        }
        "item/reasoning/summaryTextDelta" => {
            if let Some(text) = p.get("delta").and_then(Value::as_str) {
                handler(Event::ThoughtChunk {
                    content: json!({"type":"text","text":text}),
                });
            }
        }
        "item/started" | "item/completed" => {
            let item = &p["item"];
            let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
            if matches!(
                kind,
                "commandExecution"
                    | "fileChange"
                    | "mcpToolCall"
                    | "dynamicToolCall"
                    | "collabAgentToolCall"
            ) && let Some(id) = item.get("id").and_then(Value::as_str)
            {
                let done = msg["method"] == "item/completed";
                let failed = matches!(
                    item.get("status").and_then(Value::as_str),
                    Some("failed" | "declined")
                );
                let status = if failed {
                    "failed"
                } else if done {
                    "completed"
                } else {
                    "in_progress"
                };
                if done {
                    handler(Event::ToolCallUpdate {
                        tool_call_id: id.into(),
                        status: status.into(),
                        content: item
                            .get("aggregatedOutput")
                            .and_then(Value::as_str)
                            .map(|s| json!({"type":"text","text":s})),
                    });
                } else {
                    handler(Event::ToolCall {
                        tool_call_id: id.into(),
                        title: item
                            .get("command")
                            .or_else(|| item.get("tool"))
                            .and_then(Value::as_str)
                            .unwrap_or(kind)
                            .into(),
                        kind: kind.into(),
                        status: status.into(),
                    });
                }
            }
        }
        "item/commandExecution/outputDelta" => {
            if let (Some(id), Some(delta)) = (
                p.get("itemId").and_then(Value::as_str),
                p.get("delta").and_then(Value::as_str),
            ) {
                handler(Event::ToolCallUpdate {
                    tool_call_id: id.into(),
                    status: "in_progress".into(),
                    content: Some(json!({"type":"text","text":delta})),
                });
            }
        }
        "turn/completed" => {
            return match p.pointer("/turn/status").and_then(Value::as_str) {
                Some("completed") => Ok(Some(StopReason::EndTurn)),
                Some("interrupted") => Ok(Some(StopReason::Cancelled)),
                Some("failed") => Err(anyhow::anyhow!("Codex turn failed")),
                _ => Err(anyhow::anyhow!(
                    "Codex turn completed with an unknown status"
                )),
            };
        }
        _ => {}
    }
    Ok(None)
}
