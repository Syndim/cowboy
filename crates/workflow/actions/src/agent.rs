use async_trait::async_trait;
use cowboy_workflow_agent::{AgentExecutor, ClientFactory};
use cowboy_workflow_core::{
    ActionResult, AgentAction, ExecutionContext, Result, RunStore, StepRecord, WorkflowError,
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
    S: RunStore + 'static,
{
    async fn run_agent(
        &self,
        action: AgentAction,
        context: ExecutionContext,
    ) -> Result<StepRecord> {
        self.execute_agent(action, context)
            .await
            .map(|execution| execution.record)
            .map_err(WorkflowError::from)
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
