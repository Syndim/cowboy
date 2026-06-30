use chrono::Utc;
use cowboy_workflow_core::{RunStatus, WorkflowError, WorkflowRun};
use serde_json::{Map, Value};

/// Applies TUI/user answers to a workflow run waiting on `action.ask_user`.
///
/// `action.ask_user` is a step boundary: the runner persists a
/// `WaitingForInput` run status and stops. When the user answers, this module
/// validates the answer, stores it under `run.resume[prompt_id]`, and marks the
/// run as `Running` so the next runner pass re-evaluates the same Lua step with
/// `ctx.resume` populated.
#[derive(Debug, Clone, Default)]
pub struct InputRouter;

impl InputRouter {
    pub fn new() -> Self {
        Self
    }

    pub fn answer(
        &self,
        run: &mut WorkflowRun,
        prompt_id: &str,
        answer: impl Into<String>,
    ) -> cowboy_workflow_core::Result<RunStatus> {
        let answer = answer.into();
        let RunStatus::WaitingForInput {
            step,
            prompt_id: expected_prompt_id,
            choices,
            ..
        } = &run.status
        else {
            return Err(WorkflowError::InvalidAction(
                "workflow run is not waiting for input".to_string(),
            ));
        };

        if prompt_id != expected_prompt_id {
            return Err(WorkflowError::InvalidAction(format!(
                "answer prompt id {prompt_id:?} does not match waiting prompt {expected_prompt_id:?}"
            )));
        }

        if !choices.is_empty() && !choices.iter().any(|choice| choice == &answer) {
            return Err(WorkflowError::InvalidAction(format!(
                "answer {answer:?} is not one of the allowed choices"
            )));
        }

        run.current_step = step.clone();
        insert_resume_answer(&mut run.resume, prompt_id, answer);
        run.status = RunStatus::Running;
        run.updated_at = Utc::now();
        Ok(run.status.clone())
    }
}

fn insert_resume_answer(resume: &mut Value, prompt_id: &str, answer: String) {
    if !resume.is_object() {
        *resume = Value::Object(Map::new());
    }
    let object = resume
        .as_object_mut()
        .expect("resume was normalized to object");
    object.insert(prompt_id.to_string(), Value::String(answer));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Utc;

    use super::*;

    fn waiting_run() -> WorkflowRun {
        let now = Utc::now();
        WorkflowRun {
            id: "run-1".to_string(),
            workflow_name: "wf".to_string(),
            workflow_api_version: 1,
            workflow_hash: "hash".to_string(),
            workflow_sources: BTreeMap::new(),
            original_request: "do it".to_string(),
            status: RunStatus::WaitingForInput {
                step: "approve".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string(), "no".to_string()],
            },
            current_step: "approve".to_string(),
            head: None,
            resume: Value::Null,
            steps_executed: 1,
            step_visits: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn answer_stores_resume_value_and_restarts_run() {
        let mut run = waiting_run();
        let status = InputRouter::new()
            .answer(&mut run, "approval", "yes")
            .unwrap();

        assert_eq!(status, RunStatus::Running);
        assert_eq!(run.status, RunStatus::Running);
        assert_eq!(run.current_step, "approve");
        assert_eq!(run.resume["approval"], "yes");
    }

    #[test]
    fn answer_rejects_wrong_prompt_id() {
        let mut run = waiting_run();
        let err = InputRouter::new()
            .answer(&mut run, "other", "yes")
            .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }

    #[test]
    fn answer_rejects_invalid_choice() {
        let mut run = waiting_run();
        let err = InputRouter::new()
            .answer(&mut run, "approval", "maybe")
            .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }
}
