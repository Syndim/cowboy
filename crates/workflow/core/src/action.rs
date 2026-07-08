use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{RoleId, Status};

/// Declarative action returned by a Lua `step.run(ctx)` function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum StepAction {
    /// Run an agent with a role and prompt, then normalize the agent output.
    Agent(AgentAction),
    /// Run one command-line program directly with explicit arguments.
    Command(CommandAction),
    /// Complete the step immediately with a status and optional data.
    Status(StatusAction),
    /// Pause the run and ask the user for input.
    AskUser(AskUserAction),
    /// Fail the run immediately with a reason.
    Fail(FailAction),
}

impl StepAction {
    pub fn action_name(&self) -> &'static str {
        match self {
            Self::Agent(_) => "agent",
            Self::Command(_) => "command",
            Self::Status(_) => "status",
            Self::AskUser(_) => "ask_user",
            Self::Fail(_) => "fail",
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

/// Request to execute one command-line program directly, without a shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAction {
    /// Program executable name or path passed to the OS process spawner.
    pub program: String,
    /// Exact argument vector passed to the program.
    #[serde(default)]
    pub args: Vec<String>,
    /// Output status used when the command exits with status code 0.
    #[serde(default = "default_command_success_status")]
    pub success_status: Status,
    /// Output status used when the command fails, cannot spawn, or times out.
    #[serde(default = "default_command_failure_status")]
    pub failure_status: Status,
    /// Optional wall-clock timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

pub fn default_command_success_status() -> Status {
    "success".to_string()
}

pub fn default_command_failure_status() -> Status {
    "failed".to_string()
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskUserAction {
    /// Stable prompt id shown in waiting state and answer validation.
    pub id: String,
    /// Message shown to the user.
    pub message: String,
    /// Optional finite set of accepted choices.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Output status used when the user answers.
    #[serde(default = "default_ask_user_status")]
    pub status: Status,
    /// Structured fields carried into the eventual ask-user step output.
    #[serde(default)]
    pub fields: Value,
}

fn default_ask_user_status() -> Status {
    "answered".to_string()
}

/// Request to fail the workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailAction {
    /// Human-readable failure reason.
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

    #[test]
    fn command_action_serializes_and_defaults() {
        let action = StepAction::Command(CommandAction {
            program: "printf".to_string(),
            args: vec!["hello".to_string()],
            success_status: "ok".to_string(),
            failure_status: "nope".to_string(),
            timeout_ms: Some(250),
        });

        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["action"], "command");
        assert_eq!(json["program"], "printf");
        assert_eq!(json["args"], serde_json::json!(["hello"]));
        assert_eq!(json["success_status"], "ok");
        assert_eq!(json["failure_status"], "nope");
        assert_eq!(json["timeout_ms"], 250);
        assert_eq!(action.action_name(), "command");

        let defaulted = serde_json::from_value::<StepAction>(serde_json::json!({
            "action": "command",
            "program": "true"
        }))
        .unwrap();
        let StepAction::Command(defaulted) = defaulted else {
            panic!("expected command action")
        };
        assert_eq!(defaulted.program, "true");
        assert!(defaulted.args.is_empty());
        assert_eq!(defaulted.success_status, "success");
        assert_eq!(defaulted.failure_status, "failed");
        assert_eq!(defaulted.timeout_ms, None);
    }

    #[test]
    fn deserializing_suspend_is_unknown() {
        let err = serde_json::from_value::<StepAction>(serde_json::json!({
            "action": "suspend",
            "reason": "pause",
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn action_names_cover_remaining_variants() {
        assert_eq!(
            StepAction::Agent(AgentAction {
                role: "developer".to_string(),
                prompt: "do it".to_string(),
                output: None,
            })
            .action_name(),
            "agent"
        );
        assert_eq!(
            StepAction::Command(CommandAction {
                program: "echo".to_string(),
                args: Vec::new(),
                success_status: "success".to_string(),
                failure_status: "failed".to_string(),
                timeout_ms: None,
            })
            .action_name(),
            "command"
        );
        assert_eq!(
            StepAction::AskUser(AskUserAction {
                id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: Vec::new(),
                status: "answered".to_string(),
                fields: Value::Null,
            })
            .action_name(),
            "ask_user"
        );
        assert_eq!(
            StepAction::Fail(FailAction {
                reason: "bad".to_string(),
            })
            .action_name(),
            "fail"
        );
    }
}
