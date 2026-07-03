use chrono::Utc;
use cowboy_workflow_core::{
    ActionResult, ExecutionContext, StatusAction, StepDetail, StepInput, StepOutput, StepRecord,
};
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct StatusActionRunner;

impl StatusActionRunner {
    pub fn run(&self, action: StatusAction, context: ExecutionContext) -> ActionResult {
        let now = Utc::now();
        ActionResult::completed(StepRecord {
            id: context.step_record_id,
            prev: context.prev,
            step: context.step_id,
            action: "status".to_string(),
            input: StepInput {
                prompt: None,
                context: Value::Null,
            },
            output: Some(StepOutput {
                status: action.status,
                fields: action.fields,
                body: action.body,
                raw: Value::Null,
            }),
            detail: StepDetail {
                backend: None,
                session_id: None,
                duration_ms: 0,
                turn_count: 0,
                usage: None,
            },
            started_at: now,
            completed_at: Some(now),
        })
    }
}
