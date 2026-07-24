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
        assert_eq!(action.output.unwrap().statuses, vec!["success", "failed"]);
    }

    #[test]
    fn converts_ask_user_action() {
        let source = snapshot(
            r#"
            local step = step("approve")
            step.run = function(ctx)
              return action.ask_user { id = "approval", message = "Approve?", choices = { "yes", "no" }, status = "accepted", fields = { plan = "ship" } }
            end
            return workflow("wf", step)
            "#,
        );
        let result = run_step(&source, "approve", serde_json::json!({})).unwrap();
        let StepAction::AskUser(action) = result.action else {
            panic!("expected ask_user action")
        };
        assert_eq!(action.id, "approval");
        assert_eq!(action.choices, vec!["yes", "no"]);
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
                success_status = "ok",
                failure_status = "bad",
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
        assert_eq!(action.success_status, "ok");
        assert_eq!(action.failure_status, "bad");
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
        assert_eq!(action.success_status, "success");
        assert_eq!(action.failure_status, "failed");
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
                "empty success status",
                "return action.command { program = \"echo\", success_status = \"\" }",
                "success_status",
            ),
            (
                "empty failure status",
                "return action.command { program = \"echo\", failure_status = \"\" }",
                "failure_status",
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
