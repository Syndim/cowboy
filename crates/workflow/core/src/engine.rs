use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    ActionDispatcher, ActionResult, Result, RunStatus, StepAction, StepActionProvider, StepRecord,
    UserPromptStore, WorkflowDefinition, WorkflowError, WorkflowObjectStore, WorkflowRun,
    WorkflowStateStore, next_step,
};

/// Safety budgets enforced by the workflow runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunnerLimits {
    /// Maximum number of handled step actions in one run.
    pub max_steps_per_run: u32,
    /// Maximum number of visits to the same step in one run.
    pub max_visits_per_step: u32,
    /// Maximum number of recoverable retries across one durable run.
    pub max_retries_per_run: u32,
    /// Maximum number of recoverable retries for one step id across all visits.
    pub max_retries_per_step: u32,
}

impl Default for RunnerLimits {
    fn default() -> Self {
        Self {
            max_steps_per_run: 100,
            max_visits_per_step: 20,
            max_retries_per_run: 200,
            max_retries_per_step: 2,
        }
    }
}

/// Execute one workflow step action and persist the resulting run state.
///
/// This consumes the step's visit/step budget once. Recoverable retries of the
/// same logical step must use [`retry_current_step`], which reruns the current
/// step without spending additional budget.
pub async fn execute_step<S, D, P>(
    store: &S,
    dispatcher: &D,
    provider: &P,
    definition: &WorkflowDefinition,
    run: &mut WorkflowRun,
    limits: &RunnerLimits,
) -> Result<RunStatus>
where
    S: WorkflowStateStore + WorkflowObjectStore + UserPromptStore + ?Sized,
    D: ActionDispatcher,
    P: StepActionProvider,
{
    enforce_budget(run, limits)?;
    increment_budget(run);
    dispatch_current_step(store, dispatcher, provider, definition, run, 1, None).await
}

/// Re-run the current step after a recoverable failure without consuming the
/// step/visit budget again.
///
/// `attempt` is the 1-based attempt number (>= 2 for retries) and `retry_reason`
/// carries the previous failure so agent actions can emit a corrective nudge.
pub async fn retry_current_step<S, D, P>(
    store: &S,
    dispatcher: &D,
    provider: &P,
    definition: &WorkflowDefinition,
    run: &mut WorkflowRun,
    attempt: u64,
    retry_reason: Option<String>,
) -> Result<RunStatus>
where
    S: WorkflowStateStore + WorkflowObjectStore + UserPromptStore + ?Sized,
    D: ActionDispatcher,
    P: StepActionProvider,
{
    dispatch_current_step(
        store,
        dispatcher,
        provider,
        definition,
        run,
        attempt,
        retry_reason,
    )
    .await
}

async fn dispatch_current_step<S, D, P>(
    store: &S,
    dispatcher: &D,
    provider: &P,
    definition: &WorkflowDefinition,
    run: &mut WorkflowRun,
    attempt: u64,
    retry_reason: Option<String>,
) -> Result<RunStatus>
where
    S: WorkflowStateStore + WorkflowObjectStore + UserPromptStore + ?Sized,
    D: ActionDispatcher,
    P: StepActionProvider,
{
    let step =
        definition
            .steps
            .get(&run.current_step)
            .ok_or_else(|| WorkflowError::UnknownStep {
                step: run.current_step.clone(),
            })?;

    let previous_head = run.head.clone();
    let prev_record = match previous_head {
        Some(head) => Some(store.load_step_record(&head).await?),
        None => None,
    };
    let user_prompts = store.load_user_prompts(&run.id).await?;
    let action =
        provider.step_action(definition, run, step, prev_record.as_ref(), &user_prompts)?;
    let role = match &action {
        StepAction::Agent(action) => Some(
            definition
                .roles
                .get(&action.role)
                .ok_or_else(|| WorkflowError::UnknownRole {
                    step: step.id.clone(),
                    role: action.role.clone(),
                })?
                .clone(),
        ),
        _ => None,
    };
    let context = crate::ExecutionContext {
        run_id: run.id.clone(),
        step_id: step.id.clone(),
        step_record_id: next_record_id(run),
        prev: run.head.clone(),
        role,
        attempt,
        retry_reason,
        original_request: run.original_request.clone(),
        run_created_at: run.created_at,
        user_prompts,
    };

    match dispatcher.dispatch(action, context).await? {
        ActionResult::Completed(record) => apply_step_record(store, definition, run, *record).await,
        ActionResult::Blocked(status) => apply_run_status(store, run, status).await,
    }
}

