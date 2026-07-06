use crate::{RoleId, Status, StepId, WorkflowId};

pub type Result<T, E = WorkflowError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkflowError {
    #[error("workflow id must not be empty")]
    EmptyWorkflowId,
    #[error("workflow {workflow:?} head step must not be empty")]
    EmptyHead { workflow: WorkflowId },
    #[error("workflow {workflow:?} head step {step:?} is not defined")]
    MissingHead { workflow: WorkflowId, step: StepId },
    #[error("step {step:?} is not defined")]
    UnknownStep { step: StepId },
    #[error("step map key {key:?} does not match step id {id:?}")]
    StepIdMismatch { key: StepId, id: StepId },
    #[error("role map key {key:?} does not match role id {id:?}")]
    RoleIdMismatch { key: RoleId, id: RoleId },
    #[error("step id must not be empty")]
    EmptyStepId,
    #[error("role id must not be empty")]
    EmptyRoleId,
    #[error("role {role:?} agent must not be empty")]
    EmptyRoleAgent { role: RoleId },
    #[error("step {step:?} references unknown role {role:?}")]
    UnknownRole { step: StepId, role: RoleId },
    #[error("step {step:?} has an empty transition status")]
    EmptyTransitionStatus { step: StepId },
    #[error("step {step:?} status {status:?} targets unknown step {target:?}")]
    UnknownTransitionTarget {
        step: StepId,
        status: Status,
        target: StepId,
    },
    #[error("step {step:?} returned status {status:?} with no transition")]
    UnknownRuntimeTransition { step: StepId, status: Status },
    #[error("invalid action: {0}")]
    InvalidAction(String),
    #[error("recoverable action failure: {0}")]
    RecoverableAction(String),
}

impl WorkflowError {
    /// Whether the runner should retry the current step for this failure.
    ///
    /// Only [`WorkflowError::RecoverableAction`] is retryable; graph/definition
    /// and generic invalid-action errors are terminal.
    pub fn recoverable(&self) -> bool {
        matches!(self, WorkflowError::RecoverableAction(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_recoverable_action_is_recoverable() {
        assert!(WorkflowError::RecoverableAction("nudge me".to_string()).recoverable());
        assert!(!WorkflowError::InvalidAction("bad".to_string()).recoverable());
        assert!(
            !WorkflowError::UnknownStep {
                step: "plan".to_string()
            }
            .recoverable()
        );
        assert!(
            !WorkflowError::UnknownRuntimeTransition {
                step: "plan".to_string(),
                status: "weird".to_string()
            }
            .recoverable()
        );
    }
}
