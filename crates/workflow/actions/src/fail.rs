use cowboy_workflow_core::{ActionResult, FailAction, RunStatus};

#[derive(Debug, Clone, Default)]
pub struct FailActionRunner;

impl FailActionRunner {
    pub fn run(&self, action: FailAction) -> ActionResult {
        ActionResult::blocked(RunStatus::Failed {
            reason: action.reason,
        })
    }
}
