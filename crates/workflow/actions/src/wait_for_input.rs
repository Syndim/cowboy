use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cowboy_workflow_core::{
    ActionResult, ExecutionContext, Result, ResumeCallback, ResumeCallbackHandler, ResumeInput,
    RunStatus, StepDetail, StepInput, StepOutput, StepRecord, WaitForInputAction, WorkflowError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const WAIT_FOR_INPUT_CALLBACK_KIND: &str = "wait_for_input";

#[derive(Debug, Clone, Default)]
pub struct WaitForInputActionRunner;

impl WaitForInputActionRunner {
    pub fn run(&self, action: WaitForInputAction, context: ExecutionContext) -> ActionResult {
        let pending = PendingWaitForInput {
            record_id: context.step_record_id,
            prev: context.prev,
            started_at: Utc::now(),
            output_status: action.status,
            output_fields: Value::Object(action.fields.into_iter().collect()),
        };
        let resume_callback = ResumeCallback::new(
            WAIT_FOR_INPUT_CALLBACK_KIND,
            serde_json::to_value(pending).expect("pending wait-for-input payload serializes"),
        )
        .expect("wait-for-input resume callback kind is static and non-empty");

        ActionResult::blocked(RunStatus::WaitingForInput {
            step: context.step_id,
            input_id: action.id,
            message: action.message,
            choices: action.choices,
            resume_callback,
        })
    }

    pub fn complete(&self, pending: PendingWaitForInput, input: ResumeInput) -> StepRecord {
        let fields = fields_with_input(pending.output_fields, &input.input);
        StepRecord {
            id: pending.record_id,
            prev: pending.prev,
            step: input.step,
            action: "wait_for_input".to_string(),
            input: StepInput {
                prompt: Some(input.message.clone()),
                context: json!({
                    "input_id": input.input_id,
                    "choices": input.choices,
                }),
            },
            output: Some(StepOutput {
                status: pending.output_status,
                fields,
                body: input.input.clone(),
                raw: json!({
                    "input_id": input.input_id,
                    "message": input.message,
                    "choices": input.choices,
                    "input": input.input,
                }),
            }),
            detail: StepDetail {
                backend: None,
                session_id: None,
                duration_ms: (input.completed_at - pending.started_at)
                    .num_milliseconds()
                    .max(0) as u64,
                turn_count: 0,
                usage: None,
            },
            started_at: pending.started_at,
            completed_at: Some(input.completed_at),
        }
    }
}

#[async_trait]
impl ResumeCallbackHandler for WaitForInputActionRunner {
    async fn resume(&self, callback: &ResumeCallback, input: ResumeInput) -> Result<ActionResult> {
        let pending = PendingWaitForInput::from_callback(callback)?;
        Ok(ActionResult::completed(self.complete(pending, input)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingWaitForInput {
    pub record_id: String,
    pub prev: Option<String>,
    pub started_at: DateTime<Utc>,
    pub output_status: String,
    pub output_fields: Value,
}

impl PendingWaitForInput {
    pub fn from_callback(callback: &ResumeCallback) -> Result<Self> {
        if callback.kind() != WAIT_FOR_INPUT_CALLBACK_KIND {
            return Err(WorkflowError::InvalidAction(format!(
                "resume callback kind {:?} is not supported by wait_for_input",
                callback.kind()
            )));
        }
        serde_json::from_value(callback.payload().clone()).map_err(|err| {
            WorkflowError::InvalidAction(format!(
                "invalid wait_for_input resume callback payload: {err}"
            ))
        })
    }
}

fn fields_with_input(fields: Value, input: &str) -> Value {
    match fields {
        Value::Object(mut object) => {
            object.insert("input".to_string(), Value::String(input.to_string()));
            Value::Object(object)
        }
        Value::Null => {
            let mut object = Map::new();
            object.insert("input".to_string(), Value::String(input.to_string()));
            Value::Object(object)
        }
        other => {
            let mut object = Map::new();
            object.insert("value".to_string(), other);
            object.insert("input".to_string(), Value::String(input.to_string()));
            Value::Object(object)
        }
    }
}
