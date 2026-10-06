use cowboy_workflow_core::{
    AgentAction, AgentFailureCategory, Result, ResumeCallback, Run, RunStatus, WorkflowError,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

pub(crate) const CALLBACK_KIND: &str = "agent_human_input";
pub(crate) const MAX_INPUT_BYTES: usize = 4096;

/// The original incomplete action is durable; it is not recomputed from Lua on
/// a human answer. No provider error, transcript or backend session id is stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingAgentInput {
    pub run_id: String,
    pub step: String,
    pub role: String,
    pub visit: u32,
    pub previous_head: Option<String>,
    pub generation: String,
    pub category: AgentFailureCategory,
    pub action: AgentAction,
}

impl PendingAgentInput {
    pub fn wait(run: &Run, action: AgentAction, category: AgentFailureCategory) -> RunStatus {
        let generation = Uuid::new_v4().to_string();
        let pending = Self {
            run_id: run.id.clone(),
            step: run.step.next.clone(),
            role: action.role.clone(),
            visit: run.step.visits.get(&run.step.next).copied().unwrap_or(0),
            previous_head: run.step.head.clone(),
            generation: generation.clone(),
            category,
            action,
        };
        RunStatus::WaitingForInput {
            step: pending.step.clone(),
            input_id: format!("agent-{generation}"),
            message: format!(
                "Agent step {:?} (role {:?}): {}",
                pending.step,
                pending.role,
                category.question()
            ),
            choices: Vec::new(),
            resume_callback: ResumeCallback::new(CALLBACK_KIND, json!(pending))
                .expect("static agent callback kind is valid"),
        }
    }

    pub fn from_wait(run: &Run) -> Result<Option<Self>> {
        let RunStatus::WaitingForInput {
            step,
            input_id,
            resume_callback,
            choices,
            ..
        } = &run.status
        else {
            return Ok(None);
        };
        if resume_callback.kind() != CALLBACK_KIND {
            return Ok(None);
        }

        let pending: Self =
            serde_json::from_value(resume_callback.payload().clone()).map_err(|_| {
                WorkflowError::InvalidAction("invalid agent human-input callback".to_string())
            })?;
        if pending.run_id != run.id
            || pending.step != *step
            || pending.step != run.step.next
            || pending.role != pending.action.role
            || pending.visit != run.step.visits.get(step).copied().unwrap_or(0)
            || !choices.is_empty()
            || pending.previous_head != run.step.head
            || input_id.strip_prefix("agent-") != Some(pending.generation.as_str())
            || Uuid::parse_str(&pending.generation).is_err()
        {
            return Err(WorkflowError::InvalidAction(
                "agent human-input callback does not match the current run visit".to_string(),
            ));
        }

        Ok(Some(pending))
    }

    pub fn validate_answer(answer: &str) -> Result<()> {
        if answer.trim().is_empty() || answer.len() > MAX_INPUT_BYTES {
            return Err(WorkflowError::InvalidAction(format!(
                "agent human input must be nonempty and at most {MAX_INPUT_BYTES} bytes"
            )));
        }

        Ok(())
    }
}
