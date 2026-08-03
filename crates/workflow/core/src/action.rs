use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{RoleId, Status};

/// Named field values exposed to later steps, e.g. as `ctx.prev.fields`.
pub type Fields = BTreeMap<String, Value>;

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
    /// Invoke another catalog workflow as a durable child run.
    Workflow(WorkflowAction),
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
            Self::Workflow(_) => "workflow",
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
    /// Declared structured output fields, keyed by field name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Field>,
}

/// Declared type, requirement, and prompt guidance for one structured output field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// Declared value type used to validate and describe the field.
    #[serde(rename = "type")]
    pub field_type: FieldType,
    /// Whether the field must be present and non-null in the structured output.
    #[serde(default)]
    pub required: bool,
    /// Prompt guidance describing what the agent should return for this field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Supported declared value types for a structured output [`Field`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Array,
    Boolean,
    Number,
    String,
}

impl FieldType {
    /// Human-readable/wire name for this type, e.g. for prompt guidance.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Array => "array",
            Self::Boolean => "boolean",
            Self::Number => "number",
            Self::String => "string",
        }
    }
}

/// Request to execute one command-line program directly, without a shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAction {
    /// Program executable name or path passed to the OS process spawner.
    pub program: String,
    /// Exact argument vector passed to the program.
    #[serde(default)]
    pub args: Vec<String>,
    /// Maps an exit code (stringified, e.g. `"0"`) to the resulting output
    /// status. The catch-all `"_"` key handles any exit code without an
    /// exact match, plus spawn errors and timeouts, which have no exit code.
    #[serde(default = "default_command_status_map")]
    pub status_map: BTreeMap<String, Status>,
    /// Optional wall-clock timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl CommandAction {
    /// Resolves the output status for a finished command using
    /// `status_map`. Exit-code lookup is skipped in favor of the catch-all
    /// `"_"` entry for timeouts and spawn failures, which have no
    /// meaningful exit code. A missing catch-all falls back to `"failed"`.
    pub fn status_for(&self, exit_code: Option<i32>, timed_out: bool, spawn_error: bool) -> Status {
        let code_key = (!timed_out && !spawn_error).then_some(exit_code).flatten();
        code_key
            .and_then(|code| self.status_map.get(&code.to_string()))
            .or_else(|| self.status_map.get("_"))
            .cloned()
            .unwrap_or_else(|| "failed".to_string())
    }
}

pub fn default_command_status_map() -> BTreeMap<String, Status> {
    BTreeMap::from([
        ("0".to_string(), "success".to_string()),
        ("_".to_string(), "failed".to_string()),
    ])
}

/// Immediate non-agent step result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusAction {
    /// Status used by workflow routing.
    pub status: Status,
    /// Structured fields exposed to later steps as `ctx.prev.fields`.
    #[serde(default)]
    pub fields: Fields,
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
    pub choices: Vec<Choice>,
    /// Output status used when the user answers.
    #[serde(default = "default_ask_user_status")]
    pub status: Status,
    /// Structured fields carried into the eventual ask-user step output.
    #[serde(default)]
    pub fields: Fields,
}

/// One accepted answer for an [`AskUserAction`], with a stable key matched
/// against the user's answer and a human-readable description shown to them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// Stable key matched against the user's answer text.
    pub key: String,
    /// Human-readable description of what this choice means.
    pub description: String,
}

impl Choice {
    /// Human-readable "key: description" label for UI/event display.
    pub fn label(&self) -> String {
        format!("{}: {}", self.key, self.description)
    }
}

fn default_ask_user_status() -> Status {
    "answered".to_string()
}

/// Request to invoke another workflow from the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAction {
    /// Catalog workflow id, matching the id shown by `/workflows`.
    pub workflow: String,
    /// Exact initial request supplied to the child workflow.
    pub request: String,
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
            fields: Fields::from([("ok".to_string(), serde_json::json!(true))]),
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
            status_map: BTreeMap::from([
                ("0".to_string(), "ok".to_string()),
                ("_".to_string(), "nope".to_string()),
            ]),
            timeout_ms: Some(250),
        });

        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["action"], "command");
        assert_eq!(json["program"], "printf");
        assert_eq!(json["args"], serde_json::json!(["hello"]));
        assert_eq!(
            json["status_map"],
            serde_json::json!({"0": "ok", "_": "nope"})
        );
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
        assert_eq!(defaulted.status_map, default_command_status_map());
        assert_eq!(defaulted.timeout_ms, None);
    }

    #[test]
    fn command_action_status_for_resolves_exit_code_or_catch_all() {
        let action = CommandAction {
            program: "echo".to_string(),
            args: Vec::new(),
            status_map: BTreeMap::from([
                ("0".to_string(), "clean".to_string()),
                ("1".to_string(), "dirty".to_string()),
                ("_".to_string(), "unknown".to_string()),
            ]),
            timeout_ms: None,
        };

        assert_eq!(action.status_for(Some(0), false, false), "clean");
        assert_eq!(action.status_for(Some(1), false, false), "dirty");
        assert_eq!(action.status_for(Some(2), false, false), "unknown");
        assert_eq!(action.status_for(None, false, true), "unknown");
        assert_eq!(action.status_for(Some(0), true, false), "unknown");

        let no_catch_all = CommandAction {
            program: "echo".to_string(),
            args: Vec::new(),
            status_map: BTreeMap::from([("0".to_string(), "clean".to_string())]),
            timeout_ms: None,
        };
        assert_eq!(no_catch_all.status_for(Some(9), false, false), "failed");
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
    fn workflow_action_serializes_and_names_variant() {
        let action = StepAction::Workflow(WorkflowAction {
            workflow: "review/security".to_string(),
            request: "  Review this\nexactly  ".to_string(),
        });

        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "action": "workflow",
                "workflow": "review/security",
                "request": "  Review this\nexactly  ",
            })
        );
        assert_eq!(action.action_name(), "workflow");
        assert_eq!(serde_json::from_value::<StepAction>(json).unwrap(), action);
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
                status_map: default_command_status_map(),
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
                fields: Fields::new(),
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
