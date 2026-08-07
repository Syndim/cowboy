use std::sync::Arc;

use cowboy_workflow_core::{StepAction, WorkflowSourceSnapshot};
use mlua::{Function, Table, Value};
use parking_lot::Mutex;

use crate::api::ImportMode;
use crate::convert::{action_from_value, json_to_lua};
use crate::imports::normalize_relative_path;
use crate::loader::setup_lua;
use crate::{Error, Result};

/// Result of running one Lua step function.
#[derive(Debug, Clone, PartialEq)]
pub struct RunStepResult {
    /// Declarative action returned by `step.run(ctx)`.
    pub action: StepAction,
}

/// Execute one step's `run(ctx)` function from a snapshotted workflow source.
pub fn run_step(
    bundle: &WorkflowSourceSnapshot,
    step_id: &str,
    ctx: serde_json::Value,
) -> Result<RunStepResult> {
    let lua = setup_lua(ImportMode::Snapshot {
        sources: Arc::new(Mutex::new(bundle.files.clone())),
    })?;
    let entry = normalize_relative_path(&bundle.entry)?;
    let source = bundle
        .files
        .get(&entry)
        .ok_or_else(|| Error::MissingEntry(entry.clone()))?;
    lua.load(source).set_name(&entry).eval::<Value>()?;
    let steps: Table = lua.globals().get("__cowboy_steps")?;
    let step: Table = steps.get(step_id)?;
    let run: Function = step.get("run")?;
    let ctx = json_to_lua(&lua, &ctx)?;
    let value = run.call::<Value>(ctx)?;
    Ok(RunStepResult {
        action: action_from_value(value)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowboy_workflow_core::{Choice, default_command_status_map};
    use std::collections::BTreeMap;

    fn snapshot(source: &str) -> WorkflowSourceSnapshot {
        WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".into(),
            files: BTreeMap::from([("main.lua".into(), source.into())]),
        }
    }

    #[test]
    fn converts_agent_action() {
        let source = snapshot(
            r#"
            local role = role("developer", "implement things")
            local step = step("implement", { role = role })
            step.run = function(ctx)
              return action.agent {
                role = role,
                prompt = "Implement " .. ctx.request,
                output = { status = { "success", "failed" }, fields = { summary = "string" } }
              }
            end
            return workflow("wf", step)
            "#,
        );
        let result = run_step(
            &source,
            "implement",
            serde_json::json!({"request": "feature"}),
        )
        .unwrap();
        let StepAction::Agent(action) = result.action else {
            panic!("expected agent action")
        };
        assert_eq!(action.role, "developer");
        assert_eq!(action.prompt, "Implement feature");
        assert_eq!(action.task, None);
        assert_eq!(action.output.unwrap().statuses, vec!["success", "failed"]);
    }

    #[test]
    fn converts_structured_agent_task_contract() {
        let source = snapshot(
            r#"
            local role = role("developer", "implement things")
            local step = step("implement", { role = role })
            step.run = function(ctx)
              return action.agent {
                role = role,
                prompt = "Fix only the retry",
                task = {
                  key = "implementation",
                  instructions = "Implement the approved plan.",
                  recovery_context = "Plan doc: docs/plans/retry.md",
                },
              }
            end
            return workflow("wf", step)
            "#,
        );
        let result = run_step(&source, "implement", serde_json::json!({})).unwrap();
        let StepAction::Agent(action) = result.action else {
            panic!("expected agent action")
        };
        let task = action.task.unwrap();
        assert_eq!(task.key, "implementation");
        assert_eq!(task.instructions, "Implement the approved plan.");
        assert_eq!(task.recovery_context, "Plan doc: docs/plans/retry.md");
    }

    #[test]
    fn legacy_agent_prompt_remains_supported() {
        let source = snapshot(
            r#"
            local role = role("developer", "implement things")
            local step = step("implement", { role = role })
            step.run = function(ctx)
              return action.agent { role = role, prompt = "Legacy full prompt" }
            end
            return workflow("wf", step)
            "#,
        );
        let result = run_step(&source, "implement", serde_json::json!({})).unwrap();
        let StepAction::Agent(action) = result.action else {
            panic!("expected agent action")
        };
        assert_eq!(action.prompt, "Legacy full prompt");
        assert!(action.task.is_none());
    }

    #[test]
    fn rejects_blank_structured_agent_task_key() {
        let source = snapshot(
            r#"
            local role = role("developer", "implement things")
            local step = step("implement", { role = role })
            step.run = function(ctx)
              return action.agent {
                role = role,
                prompt = "turn",
                task = { key = " ", instructions = "task" },
              }
            end
            return workflow("wf", step)
            "#,
        );
        let err = run_step(&source, "implement", serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("task.key"));
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn agent_action_rejects_blank_structured_task_key() {
        rejects_blank_structured_agent_task_key();
    }

    #[test]
    fn agent_action_legacy_prompt_remains_supported() {
        legacy_agent_prompt_remains_supported();
    }

    #[test]
    fn converts_ask_user_action() {
        let source = snapshot(
            r#"
            local step = step("approve")
            step.run = function(ctx)
              return action.ask_user { id = "approval", message = "Approve?", choices = { yes = "Approve the release", no = "Reject the release" }, status = "accepted", fields = { plan = "ship" } }
            end
            return workflow("wf", step)
            "#,
        );
        let result = run_step(&source, "approve", serde_json::json!({})).unwrap();
        let StepAction::AskUser(action) = result.action else {
            panic!("expected ask_user action")
        };
        assert_eq!(action.id, "approval");
        assert_eq!(
            action.choices,
            vec![
                Choice {
                    key: "no".to_string(),
                    description: "Reject the release".to_string(),
                },
                Choice {
                    key: "yes".to_string(),
                    description: "Approve the release".to_string(),
                },
            ]
        );
        assert_eq!(action.status, "accepted");
        assert_eq!(action.fields["plan"], "ship");
    }

    #[test]
    fn workflow_action_converts_and_preserves_request() {
        let source = snapshot(
            r#"
            local step = step("delegate")
            step.run = function(ctx)
              return action.workflow {
                workflow = "review/security",
                request = "  Review this\nexactly  ",
              }
            end
            return workflow("wf", step)
            "#,
        );
        let result = run_step(&source, "delegate", serde_json::json!({})).unwrap();
        let StepAction::Workflow(action) = result.action else {
            panic!("expected workflow action")
        };
        assert_eq!(action.workflow, "review/security");
        assert_eq!(action.request, "  Review this\nexactly  ");
    }

    #[test]
    fn workflow_action_rejects_invalid_fields() {
        let cases = [
            ("missing workflow", r#"request = "x""#, "workflow"),
            (
                "blank workflow",
                r#"workflow = "  ", request = "x""#,
                "non-empty",
            ),
            (
                "non-string workflow",
                r#"workflow = 1, request = "x""#,
                "workflow",
            ),
            ("missing request", r#"workflow = "child""#, "request"),
            (
                "non-string request",
                r#"workflow = "child", request = 1"#,
                "request",
            ),
        ];

        for (name, fields, expected) in cases {
            let source = snapshot(&format!(
                r#"
                local step = step("delegate")
                step.run = function(ctx)
                  return action.workflow {{ {fields} }}
                end
                return workflow("wf", step)
                "#
            ));
            let err = run_step(&source, "delegate", serde_json::json!({})).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "{name}: expected error containing {expected:?}, got {err}"
            );
        }
    }

    #[test]
    fn converts_command_action() {
        let source = snapshot(
            r#"
            local step = step("run_command")
            step.run = function(ctx)
              return action.command {
                program = "printf",
                args = { "hello", ctx.request },
                fields = { plan_doc = "docs/plans/test.md" },
                status_map = { ["0"] = "ok", ["_"] = "bad" },
                timeout_ms = 1000,
              }
            end
            return workflow("wf", step)
            "#,
        );
        let result = run_step(
            &source,
            "run_command",
            serde_json::json!({"request": "world"}),
        )
        .unwrap();
        let StepAction::Command(action) = result.action else {
            panic!("expected command action")
        };
        assert_eq!(action.program, "printf");
        assert_eq!(action.args, vec!["hello", "world"]);
        assert_eq!(
            action.fields.get("plan_doc"),
            Some(&serde_json::json!("docs/plans/test.md"))
        );
        assert_eq!(
            action.status_map,
            BTreeMap::from([
                ("0".to_string(), "ok".to_string()),
                ("_".to_string(), "bad".to_string())
            ])
        );
        assert_eq!(action.timeout_ms, Some(1000));
    }

    #[test]
    fn command_action_defaults_optional_fields() {
        let source = snapshot(
            r#"
            local step = step("run_command")
            step.run = function(ctx)
              return action.command { program = "true" }
            end
            return workflow("wf", step)
            "#,
        );
        let result = run_step(&source, "run_command", serde_json::json!({})).unwrap();
        let StepAction::Command(action) = result.action else {
            panic!("expected command action")
        };
        assert_eq!(action.program, "true");
        assert!(action.args.is_empty());
        assert!(action.fields.is_empty());
        assert_eq!(action.status_map, default_command_status_map());
        assert_eq!(action.timeout_ms, None);
    }

    #[test]
    fn rejects_invalid_command_actions() {
        let cases = [
            (
                "missing program",
                "return action.command { args = { \"x\" } }",
                "program",
            ),
            (
                "empty program",
                "return action.command { program = \" \" }",
                "non-empty",
            ),
            (
                "non-table args",
                "return action.command { program = \"echo\", args = \"x\" }",
                "args",
            ),
            (
                "non-string arg",
                "return action.command { program = \"echo\", args = { 1 } }",
                "args",
            ),
            (
                "non-table status_map",
                "return action.command { program = \"echo\", status_map = \"x\" }",
                "status_map",
            ),
            (
                "invalid status_map key",
                "return action.command { program = \"echo\", status_map = { foo = \"ok\" } }",
                "status_map",
            ),
            (
                "empty status_map value",
                "return action.command { program = \"echo\", status_map = { [\"0\"] = \"\" } }",
                "status_map",
            ),
            (
                "zero timeout",
                "return action.command { program = \"echo\", timeout_ms = 0 }",
                "timeout_ms",
            ),
            (
                "float timeout",
                "return action.command { program = \"echo\", timeout_ms = 0.5 }",
                "timeout_ms",
            ),
        ];

        for (name, command, expected) in cases {
            let source = snapshot(&format!(
                r#"
                local step = step("run_command")
                step.run = function(ctx)
                  {command}
                end
                return workflow("wf", step)
                "#
            ));
            let err = run_step(&source, "run_command", serde_json::json!({})).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "{name}: expected error containing {expected:?}, got {err}"
            );
        }
    }

    #[test]
    fn action_suspend_is_unavailable() {
        let source = snapshot(
            r#"
            local step = step("pause")
            step.run = function(ctx)
              return action.suspend { reason = "pause" }
            end
            return workflow("wf", step)
            "#,
        );

        let err = run_step(&source, "pause", serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("suspend"));
    }
}
