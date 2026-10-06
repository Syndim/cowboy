use crate::{RoleId, Status, StepId, WorkflowId};

pub type Result<T, E = WorkflowError> = std::result::Result<T, E>;

/// Safe, stable categories for incomplete agent actions. Never embed backend text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentFailureCategory {
    DeclaredBlocked,
    Connection,
    Session,
    Prompt,
    InvalidResult,
    Interrupted,
}

impl AgentFailureCategory {
    pub fn question(self) -> &'static str {
        match self {
            Self::DeclaredBlocked => {
                "The agent declared it cannot finish this step. What missing context, instructions, or operator decision would let it continue safely?"
            }
            Self::Connection => {
                "The agent could not start or initialize. What setup or operator decision is required before attempting this step again?"
            }
            Self::Session => {
                "The agent could not establish its session. What session context or operator decision is needed before continuing?"
            }
            Self::Prompt => {
                "The agent prompt failed or timed out. Check any work already performed: what instructions allow this incomplete step to continue without repeating side effects?"
            }
            Self::InvalidResult => {
                "The agent did not produce a valid workflow result. What additional instructions are needed to finish the incomplete step?"
            }
            Self::Interrupted => {
                "The previous continuation was interrupted. Verify its side effects before providing instructions to continue this incomplete step."
            }
        }
    }
}

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
    #[error("agent action incomplete ({category:?})")]
    AgentFailure { category: AgentFailureCategory },
    #[error("agent security or process ownership rejected human recovery")]
    AgentUnsafe,
    #[error("agent result was not durably committed; manual reconciliation required")]
    AgentCommitFailed,
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
