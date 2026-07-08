use chrono::Utc;
use cowboy_workflow_core::{ActionResult, ResumeInput, RunStatus, WorkflowError, WorkflowRun};

use crate::ResumeCallbackRegistry;

/// Applies user answers to a workflow run waiting on a registered resume callback.
///
/// The router owns prompt validation. It then dispatches the persisted callback
/// descriptor by kind without mutating `run.resume`, step counters, or visit
/// counters.
#[derive(Debug, Clone)]
pub struct ResumeRouter {
    registry: ResumeCallbackRegistry,
}

impl ResumeRouter {
    pub fn new(registry: ResumeCallbackRegistry) -> Self {
        Self { registry }
    }

    pub fn with_default_registry() -> Self {
        Self::new(ResumeCallbackRegistry::default())
    }

    pub fn answer(
        &self,
        run: &WorkflowRun,
        prompt_id: &str,
        answer: impl Into<String>,
    ) -> cowboy_workflow_core::Result<ActionResult> {
        let answer = answer.into();
        let RunStatus::WaitingForInput {
            step,
            prompt_id: waiting_prompt_id,
            message,
            choices,
            resume_callback,
        } = &run.status
        else {
            return Err(WorkflowError::InvalidAction(
                "workflow run is not waiting for input".to_string(),
            ));
        };

        if prompt_id != waiting_prompt_id {
            return Err(WorkflowError::InvalidAction(format!(
                "answer prompt id {prompt_id:?} does not match waiting prompt {:?}",
                waiting_prompt_id
            )));
        }

        if !choices.is_empty() && !choices.iter().any(|choice| choice == &answer) {
            return Err(WorkflowError::InvalidAction(format!(
                "answer {answer:?} is not one of the allowed choices"
            )));
        }

        self.registry.dispatch(
            resume_callback,
            ResumeInput {
                step: step.clone(),
                prompt_id: waiting_prompt_id.clone(),
                message: message.clone(),
                choices: choices.clone(),
                answer,
                completed_at: Utc::now(),
            },
        )
    }
}

impl Default for ResumeRouter {
    fn default() -> Self {
        Self::with_default_registry()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Utc;
    use cowboy_workflow_core::{ResumeCallback, RunStatus};
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
            request_topic: None,
            status: RunStatus::WaitingForInput {
                step: "approve".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string(), "no".to_string()],
                resume_callback: ResumeCallback::new(
                    "ask_user",
                    json!({
                        "record_id": "run-1-ask",
                        "prev": "previous-hash",
                        "started_at": now,
                        "output_status": "answered",
                        "output_fields": { "plan": "ship" }
                    }),
                )
                .unwrap(),
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
    fn answer_dispatches_callback_without_mutating_resume_or_counters() {
        let run = waiting_run();
        let before_resume = run.resume.clone();
        let before_steps = run.steps_executed;
        let before_visits = run.step_visits.clone();
        let ActionResult::Completed(record) = ResumeRouter::default()
            .answer(&run, "approval", "yes")
            .unwrap()
        else {
            panic!("expected completed ask-user result")
        };

        assert_eq!(run.resume, before_resume);
        assert_eq!(run.steps_executed, before_steps);
        assert_eq!(run.step_visits, before_visits);
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
    fn answer_rejects_wrong_prompt_id_before_callback_dispatch() {
        let run = waiting_run();
        let err = ResumeRouter::default()
            .answer(&run, "other", "yes")
            .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }

    #[test]
    fn answer_rejects_invalid_choice_before_callback_dispatch() {
        let run = waiting_run();
        let err = ResumeRouter::default()
            .answer(&run, "approval", "maybe")
            .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }
}
