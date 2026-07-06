mod agent;
mod ask_user;
mod fail;
mod status;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use cowboy_workflow_core::{
    ActionDispatcher, ActionResult, ExecutionContext, Result, ResumeCallback,
    ResumeCallbackHandler, ResumeInput, StepAction, WorkflowError,
};

pub use agent::{AgentActionHandler, AgentActionRunner};
pub use ask_user::{ASK_USER_CALLBACK_KIND, AskUserActionRunner, PendingAskUser};
pub use fail::FailActionRunner;
pub use status::StatusActionRunner;

#[derive(Debug, Clone)]
pub struct EngineActionDispatcher<A> {
    agent: AgentActionRunner<A>,
    status: StatusActionRunner,
    fail: FailActionRunner,
    ask_user: AskUserActionRunner,
}

impl<A> EngineActionDispatcher<A> {
    pub fn new(agent: A) -> Self {
        Self {
            agent: AgentActionRunner::new(agent),
            status: StatusActionRunner,
            fail: FailActionRunner,
            ask_user: AskUserActionRunner,
        }
    }
}

#[async_trait]
impl<A> ActionDispatcher for EngineActionDispatcher<A>
where
    A: AgentActionHandler,
{
    async fn dispatch(
        &self,
        action: StepAction,
        context: ExecutionContext,
    ) -> Result<ActionResult> {
        match action {
            StepAction::Status(action) => Ok(self.status.run(action, context)),
            StepAction::Fail(action) => Ok(self.fail.run(action)),
            StepAction::AskUser(action) => Ok(self.ask_user.run(action, context)),
            StepAction::Agent(action) => self.agent.run(action, context).await,
        }
    }
}

#[derive(Clone)]
pub struct ResumeCallbackRegistry {
    handlers: BTreeMap<String, Arc<dyn ResumeCallbackHandler>>,
}

impl ResumeCallbackRegistry {
    pub fn new() -> Self {
        Self {
            handlers: BTreeMap::new(),
        }
    }

    pub fn with_default_handlers() -> Self {
        let mut registry = Self::new();
        registry
            .register(ASK_USER_CALLBACK_KIND, AskUserActionRunner)
            .expect("default ask-user callback kind is valid");
        registry
    }

    pub fn register<H>(&mut self, kind: impl Into<String>, handler: H) -> Result<()>
    where
        H: ResumeCallbackHandler + 'static,
    {
        let kind = kind.into();
        if kind.trim().is_empty() {
            return Err(WorkflowError::InvalidAction(
                "resume callback kind cannot be empty".to_string(),
            ));
        }
        self.handlers.insert(kind, Arc::new(handler));
        Ok(())
    }

    pub fn dispatch(&self, callback: &ResumeCallback, input: ResumeInput) -> Result<ActionResult> {
        let handler = self.handlers.get(callback.kind()).ok_or_else(|| {
            WorkflowError::InvalidAction(format!(
                "unknown resume callback kind {:?}",
                callback.kind()
            ))
        })?;
        handler.resume(callback, input)
    }
}

impl Default for ResumeCallbackRegistry {
    fn default() -> Self {
        Self::with_default_handlers()
    }
}