fn enforce_budget(run: &WorkflowRun, limits: &RunnerLimits) -> Result<()> {
    if run.steps_executed >= limits.max_steps_per_run {
        return Err(WorkflowError::InvalidAction(format!(
            "run exceeded max step count ({})",
            limits.max_steps_per_run
        )));
    }
    let visits = run.step_visits.get(&run.current_step).copied().unwrap_or(0);
    if visits >= limits.max_visits_per_step {
        return Err(WorkflowError::InvalidAction(format!(
            "step {:?} exceeded max visits ({})",
            run.current_step, limits.max_visits_per_step
        )));
    }
    Ok(())
}

fn increment_budget(run: &mut WorkflowRun) {
    run.steps_executed += 1;
    *run.step_visits.entry(run.current_step.clone()).or_default() += 1;
}

/// Persist a completed step record, advance the run head, and route by output status.
pub async fn apply_step_record<S: WorkflowStateStore + ?Sized>(
    store: &S,
    definition: &WorkflowDefinition,
    run: &mut WorkflowRun,
    record: StepRecord,
) -> Result<RunStatus> {
    let output = record
        .output
        .as_ref()
        .ok_or_else(|| WorkflowError::InvalidAction("step record missing output".to_string()))?;
    let next = next_step(definition, &record.step, &output.status)?.cloned();
    run.updated_at = Utc::now();

    let status = if let Some(next) = next {
        run.current_step = next;
        RunStatus::Running
    } else {
        RunStatus::Completed
    };
    run.status = status.clone();
    let hash = store.commit_completed_step(run, &record).await?;
    run.head = Some(hash);
    Ok(status)
}

/// Persist a run status produced by a blocked or terminal action.
pub async fn apply_run_status<S: WorkflowStateStore + ?Sized>(
    store: &S,
    run: &mut WorkflowRun,
    status: RunStatus,
) -> Result<RunStatus> {
    run.status = status.clone();
    run.updated_at = Utc::now();
    store.save_run(run).await?;
    Ok(status)
}

