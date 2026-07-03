use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    ObjectHash, ObjectKind, Result, RoleDefinition, RoleSession, RunHead, RunId, StepAction,
    StepDefinition, StepId, StepRecord, TurnRecord, WorkflowCatalog, WorkflowDefinition,
    WorkflowRun, WorkflowSourceRef, WorkflowSourceSnapshot, WorkflowSummary,
};

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
}

/// Result of executing a workflow action.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionExecution {
    /// The action finished a step. Carries the immutable, content-addressed
    /// `StepRecord` for that execution; its `output.status` drives transition
    /// routing. The engine appends the record to run history and advances to the
    /// next step (or completes the run) via the workflow graph.
    StepCompleted(Box<StepRecord>),
    /// The action did not finish a step. Instead it sets the run's head/status
    /// directly to the given `RunHead` (e.g. waiting for input, suspended, or
    /// failed). The engine applies it verbatim: no step record is appended and no
    /// graph routing happens.
    RunStateChanged(RunHead),
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
pub trait ActionExecutor: Send + Sync {
    async fn execute(
        &self,
        action: StepAction,
        context: ExecutionContext,
    ) -> Result<ActionExecution>;
}

#[async_trait]
pub trait WorkflowSummarizer: Send + Sync {
    async fn summarize(&self, run: &WorkflowRun) -> Result<WorkflowSummary>;
}

pub trait RunStore: Send + Sync {
    fn save_run(&self, run: &WorkflowRun) -> Result<()>;

    fn load_run(&self, run_id: &RunId) -> Result<WorkflowRun>;

    fn list_runs(&self) -> Result<Vec<RunHead>>;

    fn put_object<T: Serialize>(&self, kind: ObjectKind, value: &T) -> Result<ObjectHash>;

    fn get_object<T: DeserializeOwned>(&self, hash: &ObjectHash) -> Result<T>;

    fn update_run_head(&self, run_id: &str, head: RunHead) -> Result<()>;

    fn load_run_head(&self, run_id: &str) -> Result<RunHead>;

    fn save_role_session(&self, session: RoleSession) -> Result<()>;

    fn load_role_session(&self, run_id: &str, role_id: &str) -> Result<Option<RoleSession>>;

    fn delete_role_sessions(&self, run_id: &str) -> Result<()>;

    fn append_turn(&self, run_id: &str, turn: TurnRecord) -> Result<ObjectHash>;
}
