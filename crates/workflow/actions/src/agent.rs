use async_trait::async_trait;
use cowboy_workflow_agent::{AgentExecutor, ClientFactory};
use cowboy_workflow_core::{
    ActionResult, AgentAction, ExecutionContext, Result, StepRecord, WorkflowError,
};

#[async_trait]
pub trait AgentActionHandler: Send + Sync {
    async fn run_agent(&self, action: AgentAction, context: ExecutionContext)
    -> Result<StepRecord>;
}

#[async_trait]
impl<F, S> AgentActionHandler for AgentExecutor<F, S>
where
    F: ClientFactory,
    S: cowboy_workflow_agent::AgentStore + 'static,
{
    async fn run_agent(
        &self,
        action: AgentAction,
        context: ExecutionContext,
    ) -> Result<StepRecord> {
        let identity = self
            .human_input_enabled()
            .then(|| (context.run_id.clone(), action.role.clone()));
        match self.execute_agent(action, context).await {
            Ok(execution) => Ok(execution.record),
            Err(error) => {
                if matches!(
                    error,
                    cowboy_workflow_agent::Error::HumanInput { .. }
                        | cowboy_workflow_agent::Error::HumanInputUnsafe
                        | cowboy_workflow_agent::Error::Workflow(WorkflowError::AgentUnsafe)
                ) {
                    let (run_id, role_id) = identity.expect("human input was enabled");
                    self.terminate_for_human_input(&run_id, &role_id)
                        .await
                        .map_err(|_| WorkflowError::AgentUnsafe)?;
                }

                Err(WorkflowError::from(error))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentActionRunner<A> {
    agent: A,
}

impl<A> AgentActionRunner<A> {
    pub fn new(agent: A) -> Self {
        Self { agent }
    }
}

impl<A> AgentActionRunner<A>
where
    A: AgentActionHandler,
{
    pub async fn run(
        &self,
        action: AgentAction,
        context: ExecutionContext,
    ) -> Result<ActionResult> {
        let record = self.agent.run_agent(action, context).await?;
        Ok(ActionResult::completed(record))
    }
}
