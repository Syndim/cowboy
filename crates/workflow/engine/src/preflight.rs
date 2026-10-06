use std::sync::Arc;

use cowboy_workflow_core::{
    ActionResult, ExecutionContext, Result, Run, StepAction, WorkflowDefinition, WorkflowError,
    ordered_user_inputs,
};
use cowboy_workflow_store::SqliteWorkflowStore;
use serde_json::{Value, json};

use crate::{CommandActionRunner, RuntimeConfig};

const MAX_COMMAND_MS: u64 = 5_000;
const MAX_SUPPLEMENT_BYTES: usize = 4_096;

/// Evaluate the declared guard from the persisted workflow snapshot, then let
/// its verifier interpret the fresh authority result. Nothing here advances a
/// workflow step or lets the human response choose a host command.
pub(crate) async fn check(
    run: &Run,
    definition: &WorkflowDefinition,
    source: &cowboy_workflow_core::WorkflowSourceSnapshot,
    guard_id: &str,
    answer: &str,
    store: &SqliteWorkflowStore,
    config: &RuntimeConfig,
) -> Result<Arc<str>> {
    let guard = definition.steps.get(guard_id).ok_or_else(rejected)?;
    let previous = match &run.step.head {
        Some(hash) => Some(store.load_step_record(hash).await?),
        None => None,
    };

    let prompts = store.load_user_prompts(&run.id).await?;
    // The answer is intentionally absent while the trusted snapshot selects
    // the command; only its verifier receives the answer afterward.
    let prev = previous.as_ref().and_then(|record| record.output.as_ref().map(|output| json!({
        "record_id": record.id,
        "step": record.step,
        "action": record.action,
        "output": { "status": output.status, "fields": output.fields, "body": output.body, "raw": output.raw },
        "status": output.status,
        "fields": output.fields,
        "body": output.body,
        "raw": output.raw,
    })));
    let mut ctx = json!({
        "request": run.original_request,
        "user_inputs": ordered_user_inputs(run, &prompts),
        "run_id": run.id,
        "workflow": { "name": definition.name, "head": definition.head },
        "current_step": guard.id,
        "incomplete_step": run.step.next,
        "step": { "id": guard.id, "role": guard.role, "properties": guard.properties },
        "resume": Value::Null,
        "prev": prev,
        "steps_executed": run.step.executed,
        "system": crate::system::system_context(),
    });
    let StepAction::Command(command) = cowboy_workflow_lua::run_step(source, guard_id, ctx.clone())
        .map_err(|_| rejected())?
        .action
    else {
        return Err(rejected());
    };

    if !matches!(command.timeout_ms, Some(1..=MAX_COMMAND_MS)) {
        return Err(rejected());
    }

    let context = ExecutionContext {
        run_id: run.id.clone(),
        step_id: guard.id.clone(),
        step_record_id: format!("{}-pre-input", run.id),
        prev: run.step.head.clone(),
        role: None,
        attempt: 1,
        retry_reason: None,
        agent_human_input: None,
        initial_input_kind: run.initial_input_kind(),
        step_visit: run.step.visits.get(&run.step.next).copied().unwrap_or(0),
        original_request: run.original_request.clone(),
        run_created_at: run.created_at,
        user_prompts: prompts,
    };
    let ActionResult::Completed(record) =
        CommandActionRunner::new(&config.cwd, config.allowed_env.clone())
            .run(command, context)
            .await
            .map_err(|_| rejected())?
    else {
        return Err(rejected());
    };

    let output = record.output.ok_or_else(rejected)?;
    let fields = output.fields.as_object().ok_or_else(rejected)?;
    if fields.get("success") != Some(&Value::Bool(true))
        || fields.get("timed_out") != Some(&Value::Bool(false))
        || [
            "stdout_truncated",
            "stderr_truncated",
            "stdout_incomplete",
            "stderr_incomplete",
        ]
        .iter()
        .any(|field| fields.get(*field) != Some(&Value::Bool(false)))
        || fields
            .get("stdout")
            .and_then(Value::as_str)
            .is_none_or(|text| text.len() > MAX_SUPPLEMENT_BYTES)
    {
        return Err(rejected());
    }

    let verification = ctx
        .as_object_mut()
        .expect("trusted hook context is an object");
    verification.insert("input".to_string(), Value::String(answer.to_string()));
    verification.insert(
        "authority".to_string(),
        json!({
            "status": output.status,
            "fields": output.fields,
            "body": output.body,
        }),
    );
    let verified =
        cowboy_workflow_lua::verify_pre_input(source, guard_id, ctx).map_err(|_| rejected())?;
    let StepAction::Status(verified) = verified.action else {
        return Err(rejected());
    };

    if verified.status != "ready" || verified.fields.len() != 1 {
        return Err(rejected());
    }

    let proof = verified
        .fields
        .get("context")
        .and_then(Value::as_str)
        .ok_or_else(rejected)?;
    if proof.len() > MAX_SUPPLEMENT_BYTES || proof.chars().any(char::is_control) {
        return Err(rejected());
    }

    if proof.is_empty() {
        return Ok(Arc::from(answer));
    }

    let content = format!("{answer}\n\n## Verified context\n\n{proof}");
    if content.len() > MAX_SUPPLEMENT_BYTES {
        return Err(rejected());
    }

    Ok(Arc::from(content))
}

fn rejected() -> WorkflowError {
    WorkflowError::InvalidAction("pre-input guard did not validate the current context".to_string())
}
