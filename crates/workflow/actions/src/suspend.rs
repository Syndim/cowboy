use cowboy_workflow_core::{ActionResult, ExecutionContext, RunStatus, SuspendAction};

#[derive(Debug, Clone, Default)]
pub struct SuspendActionRunner;

impl SuspendActionRunner {
    pub fn run(&self, action: SuspendAction, context: ExecutionContext) -> ActionResult {
        ActionResult::blocked(RunStatus::Suspended {
            step: context.step_id,
            reason: action.reason,
        })
    }
}
