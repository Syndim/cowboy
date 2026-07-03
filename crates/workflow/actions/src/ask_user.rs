use chrono::{DateTime, Utc};
use cowboy_workflow_core::{
    ActionResult, AskUserAction, ExecutionContext, Result, RunStatus, StepDetail, StepInput,
    StepOutput, StepRecord, WorkflowError,
};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Default)]
pub struct AskUserActionRunner;

impl AskUserActionRunner {
    pub fn run(&self, action: AskUserAction, context: ExecutionContext) -> ActionResult {
        ActionResult::blocked(RunStatus::WaitingForInput {
            step: context.step_id,
            prompt_id: action.id,
            message: action.message,
            choices: action.choices,
            record_id: context.step_record_id,
            prev: context.prev,
            started_at: Utc::now(),
            output_status: action.status,
            output_fields: action.fields,
        })
    }

    pub fn complete(
        &self,
        pending: PendingAskUser,
        answer: impl Into<String>,
        completed_at: DateTime<Utc>,
    ) -> StepRecord {
        let answer = answer.into();
        let fields = fields_with_answer(pending.output_fields, &answer);
        StepRecord {
            id: pending.record_id,
            prev: pending.prev,
            step: pending.step,
            action: "ask_user".to_string(),
            input: StepInput {
                prompt: Some(pending.message.clone()),
                context: json!({
                    "prompt_id": pending.prompt_id,
                    "choices": pending.choices,
                }),
            },
            output: Some(StepOutput {
                status: pending.output_status,
                fields,
                body: answer.clone(),
                raw: json!({
                    "prompt_id": pending.prompt_id,
                    "message": pending.message,
                    "choices": pending.choices,
                    "answer": answer,
                }),
            }),
            detail: StepDetail {
                backend: None,
                session_id: None,
                duration_ms: (completed_at - pending.started_at)
                    .num_milliseconds()
                    .max(0) as u64,
                turn_count: 0,
                usage: None,
            },
            started_at: pending.started_at,
            completed_at: Some(completed_at),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingAskUser {
    pub step: String,
    pub prompt_id: String,
    pub message: String,
    pub choices: Vec<String>,
    pub record_id: String,
    pub prev: Option<String>,
    pub started_at: DateTime<Utc>,
    pub output_status: String,
    pub output_fields: Value,
}

impl PendingAskUser {
    pub fn from_status(status: &RunStatus) -> Result<Self> {
        let RunStatus::WaitingForInput {
            step,
            prompt_id,
            message,
            choices,
            record_id,
            prev,
            started_at,
            output_status,
            output_fields,
        } = status
        else {
            return Err(WorkflowError::InvalidAction(
                "workflow run is not waiting for input".to_string(),
            ));
        };

        Ok(Self {
            step: step.clone(),
            prompt_id: prompt_id.clone(),
            message: message.clone(),
            choices: choices.clone(),
            record_id: record_id.clone(),
            prev: prev.clone(),
            started_at: *started_at,
            output_status: output_status.clone(),
            output_fields: output_fields.clone(),
        })
    }
}

fn fields_with_answer(fields: Value, answer: &str) -> Value {
    match fields {
        Value::Object(mut object) => {
            object.insert("answer".to_string(), Value::String(answer.to_string()));
            Value::Object(object)
        }
        Value::Null => {
            let mut object = Map::new();
            object.insert("answer".to_string(), Value::String(answer.to_string()));
            Value::Object(object)
        }
        other => {
            let mut object = Map::new();
            object.insert("value".to_string(), other);
            object.insert("answer".to_string(), Value::String(answer.to_string()));
            Value::Object(object)
        }
    }
}
