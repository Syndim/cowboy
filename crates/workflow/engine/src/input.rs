use chrono::Utc;
use cowboy_workflow_core::{
    ActionResult, ResumeCallback, ResumeInput, RunStatus, WorkflowError, WorkflowRun,
};

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

#[derive(Debug, Clone)]
pub struct ValidatedAnswer {
    resume_callback: ResumeCallback,
    step: String,
    prompt_id: String,
    message: String,
    choices: Vec<String>,
    answer: String,
}

impl ResumeRouter {
    pub fn new(registry: ResumeCallbackRegistry) -> Self {
        Self { registry }
    }

    pub fn with_default_registry() -> Self {
        Self::new(ResumeCallbackRegistry::default())
    }

    pub fn validate_answer(
        &self,
        run: &WorkflowRun,
        prompt_id: &str,
        answer: impl Into<String>,
    ) -> cowboy_workflow_core::Result<ValidatedAnswer> {
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

        Ok(ValidatedAnswer {
            resume_callback: resume_callback.clone(),
            step: step.clone(),
            prompt_id: waiting_prompt_id.clone(),
            message: message.clone(),
            choices: choices.clone(),
            answer,
        })
    }

    pub fn dispatch_validated_answer(
        &self,
        answer: ValidatedAnswer,
    ) -> cowboy_workflow_core::Result<ActionResult> {
        self.registry.dispatch(
            &answer.resume_callback,
            ResumeInput {
                step: answer.step,
                prompt_id: answer.prompt_id,
                message: answer.message,
                choices: answer.choices,
                answer: answer.answer,
                completed_at: Utc::now(),
            },
        )
    }

    pub fn answer(
        &self,
        run: &WorkflowRun,
        prompt_id: &str,
        answer: impl Into<String>,
    ) -> cowboy_workflow_core::Result<ActionResult> {
        let answer = self.validate_answer(run, prompt_id, answer)?;
        self.dispatch_validated_answer(answer)
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
    use std::thread;
    use std::time::Duration;

    use chrono::Utc;
    use cowboy_workflow_core::{ResumeCallback, ResumeCallbackHandler, ResumeInput, RunStatus};
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
            active_duration_ms: 0,
            created_at: now,
            updated_at: now,
        }
    }

    struct SlowHandler;

    impl ResumeCallbackHandler for SlowHandler {
        fn resume(
            &self,
            _callback: &ResumeCallback,
            _input: ResumeInput,
        ) -> cowboy_workflow_core::Result<ActionResult> {
            thread::sleep(Duration::from_millis(20));
            Ok(ActionResult::blocked(RunStatus::Completed))
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
    fn validated_answer_dispatch_can_be_active_timed() {
        let mut run = waiting_run();
        let RunStatus::WaitingForInput {
            resume_callback, ..
        } = &mut run.status
        else {
            panic!("expected waiting run")
        };
        *resume_callback = ResumeCallback::new("slow", Value::Null).unwrap();
        let mut registry = crate::ResumeCallbackRegistry::new();
        registry.register("slow", SlowHandler).unwrap();
        let router = ResumeRouter::new(registry);
        let answer = router.validate_answer(&run, "approval", "yes").unwrap();
        let active_clock = crate::active_clock::ActiveRunClock::open_at(&run, Utc::now());

        router.dispatch_validated_answer(answer).unwrap();

        assert!(active_clock.active_duration_at(Utc::now()) >= 20);
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
