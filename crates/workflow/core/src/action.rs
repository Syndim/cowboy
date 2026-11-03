use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{Result, RoleId, Status, WorkflowError};

const WORKFLOW_INVOCATION_NAMESPACE: Uuid = Uuid::from_u128(0xd31e45ed_1aac_578f_9314_765ee417df28);

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
    /// Pause the run and wait for external input.
    WaitForInput(WaitForInputAction),
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
            Self::WaitForInput(_) => "wait_for_input",
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
    /// Current turn payload. For legacy actions this remains the full task prompt.
    pub prompt: String,
    /// Optional durable task contract used for minimal reused-session prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<AgentTaskContract>,
    /// Optional expected output shape used to instruct/validate the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputSpec>,
}

/// Stable task definition and recovery state for one agent responsibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTaskContract {
    /// Stable key scoped to one run and role.
    pub key: String,
    /// Static instructions sent once for each distinct task contract.
    pub instructions: String,
    /// Durable current state needed when a backend session must be recreated.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recovery_context: String,
    /// Minimal payload for the current dispatch. Excluded from the static fingerprint.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub turn: String,
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
    /// Workflow fields carried through the command result alongside command
    /// execution metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: Fields,
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

/// Request to pause and wait for external input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitForInputAction {
    /// Stable input id shown in waiting state and input validation.
    pub id: String,
    /// Message shown to the user.
    pub message: String,
    /// Optional finite set of accepted choices.
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// Output status used when the user inputs.
    #[serde(default = "default_wait_for_input_status")]
    pub status: Status,
    /// Structured fields carried into the eventual wait-for-input step output.
    #[serde(default)]
    pub fields: Fields,
}

/// One accepted input for an [`WaitForInputAction`], with a stable key matched
/// against the user's input and a human-readable description shown to them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// Stable key matched against the user's input text.
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

fn default_wait_for_input_status() -> Status {
    "provided".to_string()
}

/// Request to invoke another workflow from the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAction {
    pub workflows: Vec<WorkflowCall>,
}