impl fmt::Debug for ResumeCallbackRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResumeCallbackRegistry")
            .field("handlers", &self.handlers.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use chrono::Utc;
    use cowboy_workflow_core::{
        ActionDispatcher, AgentAction, AskUserAction, ExecutionContext, FailAction, ResumeCallback,
        ResumeCallbackHandler, ResumeInput, RunStatus, StatusAction, StepDetail, StepInput,
        StepOutput, StepRecord,
    };
    use serde_json::{Value, json};

    use super::*;

    fn context() -> ExecutionContext {
        ExecutionContext {
            run_id: "run".to_string(),
            step_id: "step".to_string(),
            step_record_id: "record".to_string(),
            prev: Some("prev-hash".to_string()),
            role: None,
            attempt: 1,
            retry_reason: None,
        }
    }

    fn resume_input(answer: &str) -> ResumeInput {
        ResumeInput {
            step: "confirm".to_string(),
            prompt_id: "approval".to_string(),
            message: "Approve?".to_string(),
            choices: vec!["yes".to_string(), "no".to_string()],
            answer: answer.to_string(),
            completed_at: Utc::now(),
        }
    }

    #[test]
    fn status_runner_completes_record() {
        let result = StatusActionRunner.run(
            StatusAction {
                status: "done".to_string(),
                fields: json!({ "x": 1 }),
                body: "body".to_string(),
            },
            context(),
        );

        let ActionResult::Completed(record) = result else {
            panic!("expected completed record")
        };
        assert_eq!(record.action, "status");
        assert_eq!(record.output.as_ref().unwrap().status, "done");
    }

    #[test]
    fn fail_runner_blocks() {
        let ActionResult::Blocked(RunStatus::Failed { reason }) =
            FailActionRunner.run(FailAction {
                reason: "bad".to_string(),
            })
        else {
            panic!("expected failed status")
        };
        assert_eq!(reason, "bad");
    }

    #[test]
    fn ask_user_initial_dispatch_registers_resume_callback() {
        let result = AskUserActionRunner.run(
            AskUserAction {
                id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string()],
                status: "accepted".to_string(),
                fields: json!({ "plan": "p" }),
            },
            context(),
        );

        let ActionResult::Blocked(RunStatus::WaitingForInput {
            step,
            prompt_id,
            message,
            choices,
            resume_callback,
        }) = result
        else {
            panic!("expected waiting status")
        };
        assert_eq!(step, "step");
        assert_eq!(prompt_id, "approval");
        assert_eq!(message, "Approve?");
        assert_eq!(choices, vec!["yes".to_string()]);
        assert_eq!(resume_callback.kind(), ASK_USER_CALLBACK_KIND);
        let pending = PendingAskUser::from_callback(&resume_callback).unwrap();
        assert_eq!(pending.record_id, "record");
        assert_eq!(pending.prev, Some("prev-hash".to_string()));
        assert_eq!(pending.output_status, "accepted");
        assert_eq!(pending.output_fields, json!({ "plan": "p" }));
    }

    #[test]
    fn ask_user_callback_completion_merges_answer() {
        let started_at = Utc::now();
        let pending = PendingAskUser {
            record_id: "record".to_string(),
            prev: Some("prev".to_string()),
            started_at,
            output_status: "answered".to_string(),
            output_fields: json!({ "plan": "p" }),
        };
        let callback = ResumeCallback::new(
            ASK_USER_CALLBACK_KIND,
            serde_json::to_value(pending).unwrap(),
        )
        .unwrap();

        let ActionResult::Completed(record) = AskUserActionRunner
            .resume(&callback, resume_input("yes"))
            .unwrap()
        else {
            panic!("expected completed ask-user record")
        };

        assert_eq!(record.id, "record");
        assert_eq!(record.prev, Some("prev".to_string()));
        assert_eq!(record.step, "confirm");
        assert_eq!(record.action, "ask_user");
        let output = record.output.unwrap();
        assert_eq!(output.status, "answered");
        assert_eq!(output.fields["plan"], "p");
        assert_eq!(output.fields["answer"], "yes");
        assert_eq!(output.body, "yes");
        assert_eq!(output.raw["prompt_id"], "approval");
        assert_eq!(output.raw["message"], "Approve?");
    }

    #[test]
    fn registry_dispatches_known_callback_and_rejects_unknown() {
        let started_at = Utc::now();
        let callback = ResumeCallback::new(
            ASK_USER_CALLBACK_KIND,
            serde_json::to_value(PendingAskUser {
                record_id: "record".to_string(),
                prev: None,
                started_at,
                output_status: "answered".to_string(),
                output_fields: Value::Null,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            ResumeCallbackRegistry::default()
                .dispatch(&callback, resume_input("yes"))
                .unwrap(),
            ActionResult::Completed(_)
        ));

        let err = ResumeCallbackRegistry::new()
            .dispatch(&callback, resume_input("yes"))
            .unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }

    #[derive(Debug, Clone)]
    struct FakeAgent;

    #[async_trait]
    impl AgentActionHandler for FakeAgent {
        async fn run_agent(
            &self,
            _action: AgentAction,
            context: ExecutionContext,
        ) -> Result<StepRecord> {
            let now = Utc::now();
            Ok(StepRecord {
                id: context.step_record_id,
                prev: context.prev,
                step: context.step_id,
                action: "agent".to_string(),
                input: StepInput {
                    prompt: Some("prompt".to_string()),
                    context: Value::Null,
                },
                output: Some(StepOutput {
                    status: "success".to_string(),
                    fields: Value::Null,
                    body: String::new(),
                    raw: Value::Null,
                }),
                detail: StepDetail {
                    backend: Some("fake".to_string()),
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

    #[tokio::test]
    async fn action_dispatcher_routes_each_remaining_variant() {
        let dispatcher = EngineActionDispatcher::new(FakeAgent);

        let ActionResult::Completed(record) = dispatcher
            .dispatch(
                StepAction::Status(StatusAction {
                    status: "done".to_string(),
                    fields: Value::Null,
                    body: String::new(),
                }),
                context(),
            )
            .await
            .unwrap()
        else {
            panic!("expected status record")
        };
        assert_eq!(record.action, "status");

        let ActionResult::Blocked(RunStatus::WaitingForInput { prompt_id, .. }) = dispatcher
            .dispatch(
                StepAction::AskUser(AskUserAction {
                    id: "approval".to_string(),
                    message: "Approve?".to_string(),
                    choices: Vec::new(),
                    status: "answered".to_string(),
                    fields: Value::Null,
                }),
                context(),
            )
            .await
            .unwrap()
        else {
            panic!("expected waiting status")
        };
        assert_eq!(prompt_id, "approval");

        assert!(matches!(
            dispatcher
                .dispatch(
                    StepAction::Fail(FailAction {
                        reason: "bad".to_string(),
                    }),
                    context(),
                )
                .await
                .unwrap(),
            ActionResult::Blocked(RunStatus::Failed { .. })
        ));

        let ActionResult::Completed(record) = dispatcher
            .dispatch(
                StepAction::Agent(AgentAction {
                    role: "developer".to_string(),
                    prompt: "do it".to_string(),
                    output: None,
                }),
                context(),
            )
            .await
            .unwrap()
        else {
            panic!("expected agent record")
        };
        assert_eq!(record.action, "agent");
    }
}
