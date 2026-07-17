use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use cowboy_agent_client::{Client, Event, ModelInfo, PromptContent, StopReason};
use cowboy_workflow_core::{
    AbortAgentPromptWindowOutcome, AgentAction, AgentPromptWindow,
    CompareAndSealPromptWindowOutcome, ExecutionContext, OpenAgentPromptWindowOutcome,
    RoleDefinition, RoleId, RoleSession, RunId, RunStore, StepDetail, StepInput, StepRecord,
    TurnRecord, WorkflowError, ordered_user_inputs_from_parts,
};
use tokio::sync::Mutex;

use crate::frontmatter::parse_frontmatter_output;
use crate::prompt::{build_agent_prompt, build_correction_prompt};
use crate::{Error, Result};

pub type ProgressSink = Arc<dyn Fn(AgentProgress) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProgress {
    pub run_id: RunId,
    pub step_id: String,
    pub kind: AgentProgressKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentProgressKind {
    SessionReady {
        role: String,
        session_id: String,
    },
    Prompt {
        role: String,
        session_id: String,
        prompt: String,
    },
    Response {
        content: String,
    },
    Thought {
        content: String,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        tool_kind: String,
        status: String,
    },
    ToolCallUpdate {
        tool_call_id: String,
        title: String,
        status: String,
        content: Option<serde_json::Value>,
    },
    Plan {
        entries: Vec<serde_json::Value>,
    },
    PromptWindowOpened {
        role: String,
        window_id: String,
    },
    PromptWindowClosed {
        role: String,
        window_id: String,
    },
}

#[cfg(feature = "test-support")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptWindowHandoffPoint {
    BeforeCompareAndSeal {
        run_id: RunId,
        window_id: String,
        applied_sequence: u64,
    },
    AfterCompareAndSeal {
        run_id: RunId,
        window_id: String,
        applied_sequence: u64,
        pending: bool,
    },
}

#[cfg(feature = "test-support")]
#[async_trait]
pub trait PromptWindowHandoffObserver: Send + Sync {
    async fn observe(&self, point: PromptWindowHandoffPoint);
}

/// Configuration shared by agent client sessions created for workflow roles.
#[derive(Clone)]
pub struct AgentExecutionConfig {
    /// Working directory passed to new backend sessions.
    pub cwd: String,
    /// MCP server configuration passed to new backend sessions.
    pub mcp_servers: Vec<serde_json::Value>,
    /// Optional progress sink for streaming UI-visible agent/tool updates.
    pub progress: Option<ProgressSink>,
    #[cfg(feature = "test-support")]
    /// Optional observer used by deterministic handoff-boundary tests.
    pub handoff_observer: Option<Arc<dyn PromptWindowHandoffObserver>>,
}

impl fmt::Debug for AgentExecutionConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("AgentExecutionConfig");
        debug
            .field("cwd", &self.cwd)
            .field("mcp_servers", &self.mcp_servers)
            .field("progress", &self.progress.as_ref().map(|_| "<sink>"));
        #[cfg(feature = "test-support")]
        debug.field(
            "handoff_observer",
            &self.handoff_observer.as_ref().map(|_| "<observer>"),
        );
        debug.finish()
    }
}

impl Default for AgentExecutionConfig {
    fn default() -> Self {
        Self {
            cwd: ".".to_string(),
            mcp_servers: Vec::new(),
            progress: None,
            #[cfg(feature = "test-support")]
            handoff_observer: None,
        }
    }
}

/// Result of a completed agent action before it is returned to the core runner.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentExecution {
    /// Durable step record produced by the agent action.
    pub record: StepRecord,
    /// Turn records captured while the backend streamed output.
    pub turns: Vec<TurnRecord>,
}

/// Client plus backend metadata resolved for a workflow role.
pub struct ResolvedAgentClient {
    pub client: Box<dyn Client>,
    pub model: ModelInfo,
    pub backend: String,
}

struct ActiveClient {
    client: Box<dyn Client>,
    model: ModelInfo,
    backend: String,
}

/// Factory that creates backend clients for role sessions.
#[async_trait]
pub trait ClientFactory: Send + Sync {
    /// Resolve and create a fresh backend client for `role`.
    async fn create_client(&self, role: &RoleDefinition) -> Result<ResolvedAgentClient>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RoleSessionKey {
    run_id: RunId,
    role_id: RoleId,
}

/// Agent action executor with per-`(run_id, role_id)` session reuse.
pub struct AgentExecutor<F, S> {
    factory: F,
    store: Arc<S>,
    config: AgentExecutionConfig,
    clients: Arc<Mutex<HashMap<RoleSessionKey, ActiveClient>>>,
}

struct PromptWindowGuard<S: RunStore> {
    store: Arc<S>,
    context: ExecutionContext,
    role: String,
    window_id: String,
    progress: Option<ProgressSink>,
    closed: bool,
}

impl<S: RunStore> PromptWindowGuard<S> {
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        emit_progress_kind(
            self.progress.as_ref(),
            &self.context,
            AgentProgressKind::PromptWindowClosed {
                role: self.role.clone(),
                window_id: self.window_id.clone(),
            },
        );
    }
}

impl<S: RunStore> Drop for PromptWindowGuard<S> {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        match self.store.abort_agent_prompt_window(
            &self.context.run_id,
            &self.window_id,
            Utc::now(),
        ) {
            Ok(
                AbortAgentPromptWindowOutcome::Aborted(_)
                | AbortAgentPromptWindowOutcome::NoWindow
                | AbortAgentPromptWindowOutcome::StaleWindow
                | AbortAgentPromptWindowOutcome::MissingRun,
            ) => {}
            Err(err) => tracing::warn!(
                run_id = %self.context.run_id,
                window_id = %self.window_id,
                error = %err,
                "failed to abort agent prompt window"
            ),
        }
        self.close();
    }
}

