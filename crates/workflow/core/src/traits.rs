use crate::{
    AbortAgentPromptWindowOutcome, AgentPromptWindow, AppendUserPromptOutcome,
    CompareAndSealPromptWindowOutcome, ObjectHash, OpenAgentPromptWindowOutcome, Result,
    ResumeCallback, RoleDefinition, RoleSession, RunHead, RunId, RunStatus, RunUserPrompt,
    StepAction, StepDefinition, StepId, StepRecord, TurnRecord, WorkflowCatalog,
    WorkflowDefinition, WorkflowRun, WorkflowSourceRef, WorkflowSourceSnapshot, WorkflowSummary,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Result of loading and compiling a workflow source.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledWorkflow {
    /// Compiled workflow definition.
    pub definition: WorkflowDefinition,
    /// Snapshotted source bundle used to compile the definition.
    pub source_bundle: WorkflowSourceSnapshot,
}

/// Chosen workflow and explanation from a selector.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowSelection {
    /// Selected workflow id.
    pub workflow_id: String,
    /// Human-readable selection rationale.
    pub rationale: String,
    /// Confidence score in the range expected by the selector implementation.
    pub confidence: f64,
}

/// Runtime context passed to host action executors.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionContext {
    /// Run id being executed.
    pub run_id: RunId,
    /// Workflow step id being executed.
    pub step_id: StepId,
    /// Step record id being built/executed.
    pub step_record_id: String,
    /// Hash of the previous completed step record, if any.
    pub prev: Option<ObjectHash>,
    /// Role metadata for agent actions, when the action targets a compiled role.
    pub role: Option<RoleDefinition>,
    /// 1-based attempt number for the current step (increments on recoverable retry).
    pub attempt: u64,
    /// Reason the previous attempt failed, when this is a corrective retry.
    pub retry_reason: Option<String>,
    /// Original request that created the run.
    pub original_request: String,
    /// Timestamp of the initial request.
    pub run_created_at: DateTime<Utc>,
    /// Ordered durable follow-up prompt snapshot used for this dispatch.
    pub user_prompts: Vec<RunUserPrompt>,
}

/// User answer and prompt metadata supplied to a registered resume callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeInput {
    /// Step that registered the callback.
    pub step: StepId,
    /// Prompt id being answered.
    pub prompt_id: String,
    /// Prompt message originally shown to the user.
    pub message: String,
    /// Accepted choices originally shown to the user.
    pub choices: Vec<String>,
    /// User-provided answer text.
    pub answer: String,
    /// Timestamp captured when the answer was accepted.
    pub completed_at: DateTime<Utc>,
}

/// Result produced by dispatching one workflow action.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionResult {
    /// The action completed a step and produced the immutable step record whose
    /// output status drives workflow routing.
    Completed(Box<StepRecord>),
    /// The action blocked or terminally changed the run without completing a
    /// step record, such as waiting for user input or failure.
    Blocked(RunStatus),
}

impl ActionResult {
    pub fn completed(record: StepRecord) -> Self {
        Self::Completed(Box::new(record))
    }

    pub fn blocked(status: RunStatus) -> Self {
        Self::Blocked(status)
    }
}

pub trait StepActionProvider: Send + Sync {
    /// Execute/evaluate the current step and return the declarative action to handle.
    ///
    /// `prev` is the latest completed step record for the run, when one exists.
    /// Providers can expose its `output` to workflow code as previous-step data.
    fn step_action(
        &self,
        definition: &WorkflowDefinition,
        run: &WorkflowRun,
        step: &StepDefinition,
        prev: Option<&StepRecord>,
        user_prompts: &[RunUserPrompt],
    ) -> Result<StepAction>;
}

#[async_trait]
pub trait DefinitionLoader: Send + Sync {
    async fn load(&self, source: &WorkflowSourceRef) -> Result<CompiledWorkflow>;
}

#[async_trait]
pub trait WorkflowSelector: Send + Sync {
    async fn select(&self, request: &str, catalog: &WorkflowCatalog) -> Result<WorkflowSelection>;
}

#[async_trait]
pub trait ActionDispatcher: Send + Sync {
    async fn dispatch(&self, action: StepAction, context: ExecutionContext)
    -> Result<ActionResult>;
}

#[async_trait]
pub trait ResumeCallbackHandler: Send + Sync {
    async fn resume(&self, callback: &ResumeCallback, input: ResumeInput) -> Result<ActionResult>;
}
#[async_trait]
pub trait WorkflowSummarizer: Send + Sync {
    async fn summarize(&self, run: &WorkflowRun) -> Result<WorkflowSummary>;
}

#[async_trait]
pub trait WorkflowStateStore: Send + Sync {
    async fn save_run(&self, run: &WorkflowRun) -> Result<()>;
    async fn load_run(&self, run_id: &RunId) -> Result<WorkflowRun>;
    async fn list_runs(&self) -> Result<Vec<RunHead>>;
    async fn load_run_head(&self, run_id: &str) -> Result<RunHead>;
    async fn commit_completed_step(
        &self,
        run: &WorkflowRun,
        record: &StepRecord,
    ) -> Result<ObjectHash>;
    async fn delete_run(&self, run_id: &str) -> Result<()>;
}