fn next_record_id(run: &WorkflowRun) -> String {
    format!("{}-{}", run.id, run.steps_executed + 1)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use async_trait::async_trait;
    use chrono::Utc;
    use parking_lot::Mutex;
    use serde_json::Value;

    use super::*;
    use crate::{
        ActionDispatcher, ActionResult, AgentAction, AskUserAction, CommandAction, FailAction,
        ResumeCallback, StatusAction, StepDefinition, StepDetail, StepInput, StepOutput,
        StepTransitions, WorkflowDefinition,
    };

    struct StaticProvider {
        actions: Mutex<Vec<StepAction>>,
        seen_prompts: Mutex<Vec<Vec<crate::RunUserPrompt>>>,
    }

    impl StaticProvider {
        fn new(actions: Vec<StepAction>) -> Self {
            Self {
                actions: Mutex::new(actions),
                seen_prompts: Mutex::new(Vec::new()),
            }
        }
    }

    impl StepActionProvider for StaticProvider {
        fn step_action(
            &self,
            _definition: &WorkflowDefinition,
            _run: &WorkflowRun,
            _step: &StepDefinition,
            _prev: Option<&StepRecord>,
            user_prompts: &[crate::RunUserPrompt],
        ) -> Result<StepAction> {
            self.seen_prompts.lock().push(user_prompts.to_vec());
            Ok(self.actions.lock().remove(0))
        }
    }

    #[derive(Default)]
    struct NoopDispatcher {
        dispatched: Mutex<Vec<String>>,
        contexts: Mutex<Vec<crate::ExecutionContext>>,
    }

    #[async_trait]
    impl ActionDispatcher for NoopDispatcher {
        async fn dispatch(
            &self,
            action: StepAction,
            context: crate::ExecutionContext,
        ) -> Result<ActionResult> {
            self.dispatched
                .lock()
                .push(action.action_name().to_string());
            let now = Utc::now();
            self.contexts.lock().push(context.clone());
            match action {
                StepAction::Agent(action) => Ok(ActionResult::completed(StepRecord {
                    id: context.step_record_id,
                    prev: context.prev,
                    step: context.step_id,
                    action: "agent".to_string(),
                    input: StepInput {
                        prompt: Some(action.prompt),
                        context: Value::Null,
                    },
                    output: Some(StepOutput {
                        status: "success".to_string(),
                        fields: Value::Null,
                        body: String::new(),
                        raw: Value::Null,
                    }),
                    detail: StepDetail {
                        backend: Some("test".to_string()),
                        session_id: None,
                        duration_ms: 0,
                        turn_count: 0,
                        usage: None,
                    },
                    started_at: now,
                    completed_at: Some(now),
                })),
                StepAction::Command(action) => Ok(ActionResult::completed(StepRecord {
                    id: context.step_record_id,
                    prev: context.prev,
                    step: context.step_id,
                    action: "command".to_string(),
                    input: StepInput {
                        prompt: None,
                        context: Value::Null,
                    },
                    output: Some(StepOutput {
                        status: action.success_status,
                        fields: Value::Null,
                        body: String::new(),
                        raw: Value::Null,
                    }),
                    detail: StepDetail {
                        backend: None,
                        session_id: None,
                        duration_ms: 0,
                        turn_count: 0,
                        usage: None,
                    },
                    started_at: now,
                    completed_at: Some(now),
                })),
                StepAction::Status(action) => Ok(ActionResult::completed(StepRecord {
                    id: context.step_record_id,
                    prev: context.prev,
                    step: context.step_id,
                    action: "status".to_string(),
                    input: StepInput {
                        prompt: None,
                        context: Value::Null,
                    },
                    output: Some(StepOutput {
                        status: action.status,
                        fields: action.fields,
                        body: action.body,
                        raw: Value::Null,
                    }),
                    detail: StepDetail {
                        backend: None,
                        session_id: None,
                        duration_ms: 0,
                        turn_count: 0,
                        usage: None,
                    },
                    started_at: now,
                    completed_at: Some(now),
                })),
                StepAction::AskUser(action) => {
                    let resume_callback = ResumeCallback::new(
                        "ask_user",
                        serde_json::json!({
                            "record_id": context.step_record_id,
                            "prev": context.prev,
                            "started_at": now,
                            "output_status": action.status,
                            "output_fields": action.fields,
                        }),
                    )?;
                    Ok(ActionResult::blocked(RunStatus::WaitingForInput {
                        step: context.step_id,
                        prompt_id: action.id,
                        message: action.message,
                        choices: action.choices,
                        resume_callback,
                    }))
                }
                StepAction::Fail(action) => Ok(ActionResult::blocked(RunStatus::Failed {
                    reason: action.reason,
                })),
            }
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        runs: Mutex<HashMap<String, WorkflowRun>>,
        heads: Mutex<HashMap<String, crate::RunHead>>,
        sessions: Mutex<HashMap<(String, String), crate::RoleSession>>,
        objects: Mutex<HashMap<String, Vec<u8>>>,
        prompts: Mutex<HashMap<String, Vec<crate::RunUserPrompt>>>,
    }

    #[async_trait]
    impl WorkflowStateStore for MemoryStore {
        async fn save_run(&self, run: &WorkflowRun) -> Result<()> {
            self.runs.lock().insert(run.id.clone(), run.clone());
            self.heads
                .lock()
                .insert(run.id.clone(), crate::RunHead::from_run(run));
            Ok(())
        }

        async fn load_run(&self, run_id: &crate::RunId) -> Result<WorkflowRun> {
            self.runs
                .lock()
                .get(run_id)
                .cloned()
                .ok_or_else(|| WorkflowError::InvalidAction("missing run".to_string()))
        }

        async fn list_runs(&self) -> Result<Vec<crate::RunHead>> {
            Ok(self.heads.lock().values().cloned().collect())
        }

        async fn load_run_head(&self, run_id: &str) -> Result<crate::RunHead> {
            self.heads
                .lock()
                .get(run_id)
                .cloned()
                .ok_or_else(|| WorkflowError::InvalidAction("missing head".to_string()))
        }

        async fn commit_completed_step(
            &self,
            run: &WorkflowRun,
            record: &StepRecord,
        ) -> Result<crate::ObjectHash> {
            let bytes = serde_json::to_vec(record).unwrap();
            let hash = format!("hash-{}", self.objects.lock().len() + 1);
            self.objects.lock().insert(hash.clone(), bytes);
            let mut stored_run = run.clone();
            stored_run.head = Some(hash.clone());
            self.save_run(&stored_run).await?;
            Ok(hash)
        }

        async fn delete_run(&self, run_id: &str) -> Result<()> {
            self.runs.lock().remove(run_id);
            self.heads.lock().remove(run_id);
            Ok(())
        }
    }

    #[async_trait]
    impl WorkflowObjectStore for MemoryStore {
        async fn store_workflow_source_snapshot(
            &self,
            value: &crate::WorkflowSourceSnapshot,
        ) -> Result<crate::ObjectHash> {
            let bytes = serde_json::to_vec(value).unwrap();
            let hash = format!("hash-{}", self.objects.lock().len() + 1);
            self.objects.lock().insert(hash.clone(), bytes);
            Ok(hash)
        }

        async fn load_workflow_source_snapshot(
            &self,
            hash: &crate::ObjectHash,
        ) -> Result<crate::WorkflowSourceSnapshot> {
            let bytes = self
                .objects
                .lock()
                .get(hash)
                .cloned()
                .ok_or_else(|| WorkflowError::InvalidAction("missing object".to_string()))?;
            Ok(serde_json::from_slice(&bytes).unwrap())
        }

        async fn store_step_record(&self, value: &StepRecord) -> Result<crate::ObjectHash> {
            let bytes = serde_json::to_vec(value).unwrap();
            let hash = format!("hash-{}", self.objects.lock().len() + 1);
            self.objects.lock().insert(hash.clone(), bytes);
            Ok(hash)
        }

        async fn load_step_record(&self, hash: &crate::ObjectHash) -> Result<StepRecord> {
            let bytes = self
                .objects
                .lock()
                .get(hash)
                .cloned()
                .ok_or_else(|| WorkflowError::InvalidAction("missing object".to_string()))?;
            Ok(serde_json::from_slice(&bytes).unwrap())
        }

        async fn delete_object(&self, hash: &crate::ObjectHash) -> Result<()> {
            self.objects.lock().remove(hash);
            Ok(())
        }
    }

    #[async_trait]
    impl crate::AgentSessionStore for MemoryStore {
        async fn save_role_session(&self, session: crate::RoleSession) -> Result<()> {
            self.sessions
                .lock()
                .insert((session.run_id.clone(), session.role_id.clone()), session);
            Ok(())
        }

        async fn load_role_session(
            &self,
            run_id: &str,
            role_id: &str,
        ) -> Result<Option<crate::RoleSession>> {
            Ok(self
                .sessions
                .lock()
                .get(&(run_id.to_string(), role_id.to_string()))
                .cloned())
        }

        async fn delete_role_sessions(&self, run_id: &str) -> Result<()> {
            self.sessions
                .lock()
                .retain(|(stored_run, _), _| stored_run != run_id);
            Ok(())
        }
    }

    #[async_trait]
    impl crate::TurnStore for MemoryStore {
        async fn append_turn(
            &self,
            _run_id: &str,
            _turn: crate::TurnRecord,
        ) -> Result<crate::ObjectHash> {
            Ok("turn".to_string())
        }

        async fn load_turn(&self, _hash: &crate::ObjectHash) -> Result<crate::TurnRecord> {
            Err(WorkflowError::InvalidAction("missing turn".to_string()))
        }

        async fn load_turns(
            &self,
            _run_id: &str,
            _step_record_id: &str,
        ) -> Result<Vec<crate::TurnRecord>> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl UserPromptStore for MemoryStore {
        async fn load_user_prompts(&self, run_id: &str) -> Result<Vec<crate::RunUserPrompt>> {
            Ok(self.prompts.lock().get(run_id).cloned().unwrap_or_default())
        }
    }

    #[async_trait]
    impl crate::PromptWindowStore for MemoryStore {
        async fn open_agent_prompt_window(
            &self,
            _window: crate::AgentPromptWindow,
        ) -> Result<crate::OpenAgentPromptWindowOutcome> {
            Err(WorkflowError::InvalidAction(
                "core test store does not execute agent prompt windows".to_string(),
            ))
        }

        async fn append_user_prompt(
            &self,
            _run_id: &str,
            _window_id: &str,
            _content: String,
        ) -> Result<crate::AppendUserPromptOutcome> {
            Err(WorkflowError::InvalidAction(
                "core test store does not accept agent prompts".to_string(),
            ))
        }

        async fn compare_and_seal_agent_prompt_window(
            &self,
            _run_id: &str,
            _window_id: &str,
            _applied_sequence: u64,
            _sealed_at: chrono::DateTime<Utc>,
        ) -> Result<crate::CompareAndSealPromptWindowOutcome> {
            Err(WorkflowError::InvalidAction(
                "core test store does not execute agent prompt windows".to_string(),
            ))
        }

        async fn abort_agent_prompt_window(
            &self,
            _run_id: &str,
            _window_id: &str,
            _aborted_at: chrono::DateTime<Utc>,
        ) -> Result<crate::AbortAgentPromptWindowOutcome> {
            Err(WorkflowError::InvalidAction(
                "core test store does not execute agent prompt windows".to_string(),
            ))
        }

        async fn clear_agent_prompt_window(
            &self,
            _run_id: &str,
        ) -> Result<Option<crate::AgentPromptWindow>> {
            Err(WorkflowError::InvalidAction(
                "core test store does not execute agent prompt windows".to_string(),
            ))
        }
    }

    fn step(id: &str) -> StepDefinition {
        StepDefinition {
            id: id.to_string(),
            role: None,
            transitions: StepTransitions::new(),
            properties: Value::Null,
        }
    }

    fn definition() -> WorkflowDefinition {
        let mut start = step("start");
        start.transitions.insert("next", "next");
        WorkflowDefinition {
            name: "wf".to_string(),
            description: None,
            config_set: None,
            source_hash: "source".to_string(),
            head: "start".to_string(),
            roles: BTreeMap::from([(
                "developer".to_string(),
                crate::RoleDefinition {
                    id: "developer".to_string(),
                    instructions: "implement".to_string(),
                    agent: None,
                    properties: Value::Null,
                },
            )]),
            steps: BTreeMap::from([
                ("start".to_string(), start),
                ("next".to_string(), step("next")),
                ("agent".to_string(), step("agent")),
            ]),
        }
    }

    fn run() -> WorkflowRun {
        let now = Utc::now();
        WorkflowRun {
            id: "run".to_string(),
            workflow_name: "wf".to_string(),
            workflow_api_version: 1,
            workflow_hash: "source".to_string(),
            workflow_sources: BTreeMap::new(),
            original_request: "do it".to_string(),
            request_topic: None,
            status: RunStatus::Running,
            current_step: "start".to_string(),
            head: None,
            resume: Value::Null,
            config_set: crate::ConfigSetRef::default(),
            retries_used: 0,
            step_retries_used: BTreeMap::new(),
            steps_executed: 0,
            step_visits: BTreeMap::new(),
            active_duration_ms: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn initial_and_retry_dispatch_share_the_loaded_prompt_baseline() {
        let store = MemoryStore::default();
        let prompt = crate::RunUserPrompt {
            sequence: 1,
            content: "correction".to_string(),
            submitted_at: Utc::now(),
        };
        store
            .prompts
            .lock()
            .insert("run".to_string(), vec![prompt.clone()]);
        let executor = NoopDispatcher::default();
        let provider = StaticProvider::new(vec![
            StepAction::Status(StatusAction {
                status: "success".to_string(),
                fields: Value::Null,
                body: String::new(),
            }),
            StepAction::Status(StatusAction {
                status: "success".to_string(),
                fields: Value::Null,
                body: String::new(),
            }),
        ]);
        let mut initial_run = run();
        execute_step(
            &store,
            &executor,
            &provider,
            &definition(),
            &mut initial_run,
            &RunnerLimits::default(),
        )
        .await
        .unwrap();
        let mut retry_run = run();
        retry_current_step(
            &store,
            &executor,
            &provider,
            &definition(),
            &mut retry_run,
            2,
            Some("retry".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(
            provider.seen_prompts.lock().as_slice(),
            [vec![prompt.clone()], vec![prompt.clone()]]
        );
        let contexts = executor.contexts.lock();
        assert_eq!(contexts.len(), 2);
        assert!(
            contexts
                .iter()
                .all(|context| context.user_prompts == vec![prompt.clone()])
        );
        assert_eq!(contexts[0].original_request, "do it");
        assert_eq!(contexts[1].attempt, 2);
    }

    #[tokio::test]
    async fn status_action_advances_to_next_step() {
        let store = MemoryStore::default();
        let executor = NoopDispatcher::default();
        let provider = StaticProvider::new(vec![StepAction::Status(StatusAction {
            status: "next".to_string(),
            fields: Value::Null,
            body: String::new(),
        })]);
        let mut run = run();

        let status = execute_step(
            &store,
            &executor,
            &provider,
            &definition(),
            &mut run,
            &RunnerLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(status, RunStatus::Running);
        assert_eq!(run.current_step, "next");
        assert_eq!(run.steps_executed, 1);
        assert!(run.head.is_some());
    }

    #[tokio::test]
    async fn success_without_transition_completes() {
        let store = MemoryStore::default();
        let executor = NoopDispatcher::default();
        let provider = StaticProvider::new(vec![StepAction::Status(StatusAction {
            status: "success".to_string(),
            fields: Value::Null,
            body: String::new(),
        })]);
        let mut run = run();
        run.current_step = "next".to_string();

        let status = execute_step(
            &store,
            &executor,
            &provider,
            &definition(),
            &mut run,
            &RunnerLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(status, RunStatus::Completed);
    }

    #[tokio::test]
    async fn ask_user_sets_waiting_status() {
        let store = MemoryStore::default();
        let executor = NoopDispatcher::default();
        let provider = StaticProvider::new(vec![StepAction::AskUser(AskUserAction {
            id: "approval".to_string(),
            message: "Approve?".to_string(),
            choices: vec!["yes".to_string(), "no".to_string()],
            status: "answered".to_string(),
            fields: Value::Null,
        })]);
        let mut run = run();

        let status = execute_step(
            &store,
            &executor,
            &provider,
            &definition(),
            &mut run,
            &RunnerLimits::default(),
        )
        .await
        .unwrap();

        let RunStatus::WaitingForInput {
            step,
            prompt_id,
            message,
            choices,
            resume_callback,
        } = status
        else {
            panic!("expected waiting status")
        };
        assert_eq!(step, "start");
        assert_eq!(prompt_id, "approval");
        assert_eq!(message, "Approve?");
        assert_eq!(choices, vec!["yes".to_string(), "no".to_string()]);
        assert_eq!(resume_callback.kind(), "ask_user");
        assert_eq!(resume_callback.payload()["record_id"], "run-2");
        assert_eq!(resume_callback.payload()["prev"], Value::Null);
        assert_eq!(resume_callback.payload()["output_status"], "answered");
        assert_eq!(resume_callback.payload()["output_fields"], Value::Null);
    }

    #[tokio::test]
    async fn fail_action_sets_failed_status() {
        let store = MemoryStore::default();
        let executor = NoopDispatcher::default();
        let provider = StaticProvider::new(vec![StepAction::Fail(FailAction {
            reason: "bad".to_string(),
        })]);
        let mut run = run();

        let status = execute_step(
            &store,
            &executor,
            &provider,
            &definition(),
            &mut run,
            &RunnerLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            status,
            RunStatus::Failed {
                reason: "bad".to_string()
            }
        );
    }

    #[tokio::test]
    async fn agent_action_uses_executor_result() {
        let store = MemoryStore::default();
        let executor = NoopDispatcher::default();
        let provider = StaticProvider::new(vec![StepAction::Agent(AgentAction {
            role: "developer".to_string(),
            prompt: "do it".to_string(),
            output: None,
        })]);
        let mut run = run();
        run.current_step = "agent".to_string();

        let status = execute_step(
            &store,
            &executor,
            &provider,
            &definition(),
            &mut run,
            &RunnerLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(status, RunStatus::Completed);
        assert!(run.head.is_some());
    }

    #[tokio::test]
    async fn action_dispatcher_receives_each_step_action_variant() {
        let cases = vec![
            StepAction::Status(StatusAction {
                status: "success".to_string(),
                fields: Value::Null,
                body: String::new(),
            }),
            StepAction::Command(CommandAction {
                program: "echo".to_string(),
                args: vec!["ok".to_string()],
                success_status: "success".to_string(),
                failure_status: "failed".to_string(),
                timeout_ms: None,
            }),
            StepAction::AskUser(AskUserAction {
                id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: Vec::new(),
                status: "answered".to_string(),
                fields: Value::Null,
            }),
            StepAction::Fail(FailAction {
                reason: "bad".to_string(),
            }),
            StepAction::Agent(AgentAction {
                role: "developer".to_string(),
                prompt: "do it".to_string(),
                output: None,
            }),
        ];

        for action in cases {
            let expected = action.action_name().to_string();
            let store = MemoryStore::default();
            let dispatcher = NoopDispatcher::default();
            let provider = StaticProvider::new(vec![action]);
            let mut run = run();
            if expected == "agent" {
                run.current_step = "agent".to_string();
            }

            execute_step(
                &store,
                &dispatcher,
                &provider,
                &definition(),
                &mut run,
                &RunnerLimits::default(),
            )
            .await
            .unwrap();

            assert_eq!(dispatcher.dispatched.lock().as_slice(), &[expected]);
        }
    }

    #[tokio::test]
    async fn apply_step_record_stores_head_and_routes() {
        let store = MemoryStore::default();
        let mut run = run();
        let now = Utc::now();
        let record = StepRecord {
            id: "record".to_string(),
            prev: None,
            step: "start".to_string(),
            action: "status".to_string(),
            input: StepInput {
                prompt: None,
                context: Value::Null,
            },
            output: Some(StepOutput {
                status: "next".to_string(),
                fields: Value::Null,
                body: String::new(),
                raw: Value::Null,
            }),
            detail: StepDetail {
                backend: None,
                session_id: None,
                duration_ms: 0,
                turn_count: 0,
                usage: None,
            },
            started_at: now,
            completed_at: Some(now),
        };

        let status = apply_step_record(&store, &definition(), &mut run, record)
            .await
            .unwrap();

        assert_eq!(status, RunStatus::Running);
        assert_eq!(run.current_step, "next");
        assert!(run.head.is_some());
        assert_eq!(
            store.load_run(&run.id).await.unwrap().status,
            RunStatus::Running
        );
        assert_eq!(
            store.load_run_head(&run.id).await.unwrap().head_step,
            run.head
        );
    }

    #[tokio::test]
    async fn retry_does_not_consume_step_or_visit_budgets() {
        let store = MemoryStore::default();
        let executor = NoopDispatcher::default();
        let provider = StaticProvider::new(vec![StepAction::Status(StatusAction {
            status: "success".to_string(),
            fields: Value::Null,
            body: String::new(),
        })]);
        let mut run = run();
        run.steps_executed = 1;
        run.step_visits.insert("start".to_string(), 1);

        let status = retry_current_step(
            &store,
            &executor,
            &provider,
            &definition(),
            &mut run,
            2,
            Some("retry".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(status, RunStatus::Completed);
        assert_eq!(run.steps_executed, 1);
        assert_eq!(run.step_visits["start"], 1);
    }

    #[tokio::test]
    async fn max_step_budget_is_enforced() {
        let store = MemoryStore::default();
        let executor = NoopDispatcher::default();
        let provider = StaticProvider::new(vec![StepAction::Status(StatusAction {
            status: "success".to_string(),
            fields: Value::Null,
            body: String::new(),
        })]);
        let mut run = run();
        run.steps_executed = 1;

        let err = execute_step(
            &store,
            &executor,
            &provider,
            &definition(),
            &mut run,
            &RunnerLimits {
                max_steps_per_run: 1,
                max_visits_per_step: 10,
                max_retries_per_run: 200,
                max_retries_per_step: 0,
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }
}
