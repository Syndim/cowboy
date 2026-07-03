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
}