impl<F, S> AgentExecutor<F, S> {
    /// Create a new executor backed by a client factory and run store.
    pub fn new(factory: F, store: S, config: AgentExecutionConfig) -> Self {
        Self {
            factory,
            store: Arc::new(store),
            config,
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<F, S> AgentExecutor<F, S>
where
    F: ClientFactory,
    S: RunStore + 'static,
{
    /// Execute an agent action using the session keyed by `(run_id, role_id)`.
    pub async fn execute_agent(
        &self,
        action: AgentAction,
        context: ExecutionContext,
    ) -> Result<AgentExecution> {
        let key = RoleSessionKey {
            run_id: context.run_id.clone(),
            role_id: action.role.clone(),
        };
        let role = context.role.as_ref().ok_or_else(|| {
            WorkflowError::InvalidAction(format!(
                "agent role {:?} is missing metadata",
                action.role
            ))
        })?;
        tracing::debug!(
            run_id = %context.run_id,
            step = %context.step_id,
            role = %action.role,
            agent = ?role.agent,
            "agent step: starting"
        );
        if !self.clients.lock().await.contains_key(&key) {
            tracing::debug!(run_id = %key.run_id, role = %key.role_id, agent = ?role.agent, "agent client missing; creating");
            let resolved = self.factory.create_client(role).await?;
            self.clients
                .lock()
                .await
                .entry(key.clone())
                .or_insert(ActiveClient {
                    client: resolved.client,
                    model: resolved.model,
                    backend: resolved.backend,
                });
        }

        let started_at = Utc::now();
        let start = Instant::now();
        let user_inputs = ordered_user_inputs_from_parts(
            &context.original_request,
            context.run_created_at,
            &context.user_prompts,
        );
        let base_prompt = build_agent_prompt(role, &action, &user_inputs);
        let prompt = if context.attempt > 1 {
            format!(
                "{base_prompt}\n\n{}",
                crate::prompt::build_retry_nudge(&action, context.retry_reason.as_deref())
            )
        } else {
            base_prompt
        };
        let mut clients = self.clients.lock().await;
        let active = clients
            .get_mut(&key)
            .ok_or_else(|| Error::MissingClient(key.role_id.clone()))?;
        let session_id = self.ensure_session(active, &key).await?;
        emit_progress_kind(
            self.config.progress.as_ref(),
            &context,
            AgentProgressKind::SessionReady {
                role: action.role.clone(),
                session_id: session_id.clone(),
            },
        );

        let window_id = format!("prompt-window-{}", uuid::Uuid::new_v4());
        let expected_baseline = context
            .user_prompts
            .last()
            .map(|prompt| prompt.sequence)
            .unwrap_or(0);
        let opened = self
            .store
            .open_agent_prompt_window(AgentPromptWindow {
                window_id: window_id.clone(),
                run_id: context.run_id.clone(),
                step_record_id: context.step_record_id.clone(),
                step_id: context.step_id.clone(),
                role_id: action.role.clone(),
                baseline_sequence: expected_baseline,
                applied_sequence: expected_baseline,
                opened_at: Utc::now(),
                sealed_at: None,
            })
            .map_err(Error::from)?;
        let OpenAgentPromptWindowOutcome::Opened(window) = opened else {
            return Err(WorkflowError::InvalidAction(format!(
                "cannot open agent prompt window for run {:?}: {opened:?}",
                context.run_id
            ))
            .into());
        };
        if window.baseline_sequence != expected_baseline {
            let _ = self
                .store
                .abort_agent_prompt_window(&context.run_id, &window_id, Utc::now());
            return Err(WorkflowError::InvalidAction(format!(
                "agent prompt baseline changed from {expected_baseline} to {} before opening",
                window.baseline_sequence
            ))
            .into());
        }
        emit_progress_kind(
            self.config.progress.as_ref(),
            &context,
            AgentProgressKind::PromptWindowOpened {
                role: action.role.clone(),
                window_id: window_id.clone(),
            },
        );
        let mut window_guard = PromptWindowGuard {
            store: self.store.clone(),
            context: context.clone(),
            role: action.role.clone(),
            window_id: window_id.clone(),
            progress: self.config.progress.clone(),
            closed: false,
        };

        emit_progress_kind(
            self.config.progress.as_ref(),
            &context,
            AgentProgressKind::Prompt {
                role: action.role.clone(),
                session_id: session_id.clone(),
                prompt: prompt.clone(),
            },
        );
        let mut turn_cursor = TurnCursor::default();
        let (mut visible, mut turns, stop_reason) = run_prompt_turn(
            active.client.as_mut(),
            &session_id,
            vec![PromptContent::text(prompt.clone())],
            &context,
            self.config.progress.clone(),
            &mut turn_cursor,
        )
        .await?;
        tracing::debug!(run_id = %context.run_id, step = %context.step_id, session_id = %session_id, stop_reason = ?stop_reason, reply_chars = visible.chars().count(), "agent step: initial reply");

        let mut applied_sequence = expected_baseline;
        let mut correction_turns = Vec::new();
        loop {
            #[cfg(feature = "test-support")]
            if let Some(observer) = &self.config.handoff_observer {
                observer
                    .observe(PromptWindowHandoffPoint::BeforeCompareAndSeal {
                        run_id: context.run_id.clone(),
                        window_id: window_id.clone(),
                        applied_sequence,
                    })
                    .await;
            }
            let outcome = self
                .store
                .compare_and_seal_agent_prompt_window(
                    &context.run_id,
                    &window_id,
                    applied_sequence,
                    Utc::now(),
                )
                .map_err(Error::from)?;
            #[cfg(feature = "test-support")]
            if let Some(observer) = &self.config.handoff_observer {
                observer
                    .observe(PromptWindowHandoffPoint::AfterCompareAndSeal {
                        run_id: context.run_id.clone(),
                        window_id: window_id.clone(),
                        applied_sequence,
                        pending: matches!(
                            &outcome,
                            CompareAndSealPromptWindowOutcome::Pending { .. }
                        ),
                    })
                    .await;
            }
            match outcome {
                CompareAndSealPromptWindowOutcome::Pending { prompts, .. } => {
                    let sequences = prompts
                        .iter()
                        .map(|prompt| prompt.sequence)
                        .collect::<Vec<_>>();
                    let blocks = build_correction_prompt(&action, &prompts);
                    correction_turns.push(serde_json::json!({
                        "window_id": window_id,
                        "role": action.role,
                        "applied_sequences": sequences,
                        "content": blocks,
                    }));
                    applied_sequence = prompts
                        .last()
                        .expect("pending prompt batch is nonempty")
                        .sequence;
                    let rendered = blocks
                        .iter()
                        .map(|block| block.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    emit_progress_kind(
                        self.config.progress.as_ref(),
                        &context,
                        AgentProgressKind::Prompt {
                            role: action.role.clone(),
                            session_id: session_id.clone(),
                            prompt: rendered,
                        },
                    );
                    let (replacement, correction_records, stop_reason) = run_prompt_turn(
                        active.client.as_mut(),
                        &session_id,
                        blocks,
                        &context,
                        self.config.progress.clone(),
                        &mut turn_cursor,
                    )
                    .await?;
                    visible = replacement;
                    turns.extend(correction_records);
                    tracing::debug!(run_id = %context.run_id, step = %context.step_id, session_id = %session_id, applied_sequence, stop_reason = ?stop_reason, reply_chars = visible.chars().count(), "agent step: correction reply");
                }
                CompareAndSealPromptWindowOutcome::Sealed(_) => {
                    window_guard.close();
                    break;
                }
                outcome => {
                    return Err(WorkflowError::InvalidAction(format!(
                        "agent prompt window handoff failed for run {:?}: {outcome:?}",
                        context.run_id
                    ))
                    .into());
                }
            }
        }

        let parsed = parse_frontmatter_output(&visible).inspect_err(|_err| {
            tracing::error!(
                run_id = %context.run_id,
                step = %context.step_id,
                reply = %visible,
                "agent step: failed to parse frontmatter output"
            );
        })?;
        let completed_at = Utc::now();
        let record = StepRecord {
            id: context.step_record_id,
            prev: context.prev,
            step: context.step_id,
            action: "agent".to_string(),
            input: StepInput {
                prompt: Some(prompt),
                context: serde_json::json!({
                    "role": action.role,
                    "user_inputs": user_inputs,
                    "correction_turns": correction_turns,
                    "final_applied_sequence": applied_sequence,
                }),
            },
            output: Some(parsed.output),
            detail: StepDetail {
                backend: Some(active.backend.clone()),
                session_id: Some(session_id),
                duration_ms: start.elapsed().as_millis() as u64,
                turn_count: turns.len() as u32,
                usage: None,
            },
            started_at,
            completed_at: Some(completed_at),
        };
        Ok(AgentExecution { record, turns })
    }

    async fn ensure_session(
        &self,
        active: &mut ActiveClient,
        key: &RoleSessionKey,
    ) -> Result<String> {
        let client = active.client.as_mut();
        if let Some(session_id) = client.session_id() {
            tracing::debug!(run_id = %key.run_id, role = %key.role_id, session_id, "agent session already active");
            return Ok(session_id.to_string());
        }

        if let Some(saved) = self
            .store
            .load_role_session(&key.run_id, &key.role_id)
            .map_err(Error::from)?
        {
            if client.supports_load_session() {
                tracing::debug!(
                    run_id = %key.run_id,
                    role = %key.role_id,
                    session_id = %saved.session_id,
                    "agent session: loading saved backend session"
                );
                match client
                    .load_session(
                        &saved.session_id,
                        &self.config.cwd,
                        &self.config.mcp_servers,
                    )
                    .await
                {
                    Ok(history) => {
                        tracing::info!(
                            run_id = %key.run_id,
                            role = %key.role_id,
                            session_id = %saved.session_id,
                            history_events = history.len(),
                            "agent session loaded"
                        );
                        return Ok(saved.session_id);
                    }
                    Err(err) => {
                        tracing::warn!(
                            run_id = %key.run_id,
                            role = %key.role_id,
                            session_id = %saved.session_id,
                            error = %err,
                            "agent session load failed; creating a new session"
                        );
                    }
                }
            } else {
                tracing::debug!(run_id = %key.run_id, role = %key.role_id, "agent backend cannot load saved sessions");
            }
        }

        tracing::debug!(
            run_id = %key.run_id,
            role = %key.role_id,
            cwd = %self.config.cwd,
            model_id = %active.model.id,
            provider = ?active.model.provider,
            "agent session: creating new backend session"
        );
        let session_id = client
            .new_session(&self.config.cwd, &self.config.mcp_servers, &active.model)
            .await?;
        self.store
            .save_role_session(RoleSession {
                run_id: key.run_id.clone(),
                role_id: key.role_id.clone(),
                backend: active.backend.clone(),
                session_id: session_id.clone(),
                updated_at: Utc::now(),
            })
            .map_err(Error::from)?;
        tracing::info!(
            run_id = %key.run_id,
            role = %key.role_id,
            session_id = %session_id,
            backend = %active.backend,
            "agent session saved"
        );
        Ok(session_id)
    }
}

#[derive(Default)]
struct TurnCursor {
    next_index: usize,
    prev: Option<String>,
}

fn push_turn(
    context: &ExecutionContext,
    cursor: &mut TurnCursor,
    turns: &mut Vec<TurnRecord>,
    role: &str,
    content: String,
) {
    cursor.next_index += 1;
    let id = format!("{}-turn-{}", context.step_record_id, cursor.next_index);
    turns.push(TurnRecord {
        id: id.clone(),
        step_id: context.step_record_id.clone(),
        role: role.to_string(),
        content,
        timestamp: Utc::now(),
        prev: cursor.prev.replace(id),
    });
}

async fn run_prompt_turn(
    client: &mut dyn Client,
    session_id: &str,
    content: Vec<PromptContent>,
    context: &ExecutionContext,
    progress: Option<ProgressSink>,
    turn_cursor: &mut TurnCursor,
) -> Result<(String, Vec<TurnRecord>, StopReason)> {
    let mut visible = String::new();
    let mut turns = Vec::new();
    let mut tool_titles = HashMap::new();
    let stop_reason = client
        .prompt(session_id, content, &mut |event| {
            collect_event(
                context,
                event,
                &progress,
                &mut tool_titles,
                &mut visible,
                &mut turns,
                turn_cursor,
            )
        })
        .await?;
    Ok((visible, turns, stop_reason))
}

fn emit_progress_kind(
    progress: Option<&ProgressSink>,
    context: &ExecutionContext,
    kind: AgentProgressKind,
) {
    if let Some(progress) = progress {
        progress(AgentProgress {
            run_id: context.run_id.clone(),
            step_id: context.step_id.clone(),
            kind,
        });
    }
}

fn collect_event(
    context: &ExecutionContext,
    event: Event,
    progress: &Option<ProgressSink>,
    tool_titles: &mut HashMap<String, String>,
    visible: &mut String,
    turns: &mut Vec<TurnRecord>,
    turn_cursor: &mut TurnCursor,
) {
    tracing::trace!(
        run_id = %context.run_id,
        step = %context.step_id,
        record_id = %context.step_record_id,
        event = ?event,
        "agent event received"
    );
    match event {
        Event::MessageChunk { content } => {
            if let Some(text) = display_content_text(&content) {
                tracing::debug!(
                    run_id = %context.run_id,
                    step = %context.step_id,
                    chunk_chars = text.chars().count(),
                    "agent message chunk collected"
                );
                visible.push_str(&text);
                emit_progress_kind(
                    progress.as_ref(),
                    context,
                    AgentProgressKind::Response {
                        content: text.clone(),
                    },
                );
                push_turn(context, turn_cursor, turns, "assistant", text);
            }
        }
        Event::ThoughtChunk { content } => {
            if let Some(text) = display_content_text(&content) {
                tracing::debug!(
                    run_id = %context.run_id,
                    step = %context.step_id,
                    chunk_chars = text.chars().count(),
                    "agent thought chunk collected"
                );
                emit_progress_kind(
                    progress.as_ref(),
                    context,
                    AgentProgressKind::Thought {
                        content: text.clone(),
                    },
                );
                push_turn(context, turn_cursor, turns, "thought", text);
            }
        }
        Event::ToolCall {
            tool_call_id,
            title,
            kind,
            status,
        } => {
            tracing::debug!(
                run_id = %context.run_id,
                step = %context.step_id,
                kind = %kind,
                status = %status,
                title = %title,
                "agent tool call collected"
            );
            if !tool_call_id.is_empty() && !title.is_empty() {
                tool_titles.insert(tool_call_id.clone(), title.clone());
            }
            emit_progress_kind(
                progress.as_ref(),
                context,
                AgentProgressKind::ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    title: title.clone(),
                    tool_kind: kind,
                    status,
                },
            );
            push_turn(context, turn_cursor, turns, "tool", title);
        }
        Event::ToolCallUpdate {
            tool_call_id,
            status,
            content,
        } => {
            let title = tool_titles
                .get(&tool_call_id)
                .cloned()
                .unwrap_or_else(unknown_tool_title);
            emit_progress_kind(
                progress.as_ref(),
                context,
                AgentProgressKind::ToolCallUpdate {
                    tool_call_id,
                    title,
                    status,
                    content,
                },
            );
        }
        Event::Plan { entries } => {
            if !entries.is_empty() {
                emit_progress_kind(
                    progress.as_ref(),
                    context,
                    AgentProgressKind::Plan { entries },
                );
            }
        }
        Event::UserMessageChunk { .. } | Event::Unknown { .. } => {}
    }
}

fn display_content_text(content: &serde_json::Value) -> Option<String> {
    extract_json_text(content)
}

fn extract_json_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => non_empty(text.clone()),
        serde_json::Value::Array(items) => join_text(items.iter().filter_map(extract_json_text)),
        serde_json::Value::Object(object) => ["text", "content", "message", "output"]
            .into_iter()
            .filter_map(|key| object.get(key))
            .find_map(extract_json_text),
        _ => None,
    }
}

fn join_text(parts: impl Iterator<Item = String>) -> Option<String> {
    let mut joined = String::new();
    for part in parts {
        joined.push_str(&part);
    }
    non_empty(joined)
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn unknown_tool_title() -> String {
    "<unknown tool>".to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};

    use anyhow::anyhow;
    use cowboy_agent_client::{AgentInfo, StopReason};
    use cowboy_workflow_core::{
        AppendUserPromptOutcome, ObjectHash, ObjectKind, Result as CoreResult, RunHead,
        RunUserPrompt, WorkflowRun,
    };
    use parking_lot::Mutex as SyncMutex;
    use serde::{Serialize, de::DeserializeOwned};

    use super::*;

    #[derive(Debug)]
    struct FakeClient {
        session_id: Option<String>,
        events: SyncMutex<VecDeque<Event>>,
        supports_load: bool,
        new_sessions: usize,
        loaded_sessions: Vec<String>,
        new_session_models: Arc<SyncMutex<Vec<ModelInfo>>>,
        prompt_calls: Arc<SyncMutex<Vec<Vec<PromptContent>>>>,
    }

    impl FakeClient {
        fn new(events: Vec<Event>) -> Self {
            Self {
                session_id: None,
                events: SyncMutex::new(events.into()),
                supports_load: false,
                new_sessions: 0,
                loaded_sessions: Vec::new(),
                new_session_models: Arc::new(SyncMutex::new(Vec::new())),
                prompt_calls: Arc::new(SyncMutex::new(Vec::new())),
            }
        }

        fn with_load(events: Vec<Event>) -> Self {
            Self {
                supports_load: true,
                ..Self::new(events)
            }
        }
    }

    #[async_trait]
    impl Client for FakeClient {
        fn is_connected(&self) -> bool {
            true
        }

        fn agent_info(&self) -> Option<&AgentInfo> {
            None
        }

        fn session_id(&self) -> Option<&str> {
            self.session_id.as_deref()
        }

        async fn new_session(
            &mut self,
            _cwd: &str,
            _mcp_servers: &[serde_json::Value],
            model: &ModelInfo,
        ) -> anyhow::Result<String> {
            self.new_session_models.lock().push(model.clone());
            self.new_sessions += 1;
            let session_id = format!("session-{}", self.new_sessions);
            self.session_id = Some(session_id.clone());
            Ok(session_id)
        }

        fn supports_load_session(&self) -> bool {
            self.supports_load
        }

        async fn load_session(
            &mut self,
            session_id: &str,
            _cwd: &str,
            _mcp_servers: &[serde_json::Value],
        ) -> anyhow::Result<Vec<Event>> {
            if !self.supports_load {
                return Err(anyhow!("unsupported"));
            }
            self.loaded_sessions.push(session_id.to_string());
            self.session_id = Some(session_id.to_string());
            Ok(Vec::new())
        }

        async fn prompt(
            &mut self,
            _session_id: &str,
            prompt_content: Vec<PromptContent>,
            event_handler: &mut (dyn FnMut(Event) + Send),
        ) -> anyhow::Result<StopReason> {
            self.prompt_calls.lock().push(prompt_content);
            while let Some(event) = self.events.lock().pop_front() {
                let completes_reply = matches!(event, Event::MessageChunk { .. });
                event_handler(event);
                if completes_reply {
                    break;
                }
            }
            Ok(StopReason::EndTurn)
        }

        async fn close(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeFactory {
        clients: SyncMutex<VecDeque<FakeClient>>,
        created_for_roles: SyncMutex<Vec<RoleDefinition>>,
        model: ModelInfo,
    }

    impl FakeFactory {
        fn new(clients: Vec<FakeClient>) -> Self {
            Self {
                clients: SyncMutex::new(clients.into()),
                created_for_roles: SyncMutex::new(Vec::new()),
                model: ModelInfo {
                    id: "fake-model".to_string(),
                    provider: Some("fake-provider".to_string()),
                },
            }
        }
    }

    #[async_trait]
    impl ClientFactory for FakeFactory {
        async fn create_client(&self, role: &RoleDefinition) -> Result<ResolvedAgentClient> {
            self.created_for_roles.lock().push(role.clone());
            let client = self
                .clients
                .lock()
                .pop_front()
                .map(|client| Box::new(client) as Box<dyn Client>)
                .ok_or_else(|| Error::MissingClient(role.id.clone()))?;
            Ok(ResolvedAgentClient {
                client,
                model: self.model.clone(),
                backend: "fake-agent".to_string(),
            })
        }
    }

    #[derive(Default)]
    struct FakeStore {
        sessions: SyncMutex<HashMap<(String, String), RoleSession>>,
        accepted_prompts: SyncMutex<Vec<RunUserPrompt>>,
        window: SyncMutex<Option<AgentPromptWindow>>,
        pending_prompt_batches: SyncMutex<VecDeque<Vec<RunUserPrompt>>>,
    }

    impl cowboy_workflow_core::RunStore for FakeStore {
        fn save_run(&self, _run: &WorkflowRun) -> CoreResult<()> {
            Ok(())
        }

        fn load_run(&self, _run_id: &cowboy_workflow_core::RunId) -> CoreResult<WorkflowRun> {
            Err(WorkflowError::InvalidAction("unused".to_string()))
        }

        fn list_runs(&self) -> CoreResult<Vec<RunHead>> {
            Ok(Vec::new())
        }

        fn put_object<T: Serialize>(
            &self,
            _kind: ObjectKind,
            _value: &T,
        ) -> CoreResult<ObjectHash> {
            Ok("hash".to_string())
        }

        fn get_object<T: DeserializeOwned>(&self, _hash: &ObjectHash) -> CoreResult<T> {
            Err(WorkflowError::InvalidAction("unused".to_string()))
        }

        fn update_run_head(&self, _run_id: &str, _head: RunHead) -> CoreResult<()> {
            Ok(())
        }

        fn load_run_head(&self, _run_id: &str) -> CoreResult<RunHead> {
            Err(WorkflowError::InvalidAction("unused".to_string()))
        }

        fn save_role_session(&self, session: RoleSession) -> CoreResult<()> {
            self.sessions
                .lock()
                .insert((session.run_id.clone(), session.role_id.clone()), session);
            Ok(())
        }

        fn load_role_session(
            &self,
            run_id: &str,
            role_id: &str,
        ) -> CoreResult<Option<RoleSession>> {
            Ok(self
                .sessions
                .lock()
                .get(&(run_id.to_string(), role_id.to_string()))
                .cloned())
        }

        fn delete_role_sessions(&self, run_id: &str) -> CoreResult<()> {
            self.sessions
                .lock()
                .retain(|(stored_run, _), _| stored_run != run_id);
            Ok(())
        }

        fn append_turn(
            &self,
            _run_id: &str,
            _turn: cowboy_workflow_core::TurnRecord,
        ) -> CoreResult<ObjectHash> {
            Ok("turn".to_string())
        }

        fn load_user_prompts(&self, _run_id: &str) -> CoreResult<Vec<RunUserPrompt>> {
            Ok(self.accepted_prompts.lock().clone())
        }

        fn append_user_prompt(
            &self,
            _run_id: &str,
            window_id: &str,
            content: String,
        ) -> CoreResult<AppendUserPromptOutcome> {
            let window = self.window.lock();
            let Some(window) = window.as_ref() else {
                return Ok(AppendUserPromptOutcome::NoWindow);
            };
            if window.window_id != window_id {
                return Ok(AppendUserPromptOutcome::StaleWindow);
            }
            if !window.is_open() {
                return Ok(AppendUserPromptOutcome::SealedWindow);
            }
            let mut prompts = self.accepted_prompts.lock();
            let prompt = RunUserPrompt {
                sequence: prompts.last().map_or(1, |prompt| prompt.sequence + 1),
                content,
                submitted_at: Utc::now(),
            };
            prompts.push(prompt.clone());
            Ok(AppendUserPromptOutcome::Accepted(prompt))
        }

        fn open_agent_prompt_window(
            &self,
            window: AgentPromptWindow,
        ) -> CoreResult<OpenAgentPromptWindowOutcome> {
            *self.window.lock() = Some(window.clone());
            Ok(OpenAgentPromptWindowOutcome::Opened(window))
        }

        fn compare_and_seal_agent_prompt_window(
            &self,
            _run_id: &str,
            window_id: &str,
            applied_sequence: u64,
            sealed_at: chrono::DateTime<Utc>,
        ) -> CoreResult<CompareAndSealPromptWindowOutcome> {
            let mut window = self.window.lock();
            let Some(active) = window.as_mut() else {
                return Ok(CompareAndSealPromptWindowOutcome::NoWindow);
            };
            if active.window_id != window_id {
                return Ok(CompareAndSealPromptWindowOutcome::StaleWindow);
            }
            let pending = self
                .pending_prompt_batches
                .lock()
                .pop_front()
                .unwrap_or_default()
                .into_iter()
                .filter(|prompt| prompt.sequence > applied_sequence)
                .collect::<Vec<_>>();
            if !pending.is_empty() {
                active.applied_sequence = applied_sequence;
                return Ok(CompareAndSealPromptWindowOutcome::Pending {
                    window: active.clone(),
                    prompts: pending,
                });
            }
            active.applied_sequence = applied_sequence;
            active.sealed_at = Some(sealed_at);
            Ok(CompareAndSealPromptWindowOutcome::Sealed(active.clone()))
        }

        fn abort_agent_prompt_window(
            &self,
            _run_id: &str,
            window_id: &str,
            aborted_at: chrono::DateTime<Utc>,
        ) -> CoreResult<AbortAgentPromptWindowOutcome> {
            let mut window = self.window.lock();
            let Some(active) = window.as_mut() else {
                return Ok(AbortAgentPromptWindowOutcome::NoWindow);
            };
            if active.window_id != window_id {
                return Ok(AbortAgentPromptWindowOutcome::StaleWindow);
            }
            active.sealed_at = Some(aborted_at);
            Ok(AbortAgentPromptWindowOutcome::Aborted(active.clone()))
        }

        fn clear_agent_prompt_window(
            &self,
            _run_id: &str,
        ) -> CoreResult<Option<AgentPromptWindow>> {
            Ok(self.window.lock().take())
        }
    }

    fn event() -> Event {
        Event::MessageChunk {
            content: serde_json::json!({"text": "---\nstatus: success\nsummary: done\n---\nbody"}),
        }
    }

    fn action(role: &str) -> AgentAction {
        AgentAction {
            role: role.to_string(),
            prompt: "Do work".into(),
            output: None,
        }
    }

    fn role(id: &str) -> RoleDefinition {
        RoleDefinition {
            id: id.to_string(),
            instructions: format!("Instructions for {id}"),
            agent: Some("fake".to_string()),
            properties: serde_json::Value::Null,
        }
    }

    fn context(run_id: &str, record_id: &str) -> ExecutionContext {
        ExecutionContext {
            run_id: run_id.into(),
            step_id: "implement".into(),
            step_record_id: record_id.into(),
            prev: Some("prev".into()),
            role: Some(role("developer")),
            attempt: 1,
            retry_reason: None,
            original_request: "Original request".to_string(),
            run_created_at: Utc::now(),
            user_prompts: Vec::new(),
        }
    }

    fn context_with_role(run_id: &str, record_id: &str, role_id: &str) -> ExecutionContext {
        ExecutionContext {
            run_id: run_id.into(),
            step_id: "implement".into(),
            step_record_id: record_id.into(),
            prev: Some("prev".into()),
            role: Some(role(role_id)),
            attempt: 1,
            retry_reason: None,
            original_request: "Original request".to_string(),
            run_created_at: Utc::now(),
            user_prompts: Vec::new(),
        }
    }

    #[tokio::test]
    async fn retry_attempt_appends_corrective_frontmatter_nudge() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![event()])]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());