impl WorkflowAction {
    pub fn validate(&self) -> Result<()> {
        if self.workflows.is_empty() {
            return Err(WorkflowError::InvalidAction(
                "workflow action workflows must contain at least one entry".to_string(),
            ));
        }
        let mut ids = BTreeSet::new();
        for (index, call) in self.workflows.iter().enumerate() {
            if call.id.trim().is_empty() {
                return Err(WorkflowError::InvalidAction(format!(
                    "workflow action workflows[{}].id must be a nonblank string",
                    index + 1
                )));
            }
            if !ids.insert(&call.id) {
                return Err(WorkflowError::InvalidAction(format!(
                    "workflow action workflows contains duplicate id {:?}",
                    call.id
                )));
            }
            if call.workflow.trim().is_empty() {
                return Err(WorkflowError::InvalidAction(format!(
                    "workflow action workflows[{}].workflow must be a nonblank string",
                    index + 1
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowCall {
    pub id: String,
    pub workflow: String,
    pub request: String,
}

pub fn workflow_branch_invocation_id(
    parent_run_id: &str,
    step_id: &str,
    previous_head: Option<&str>,
    branch_id: &str,
) -> Uuid {
    let mut name = b"cowboy.workflow.invocation.v2\0".to_vec();
    for component in [parent_run_id, step_id, branch_id] {
        name.extend_from_slice(&(component.len() as u64).to_be_bytes());
        name.extend_from_slice(component.as_bytes());
    }
    match previous_head {
        None => name.push(0),
        Some(head) => {
            name.push(1);
            name.extend_from_slice(&(head.len() as u64).to_be_bytes());
            name.extend_from_slice(head.as_bytes());
        }
    }
    Uuid::new_v5(&WORKFLOW_INVOCATION_NAMESPACE, &name)
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
            fields: Fields::from([(
                "plan_doc".to_string(),
                serde_json::json!("docs/plans/test.md"),
            )]),
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
        assert_eq!(json["fields"]["plan_doc"], "docs/plans/test.md");
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
        assert!(defaulted.fields.is_empty());
        assert_eq!(defaulted.status_map, default_command_status_map());
        assert_eq!(defaulted.timeout_ms, None);
    }

    #[test]
    fn command_action_status_for_resolves_exit_code_or_catch_all() {
        let action = CommandAction {
            program: "echo".to_string(),
            args: Vec::new(),
            fields: Fields::new(),
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
            fields: Fields::new(),
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
            workflows: vec![WorkflowCall {
                id: "security".to_string(),
                workflow: "review/security".to_string(),
                request: "  Review this\nexactly  ".to_string(),
            }],
        });

        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "action": "workflow",
                "workflows": [{
                    "id": "security",
                    "workflow": "review/security",
                    "request": "  Review this\nexactly  ",
                }],
            })
        );
        assert_eq!(action.action_name(), "workflow");
        assert_eq!(serde_json::from_value::<StepAction>(json).unwrap(), action);
    }

    #[test]
    fn workflow_branch_invocation_id_preserves_v2_encoding() {
        for (parent, step, head, branch, expected) in [
            (
                "run-parent",
                "delegate",
                None,
                "child",
                "f7513f4c-10d2-533a-a74e-f0568809e63f",
            ),
            (
                "run-parent",
                "delegate",
                Some("abc123"),
                "child",
                "891918aa-8604-51e3-a7e9-eb3a259136e5",
            ),
            (
                "run-parent",
                "delegate",
                Some(""),
                "child",
                "026f26c9-fc5c-57fd-9b43-7835a05fa6a7",
            ),
            (
                "親",
                "呼出",
                Some("前"),
                "枝",
                "0e7757fc-d890-5627-9611-eccc4f0b3a60",
            ),
        ] {
            assert_eq!(
                workflow_branch_invocation_id(parent, step, head, branch).to_string(),
                expected
            );
        }
    }

    #[test]
    fn workflow_branch_invocation_ids_are_deterministic_and_unambiguous() {
        let parent = "run-parent";
        let step = "delegate";
        let first = workflow_branch_invocation_id(parent, step, None, "child");
        assert_eq!(
            first,
            workflow_branch_invocation_id(parent, step, None, "child")
        );
        let ids = [
            first,
            workflow_branch_invocation_id(parent, step, None, "sibling"),
            workflow_branch_invocation_id("run-other", step, None, "child"),
            workflow_branch_invocation_id(parent, "other-step", None, "child"),
            workflow_branch_invocation_id(parent, step, Some("abc123"), "child"),
            workflow_branch_invocation_id(parent, step, Some(""), "child"),
            workflow_branch_invocation_id("ab", "c", None, "child"),
            workflow_branch_invocation_id("a", "bc", None, "child"),
            workflow_branch_invocation_id(parent, "a", Some("bc"), "d"),
            workflow_branch_invocation_id(parent, "ab", Some("c"), "d"),
            workflow_branch_invocation_id(parent, step, Some("a"), "bc"),
            workflow_branch_invocation_id(parent, step, Some("ab"), "c"),
        ];
        assert_eq!(ids.into_iter().collect::<BTreeSet<_>>().len(), ids.len());
    }

    fn workflow_action() -> WorkflowAction {
        WorkflowAction {
            workflows: vec![WorkflowCall {
                id: "security".to_string(),
                workflow: "review/security".to_string(),
                request: "  Review this\nexactly  ".to_string(),
            }],
        }
    }

    #[test]
    fn workflow_action_deserializes_calls() {
        let action: StepAction = serde_json::from_value(serde_json::json!({
            "action": "workflow",
            "workflows": [{
                "id": "security",
                "workflow": "review/security",
                "request": "  Review this\nexactly  ",
            }],
        }))
        .unwrap();
        assert_eq!(action, StepAction::Workflow(workflow_action()));
    }

    #[test]
    fn workflow_action_rejects_legacy_unknown_and_missing_fields() {
        let valid = serde_json::to_value(workflow_action()).unwrap();
        for field in ["workflow", "request", "concurrency", "concurency", "extra"] {
            let mut action = valid.clone();
            action[field] = serde_json::json!("unexpected");
            let err = serde_json::from_value::<WorkflowAction>(action.clone()).unwrap_err();
            assert!(err.to_string().contains("unknown field"), "{err}");
            action["action"] = serde_json::json!("workflow");
            assert!(serde_json::from_value::<StepAction>(action).is_err());
        }
        for field in ["extra", "requests", "action"] {
            let mut action = valid.clone();
            action["workflows"][0][field] = serde_json::json!("unexpected");
            let err = serde_json::from_value::<WorkflowAction>(action).unwrap_err();
            assert!(err.to_string().contains("unknown field"), "{err}");
        }
        for field in ["id", "workflow", "request"] {
            let mut action = valid.clone();
            action["workflows"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            let err = serde_json::from_value::<WorkflowAction>(action).unwrap_err();
            assert!(err.to_string().contains("missing field"), "{err}");
        }
        for action in [
            serde_json::json!({"workflow": "child", "request": "legacy"}),
            serde_json::json!({"concurrency": 4}),
        ] {
            assert!(serde_json::from_value::<WorkflowAction>(action).is_err());
        }
    }

    #[test]
    fn workflow_action_rejects_concurrency_as_unknown_field() {
        for concurrency in [serde_json::json!(4), serde_json::json!(0), Value::Null] {
            let mut action = serde_json::to_value(workflow_action()).unwrap();
            action["concurrency"] = concurrency;
            let err = serde_json::from_value::<WorkflowAction>(action.clone()).unwrap_err();
            assert!(
                err.to_string().contains("unknown field `concurrency`"),
                "{err}"
            );
            action["action"] = serde_json::json!("workflow");
            let err = serde_json::from_value::<StepAction>(action).unwrap_err();
            assert!(
                err.to_string().contains("unknown field `concurrency`"),
                "{err}"
            );
        }
    }

    #[test]
    fn workflow_action_accepts_large_lists_and_preserves_calls() {
        for count in [1, 128, 129, 256] {
            let mut action = workflow_action();
            action.workflows = (0..count)
                .map(|index| WorkflowCall {
                    id: format!("branch-{index}"),
                    workflow: " review/security ".to_string(),
                    request: if index % 2 == 0 {
                        "  Review this\nexactly  "
                    } else {
                        ""
                    }
                    .to_string(),
                })
                .collect();
            let original = action.clone();
            action.validate().unwrap();
            assert_eq!(action, original);
            let json = serde_json::to_value(&action).unwrap();
            assert_eq!(
                serde_json::from_value::<WorkflowAction>(json).unwrap(),
                action
            );
        }
    }

    #[test]
    fn workflow_action_rejects_invalid_programmatic_values() {
        let mut cases = Vec::new();
        let mut action = workflow_action();
        action.workflows.clear();
        cases.push(action);
        for blank in ["", " \t\n", "\u{2003}"] {
            let mut action = workflow_action();
            action.workflows[0].id = blank.to_string();
            cases.push(action);
            let mut action = workflow_action();
            action.workflows[0].workflow = blank.to_string();
            cases.push(action);
        }
        let mut action = workflow_action();
        action.workflows.push(action.workflows[0].clone());
        cases.push(action);
        for action in cases {
            assert!(
                matches!(action.validate(), Err(WorkflowError::InvalidAction(_))),
                "accepted invalid workflow action: {action:?}"
            );
        }
    }

    #[test]
    fn action_names_cover_remaining_variants() {
        assert_eq!(
            StepAction::Agent(AgentAction {
                role: "developer".to_string(),
                prompt: "do it".to_string(),
                task: None,
                output: None,
            })
            .action_name(),
            "agent"
        );
        assert_eq!(
            StepAction::Command(CommandAction {
                program: "echo".to_string(),
                args: Vec::new(),
                fields: Fields::new(),
                status_map: default_command_status_map(),
                timeout_ms: None,
            })
            .action_name(),
            "command"
        );
        assert_eq!(
            StepAction::WaitForInput(WaitForInputAction {
                id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: Vec::new(),
                status: "provided".to_string(),
                fields: Fields::new(),
            })
            .action_name(),
            "wait_for_input"
        );
        assert_eq!(
            StepAction::Fail(FailAction {
                reason: "bad".to_string(),
            })
            .action_name(),
            "fail"
        );
    }

    #[test]
    fn structured_agent_task_contract_round_trips() {
        let action = StepAction::Agent(AgentAction {
            role: "developer".to_string(),
            prompt: "Changes needed:\n- fix retry".to_string(),
            task: Some(AgentTaskContract {
                key: "implementation".to_string(),
                instructions: "Implement the approved plan.".to_string(),
                recovery_context: "Plan doc: docs/plans/retry.md".to_string(),
                turn: "Changes needed:\n- fix retry".to_string(),
            }),
            output: Some(OutputSpec {
                statuses: vec!["implemented".to_string()],
                fields: BTreeMap::new(),
            }),
        });

        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["task"]["key"], "implementation");
        assert_eq!(serde_json::from_value::<StepAction>(json).unwrap(), action);
    }

    #[test]
    fn agent_action_structured_contract_round_trips() {
        structured_agent_task_contract_round_trips();
    }
}
