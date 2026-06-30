//! Identifier aliases used by the workflow model.
//!
//! These are aliases for now to keep serialization and Lua interop simple. We
//! can promote high-churn ids to newtypes after the model settles.

/// Stable id for a workflow definition/source.
pub type WorkflowId = String;
/// Stable id for a reusable role/persona inside a workflow.
pub type RoleId = String;
/// Stable id for a step inside a workflow.
pub type StepId = String;
/// Stable id for one workflow execution run.
pub type RunId = String;
/// Stable id for a persisted step record.
pub type RecordId = String;
/// Stable id for a persisted turn record.
pub type TurnId = String;
/// Content hash for immutable stored objects.
pub type ObjectHash = String;
/// Step output status used for workflow routing.
pub type Status = String;