        let mut context = context("run", "record");
        context.attempt = 2;
        context.retry_reason = Some("missing frontmatter".to_string());

        let execution = executor
            .execute_agent(action("developer"), context)
            .await
            .unwrap();

        let prompt = execution.record.input.prompt.as_ref().unwrap();
        // The original task prompt is preserved and the corrective nudge appended.
        assert!(prompt.contains("Do work"));
        assert!(prompt.contains("## Retry"));
        assert!(prompt.contains("missing frontmatter"));
        assert!(prompt.contains("valid YAML frontmatter"));
    }

    #[tokio::test]
    async fn executes_agent_action_and_normalizes_output() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![event()])]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());

        let execution = executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap();

        let output = execution.record.output.unwrap();
        assert_eq!(output.status, "success");
        assert_eq!(output.fields["summary"], "done");
        assert_eq!(output.body, "body");
        let prompt = execution.record.input.prompt.as_ref().unwrap();
        assert!(prompt.contains("Do work"));
        assert!(prompt.contains("Instructions for developer"));
        assert_eq!(
            execution.record.detail.backend.as_deref(),
            Some("fake-agent")
        );
        assert_eq!(execution.turns.len(), 1);
    }

    #[tokio::test]
    async fn session_creation_uses_resolved_agent_model() {
        let client = FakeClient::new(vec![event()]);
        let observed_models = client.new_session_models.clone();
        let factory = FakeFactory::new(vec![client]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());

        executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap();

        let observed_models = observed_models.lock();
        assert_eq!(observed_models.len(), 1);
        assert_eq!(observed_models[0].id, executor.factory.model.id);
        assert_eq!(observed_models[0].provider, executor.factory.model.provider);
    }

    #[tokio::test]
    async fn string_message_chunk_updates_visible_reply_turns_and_progress() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![Event::MessageChunk {
            content: serde_json::json!("---\nstatus: success\nsummary: done\n---\nbody"),
        }])]);
        let store = FakeStore::default();
        let progress_events = Arc::new(SyncMutex::new(Vec::new()));
        let progress_sink = {
            let progress_events = progress_events.clone();
            Arc::new(move |progress: AgentProgress| {
                progress_events.lock().push(progress.kind);
            })
        };
        let executor = AgentExecutor::new(
            factory,
            store,
            AgentExecutionConfig {
                progress: Some(progress_sink),
                ..AgentExecutionConfig::default()
            },
        );

        let execution = executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap();

        let output = execution.record.output.unwrap();
        assert_eq!(output.status, "success");
        assert_eq!(output.fields["summary"], "done");
        assert_eq!(output.body, "body");
        assert_eq!(execution.turns.len(), 1);
        assert_eq!(
            execution.turns[0].content,
            "---\nstatus: success\nsummary: done\n---\nbody"
        );
        assert!(progress_events.lock().iter().any(|event| {
            matches!(
                event,
                AgentProgressKind::Response { content }
                    if content.contains("status: success") && content.ends_with("body")
            )
        }));
    }

    #[tokio::test]
    async fn progress_reports_prompt_and_agent_stream_chunks() {
        let events = vec![
            Event::ThoughtChunk {
                content: serde_json::json!({"text":"checking approach"}),
            },
            Event::ToolCall {
                tool_call_id: "call_abc".to_string(),
                title: "Reading app state".to_string(),
                kind: "read".to_string(),
                status: "pending".to_string(),
            },
            Event::ToolCallUpdate {
                tool_call_id: "call_abc".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"text":"read complete"})),
            },
            event(),
        ];
        let factory = FakeFactory::new(vec![FakeClient::new(events)]);
        let store = FakeStore::default();
        let progress_events = Arc::new(SyncMutex::new(Vec::new()));
        let progress_sink = {
            let progress_events = progress_events.clone();
            Arc::new(move |progress: AgentProgress| {
                progress_events.lock().push(progress.kind);
            })
        };
        let executor = AgentExecutor::new(
            factory,
            store,
            AgentExecutionConfig {
                progress: Some(progress_sink),
                ..AgentExecutionConfig::default()
            },
        );

        executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap();

        let progress_events = progress_events.lock();
        assert_eq!(progress_events.len(), 8);
        assert!(matches!(
            &progress_events[0],
            AgentProgressKind::SessionReady { role, session_id }
                if role == "developer" && session_id == "session-1"
        ));
        assert!(matches!(
            &progress_events[1],
            AgentProgressKind::PromptWindowOpened { role, .. } if role == "developer"
        ));
        assert!(matches!(
            &progress_events[2],
            AgentProgressKind::Prompt { role, session_id, prompt }
                if role == "developer" && session_id == "session-1" && prompt.contains("Do work")
        ));
        assert_eq!(
            progress_events[3],
            AgentProgressKind::Thought {
                content: "checking approach".to_string(),
            }
        );
        assert_eq!(
            progress_events[4],
            AgentProgressKind::ToolCall {
                tool_call_id: "call_abc".to_string(),
                title: "Reading app state".to_string(),
                tool_kind: "read".to_string(),
                status: "pending".to_string(),
            }
        );
        assert_eq!(
            progress_events[5],
            AgentProgressKind::ToolCallUpdate {
                tool_call_id: "call_abc".to_string(),
                title: "Reading app state".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"text":"read complete"})),
            }
        );
        assert!(matches!(
            &progress_events[6],
            AgentProgressKind::Response { content } if content.contains("status: success")
        ));
        assert!(matches!(
            &progress_events[7],
            AgentProgressKind::PromptWindowClosed { role, .. } if role == "developer"
        ));
    }

    #[tokio::test]
    async fn array_content_blocks_update_visible_reply_thoughts_and_progress() {
        let events = vec![
            Event::ThoughtChunk {
                content: serde_json::json!([
                    {"type": "text", "text": "thinking"},
                    {"type": "text", "text": " clearly"}
                ]),
            },
            Event::MessageChunk {
                content: serde_json::json!([
                    {"type": "text", "text": "---\nstatus: success\nsummary: done\n---\n"},
                    {"type": "text", "text": "body"}
                ]),
            },
        ];
        let factory = FakeFactory::new(vec![FakeClient::new(events)]);
        let store = FakeStore::default();
        let progress_events = Arc::new(SyncMutex::new(Vec::new()));
        let progress_sink = {
            let progress_events = progress_events.clone();
            Arc::new(move |progress: AgentProgress| {
                progress_events.lock().push(progress.kind);
            })
        };
        let executor = AgentExecutor::new(
            factory,
            store,
            AgentExecutionConfig {
                progress: Some(progress_sink),
                ..AgentExecutionConfig::default()
            },
        );

        let execution = executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap();

        assert_eq!(execution.record.output.unwrap().body, "body");
        let progress_events = progress_events.lock();
        assert!(progress_events.iter().any(|event| matches!(
            event,
            AgentProgressKind::Thought { content } if content == "thinking clearly"
        )));
        assert!(progress_events.iter().any(|event| matches!(
            event,
            AgentProgressKind::Response { content } if content.ends_with("body")
        )));
    }

    #[tokio::test]
    async fn progress_suppresses_user_message_and_unknown_housekeeping() {
        let events = vec![
            Event::UserMessageChunk {
                content: serde_json::json!({"text":"echoed user prompt"}),
            },
            Event::Unknown {
                session_update: "usage".to_string(),
                raw: serde_json::json!({"sessionUpdate":"usage"}),
            },
            event(),
        ];
        let factory = FakeFactory::new(vec![FakeClient::new(events)]);
        let store = FakeStore::default();
        let progress_events = Arc::new(SyncMutex::new(Vec::new()));
        let progress_sink = {
            let progress_events = progress_events.clone();
            Arc::new(move |progress: AgentProgress| {
                progress_events.lock().push(progress.kind);
            })
        };
        let executor = AgentExecutor::new(
            factory,
            store,
            AgentExecutionConfig {
                progress: Some(progress_sink),
                ..AgentExecutionConfig::default()
            },
        );

        executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap();

        let progress_events = progress_events.lock();
        assert_eq!(progress_events.len(), 5);
        assert!(matches!(
            &progress_events[0],
            AgentProgressKind::SessionReady { .. }
        ));
        assert!(matches!(
            &progress_events[1],
            AgentProgressKind::PromptWindowOpened { .. }
        ));
        assert!(matches!(
            &progress_events[2],
            AgentProgressKind::Prompt { .. }
        ));
        assert!(matches!(
            &progress_events[3],
            AgentProgressKind::Response { .. }
        ));
        assert!(matches!(
            &progress_events[4],
            AgentProgressKind::PromptWindowClosed { .. }
        ));
    }
    #[tokio::test]
    async fn progress_uses_tool_titles_for_updates() {
        let events = vec![
            Event::ToolCall {
                tool_call_id: "call_abc|opaque-base64-payload".to_string(),
                title: "Finding app tests".to_string(),
                kind: "search".to_string(),
                status: "pending".to_string(),
            },
            Event::ToolCallUpdate {
                tool_call_id: "call_abc|opaque-base64-payload".to_string(),
                status: "completed".to_string(),
                content: None,
            },
            event(),
        ];
        let factory = FakeFactory::new(vec![FakeClient::new(events)]);
        let store = FakeStore::default();
        let progress_events = Arc::new(SyncMutex::new(Vec::new()));
        let progress_sink = {
            let progress_events = progress_events.clone();
            Arc::new(move |progress: AgentProgress| {
                progress_events.lock().push(progress.kind);
            })
        };
        let executor = AgentExecutor::new(
            factory,
            store,
            AgentExecutionConfig {
                progress: Some(progress_sink),
                ..AgentExecutionConfig::default()
            },
        );

        executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap();

        let progress_events = progress_events.lock();
        assert!(progress_events.iter().any(|event| {
            matches!(
                event,
                AgentProgressKind::ToolCall {
                    tool_call_id,
                    title,
                    tool_kind,
                    status,
                } if tool_call_id == "call_abc|opaque-base64-payload"
                    && title == "Finding app tests"
                    && tool_kind == "search"
                    && status == "pending"
            )
        }));
        assert!(progress_events.iter().any(|event| {
            matches!(
                event,
                AgentProgressKind::ToolCallUpdate {
                    tool_call_id,
                    title,
                    status,
                    content,
                } if tool_call_id == "call_abc|opaque-base64-payload"
                    && title == "Finding app tests"
                    && status == "completed"
                    && content.is_none()
            )
        }));
    }
    #[tokio::test]
    async fn reuses_same_client_for_same_run_and_role() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![event(), event()])]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());

        executor
            .execute_agent(action("developer"), context("run", "record-1"))
            .await
            .unwrap();
        executor
            .execute_agent(action("developer"), context("run", "record-2"))
            .await
            .unwrap();

        let created_roles: Vec<_> = executor
            .factory
            .created_for_roles
            .lock()
            .iter()
            .map(|role| role.id.clone())
            .collect();
        assert_eq!(created_roles, ["developer"]);
    }

    #[tokio::test]
    async fn uses_different_clients_for_different_roles() {
        let factory = FakeFactory::new(vec![
            FakeClient::new(vec![event()]),
            FakeClient::new(vec![event()]),
        ]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());

        executor
            .execute_agent(action("developer"), context("run", "record-1"))
            .await
            .unwrap();
        executor
            .execute_agent(
                action("reviewer"),
                context_with_role("run", "record-2", "reviewer"),
            )
            .await
            .unwrap();

        let created_roles: Vec<_> = executor
            .factory
            .created_for_roles
            .lock()
            .iter()
            .map(|role| role.id.clone())
            .collect();
        assert_eq!(created_roles, ["developer", "reviewer"]);
    }

    #[tokio::test]
    async fn loads_persisted_role_session() {
        let factory = FakeFactory::new(vec![FakeClient::with_load(vec![event()])]);
        let store = FakeStore::default();
        store
            .save_role_session(RoleSession {
                run_id: "run".into(),
                role_id: "developer".into(),
                backend: "agent".into(),
                session_id: "persisted-session".into(),
                updated_at: Utc::now(),
            })
            .unwrap();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());

        let execution = executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap();

        assert_eq!(
            execution.record.detail.session_id.as_deref(),
            Some("persisted-session")
        );
    }

    #[tokio::test]
    async fn correction_turns_use_verbatim_blocks_and_replace_the_initial_response() {
        let client = FakeClient::new(vec![
            Event::MessageChunk {
                content: serde_json::json!({"text": "---\nstatus: success\n---\ninitial"}),
            },
            Event::MessageChunk {
                content: serde_json::json!({"text": "---\nstatus: success\n---\ncorrected"}),
            },
            Event::MessageChunk {
                content: serde_json::json!({"text": "---\nstatus: success\n---\ncorrected twice"}),
            },
        ]);
        let prompt_calls = client.prompt_calls.clone();
        let factory = FakeFactory::new(vec![client]);
        let store = FakeStore::default();
        store
            .pending_prompt_batches
            .lock()
            .push_back(vec![RunUserPrompt {
                sequence: 2,
                content: "  preserve\nthis correction  ".to_string(),
                submitted_at: Utc::now(),
            }]);
        store
            .pending_prompt_batches
            .lock()
            .push_back(vec![RunUserPrompt {
                sequence: 3,
                content: "second correction".to_string(),
                submitted_at: Utc::now(),
            }]);
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());
        let mut context = context("run", "record");
        context.user_prompts.push(RunUserPrompt {
            sequence: 1,
            content: "prior direction".to_string(),
            submitted_at: Utc::now(),
        });
        let mut action = action("developer");
        action.output = Some(cowboy_workflow_core::OutputSpec {
            statuses: vec!["success".to_string()],
            fields: serde_json::json!({"summary": "string"}),
        });

        let execution = executor.execute_agent(action, context).await.unwrap();

        assert_eq!(
            execution.record.output.as_ref().unwrap().body,
            "corrected twice"
        );
        assert_eq!(execution.turns.len(), 3);
        assert_eq!(
            execution
                .turns
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            ["record-turn-1", "record-turn-2", "record-turn-3"]
        );
        assert_eq!(execution.turns[0].prev, None);
        assert_eq!(execution.turns[1].prev.as_deref(), Some("record-turn-1"));
        assert_eq!(execution.turns[2].prev.as_deref(), Some("record-turn-2"));
        let calls = prompt_calls.lock();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].len(), 1);
        assert!(calls[0][0].text.contains("\"sequence\": 0"));
        assert!(calls[0][0].text.contains("Original request"));
        assert!(calls[0][0].text.contains("prior direction"));
        assert_eq!(calls[1].len(), 4);
        assert!(calls[1][0].text.contains("Revise work already performed"));
        assert!(calls[1][0].text.contains("complete replacement response"));
        assert!(calls[1][1].text.contains("sequence 2"));
        assert_eq!(calls[1][2].text, "  preserve\nthis correction  ");
        assert!(calls[1][3].text.contains("valid YAML frontmatter"));
        assert_eq!(calls[2][2].text, "second correction");

        let input = &execution.record.input;
        assert!(input.prompt.as_ref().unwrap().contains("prior direction"));
        assert_eq!(input.context["final_applied_sequence"], 3);
        assert_eq!(
            input.context["correction_turns"][0]["applied_sequences"],
            serde_json::json!([2])
        );
        assert_eq!(
            input.context["correction_turns"][1]["applied_sequences"],
            serde_json::json!([3])
        );
        assert_eq!(
            input.context["correction_turns"][0]["content"][2]["text"],
            "  preserve\nthis correction  "
        );
    }

    #[tokio::test]
    async fn malformed_final_response_leaves_prompt_window_closed() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![Event::MessageChunk {
            content: serde_json::json!({"text": "not frontmatter"}),
        }])]);
        let executor = AgentExecutor::new(
            factory,
            FakeStore::default(),
            AgentExecutionConfig::default(),
        );

        let error = executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap_err();

        assert!(matches!(error, Error::MissingFrontmatter));
        let window = executor.store.window.lock();
        assert!(window.as_ref().is_some_and(|window| !window.is_open()));
    }
}
