use cowboy_workflow_core::StepId;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("lua error: {0}")]
    Lua(#[from] mlua::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("workflow source root is required for filesystem loading")]
    MissingRoot,
    #[error("workflow import path must not be empty")]
    EmptyImport,
    #[error("workflow import path {0:?} is outside the workflow root")]
    ImportOutsideRoot(String),
    #[error("workflow entry {0:?} was not loaded")]
    MissingEntry(String),
    #[error("workflow script must return workflow(...)")]
    MissingWorkflow,
    #[error("workflow name must not be empty")]
    EmptyWorkflowName,
    #[error("workflow config_set must be a non-empty string")]
    InvalidWorkflowConfigSet,
    #[error("workflow head step is missing or invalid")]
    MissingHead,
    #[error("role id must be a non-empty string")]
    InvalidRoleId,
    #[error("role agent must be a non-empty string")]
    InvalidRoleAgent,
    #[error("step id must be a non-empty string")]
    InvalidStepId,
    #[error("step {0:?} must define run(ctx)")]
    MissingRunFunction(StepId),
    #[error("transition status for step {0:?} must be a non-empty string")]
    InvalidTransitionStatus(StepId),
    #[error("transition target for step {0:?} is invalid")]
    InvalidTransitionTarget(StepId),
    #[error("unsupported lua value at {0}")]
    UnsupportedValue(String),
    #[error("action must be a table")]
    ActionNotTable,
    #[error("action field must be a string")]
    MissingActionKind,
    #[error("unknown action kind {0:?}")]
    UnknownAction(String),
    #[error("field {field:?} for action {action:?} is required")]
    MissingActionField { action: String, field: String },
    #[error("invalid action field {field:?} for action {action:?}: {reason}")]
    InvalidActionField {
        action: String,
        field: String,
        reason: String,
    },
    #[error("workflow validation failed: {0}")]
    Validation(#[from] cowboy_workflow_core::WorkflowError),
}
