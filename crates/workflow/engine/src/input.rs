use chrono::Utc;
use cowboy_workflow_core::{StepRecord, WorkflowError, WorkflowRun};

use crate::{AskUserActionRunner, PendingAskUser};

/// Applies TUI/user answers to a workflow run waiting on `action.ask_user`.
///
/// `action.ask_user` is a step boundary: the dispatcher persists a
/// `WaitingForInput` run status and stops. When the user answers, this module
/// validates the answer and completes the pending ask-user action into a normal
/// `StepRecord`. It does not mutate `run.resume` or step counters.
#[derive(Debug, Clone, Default)]
pub struct InputRouter;

impl InputRouter {
    pub fn new() -> Self {
        Self
    }

    pub fn answer(
        &self,
        run: &WorkflowRun,
        prompt_id: &str,
        answer: impl Into<String>,
    ) -> cowboy_workflow_core::Result<StepRecord> {
        let answer = answer.into();
        let pending = PendingAskUser::from_status(&run.status)?;

        if prompt_id != pending.prompt_id {
            return Err(WorkflowError::InvalidAction(format!(
                "answer prompt id {prompt_id:?} does not match waiting prompt {:?}",
                pending.prompt_id
            )));
        }

        if !pending.choices.is_empty() && !pending.choices.iter().any(|choice| choice == &answer) {
            return Err(WorkflowError::InvalidAction(format!(
                "answer {answer:?} is not one of the allowed choices"
            )));
        }

        Ok(AskUserActionRunner.complete(pending, answer, Utc::now()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Utc;
    use cowboy_workflow_core::RunStatus;
    use serde_json::{Value, json};

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
                record_id: "run-1-ask".to_string(),
                prev: Some("previous-hash".to_string()),
                started_at: now,
                output_status: "answered".to_string(),
                output_fields: json!({ "plan": "ship" }),
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
    fn answer_builds_record_without_mutating_resume_or_counters() {
        let run = waiting_run();
        let before_resume = run.resume.clone();
        let before_steps = run.steps_executed;
        let record = InputRouter::new().answer(&run, "approval", "yes").unwrap();

        assert_eq!(run.resume, before_resume);
        assert_eq!(run.steps_executed, before_steps);
        assert_eq!(record.id, "run-1-ask");
        assert_eq!(record.prev, Some("previous-hash".to_string()));
        assert_eq!(record.step, "approve");
        assert_eq!(record.action, "ask_user");
        let output = record.output.unwrap();
        assert_eq!(output.status, "answered");
        assert_eq!(output.fields["plan"], "ship");
        assert_eq!(output.fields["answer"], "yes");
        assert_eq!(output.raw["prompt_id"], "approval");
    }

    #[test]
    fn answer_rejects_wrong_prompt_id() {
        let run = waiting_run();
        let err = InputRouter::new().answer(&run, "other", "yes").unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }

    #[test]
    fn answer_rejects_invalid_choice() {
        let run = waiting_run();
        let err = InputRouter::new()
            .answer(&run, "approval", "maybe")
            .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }
}
