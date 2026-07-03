use chrono::{DateTime, Utc};
use cowboy_workflow_core::{
    ActionResult, AskUserAction, ExecutionContext, Result, ResumeCallback, ResumeCallbackHandler,
    ResumeInput, RunStatus, StepDetail, StepInput, StepOutput, StepRecord, WorkflowError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const ASK_USER_CALLBACK_KIND: &str = "ask_user";

#[derive(Debug, Clone, Default)]
pub struct AskUserActionRunner;

impl AskUserActionRunner {
    pub fn run(&self, action: AskUserAction, context: ExecutionContext) -> ActionResult {
        let pending = PendingAskUser {
            record_id: context.step_record_id,
            prev: context.prev,
            started_at: Utc::now(),
            output_status: action.status,
            output_fields: action.fields,
        };
        let resume_callback = ResumeCallback::new(
            ASK_USER_CALLBACK_KIND,
            serde_json::to_value(pending).expect("pending ask-user payload serializes"),
        )
        .expect("ask-user resume callback kind is static and non-empty");

        ActionResult::blocked(RunStatus::WaitingForInput {
            step: context.step_id,
            prompt_id: action.id,
            message: action.message,
            choices: action.choices,
            resume_callback,
        })
    }

    pub fn complete(&self, pending: PendingAskUser, input: ResumeInput) -> StepRecord {
        let fields = fields_with_answer(pending.output_fields, &input.answer);
        StepRecord {
            id: pending.record_id,
            prev: pending.prev,
            step: input.step,
            action: "ask_user".to_string(),
            input: StepInput {
                prompt: Some(input.message.clone()),
                context: json!({
                    "prompt_id": input.prompt_id,
                    "choices": input.choices,
                }),
            },
            output: Some(StepOutput {
                status: pending.output_status,
                fields,
                body: input.answer.clone(),
                raw: json!({
                    "prompt_id": input.prompt_id,
                    "message": input.message,
                    "choices": input.choices,
                    "answer": input.answer,
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

impl ResumeCallbackHandler for AskUserActionRunner {
    fn resume(&self, callback: &ResumeCallback, input: ResumeInput) -> Result<ActionResult> {
        let pending = PendingAskUser::from_callback(callback)?;
        Ok(ActionResult::completed(self.complete(pending, input)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingAskUser {
    pub record_id: String,
    pub prev: Option<String>,
    pub started_at: DateTime<Utc>,
    pub output_status: String,
    pub output_fields: Value,
}

impl PendingAskUser {
    pub fn from_callback(callback: &ResumeCallback) -> Result<Self> {
        if callback.kind() != ASK_USER_CALLBACK_KIND {
            return Err(WorkflowError::InvalidAction(format!(
                "resume callback kind {:?} is not supported by ask_user",
                callback.kind()
            )));
        }
        serde_json::from_value(callback.payload().clone()).map_err(|err| {
            WorkflowError::InvalidAction(format!("invalid ask_user resume callback payload: {err}"))
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