#[async_trait]
pub trait WorkflowObjectStore: Send + Sync {
    async fn store_workflow_source_snapshot(
        &self,
        snapshot: &WorkflowSourceSnapshot,
    ) -> Result<ObjectHash>;
    async fn load_workflow_source_snapshot(
        &self,
        hash: &ObjectHash,
    ) -> Result<WorkflowSourceSnapshot>;
    async fn store_step_record(&self, record: &StepRecord) -> Result<ObjectHash>;
    async fn load_step_record(&self, hash: &ObjectHash) -> Result<StepRecord>;
    async fn delete_object(&self, hash: &ObjectHash) -> Result<()>;
}

#[async_trait]
pub trait AgentSessionStore: Send + Sync {
    async fn save_role_session(&self, session: RoleSession) -> Result<()>;
    async fn load_role_session(&self, run_id: &str, role_id: &str) -> Result<Option<RoleSession>>;
    async fn delete_role_sessions(&self, run_id: &str) -> Result<()>;
}

#[async_trait]
pub trait TurnStore: Send + Sync {
    async fn append_turn(&self, run_id: &str, turn: TurnRecord) -> Result<ObjectHash>;
    async fn load_turn(&self, hash: &ObjectHash) -> Result<TurnRecord>;
    async fn load_turns(&self, run_id: &str, step_record_id: &str) -> Result<Vec<TurnRecord>>;
}

#[async_trait]
pub trait UserPromptStore: Send + Sync {
    async fn load_user_prompts(&self, run_id: &str) -> Result<Vec<RunUserPrompt>>;
}

#[async_trait]
pub trait PromptWindowStore: Send + Sync {
    async fn open_agent_prompt_window(
        &self,
        window: AgentPromptWindow,
    ) -> Result<OpenAgentPromptWindowOutcome>;
    async fn append_user_prompt(
        &self,
        run_id: &str,
        window_id: &str,
        content: String,
    ) -> Result<AppendUserPromptOutcome>;
    async fn compare_and_seal_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        applied_sequence: u64,
        sealed_at: DateTime<Utc>,
    ) -> Result<CompareAndSealPromptWindowOutcome>;

    async fn abort_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        aborted_at: DateTime<Utc>,
    ) -> Result<AbortAgentPromptWindowOutcome>;

    /// Clear any process-stale prompt window while holding the run execution guard.
    async fn clear_agent_prompt_window(&self, run_id: &str) -> Result<Option<AgentPromptWindow>>;
}

pub trait WorkflowStore:
    WorkflowStateStore
    + WorkflowObjectStore
    + AgentSessionStore
    + TurnStore
    + UserPromptStore
    + PromptWindowStore
{
}

impl<T> WorkflowStore for T where
    T: WorkflowStateStore
        + WorkflowObjectStore
        + AgentSessionStore
        + TurnStore
        + UserPromptStore
        + PromptWindowStore
        + ?Sized
{
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::Value;

    use super::*;
    use crate::{StepDetail, StepInput, StepOutput};

    #[allow(dead_code)]
    fn assert_object_safe(
        _: &dyn WorkflowStateStore,
        _: &dyn WorkflowObjectStore,
        _: &dyn AgentSessionStore,
        _: &dyn TurnStore,
        _: &dyn UserPromptStore,
        _: &dyn PromptWindowStore,
        _: &dyn WorkflowStore,
    ) {
    }

    #[test]
    fn workflow_store_capabilities_are_object_safe() {
        let _ = assert_object_safe;
        println!("EVIDENCE workflow-store-traits object_safe=7 async=true");
    }

    fn record() -> StepRecord {
        let now = Utc::now();
        StepRecord {
            id: "record".to_string(),
            prev: None,
            step: "step".to_string(),
            action: "status".to_string(),
            input: StepInput {
                prompt: None,
                context: Value::Null,
            },
            output: Some(StepOutput {
                status: "success".to_string(),
                fields: Value::Null,
                body: String::new(),
                raw: Value::Null,
            }),
            detail: StepDetail {
                backend: None,
                session_id: None,
                duration_ms: 0,
                turn_count: 0,
                usage: None,
            },
            started_at: now,
            completed_at: Some(now),
        }
    }

    #[test]
    fn action_result_completed_contains_only_record() {
        let ActionResult::Completed(record) = ActionResult::completed(record()) else {
            panic!("expected completed result")
        };
        assert_eq!(record.id, "record");
    }

    #[test]
    fn action_result_blocked_contains_only_status() {
        let ActionResult::Blocked(status) = ActionResult::blocked(RunStatus::Failed {
            reason: "bad".to_string(),
        }) else {
            panic!("expected blocked result")
        };
        assert_eq!(
            status,
            RunStatus::Failed {
                reason: "bad".to_string(),
            }
        );
    }
}
