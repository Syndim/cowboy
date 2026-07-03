mod agent;
mod ask_user;
mod fail;
mod status;
mod suspend;

use async_trait::async_trait;
use cowboy_workflow_core::{ActionDispatcher, ActionResult, ExecutionContext, Result, StepAction};

pub use agent::{AgentActionHandler, AgentActionRunner};
pub use ask_user::{AskUserActionRunner, PendingAskUser};
pub use fail::FailActionRunner;
pub use status::StatusActionRunner;
pub use suspend::SuspendActionRunner;

#[derive(Debug, Clone)]
pub struct EngineActionDispatcher<A> {
    agent: AgentActionRunner<A>,
    status: StatusActionRunner,
    fail: FailActionRunner,
    suspend: SuspendActionRunner,
    ask_user: AskUserActionRunner,
}

impl<A> EngineActionDispatcher<A> {
    pub fn new(agent: A) -> Self {
        Self {
            agent: AgentActionRunner::new(agent),
            status: StatusActionRunner,
            fail: FailActionRunner,
            suspend: SuspendActionRunner,
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
            StepAction::Suspend(action) => Ok(self.suspend.run(action, context)),
            StepAction::AskUser(action) => Ok(self.ask_user.run(action, context)),
            StepAction::Agent(action) => self.agent.run(action, context).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use chrono::Utc;
    use cowboy_workflow_core::{
        ActionDispatcher, AgentAction, AskUserAction, ExecutionContext, FailAction, RunStatus,
        StatusAction, StepDetail, StepInput, StepOutput, StepRecord, SuspendAction,
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
    fn fail_and_suspend_runners_block() {
        let ActionResult::Blocked(RunStatus::Failed { reason }) =
            FailActionRunner.run(FailAction {
                reason: "bad".to_string(),
            })
        else {
            panic!("expected failed status")
        };
        assert_eq!(reason, "bad");

        let ActionResult::Blocked(RunStatus::Suspended { step, reason }) = SuspendActionRunner.run(
            SuspendAction {
                reason: "pause".to_string(),
            },
            context(),
        ) else {
            panic!("expected suspended status")
        };
        assert_eq!(step, "step");
        assert_eq!(reason, "pause");
    }

    #[test]
    fn ask_user_initial_dispatch_captures_pending_metadata() {
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
            record_id,
            prev,
            output_status,
            output_fields,
            ..
        }) = result
        else {
            panic!("expected waiting status")
        };
        assert_eq!(step, "step");
        assert_eq!(prompt_id, "approval");
        assert_eq!(record_id, "record");
        assert_eq!(prev, Some("prev-hash".to_string()));
        assert_eq!(output_status, "accepted");
        assert_eq!(output_fields, json!({ "plan": "p" }));
    }

    #[test]
    fn ask_user_completion_merges_answer() {
        let started_at = Utc::now();
        let pending = PendingAskUser {
            step: "confirm".to_string(),
            prompt_id: "approval".to_string(),
            message: "Approve?".to_string(),
            choices: vec!["yes".to_string(), "no".to_string()],
            record_id: "record".to_string(),
            prev: Some("prev".to_string()),
            started_at,
            output_status: "answered".to_string(),
            output_fields: json!({ "plan": "p" }),
        };

        let record = AskUserActionRunner.complete(pending, "yes", started_at);

        assert_eq!(record.id, "record");
        assert_eq!(record.prev, Some("prev".to_string()));
        assert_eq!(record.action, "ask_user");
        let output = record.output.unwrap();
        assert_eq!(output.status, "answered");
        assert_eq!(output.fields["plan"], "p");
        assert_eq!(output.fields["answer"], "yes");
        assert_eq!(output.raw["prompt_id"], "approval");
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
    async fn action_dispatcher_routes_each_variant() {
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
        assert!(matches!(
            dispatcher
                .dispatch(
                    StepAction::Suspend(SuspendAction {
                        reason: "pause".to_string(),
                    }),
                    context(),
                )
                .await
                .unwrap(),
            ActionResult::Blocked(RunStatus::Suspended { .. })
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
