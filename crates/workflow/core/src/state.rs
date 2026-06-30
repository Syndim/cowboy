use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ObjectHash, RecordId, RoleId, RunId, Status, StepId, TurnId, WorkflowId};

/// Durable state of a workflow run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRun {
    /// Stable run id.
    pub id: RunId,
    /// Name/id of the workflow used for this run.
    pub workflow_name: WorkflowId,
    /// Host API version used to interpret the snapshotted Lua sources.
    pub workflow_api_version: u32,
    /// Hash of the source bundle used to compile the workflow.
    pub workflow_hash: ObjectHash,
    /// Workflow-local source files keyed by normalized path relative to workflow root.
    pub workflow_sources: BTreeMap<String, String>,
    /// Original user request that started the run.
    pub original_request: String,
    /// Current lifecycle status for the run.
    pub status: RunStatus,
    /// Step id that should run next when the run resumes.
    pub current_step: StepId,
    /// Hash of the latest completed step record.
    pub head: Option<ObjectHash>,
    /// Resume data exposed to Lua as `ctx.resume`.
    #[serde(default)]
    pub resume: Value,
    /// Total number of step actions completed or terminally handled.
    #[serde(default)]
    pub steps_executed: u32,
    /// Number of times each step has been visited in this run.
    #[serde(default)]
    pub step_visits: BTreeMap<StepId, u32>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Workflow run lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunStatus {
    /// Run can continue executing workflow steps.
    Running,
    /// Run is waiting for user input requested by a step.
    WaitingForInput {
        /// Step that requested input.
        step: StepId,
        /// Prompt/input id exposed to Lua resume data.
        prompt_id: String,
        /// Message shown to the user.
        message: String,
        /// Accepted choices, empty when free-form input is allowed.
        choices: Vec<String>,
    },
    /// Run is paused without being failed.
    Suspended {
        /// Step that requested or caused suspension.
        step: StepId,
        /// Human-readable suspension reason.
        reason: String,
    },
    /// Run completed successfully.
    Completed,
    /// Run failed permanently.
    Failed {
        /// Human-readable failure reason.
        reason: String,
    },
    /// Run was cancelled by the user/system.
    Cancelled,
}

/// Immutable record for one completed or failed step execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    /// Stable step record id.
    pub id: RecordId,
    /// Hash of the previous step record in the run, if any.
    pub prev: Option<ObjectHash>,
    /// Workflow step id that produced this record.
    pub step: StepId,
    /// Action kind that was executed (`agent`, `status`, etc.).
    pub action: String,
    /// Input/context captured before executing the action.
    pub input: StepInput,
    /// Output produced by the action, absent for incomplete/failed records.
    pub output: Option<StepOutput>,
    /// Runtime details and usage metadata.
    pub detail: StepDetail,
    /// Step start timestamp.
    pub started_at: DateTime<Utc>,
    /// Step completion timestamp.
    pub completed_at: Option<DateTime<Utc>>,
}

/// Input captured for a step action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepInput {
    /// Exact prompt sent for agent actions, if any.
    pub prompt: Option<String>,
    /// Additional action context useful for debugging or replay.
    #[serde(default)]
    pub context: Value,
}

/// Normalized output produced by a step action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepOutput {
    /// Status string used for routing to the next step.
    pub status: Status,
    /// Structured fields exposed to later steps.
    #[serde(default)]
    pub fields: Value,
    /// Human-readable markdown/text body.
    #[serde(default)]
    pub body: String,
    /// Raw backend output or host action result.
    #[serde(default)]
    pub raw: Value,
}

/// Runtime details for one step execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepDetail {
    /// Backend used for the step, such as `acp`.
    pub backend: Option<String>,
    /// Backend session id, when available.
    pub session_id: Option<String>,
    /// Step duration in milliseconds.
    pub duration_ms: u64,
    /// Number of turns observed during the step.
    pub turn_count: u32,
    /// Token/turn usage, when available.
    pub usage: Option<Usage>,
}

/// Usage statistics reported by a backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Number of turns in the step.
    pub turns: u32,
    /// Input tokens consumed.
    pub input_tokens: u64,
    /// Output tokens produced.
    pub output_tokens: u64,
    /// Backend-reported duration in milliseconds.
    pub duration_ms: u64,
}

/// One turn of agent-visible output or tool activity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRecord {
    /// Stable turn id.
    pub id: TurnId,
    /// Step record that owns this turn.
    pub step_id: RecordId,
    /// Turn role/kind, such as `assistant` or `tool`.
    pub role: String,
    /// Turn content.
    pub content: String,
    /// Turn timestamp.
    pub timestamp: DateTime<Utc>,
    /// Hash of the previous turn, if tracked.
    pub prev: Option<ObjectHash>,
}

/// Snapshotted Lua workflow source bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSourceSnapshot {
    /// Optional workflow root path where imports were resolved.
    pub root: Option<String>,
    /// Entry file path relative to the workflow root.
    pub entry: String,
    /// Source files keyed by normalized path relative to workflow root.
    pub files: BTreeMap<String, String>,
}

/// Mutable run pointer stored separately from immutable objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHead {
    /// Run id this head belongs to.
    pub run_id: RunId,
    /// Workflow source bundle hash used by this run.
    pub workflow_hash: ObjectHash,
    /// Latest completed step record hash.
    pub head_step: Option<ObjectHash>,
    /// Current run status.
    pub status: RunStatus,
    /// Last update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Persisted backend session for one role within one workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleSession {
    /// Run id this backend session belongs to.
    pub run_id: RunId,
    /// Role id whose agent calls reuse this session.
    pub role_id: RoleId,
    /// Backend identifier, such as `acp`.
    pub backend: String,
    /// Backend-specific session id.
    pub session_id: String,
    /// Last update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Type tag for content-addressed stored objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    /// Workflow source bundle object.
    WorkflowSourceSnapshot,
    /// Step record object.
    StepRecord,
    /// Turn record object.
    TurnRecord,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_status_is_tagged_for_readable_json() {
        let status = RunStatus::WaitingForInput {
            step: "approve".to_string(),
            prompt_id: "approval".to_string(),
            message: "Approve?".to_string(),
            choices: vec!["yes".to_string(), "no".to_string()],
        };
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["status"], "waiting_for_input");
        assert_eq!(value["step"], "approve");
    }
}
