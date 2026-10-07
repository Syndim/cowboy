use chrono::Utc;
use cowboy_workflow_core::{
    ActionResult, Choice, ResumeCallback, ResumeInput, Run, RunStatus, WorkflowError,
};

use crate::ResumeCallbackRegistry;

/// Applies supplied input to a workflow run waiting on a registered callback.
///
/// The router owns input validation. It then dispatches the persisted callback
/// descriptor by kind without mutating step counters or visit counters.
#[derive(Debug, Clone)]
pub struct ResumeRouter {
    registry: ResumeCallbackRegistry,
}

#[derive(Debug, Clone)]
pub struct ValidatedInput {
    resume_callback: ResumeCallback,
    step: String,
    input_id: String,
    message: String,
    choices: Vec<Choice>,
    input: String,
}

impl ResumeRouter {
    pub fn new(registry: ResumeCallbackRegistry) -> Self {
        Self { registry }
    }

    pub fn with_default_registry() -> Self {
        Self::new(ResumeCallbackRegistry::default())
    }

    pub fn validate_input(
        &self,
        run: &Run,
        input_id: &str,
        input: impl Into<String>,
    ) -> cowboy_workflow_core::Result<ValidatedInput> {
        let input = input.into();
        let RunStatus::WaitingForInput {
            step,
            input_id: waiting_input_id,
            message,
            choices,
            resume_callback,
        } = &run.status
        else {
            return Err(WorkflowError::InvalidAction(
                "workflow run is not waiting for input".to_string(),
            ));
        };

        if input_id != waiting_input_id {
            return Err(WorkflowError::InvalidAction(format!(
                "input id {input_id:?} does not match waiting input {:?}",
                waiting_input_id
            )));
        }

        if !choices.is_empty() && !choices.iter().any(|choice| choice.key == input) {
            return Err(WorkflowError::InvalidAction(format!(
                "input {input:?} is not one of the allowed choices"
            )));
        }

        Ok(ValidatedInput {
            resume_callback: resume_callback.clone(),
            step: step.clone(),
            input_id: waiting_input_id.clone(),
            message: message.clone(),
            choices: choices.clone(),
            input,
        })
    }

    pub async fn dispatch_validated_input(
        &self,
        input: ValidatedInput,
    ) -> cowboy_workflow_core::Result<ActionResult> {
        self.registry
            .dispatch(
                &input.resume_callback,
                ResumeInput {
                    step: input.step,
                    input_id: input.input_id,
                    message: input.message,
                    choices: input.choices,
                    input: input.input,
                    completed_at: Utc::now(),
                },
            )
            .await
    }

    pub async fn provide_input(
        &self,
        run: &Run,
        input_id: &str,
        input: impl Into<String>,
    ) -> cowboy_workflow_core::Result<ActionResult> {
        let input = self.validate_input(run, input_id, input)?;
        self.dispatch_validated_input(input).await
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
    use std::time::Duration;

    use async_trait::async_trait;
    use chrono::Utc;
    use cowboy_workflow_core::{
        Choice, ResumeCallback, ResumeCallbackHandler, ResumeInput, RunStatus,
    };
    use serde_json::{Value, json};

    use super::*;

    fn waiting_run() -> Run {
        let now = Utc::now();
        Run {
            id: "run-1".to_string(),
            workflow: cowboy_workflow_core::WorkflowSnapshot {
                name: "wf".to_string(),
                api_version: 1,
                hash: "hash".to_string(),
                sources: BTreeMap::new(),
            },
            original_request: "do it".to_string(),
            start_fields: BTreeMap::new(),
            request_topic: None,
            config_set: Default::default(),
            parent: None,
            restart_source_run_id: None,
            status: RunStatus::WaitingForInput {
                step: "approve".to_string(),
                input_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec![
                    Choice {
                        key: "yes".to_string(),
                        description: "Approve".to_string(),
                    },
                    Choice {
                        key: "no".to_string(),
                        description: "Reject".to_string(),
                    },
                ],
                resume_callback: ResumeCallback::new(
                    "wait_for_input",
                    json!({
                        "record_id": "run-1-ask",
                        "prev": "previous-hash",
                        "started_at": now,
                        "output_status": "provided",
                        "output_fields": { "plan": "ship" }
                    }),
                )
                .unwrap(),
            },
            retries_used: 0,
            step: cowboy_workflow_core::StepState {
                next: "approve".to_string(),
                head: None,
                executed: 1,
                visits: BTreeMap::new(),
                retries_used: Default::default(),
            },
            active_duration_ms: 0,
            created_at: now,
            updated_at: now,
        }
    }

    struct SlowHandler;

    #[async_trait]
    impl ResumeCallbackHandler for SlowHandler {
        async fn resume(
            &self,
            _callback: &ResumeCallback,
            _input: ResumeInput,
        ) -> cowboy_workflow_core::Result<ActionResult> {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(ActionResult::blocked(RunStatus::Completed))
        }
    }

    #[tokio::test]
    async fn provide_input_dispatches_callback_without_mutating_counters() {
        let run = waiting_run();
        let before_steps = run.step.executed;
        let before_visits = run.step.visits.clone();
        let ActionResult::Completed(record) = ResumeRouter::default()
            .provide_input(&run, "approval", "yes")
            .await
            .unwrap()
        else {
            panic!("expected completed wait-for-input result")
        };

        assert_eq!(run.step.executed, before_steps);
        assert_eq!(run.step.visits, before_visits);
        assert_eq!(record.id, "run-1-ask");
        assert_eq!(record.prev, Some("previous-hash".to_string()));
        assert_eq!(record.step, "approve");
        assert_eq!(record.action, "wait_for_input");
        let output = record.output.unwrap();
        assert_eq!(output.status, "provided");
        assert_eq!(output.fields["plan"], "ship");
        assert_eq!(output.fields["input"], "yes");
        assert_eq!(output.raw["input_id"], "approval");
    }

    #[tokio::test]
    async fn validated_input_dispatch_can_be_active_timed() {
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
        let input = router.validate_input(&run, "approval", "yes").unwrap();
        let active_clock = crate::active_clock::ActiveRunClock::open_at(&run, Utc::now());

        router.dispatch_validated_input(input).await.unwrap();

        assert!(active_clock.active_duration_at(Utc::now()) >= 20);
    }

    #[tokio::test]
    async fn provide_input_rejects_wrong_input_id_before_callback_dispatch() {
        let run = waiting_run();
        let err = ResumeRouter::default()
            .provide_input(&run, "other", "yes")
            .await
            .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }

    #[tokio::test]
    async fn provide_input_rejects_invalid_choice_before_callback_dispatch() {
        let run = waiting_run();
        let err = ResumeRouter::default()
            .provide_input(&run, "approval", "maybe")
            .await
            .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }

    #[tokio::test]
    async fn async_resume_router_preserves_wait_for_input_behavior() {
        let run = waiting_run();
        let ActionResult::Completed(record) = ResumeRouter::default()
            .provide_input(&run, "approval", "yes")
            .await
            .unwrap()
        else {
            panic!("expected completed wait-for-input result")
        };
        assert_eq!(record.step, "approve");
        assert_eq!(record.input.prompt.as_deref(), Some("Approve?"));
        assert_eq!(record.input.context["input_id"], "approval");
        assert_eq!(
            record.input.context["choices"],
            json!([
                { "key": "yes", "description": "Approve" },
                { "key": "no", "description": "Reject" },
            ])
        );
        assert_eq!(record.output.as_ref().unwrap().fields["input"], "yes");

        let err = ResumeRouter::default()
            .provide_input(&run, "approval", "maybe")
            .await
            .unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }
}
