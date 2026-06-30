use serde::{Deserialize, Serialize};

use crate::{RecordId, StepId, WorkflowId, WorkflowSourceRef};

/// Post-run summary used to decide whether workflows should be improved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSummary {
    /// Original user goal/request summarized for workflow improvement.
    pub goal: String,
    /// Workflow selected for the completed run.
    pub selected_workflow_id: WorkflowId,
    /// Summaries of steps executed during the run.
    pub steps: Vec<StepSummary>,
    /// Human-readable outcome of the run.
    pub outcome: String,
    /// Proposed workflow catalog improvement.
    pub improvement: WorkflowImprovement,
}

/// Compact step summary for post-run analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepSummary {
    /// Step record id being summarized.
    pub record_id: RecordId,
    /// Workflow step id.
    pub step: StepId,
    /// Step output status.
    pub status: String,
    /// Human-readable step summary.
    pub summary: String,
}

/// Proposed workflow catalog change after a successful run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowImprovement {
    /// No workflow change is needed.
    None {
        /// Explanation for why no change is needed.
        rationale: String,
    },
    /// Update an existing workflow source.
    UpdateExisting {
        /// Workflow id to update.
        workflow_id: WorkflowId,
        /// Proposed patch/replacement.
        patch: WorkflowPatch,
        /// Explanation for the proposed update.
        rationale: String,
    },
    /// Create a new workflow source.
    CreateNew {
        /// Draft workflow source descriptor.
        draft: WorkflowSourceRef,
        /// Explanation for why a new workflow is needed.
        rationale: String,
    },
}

/// Proposed workflow source change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPatch {
    /// Human-readable patch description.
    pub description: String,
    /// Optional full replacement source for the workflow entry file.
    pub replacement_source: Option<String>,
}
