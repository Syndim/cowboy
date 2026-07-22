use std::collections::HashMap;
use std::fmt;
use std::future::pending;
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use cowboy_agent_client::{
    AgentSessionDescriptor, Client, Event, ModelInfo, PromptContent, PromptTurnCancellation,
    StopReason,
};
use cowboy_workflow_core::{
    AbortAgentPromptWindowOutcome, AgentAction, AgentPromptWindow,
    CompareAndSealPromptWindowOutcome, ExecutionContext, OpenAgentPromptWindowOutcome,
    RoleDefinition, RoleId, RoleSession, RunId, RunStore, StepDetail, StepInput, StepRecord,
    TurnRecord, WorkflowError, ordered_user_inputs_from_parts,
};
use tokio::sync::{Mutex, watch};

use crate::frontmatter::parse_frontmatter_output;
use crate::prompt::{build_agent_prompt, build_correction_prompt};
use crate::{Error, Result};

type PromptTurnControls = HashMap<RunId, HashMap<String, watch::Sender<u64>>>;

/// Process-local controls that connect durable prompt acceptance to active turns.
#[derive(Clone, Default)]
pub struct PromptTurnControlRegistry {
    controls: Arc<SyncMutex<PromptTurnControls>>,
}

impl fmt::Debug for PromptTurnControlRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PromptTurnControlRegistry")
            .finish_non_exhaustive()
    }
}

impl PromptTurnControlRegistry {
    /// Register one open prompt window at its durable sequence baseline.
    pub fn register(&self, run_id: &str, window_id: &str, baseline_sequence: u64) {
        let (sender, _receiver) = watch::channel(baseline_sequence);
        self.controls
            .lock()
            .expect("prompt-turn control registry lock poisoned")
            .entry(run_id.to_string())
            .or_default()
            .insert(window_id.to_string(), sender);
    }

    /// Publish a newly accepted durable sequence to the matching active window.
    pub fn publish(&self, run_id: &str, window_id: &str, sequence: u64) -> bool {
        let controls = self
            .controls
            .lock()
            .expect("prompt-turn control registry lock poisoned");
        let Some(sender) = controls
            .get(run_id)
            .and_then(|windows| windows.get(window_id))
        else {
            return false;
        };
        if sequence <= *sender.borrow() {
            return false;
        }
        sender.send_replace(sequence);
        true
    }

    /// Create a one-shot cancellation signal for sequences newer than this turn.
    pub fn cancellation(
        &self,
        run_id: &str,
        window_id: &str,
        applied_sequence: u64,
    ) -> PromptTurnCancellation {
        let receiver = self
            .controls
            .lock()
            .expect("prompt-turn control registry lock poisoned")
            .get(run_id)
            .and_then(|windows| windows.get(window_id))
            .map(watch::Sender::subscribe);
        let Some(mut receiver) = receiver else {
            return PromptTurnCancellation::disabled();
        };

        PromptTurnCancellation::from_future(async move {
            loop {
                if *receiver.borrow_and_update() > applied_sequence {
                    return;
                }
                if receiver.changed().await.is_err() {
                    pending::<()>().await;
                }
            }
        })
    }

    fn unregister(&self, run_id: &str, window_id: &str) {
        let mut controls = self
            .controls
            .lock()
            .expect("prompt-turn control registry lock poisoned");
        let remove_run = controls.get_mut(run_id).is_some_and(|windows| {
            windows.remove(window_id);
            windows.is_empty()
        });
        if remove_run {
            controls.remove(run_id);
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.controls
            .lock()
            .expect("prompt-turn control registry lock poisoned")
            .is_empty()
    }
}

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
        descriptor: Option<String>,
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

#[cfg(any(test, feature = "test-support"))]
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

#[cfg(any(test, feature = "test-support"))]
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
    #[cfg(any(test, feature = "test-support"))]
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
        #[cfg(any(test, feature = "test-support"))]
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
            #[cfg(any(test, feature = "test-support"))]
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
    pub model: Option<ModelInfo>,
    pub backend: String,
}

struct ActiveClient {
    client: Box<dyn Client>,
    model: Option<ModelInfo>,
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
    prompt_turn_controls: PromptTurnControlRegistry,
}

struct PromptWindowGuard<S: RunStore> {
    store: Arc<S>,
    context: ExecutionContext,
    role: String,
    window_id: String,
    progress: Option<ProgressSink>,
    prompt_turn_controls: PromptTurnControlRegistry,
    closed: bool,
}

