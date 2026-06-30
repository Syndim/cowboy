use chrono::Utc;
use serde_json::Value;

use crate::{
    ActionExecution, ActionExecutor, ObjectKind, Result, RunHead, RunStatus, RunStore,
    StatusAction, StepAction, StepActionProvider, StepDefinition, StepDetail, StepInput,
    StepOutput, StepRecord, WorkflowDefinition, WorkflowError, WorkflowRun, next_step,
};

/// Safety budgets enforced by the workflow runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerLimits {
    /// Maximum number of handled step actions in one run.
    pub max_steps_per_run: u32,
    /// Maximum number of visits to the same step in one run.
    pub max_visits_per_step: u32,
}

impl Default for RunnerLimits {
    fn default() -> Self {
        Self {
            max_steps_per_run: 100,
            max_visits_per_step: 20,
        }
    }
}

/// Execute one workflow step action and persist the resulting run state.
pub async fn execute_step<S, E, P>(
    store: &S,
    executor: &E,
    provider: &P,
    definition: &WorkflowDefinition,
    run: &mut WorkflowRun,
    limits: &RunnerLimits,
) -> Result<RunStatus>
where
    S: RunStore,
    E: ActionExecutor,
    P: StepActionProvider,
{
    enforce_budget(run, limits)?;
    let step =
        definition
            .steps
            .get(&run.current_step)
            .ok_or_else(|| WorkflowError::UnknownStep {
                step: run.current_step.clone(),
            })?;
    increment_budget(run);

    let prev_record = run
        .head
        .as_ref()
        .map(|head| store.get_object::<StepRecord>(head))
        .transpose()?;
    let action = provider.step_action(definition, run, step, prev_record.as_ref())?;
    let status = match action {
        StepAction::Status(action) => {
            handle_step_record(store, definition, run, status_record(run, step, action))?
        }
        StepAction::Fail(action) => set_run_status(
            store,
            run,
            RunStatus::Failed {
                reason: action.reason,
            },
        )?,
        StepAction::Suspend(action) => set_run_status(
            store,
            run,
            RunStatus::Suspended {
                step: step.id.clone(),
                reason: action.reason,
            },
        )?,
        StepAction::AskUser(action) => set_run_status(
            store,
            run,
            RunStatus::WaitingForInput {
                step: step.id.clone(),
                prompt_id: action.id,
                message: action.message,
                choices: action.choices,
            },
        )?,
        agent_action @ StepAction::Agent(_) => {
            match executor
                .execute(
                    agent_action,
                    crate::ExecutionContext {
                        run_id: run.id.clone(),
                        step_id: step.id.clone(),
                        step_record_id: next_record_id(run),
                        prev: run.head.clone(),
                    },
                )
                .await?
            {
                ActionExecution::StepCompleted(record) => {
                    handle_step_record(store, definition, run, *record)?
                }
                ActionExecution::RunStateChanged(head) => {
                    let status = head.status.clone();
                    apply_run_head(store, run, head)?;
                    status
                }
            }
        }
    };
    Ok(status)
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

fn status_record(run: &WorkflowRun, step: &StepDefinition, action: StatusAction) -> StepRecord {
    let now = Utc::now();
    StepRecord {
        id: next_record_id(run),
        prev: run.head.clone(),
        step: step.id.clone(),
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
    }
}

fn handle_step_record<S: RunStore>(
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
    let hash = store.put_object(ObjectKind::StepRecord, &record)?;
    run.head = Some(hash.clone());
    run.updated_at = Utc::now();

    let status = if let Some(next) = next {
        run.current_step = next;
        RunStatus::Running
    } else {
        RunStatus::Completed
    };
    run.status = status.clone();
    store.save_run(run)?;
    store.update_run_head(&run.id, run_head(run))?;
    Ok(status)
}

fn set_run_status<S: RunStore>(
    store: &S,
    run: &mut WorkflowRun,
    status: RunStatus,
) -> Result<RunStatus> {
    run.status = status.clone();
    run.updated_at = Utc::now();
    store.save_run(run)?;
    store.update_run_head(&run.id, run_head(run))?;
    Ok(status)
}

fn apply_run_head<S: RunStore>(store: &S, run: &mut WorkflowRun, head: RunHead) -> Result<()> {
    run.head = head.head_step.clone();
    run.status = head.status.clone();
    run.updated_at = head.updated_at;
    store.save_run(run)?;
    store.update_run_head(&run.id, head)?;
    Ok(())
}

fn run_head(run: &WorkflowRun) -> RunHead {
    RunHead {
        run_id: run.id.clone(),
        workflow_hash: run.workflow_hash.clone(),
        head_step: run.head.clone(),
        status: run.status.clone(),
        updated_at: run.updated_at,
    }
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
    use serde::{Serialize, de::DeserializeOwned};

    use super::*;
    use crate::{
        AgentAction, AskUserAction, FailAction, StepTransitions, SuspendAction, WorkflowDefinition,
    };

    struct StaticProvider {
        actions: Mutex<Vec<StepAction>>,
    }

    impl StaticProvider {
        fn new(actions: Vec<StepAction>) -> Self {
            Self {
                actions: Mutex::new(actions),
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
        ) -> Result<StepAction> {
            Ok(self.actions.lock().remove(0))
        }
    }

    struct NoopExecutor;

    #[async_trait]
    impl ActionExecutor for NoopExecutor {
        async fn execute(
            &self,
            action: StepAction,
            context: crate::ExecutionContext,
        ) -> Result<ActionExecution> {
            let StepAction::Agent(action) = action else {
                return Err(WorkflowError::InvalidAction("expected agent".to_string()));
            };
            let now = Utc::now();
            Ok(ActionExecution::StepCompleted(Box::new(StepRecord {
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
            })))
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        runs: Mutex<HashMap<String, WorkflowRun>>,
        heads: Mutex<HashMap<String, RunHead>>,
        sessions: Mutex<HashMap<(String, String), crate::RoleSession>>,
        objects: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl RunStore for MemoryStore {
        fn save_run(&self, run: &WorkflowRun) -> Result<()> {
            self.runs.lock().insert(run.id.clone(), run.clone());
            Ok(())
        }

        fn load_run(&self, run_id: &crate::RunId) -> Result<WorkflowRun> {
            self.runs
                .lock()
                .get(run_id)
                .cloned()
                .ok_or_else(|| WorkflowError::InvalidAction("missing run".to_string()))
        }

        fn list_runs(&self) -> Result<Vec<RunHead>> {
            Ok(self.heads.lock().values().cloned().collect())
        }

        fn put_object<T: Serialize>(
            &self,
            _kind: ObjectKind,
            value: &T,
        ) -> Result<crate::ObjectHash> {
            let bytes = serde_json::to_vec(value).unwrap();
            let hash = format!("hash-{}", self.objects.lock().len() + 1);
            self.objects.lock().insert(hash.clone(), bytes);
            Ok(hash)
        }

        fn get_object<T: DeserializeOwned>(&self, hash: &crate::ObjectHash) -> Result<T> {
            let bytes = self
                .objects
                .lock()
                .get(hash)
                .cloned()
                .ok_or_else(|| WorkflowError::InvalidAction("missing object".to_string()))?;
            Ok(serde_json::from_slice(&bytes).unwrap())
        }

        fn update_run_head(&self, run_id: &str, head: RunHead) -> Result<()> {
            self.heads.lock().insert(run_id.to_string(), head);
            Ok(())
        }

        fn load_run_head(&self, run_id: &str) -> Result<RunHead> {
            self.heads
                .lock()
                .get(run_id)
                .cloned()
                .ok_or_else(|| WorkflowError::InvalidAction("missing head".to_string()))
        }

        fn save_role_session(&self, session: crate::RoleSession) -> Result<()> {
            self.sessions
                .lock()
                .insert((session.run_id.clone(), session.role_id.clone()), session);
            Ok(())
        }

        fn load_role_session(
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

        fn delete_role_sessions(&self, run_id: &str) -> Result<()> {
            self.sessions
                .lock()
                .retain(|(stored_run, _), _| stored_run != run_id);
            Ok(())
        }

        fn append_turn(
            &self,
            _run_id: &str,
            _turn: crate::TurnRecord,
        ) -> Result<crate::ObjectHash> {
            Ok("turn".to_string())
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
            source_hash: "source".to_string(),
            head: "start".to_string(),
            roles: BTreeMap::new(),
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
            status: RunStatus::Running,
            current_step: "start".to_string(),
            head: None,
            resume: Value::Null,
            steps_executed: 0,
            step_visits: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn status_action_advances_to_next_step() {
        let store = MemoryStore::default();
        let executor = NoopExecutor;
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
        let executor = NoopExecutor;
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
        let executor = NoopExecutor;
        let provider = StaticProvider::new(vec![StepAction::AskUser(AskUserAction {
            id: "approval".to_string(),
            message: "Approve?".to_string(),
            choices: vec!["yes".to_string(), "no".to_string()],
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
            RunStatus::WaitingForInput {
                step: "start".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string(), "no".to_string()],
            }
        );
    }

    #[tokio::test]
    async fn fail_action_sets_failed_status() {
        let store = MemoryStore::default();
        let executor = NoopExecutor;
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
    async fn suspend_action_sets_suspended_status() {
        let store = MemoryStore::default();
        let executor = NoopExecutor;
        let provider = StaticProvider::new(vec![StepAction::Suspend(SuspendAction {
            reason: "pause".to_string(),
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
            RunStatus::Suspended {
                step: "start".to_string(),
                reason: "pause".to_string()
            }
        );
    }

    #[tokio::test]
    async fn agent_action_uses_executor_result() {
        let store = MemoryStore::default();
        let executor = NoopExecutor;
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
    async fn max_step_budget_is_enforced() {
        let store = MemoryStore::default();
        let executor = NoopExecutor;
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
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }
}
