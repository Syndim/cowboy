use async_trait::async_trait;
use cowboy_workflow_core::{ActionResult, ExecutionContext, Result, WorkflowAction, WorkflowError};

/// Provider-neutral adapter implemented by the product runtime for workflow calls.
#[async_trait]
pub trait WorkflowActionHandler: Send + Sync {
    async fn run_workflow(
        &self,
        action: WorkflowAction,
        context: ExecutionContext,
    ) -> Result<ActionResult>;
}

#[derive(Debug, Clone, Default)]
pub struct UnsupportedWorkflowActionHandler;

#[async_trait]
impl WorkflowActionHandler for UnsupportedWorkflowActionHandler {
    async fn run_workflow(
        &self,
        _action: WorkflowAction,
        _context: ExecutionContext,
    ) -> Result<ActionResult> {
        Err(WorkflowError::InvalidAction(
            "workflow actions are not configured for this dispatcher".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowActionRunner<W> {
    handler: W,
}

impl<W> WorkflowActionRunner<W> {
    pub fn new(handler: W) -> Self {
        Self { handler }
    }
}

impl<W> WorkflowActionRunner<W>
where
    W: WorkflowActionHandler,
{
    pub async fn run(
        &self,
        action: WorkflowAction,
        context: ExecutionContext,
    ) -> Result<ActionResult> {
        self.handler.run_workflow(action, context).await
    }
}