impl<S: RunStore> PromptWindowGuard<S> {
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.prompt_turn_controls
            .unregister(&self.context.run_id, &self.window_id);
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
            prompt_turn_controls: PromptTurnControlRegistry::default(),
        }
    }

    /// Use shared prompt-turn controls owned by the product runtime.
    pub fn with_prompt_turn_controls(mut self, controls: PromptTurnControlRegistry) -> Self {
        self.prompt_turn_controls = controls;
        self
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
        let descriptor = active
            .client
            .session_descriptor()
            .and_then(aggregate_session_descriptor);
        emit_progress_kind(
            self.config.progress.as_ref(),
            &context,
            AgentProgressKind::SessionReady {
                role: action.role.clone(),
                session_id: session_id.clone(),
                descriptor,
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
        self.prompt_turn_controls
            .register(&context.run_id, &window_id, expected_baseline);
        let mut window_guard = PromptWindowGuard {
            store: self.store.clone(),
            context: context.clone(),
            role: action.role.clone(),
            window_id: window_id.clone(),
            progress: self.config.progress.clone(),
            prompt_turn_controls: self.prompt_turn_controls.clone(),
            closed: false,
        };
        emit_progress_kind(
            self.config.progress.as_ref(),
            &context,
            AgentProgressKind::PromptWindowOpened {
                role: action.role.clone(),
                window_id: window_id.clone(),
            },
        );

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
            self.prompt_turn_controls
                .cancellation(&context.run_id, &window_id, expected_baseline),
            &context,
            self.config.progress.clone(),
            &mut turn_cursor,
        )
        .await?;
        tracing::debug!(run_id = %context.run_id, step = %context.step_id, session_id = %session_id, stop_reason = ?stop_reason, reply_chars = visible.chars().count(), "agent step: initial reply");

        let mut applied_sequence = expected_baseline;
        let mut correction_turns = Vec::new();
        loop {
            #[cfg(any(test, feature = "test-support"))]
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
            #[cfg(any(test, feature = "test-support"))]
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
                        self.prompt_turn_controls.cancellation(
                            &context.run_id,
                            &window_id,
                            applied_sequence,
                        ),
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

        let parsed = parse_frontmatter_output(&visible)
            .map_err(|err| match err {
                Error::MissingFrontmatter => Error::NoWorkflowResult,
                other => other,
            })
            .inspect_err(|_err| {
                tracing::error!(
                    run_id = %context.run_id,
                    step = %context.step_id,
                    reply = %visible,
                    "agent step: failed to parse frontmatter output"
                );
            })?;
        validate_output_spec(action.output.as_ref(), &parsed.output)?;
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
            model_id = ?active.model.as_ref().map(|model| model.id.as_str()),
            provider = ?active.model.as_ref().and_then(|model| model.provider.as_deref()),
            "agent session: creating new backend session"
        );
        let session_id = client
            .new_session(
                &self.config.cwd,
                &self.config.mcp_servers,
                active.model.as_ref(),
            )
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

fn validate_output_spec(
    spec: Option<&cowboy_workflow_core::OutputSpec>,
    output: &cowboy_workflow_core::StepOutput,
) -> Result<()> {
    let Some(spec) = spec else {
        return Ok(());
    };

    if !spec.statuses.is_empty() && !spec.statuses.iter().any(|status| status == &output.status) {
        return Err(Error::DisallowedStatus {
            status: output.status.clone(),
            allowed: spec.statuses.join(", "),
        });
    }

    let Some(fields) = spec.fields.as_object() else {
        return Ok(());
    };

    for (field, descriptor) in fields {
        validate_supported_descriptor(field, descriptor)?;
    }

    let output_fields = output.fields.as_object();
    let missing = spec
        .required_fields
        .iter()
        .filter(|field| {
            !output_fields
                .and_then(|values| values.get(*field))
                .map(|value| !value.is_null())
                .unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();

    if !missing.is_empty() {
        return Err(Error::MissingOutputFields(missing.join(", ")));
    }

    let Some(output_fields) = output_fields else {
        return Ok(());
    };

    for (field, descriptor) in fields {
        if let Some(value) = output_fields.get(field)
            && !value.is_null()
        {
            validate_output_field_type(field, descriptor, value)?;
        }
    }

    Ok(())
}

fn validate_supported_descriptor(field: &str, descriptor: &serde_json::Value) -> Result<()> {
    match descriptor.as_str() {
        Some("array" | "boolean" | "number" | "string") => Ok(()),
        Some(value) => Err(Error::UnsupportedOutputFieldDescriptor {
            field: field.to_string(),
            descriptor: value.to_string(),
        }),
        None => Err(Error::UnsupportedOutputFieldDescriptor {
            field: field.to_string(),
            descriptor: descriptor.to_string(),
        }),
    }
}

fn validate_output_field_type(
    field: &str,
    descriptor: &serde_json::Value,
    value: &serde_json::Value,
) -> Result<()> {
    let expected = descriptor.as_str().expect("descriptor validated first");
    let valid = match expected {
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "string" => value.is_string(),
        _ => unreachable!("descriptor validated first"),
    };

    if valid {
        return Ok(());
    }

    Err(Error::InvalidOutputFieldType {
        field: field.to_string(),
        expected: expected.to_string(),
        actual: json_type_name(value).to_string(),
    })
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
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
    cancellation: PromptTurnCancellation,
    context: &ExecutionContext,
    progress: Option<ProgressSink>,
    turn_cursor: &mut TurnCursor,
) -> Result<(String, Vec<TurnRecord>, StopReason)> {
    let mut visible = String::new();
    let mut turns = Vec::new();
    let mut tool_titles = HashMap::new();
    let stop_reason = client
        .prompt(session_id, content, cancellation, &mut |event| {
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

/// Sanitize a single agent-returned descriptor segment into a safe token.
///
/// Agent-returned values are untrusted and flow into a terminal title, so:
/// - allow only ASCII alphanumeric plus `.`, `_`, and `-`;
/// - drop control characters (newline, tab, ANSI escape bytes, etc.);
/// - map any other disallowed run to a single `-`;
/// - collapse consecutive separators and trim leading/trailing separators.
///
/// Returns `None` when nothing survives.
fn sanitize_descriptor_token(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut pending_separator = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            if pending_separator && !out.is_empty() {
                out.push('-');
            }

            pending_separator = false;
            out.push(ch);
        } else {
            // Any disallowed char (including control chars) becomes a pending
            // separator; runs collapse into a single `-`.
            pending_separator = true;
        }
    }

    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Aggregate an agent-returned descriptor into one hyphen-joined token in the
/// order model, context, reasoning.
///
/// The model provider prefix is stripped by keeping only the final
/// `/`-separated segment before sanitization. Each present segment is sanitized;
/// empty segments are skipped. Returns `None` when nothing survives.
fn aggregate_session_descriptor(descriptor: &AgentSessionDescriptor) -> Option<String> {
    let model = descriptor
        .model
        .as_deref()
        .map(|value| value.rsplit('/').next().unwrap_or(value))
        .and_then(sanitize_descriptor_token);
    let context = descriptor
        .context
        .as_deref()
        .and_then(sanitize_descriptor_token);
    let reasoning = descriptor
        .reasoning
        .as_deref()
        .and_then(sanitize_descriptor_token);

    let token = [model, context, reasoning]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("-");
    if token.is_empty() { None } else { Some(token) }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::time::Duration;

    use anyhow::anyhow;
    use cowboy_agent_client::{AgentInfo, StopReason};
    use cowboy_workflow_core::{
        AppendUserPromptOutcome, ObjectHash, ObjectKind, Result as CoreResult, RunHead,
        RunUserPrompt, WorkflowRun,
    };
    use parking_lot::Mutex as SyncMutex;
    use serde::{Serialize, de::DeserializeOwned};
    use tokio::sync::mpsc;

    use super::*;

    #[derive(Debug)]
    enum FakePromptBehavior {
        Reply,
        Error(String),
        Pending,
    }

    #[derive(Debug)]
    struct FakeClient {
        session_id: Option<String>,
        events: SyncMutex<VecDeque<Event>>,
        supports_load: bool,
        new_sessions: usize,
        loaded_sessions: Vec<String>,
        new_session_models: Arc<SyncMutex<Vec<Option<ModelInfo>>>>,
        prompt_calls: Arc<SyncMutex<Vec<Vec<PromptContent>>>>,
        prompt_behavior: FakePromptBehavior,
        session_descriptor: Option<AgentSessionDescriptor>,
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
                prompt_behavior: FakePromptBehavior::Reply,
                session_descriptor: None,
            }
        }

        fn with_load(events: Vec<Event>) -> Self {
            Self {
                supports_load: true,
                ..Self::new(events)
            }
        }

        fn with_prompt_error(message: impl Into<String>) -> Self {
            Self {
                prompt_behavior: FakePromptBehavior::Error(message.into()),
                ..Self::new(Vec::new())
            }
        }

        fn blocking() -> Self {
            Self {
                prompt_behavior: FakePromptBehavior::Pending,
                ..Self::new(Vec::new())
            }
        }

        fn with_descriptor(events: Vec<Event>, descriptor: AgentSessionDescriptor) -> Self {
            Self {
                session_descriptor: Some(descriptor),
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

        fn session_descriptor(&self) -> Option<&AgentSessionDescriptor> {
            self.session_descriptor.as_ref()
        }

        fn session_id(&self) -> Option<&str> {
            self.session_id.as_deref()
        }

        async fn new_session(
            &mut self,
            _cwd: &str,
            _mcp_servers: &[serde_json::Value],
            model: Option<&ModelInfo>,
        ) -> anyhow::Result<String> {
            self.new_session_models.lock().push(model.cloned());

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
            _cancellation: PromptTurnCancellation,
            event_handler: &mut (dyn FnMut(Event) + Send),
        ) -> anyhow::Result<StopReason> {
            self.prompt_calls.lock().push(prompt_content);
            match &self.prompt_behavior {
                FakePromptBehavior::Reply => {}
                FakePromptBehavior::Error(message) => return Err(anyhow!(message.clone())),
                FakePromptBehavior::Pending => pending::<()>().await,
            }
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
        model: Option<ModelInfo>,
    }

    impl FakeFactory {
        fn new(clients: Vec<FakeClient>) -> Self {
            Self {
                clients: SyncMutex::new(clients.into()),
                created_for_roles: SyncMutex::new(Vec::new()),
                model: Some(ModelInfo {
                    id: "fake-model".to_string(),
                    provider: Some("fake-provider".to_string()),
                }),
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

    #[derive(Debug)]
    struct SequencedCancellationClient {
        session_id: Option<String>,
        prompt_calls: Arc<SyncMutex<Vec<Vec<PromptContent>>>>,
        started: mpsc::UnboundedSender<usize>,
        cancelled: mpsc::UnboundedSender<usize>,
        next_call: usize,
    }

    #[async_trait]
    impl Client for SequencedCancellationClient {
        fn is_connected(&self) -> bool {
            true
        }

        fn agent_info(&self) -> Option<&AgentInfo> {
            None
        }

        fn session_descriptor(&self) -> Option<&AgentSessionDescriptor> {
            None
        }

        fn session_id(&self) -> Option<&str> {
            self.session_id.as_deref()
        }

        async fn new_session(
            &mut self,
            _cwd: &str,
            _mcp_servers: &[serde_json::Value],
            _model: Option<&ModelInfo>,
        ) -> anyhow::Result<String> {
            let session_id = "session-1".to_string();
            self.session_id = Some(session_id.clone());
            Ok(session_id)
        }

        fn supports_load_session(&self) -> bool {
            false
        }

        async fn load_session(
            &mut self,
            _session_id: &str,
            _cwd: &str,
            _mcp_servers: &[serde_json::Value],
        ) -> anyhow::Result<Vec<Event>> {
            Err(anyhow!("unsupported"))
        }

        async fn prompt(
            &mut self,
            _session_id: &str,
            prompt_content: Vec<PromptContent>,
            mut cancellation: PromptTurnCancellation,
            event_handler: &mut (dyn FnMut(Event) + Send),
        ) -> anyhow::Result<StopReason> {
            self.next_call += 1;
            let call = self.next_call;
            self.prompt_calls.lock().push(prompt_content);
            self.started
                .send(call)
                .map_err(|_| anyhow!("prompt-start receiver closed"))?;
            match call {
                1 | 2 => {
                    cancellation.cancelled().await;
                    self.cancelled
                        .send(call)
                        .map_err(|_| anyhow!("prompt-cancellation receiver closed"))?;
                    Ok(StopReason::Cancelled)
                }
                3 => {
                    event_handler(Event::MessageChunk {
                        content: serde_json::json!({
                            "text": "---\nstatus: success\nsummary: final\n---\nfinal"
                        }),
                    });
                    Ok(StopReason::EndTurn)
                }
                _ => Err(anyhow!("unexpected prompt call {call}")),
            }
        }

        async fn close(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct SequencedClientFactory {
        client: SyncMutex<Option<SequencedCancellationClient>>,
    }

    #[async_trait]
    impl ClientFactory for SequencedClientFactory {
        async fn create_client(&self, _role: &RoleDefinition) -> Result<ResolvedAgentClient> {
            let client = self
                .client
                .lock()
                .take()
                .ok_or_else(|| Error::MissingClient("developer".to_string()))?;
            Ok(ResolvedAgentClient {
                client: Box::new(client),
                model: Some(ModelInfo {
                    id: "fake-model".to_string(),
                    provider: Some("fake-provider".to_string()),
                }),
                backend: "fake-agent".to_string(),
            })
        }
    }

    struct BlockingHandoffObserver {
        before: mpsc::UnboundedSender<PromptWindowHandoffPoint>,
        resume: Mutex<mpsc::UnboundedReceiver<()>>,
    }

    #[async_trait]
    impl PromptWindowHandoffObserver for BlockingHandoffObserver {
        async fn observe(&self, point: PromptWindowHandoffPoint) {
            if !matches!(point, PromptWindowHandoffPoint::BeforeCompareAndSeal { .. }) {
                return;
            }
            self.before
                .send(point)
                .expect("handoff-point receiver should remain open");
            self.resume
                .lock()
                .await
                .recv()
                .await
                .expect("handoff resume sender should remain open");
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
    async fn retry_prompt_selects_no_result_branch_for_no_result_reason() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![event()])]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());

        let mut context = context("run", "record");
        context.attempt = 2;
        // Full wrapped runner-style reason produced by Error::NoWorkflowResult.
        context.retry_reason = Some(
            "recoverable action failure: agent reply did not contain a workflow result".to_string(),
        );

        let execution = executor
            .execute_agent(action("developer"), context)
            .await
            .unwrap();

        let prompt = execution.record.input.prompt.as_ref().unwrap();
        assert!(prompt.contains("## Retry"));
        // Selects the no-result branch: inspect/continue/complete-as-needed guidance
        // and the do-not-repeat-completed-side-effects protection.
        assert!(prompt.contains("Inspect the existing work"));
        assert!(prompt.contains("Continue or complete any unfinished work"));
        assert!(prompt.contains("without repeating actions or side effects already completed"));
        // Does NOT select the malformed-frontmatter branch.
        assert!(!prompt.contains("Do not redo the work"));
    }

    #[tokio::test]
    async fn rejects_agent_output_missing_declared_required_fields() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![Event::MessageChunk {
            content: serde_json::json!({"text":"---\nstatus: passed\nsummary: ok\n---\nbody"}),
        }])]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());
        let mut action = action("developer");
        action.output = Some(cowboy_workflow_core::OutputSpec {
            statuses: vec!["passed".to_string(), "failed".to_string()],
            fields: serde_json::json!({
                "summary": "string",
                "implementation_commands": "array",
                "implementation_evidence": "array",
                "tester_commands": "array",
                "tester_evidence": "array",
            }),
            required_fields: vec![
                "implementation_commands".to_string(),
                "implementation_evidence".to_string(),
                "tester_commands".to_string(),
                "tester_evidence".to_string(),
            ],
        });

        let error = executor
            .execute_agent(action, context("run", "record"))
            .await
            .unwrap_err();

        match error {
            Error::MissingOutputFields(fields) => {
                assert!(fields.contains("implementation_commands"));
                assert!(fields.contains("implementation_evidence"));
                assert!(fields.contains("tester_commands"));
                assert!(fields.contains("tester_evidence"));
                assert!(!fields.contains("summary"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn allows_omitted_optional_declared_fields() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![Event::MessageChunk {
            content: serde_json::json!({"text":"---\nstatus: ready\nsummary: ok\n---\nbody"}),
        }])]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());
        let mut action = action("developer");
        action.output = Some(cowboy_workflow_core::OutputSpec {
            statuses: vec!["ready".to_string()],
            fields: serde_json::json!({
                "summary": "string",
                "validation_doc": "string",
                "rca_doc": "string",
                "repro_test": "string",
            }),
            required_fields: vec!["summary".to_string()],
        });

        let execution = executor
            .execute_agent(action, context("run", "record"))
            .await
            .unwrap();

        let fields = &execution.record.output.as_ref().unwrap().fields;
        assert_eq!(fields["summary"], "ok");
        assert!(fields.get("validation_doc").is_none());
        assert!(fields.get("rca_doc").is_none());
        assert!(fields.get("repro_test").is_none());
    }

    #[tokio::test]
    async fn rejects_agent_output_with_disallowed_status() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![Event::MessageChunk {
            content: serde_json::json!({"text":"---\nstatus: skipped\nsummary: ok\nimplementation_commands: []\nimplementation_evidence: []\ntester_commands: []\ntester_evidence: []\n---\nbody"}),
        }])]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());
        let mut action = action("developer");
        action.output = Some(cowboy_workflow_core::OutputSpec {
            statuses: vec!["passed".to_string(), "failed".to_string()],
            fields: serde_json::json!({
                "summary": "string",
                "implementation_commands": "array",
                "implementation_evidence": "array",
                "tester_commands": "array",
                "tester_evidence": "array",
            }),
            required_fields: vec![
                "implementation_commands".to_string(),
                "implementation_evidence".to_string(),
                "tester_commands".to_string(),
                "tester_evidence".to_string(),
            ],
        });

        let error = executor
            .execute_agent(action, context("run", "record"))
            .await
            .unwrap_err();

        match error {
            Error::DisallowedStatus { status, allowed } => {
                assert_eq!(status, "skipped");
                assert_eq!(allowed, "passed, failed");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_agent_output_with_wrong_declared_field_types() {
        let factory = FakeFactory::new(vec![FakeClient::new(vec![Event::MessageChunk {
            content: serde_json::json!({"text":"---\nstatus: passed\nsummary: ok\nplan_doc: [not, string]\nfiles: not-array\nimplementation_commands: not-array\nimplementation_evidence: {}\ntester_commands: not-array\ntester_evidence: passed\n---\nbody"}),
        }])]);
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());
        let mut action = action("developer");
        action.output = Some(cowboy_workflow_core::OutputSpec {
            statuses: vec!["passed".to_string()],
            fields: serde_json::json!({
                "summary": "string",
                "plan_doc": "string",
                "files": "array",
                "implementation_commands": "array",
                "implementation_evidence": "array",
                "tester_commands": "array",
                "tester_evidence": "array",
            }),
            required_fields: vec![],
        });

        let error = executor
            .execute_agent(action, context("run", "record"))
            .await
            .unwrap_err();

        match error {
            Error::InvalidOutputFieldType {
                field,
                expected,
                actual,
            } => {
                assert_eq!(field, "files");
                assert_eq!(expected, "array");
                assert_eq!(actual, "string");
            }
            other => panic!("unexpected error: {other:?}"),
        }
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
        assert_eq!(
            observed_models[0].as_ref().unwrap().id,
            executor.factory.model.as_ref().unwrap().id
        );
        assert_eq!(
            observed_models[0].as_ref().unwrap().provider,
            executor.factory.model.as_ref().unwrap().provider
        );
    }

    #[tokio::test]
    async fn session_creation_preserves_absent_agent_model() {
        let client = FakeClient::new(vec![event()]);
        let observed_models = client.new_session_models.clone();
        let mut factory = FakeFactory::new(vec![client]);
        factory.model = None;
        let store = FakeStore::default();
        let executor = AgentExecutor::new(factory, store, AgentExecutionConfig::default());

        executor
            .execute_agent(action("developer"), context("run", "record"))
            .await
            .unwrap();

        let observed_models = observed_models.lock();
        assert_eq!(observed_models.len(), 1);
        assert!(observed_models[0].is_none());
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
            AgentProgressKind::SessionReady { role, session_id, .. }
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
                content: serde_json::json!({"text": "---\nstatus: success\nsummary: initial\n---\ninitial"}),
            },
            Event::MessageChunk {
                content: serde_json::json!({"text": "---\nstatus: success\nsummary: corrected\n---\ncorrected"}),
            },
            Event::MessageChunk {
                content: serde_json::json!({"text": "---\nstatus: success\nsummary: corrected twice\n---\ncorrected twice"}),
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
            required_fields: vec!["summary".to_string()],
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
    async fn prompt_cancellation_sequences_coalesce_without_cancelling_replacement() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (cancelled_tx, mut cancelled_rx) = mpsc::unbounded_channel();
        let prompt_calls = Arc::new(SyncMutex::new(Vec::new()));
        let factory = SequencedClientFactory {
            client: SyncMutex::new(Some(SequencedCancellationClient {
                session_id: None,
                prompt_calls: prompt_calls.clone(),
                started: started_tx,
                cancelled: cancelled_tx,
                next_call: 0,
            })),
        };
        let (window_tx, mut window_rx) = mpsc::unbounded_channel();
        let progress = Arc::new(move |progress: AgentProgress| {
            if let AgentProgressKind::PromptWindowOpened { window_id, .. } = progress.kind {
                window_tx
                    .send(window_id)
                    .expect("window receiver should remain open");
            }
        });
        let (handoff_tx, mut handoff_rx) = mpsc::unbounded_channel();
        let (resume_tx, resume_rx) = mpsc::unbounded_channel();
        let controls = PromptTurnControlRegistry::default();
        let executor = AgentExecutor::new(
            factory,
            FakeStore::default(),
            AgentExecutionConfig {
                progress: Some(progress),
                handoff_observer: Some(Arc::new(BlockingHandoffObserver {
                    before: handoff_tx,
                    resume: Mutex::new(resume_rx),
                })),
                ..AgentExecutionConfig::default()
            },
        )
        .with_prompt_turn_controls(controls.clone());
        let store = executor.store.clone();
        let execution = tokio::spawn(async move {
            executor
                .execute_agent(action("developer"), context("run", "record"))
                .await
        });

        let window_id = tokio::time::timeout(Duration::from_secs(1), window_rx.recv())
            .await
            .expect("prompt window should open")
            .expect("window sender should remain open");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
                .await
                .expect("initial prompt should start"),
            Some(1)
        );

        let first = match store
            .append_user_prompt("run", &window_id, "first follow-up".to_string())
            .unwrap()
        {
            AppendUserPromptOutcome::Accepted(prompt) => prompt,
            outcome => panic!("first follow-up was not accepted: {outcome:?}"),
        };
        let second = match store
            .append_user_prompt("run", &window_id, "second follow-up".to_string())
            .unwrap()
        {
            AppendUserPromptOutcome::Accepted(prompt) => prompt,
            outcome => panic!("second follow-up was not accepted: {outcome:?}"),
        };
        store
            .pending_prompt_batches
            .lock()
            .push_back(vec![first.clone(), second.clone()]);
        assert!(controls.publish("run", &window_id, first.sequence));
        assert!(controls.publish("run", &window_id, second.sequence));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), cancelled_rx.recv())
                .await
                .expect("initial prompt should be cancelled"),
            Some(1)
        );

        let first_handoff = tokio::time::timeout(Duration::from_secs(1), handoff_rx.recv())
            .await
            .expect("first compare-and-seal handoff should be observed")
            .expect("handoff sender should remain open");
        assert!(matches!(
            first_handoff,
            PromptWindowHandoffPoint::BeforeCompareAndSeal {
                applied_sequence: 0,
                ..
            }
        ));
        resume_tx.send(()).unwrap();

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
                .await
                .expect("first replacement should start"),
            Some(2)
        );
        assert!(!controls.publish("run", &window_id, second.sequence));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), cancelled_rx.recv())
                .await
                .is_err(),
            "already-applied sequences must not cancel the first replacement"
        );

        let third = match store
            .append_user_prompt("run", &window_id, "third follow-up".to_string())
            .unwrap()
        {
            AppendUserPromptOutcome::Accepted(prompt) => prompt,
            outcome => panic!("third follow-up was not accepted: {outcome:?}"),
        };
        store
            .pending_prompt_batches
            .lock()
            .push_back(vec![third.clone()]);
        assert!(controls.publish("run", &window_id, third.sequence));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), cancelled_rx.recv())
                .await
                .expect("new sequence should cancel the first replacement"),
            Some(2)
        );

        let second_handoff = tokio::time::timeout(Duration::from_secs(1), handoff_rx.recv())
            .await
            .expect("second compare-and-seal handoff should be observed")
            .expect("handoff sender should remain open");
        assert!(matches!(
            second_handoff,
            PromptWindowHandoffPoint::BeforeCompareAndSeal {
                applied_sequence: 2,
                ..
            }
        ));
        resume_tx.send(()).unwrap();

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
                .await
                .expect("second replacement should start"),
            Some(3)
        );
        let final_handoff = tokio::time::timeout(Duration::from_secs(1), handoff_rx.recv())
            .await
            .expect("final compare-and-seal handoff should be observed")
            .expect("handoff sender should remain open");
        assert!(matches!(
            final_handoff,
            PromptWindowHandoffPoint::BeforeCompareAndSeal {
                applied_sequence: 3,
                ..
            }
        ));
        resume_tx.send(()).unwrap();

        let execution = execution.await.unwrap().unwrap();
        assert_eq!(execution.record.output.as_ref().unwrap().body, "final");
        assert_eq!(
            execution.record.input.context["correction_turns"][0]["applied_sequences"],
            serde_json::json!([1, 2])
        );
        assert_eq!(
            execution.record.input.context["correction_turns"][1]["applied_sequences"],
            serde_json::json!([3])
        );
        assert_eq!(execution.record.input.context["final_applied_sequence"], 3);

        let calls = prompt_calls.lock();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1].len(), 6);
        assert!(calls[1][1].text.contains("sequence 1"));
        assert_eq!(calls[1][2].text, "first follow-up");
        assert!(calls[1][3].text.contains("sequence 2"));
        assert_eq!(calls[1][4].text, "second follow-up");
        assert_eq!(calls[2].len(), 4);
        assert!(calls[2][1].text.contains("sequence 3"));
        assert_eq!(calls[2][2].text, "third follow-up");
        drop(calls);

        assert!(cancelled_rx.try_recv().is_err());
        assert!(controls.is_empty());
        assert_eq!(
            store
                .accepted_prompts
                .lock()
                .iter()
                .map(|prompt| prompt.sequence)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(
            store
                .window
                .lock()
                .as_ref()
                .is_some_and(|window| !window.is_open())
        );
    }

    #[tokio::test]
    async fn prompt_window_controls_cleanup_on_success_and_backend_error() {
        let success_controls = PromptTurnControlRegistry::default();
        let success_executor = AgentExecutor::new(
            FakeFactory::new(vec![FakeClient::new(vec![event()])]),
            FakeStore::default(),
            AgentExecutionConfig::default(),
        )
        .with_prompt_turn_controls(success_controls.clone());
        success_executor
            .execute_agent(
                action("developer"),
                context("success-run", "success-record"),
            )
            .await
            .unwrap();
        assert!(success_controls.is_empty());
        assert!(
            success_executor
                .store
                .window
                .lock()
                .as_ref()
                .is_some_and(|window| !window.is_open())
        );

        let error_controls = PromptTurnControlRegistry::default();
        let error_executor = AgentExecutor::new(
            FakeFactory::new(vec![FakeClient::with_prompt_error("transport reset")]),
            FakeStore::default(),
            AgentExecutionConfig::default(),
        )
        .with_prompt_turn_controls(error_controls.clone());
        let error = error_executor
            .execute_agent(action("developer"), context("error-run", "error-record"))
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Client(_)));
        assert!(error_controls.is_empty());
        assert!(
            error_executor
                .store
                .window
                .lock()
                .as_ref()
                .is_some_and(|window| !window.is_open())
        );
    }

    #[tokio::test]
    async fn dropped_execution_removes_prompt_window_control_and_aborts_window() {
        let opened = Arc::new(tokio::sync::Notify::new());
        let progress = {
            let opened = opened.clone();
            Arc::new(move |progress: AgentProgress| {
                if matches!(progress.kind, AgentProgressKind::PromptWindowOpened { .. }) {
                    opened.notify_one();
                }
            })
        };
        let controls = PromptTurnControlRegistry::default();
        let executor = AgentExecutor::new(
            FakeFactory::new(vec![FakeClient::blocking()]),
            FakeStore::default(),
            AgentExecutionConfig {
                progress: Some(progress),
                ..AgentExecutionConfig::default()
            },
        )
        .with_prompt_turn_controls(controls.clone());
        let store = executor.store.clone();
        let execution = tokio::spawn(async move {
            executor
                .execute_agent(
                    action("developer"),
                    context("dropped-run", "dropped-record"),
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(1), opened.notified())
            .await
            .expect("prompt window should open before the blocking turn");
        assert!(!controls.is_empty());
        assert!(
            store
                .window
                .lock()
                .as_ref()
                .is_some_and(AgentPromptWindow::is_open)
        );

        execution.abort();
        assert!(execution.await.unwrap_err().is_cancelled());
        assert!(controls.is_empty());
        assert!(
            store
                .window
                .lock()
                .as_ref()
                .is_some_and(|window| !window.is_open())
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

        assert!(matches!(error, Error::NoWorkflowResult));
        let window = executor.store.window.lock();
        assert!(window.as_ref().is_some_and(|window| !window.is_open()));
    }

    #[test]
    fn sanitize_descriptor_token_allows_safe_charset_and_strips_control() {
        assert_eq!(
            sanitize_descriptor_token("gpt-5.6-sol"),
            Some("gpt-5.6-sol".to_string())
        );
        assert_eq!(
            sanitize_descriptor_token("gpt_5.6"),
            Some("gpt_5.6".to_string())
        );
        // Newline, tab, ANSI escape bytes, and other control chars become
        // separators and never survive.
        assert_eq!(
            sanitize_descriptor_token("gpt\n5\t6"),
            Some("gpt-5-6".to_string())
        );
        // The ESC byte and `[` are stripped as separators; printable residue of
        // the sequence (`31m`) is harmless text that survives as ordinary chars.
        assert_eq!(
            sanitize_descriptor_token("gpt\x1b[31m5"),
            Some("gpt-31m5".to_string())
        );
        assert!(
            !sanitize_descriptor_token("gpt\x1b[31m5")
                .unwrap()
                .contains('\x1b'),
            "ESC byte must never survive"
        );
        // Repeated disallowed runs collapse to a single separator; leading and
        // trailing separators are trimmed.
        assert_eq!(
            sanitize_descriptor_token("  gpt   5  "),
            Some("gpt-5".to_string())
        );
        assert_eq!(
            sanitize_descriptor_token("gpt///5"),
            Some("gpt-5".to_string())
        );
        // Empty after sanitization -> None.
        assert_eq!(sanitize_descriptor_token(""), None);
        assert_eq!(sanitize_descriptor_token("\n\t "), None);
        assert_eq!(sanitize_descriptor_token("///"), None);
    }

    #[test]
    fn aggregate_session_descriptor_joins_present_segments() {
        let descriptor = AgentSessionDescriptor {
            model: Some("github-copilot/gpt-5.6-sol".to_string()),
            context: Some("1m".to_string()),
            reasoning: Some("high".to_string()),
        };
        // Provider prefix stripped to final `/` segment; joined model-context-reasoning.
        assert_eq!(
            aggregate_session_descriptor(&descriptor),
            Some("gpt-5.6-sol-1m-high".to_string())
        );
    }

    #[test]
    fn aggregate_session_descriptor_skips_missing_middle_segment() {
        let descriptor = AgentSessionDescriptor {
            model: Some("gpt-5.6-sol".to_string()),
            context: None,
            reasoning: Some("high".to_string()),
        };
        assert_eq!(
            aggregate_session_descriptor(&descriptor),
            Some("gpt-5.6-sol-high".to_string())
        );
    }

    #[test]
    fn aggregate_session_descriptor_none_when_all_empty() {
        let descriptor = AgentSessionDescriptor {
            model: Some("\n".to_string()),
            context: Some("".to_string()),
            reasoning: None,
        };
        assert_eq!(aggregate_session_descriptor(&descriptor), None);
        assert_eq!(
            aggregate_session_descriptor(&AgentSessionDescriptor::default()),
            None
        );
    }

    #[test]
    fn aggregate_session_descriptor_sanitizes_adversarial_values() {
        let descriptor = AgentSessionDescriptor {
            model: Some("evil/g\x1b[31mpt".to_string()),
            context: Some("1\nm".to_string()),
            reasoning: Some("hi\tgh".to_string()),
        };
        let token = aggregate_session_descriptor(&descriptor).unwrap();
        assert_eq!(token, "g-31mpt-1-m-hi-gh");
        assert!(
            token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')),
            "unsafe char survived: {token}"
        );
    }

    #[tokio::test]
    async fn session_ready_progress_carries_aggregated_descriptor() {
        let client = FakeClient::with_descriptor(
            vec![event()],
            AgentSessionDescriptor {
                model: Some("github-copilot/gpt-5.6-sol".to_string()),
                context: Some("1m".to_string()),
                reasoning: Some("high".to_string()),
            },
        );
        let factory = FakeFactory::new(vec![client]);
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
        assert!(matches!(
            &progress_events[0],
            AgentProgressKind::SessionReady { descriptor, .. }
                if descriptor.as_deref() == Some("gpt-5.6-sol-1m-high")
        ));
    }

    // Regression (systemic missing-frontmatter volume): the genuine executor
    // `failed to parse frontmatter output` emissions across the diagnostic logs
    // (11 across 5 runs; see
    // docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/) are
    // nonempty replies that carry NO parseable workflow result — 8 are
    // unambiguous backend notices (7 stall, 1 stream-close) and 3 are ambiguous
    // prose/preamble. Such a reply returns `StopReason::EndTurn` with the notice
    // text as its only content, so at the provider-neutral `Client::prompt` seam
    // it is shaped like an ordinary completed prose reply that omitted
    // frontmatter — Cowboy has NO structured signal that the turn was incomplete
    // (`PromptTurnActivity` is private to `cowboy-agent-acp` and consumed inside
    // `prompt()`; only `StopReason` crosses the boundary). The feasible, grounded
    // contract is therefore an ACCURATE GENERIC diagnostic for a reply that
    // contained no workflow result, NOT a claim that the turn was incomplete and
    // NOT any change to the (already complete-replacement) retry nudge.
    //
    // Observable contract this test pins, exercised through the real
    // executor/client seam:
    //   (a) POSITIVE: the diagnostic must accurately say the reply contained no
    //       workflow result (asserted on the user-visible message text), so an
    //       unrelated recoverable error does not satisfy it;
    //   (b) it must be classified distinctly from `Error::MissingFrontmatter`
    //       (asserted on the variant, so a bare message rewording of that variant
    //       is rejected); and
    //   (c) the failure must stay recoverable so the retry/back-off policy still
    //       applies.
    // (a) and (b) fail before the fix: the executor returns `MissingFrontmatter`,
    // whose message is "agent response is missing YAML frontmatter".
    #[tokio::test]
    async fn no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter() {
        let stall_notice = "Anthropic stream stalled while waiting for the next event";
        let factory = FakeFactory::new(vec![FakeClient::new(vec![Event::MessageChunk {
            content: serde_json::json!({ "text": stall_notice }),
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

        // (a) Positive: the user-visible diagnostic accurately describes a reply
        // that carried no workflow result.
        assert!(
            error
                .to_string()
                .contains("did not contain a workflow result"),
            "a nonempty reply carrying no parseable result must be diagnosed as \
\"agent reply did not contain a workflow result\", got: {error} ({error:?})"
        );
        // (b) Distinct classification — not the misleading missing-frontmatter
        // variant (rejects a fix that only reworded `MissingFrontmatter`).
        assert!(
            !matches!(error, Error::MissingFrontmatter),
            "a no-result reply must be classified distinctly from \
MissingFrontmatter, got: {error:?}"
        );
        // (c) Still recoverable, so retry/back-off policy is preserved.
        assert!(
            error.recoverable(),
            "a no-result reply must stay recoverable so retry/back-off policy \
still applies, got non-recoverable: {error:?}"
        );
    }
}
