use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{RoleId, Status};

/// Declarative action returned by a Lua `step.run(ctx)` function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum StepAction {
    /// Run an agent with a role and prompt, then normalize the agent output.
    Agent(AgentAction),
    /// Complete the step immediately with a status and optional data.
    Status(StatusAction),
    /// Pause the run and ask the user for input.
    AskUser(AskUserAction),
    /// Fail the run immediately with a reason.
    Fail(FailAction),
    /// Suspend the run without marking it failed.
    Suspend(SuspendAction),
}

impl StepAction {
    pub fn action_name(&self) -> &'static str {
        match self {
            Self::Agent(_) => "agent",
            Self::Status(_) => "status",
            Self::AskUser(_) => "ask_user",
            Self::Fail(_) => "fail",
            Self::Suspend(_) => "suspend",
        }
    }
}

/// Request to execute an agent-backed step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentAction {
    /// Role id whose instructions/persona should be used for the agent run.
    pub role: RoleId,
    /// Fully rendered prompt sent to the agent.
    pub prompt: String,
    /// Optional expected output shape used to instruct/validate the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputSpec>,
}

/// Expected structured output from an agent action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputSpec {
    /// Allowed status values for the resulting `StepOutput`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<Status>,
    /// Field specification for the structured output body.
    #[serde(default)]
    pub fields: Value,
}

/// Immediate non-agent step result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusAction {
    /// Status used by workflow routing.
    pub status: Status,
    /// Structured fields exposed to later steps as `ctx.prev.fields`.
    #[serde(default)]
    pub fields: Value,
    /// Optional human-readable body exposed to later steps as `ctx.prev.body`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
}

/// Request to pause and ask the user for input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskUserAction {
    /// Stable input id used as key in `ctx.resume` after the answer arrives.
    pub id: String,
    /// Message shown to the user.
    pub message: String,
    /// Optional finite set of accepted choices.
    #[serde(default)]
    pub choices: Vec<String>,
}

/// Request to fail the workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailAction {
    /// Human-readable failure reason.
    pub reason: String,
}

/// Request to suspend the workflow run without treating it as failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspendAction {
    /// Human-readable suspension reason.
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_tagged_action() {
        let action = StepAction::Status(StatusAction {
            status: "success".to_string(),
            fields: serde_json::json!({"ok": true}),
            body: "done".to_string(),
        });

        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["action"], "status");
        assert_eq!(json["status"], "success");
        assert_eq!(action.action_name(), "status");
    }
}
