use std::sync::Arc;

use cowboy_workflow_core::{
    ActionExecutor, Result, RunStatus, RunStore, RunnerLimits, StepAction, StepActionProvider,
    StepDefinition, StepRecord, WorkflowDefinition, WorkflowError, WorkflowRun,
    WorkflowSourceSnapshot, execute_step,
};
use serde_json::{Value, json};

use crate::events::{EventBus, WorkflowEvent, WorkflowEventKind};

/// Runs one workflow until it completes, fails, suspends, waits for input, or hits a budget.
///
/// This is the TUI-facing orchestration seam over `cowboy-workflow-core`: the
/// core engine owns step semantics, while this runner owns event projection and
/// the outer loop the terminal will drive.
pub struct WorkflowRunner<S, E, P> {
    store: S,
    executor: E,
    provider: P,
    events: Arc<EventBus>,
    limits: RunnerLimits,
}

impl<S, E, P> WorkflowRunner<S, E, P> {
    pub fn new(store: S, executor: E, provider: P, events: Arc<EventBus>) -> Self {
        Self {
            store,
            executor,
            provider,
            events,
            limits: RunnerLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: RunnerLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn events(&self) -> &Arc<EventBus> {
        &self.events
    }
}

impl<S, E, P> WorkflowRunner<S, E, P>
where
    S: RunStore,
    E: ActionExecutor,
    P: StepActionProvider,
{
    /// Execute `run` from its current step until the core engine returns a
    /// non-running state. The returned run is the latest durable run snapshot.
    pub async fn run_until_blocked(
        &self,
        definition: &WorkflowDefinition,
        mut run: WorkflowRun,
    ) -> Result<WorkflowRun> {
        self.events.emit(WorkflowEvent::run_started(&run));

        while matches!(run.status, RunStatus::Running) {
            self.execute_one(definition, &mut run).await?;
        }

        Ok(run)
    }

    /// Execute exactly one workflow step when the run is still running, then
    /// return without looping. The run may remain `Running`, advanced to its
    /// next step, so callers can drive a workflow one step at a time.
    pub async fn step_once(
        &self,
        definition: &WorkflowDefinition,
        mut run: WorkflowRun,
    ) -> Result<WorkflowRun> {
        self.events.emit(WorkflowEvent::run_started(&run));

        if matches!(run.status, RunStatus::Running) {
            self.execute_one(definition, &mut run).await?;
        }

        Ok(run)
    }

    /// Execute one core step and emit its lifecycle events, returning the
    /// resulting run status. Shared by [`run_until_blocked`] and [`step_once`].
    async fn execute_one(
        &self,
        definition: &WorkflowDefinition,
        run: &mut WorkflowRun,
    ) -> Result<RunStatus> {
        let step_id = run.current_step.clone();
        let previous_head = run.head.clone();
        self.events.emit(WorkflowEvent::new(
            run.id.clone(),
            WorkflowEventKind::StepStarted {
                step_id: step_id.clone(),
            },
        ));

        let status = match execute_step(
            &self.store,
            &self.executor,
            &self.provider,
            definition,
            run,
            &self.limits,
        )
        .await
        {
            Ok(status) => status,
            Err(err) => {
                self.events.emit(WorkflowEvent::new(
                    run.id.clone(),
                    WorkflowEventKind::RunFailed {
                        reason: err.to_string(),
                    },
                ));
                return Err(err);
            }
        };

        if run.head != previous_head {
            if let Some(head) = &run.head {
                let record = self.store.get_object::<StepRecord>(head)?;
                self.events
                    .emit(WorkflowEvent::step_completed(run.id.clone(), &record));
            }
        }
        self.events
            .emit(WorkflowEvent::run_status(run.id.clone(), &status));

        Ok(status)
    }
}

/// Step action provider that evaluates the current step from a snapshotted Lua workflow.
#[derive(Debug, Clone)]
pub struct LuaStepActionProvider {
    source_bundle: WorkflowSourceSnapshot,
}

impl LuaStepActionProvider {
    pub fn new(source_bundle: WorkflowSourceSnapshot) -> Self {
        Self { source_bundle }
    }

    pub fn source_bundle(&self) -> &WorkflowSourceSnapshot {
        &self.source_bundle
    }
}

fn previous_step_context(prev: Option<&StepRecord>) -> Value {
    let Some(record) = prev else {
        return Value::Null;
    };
    let Some(output) = &record.output else {
        return Value::Null;
    };
    json!({
        "record_id": record.id,
        "step": record.step,
        "action": record.action,
        "status": output.status,
        "fields": output.fields,
        "body": output.body,
        "raw": output.raw,
    })
}

impl StepActionProvider for LuaStepActionProvider {
    fn step_action(
        &self,
        definition: &WorkflowDefinition,
        run: &WorkflowRun,
        step: &StepDefinition,
        prev: Option<&StepRecord>,
    ) -> Result<StepAction> {
        let ctx = json!({
            "request": run.original_request,
            "run_id": run.id,
            "workflow": {
                "name": definition.name,
                "head": definition.head,
            },
            "current_step": step.id,
            "step": {
                "id": step.id,
                "role": step.role,
                "properties": step.properties,
            },
            "resume": run.resume,
            "prev": previous_step_context(prev),
            "steps_executed": run.steps_executed,
        });
        cowboy_workflow_lua::run_step(&self.source_bundle, &step.id, ctx)
            .map(|result| result.action)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use async_trait::async_trait;
    use chrono::Utc;
    use cowboy_workflow_core::{
        ActionExecution, ObjectHash, ObjectKind, RunHead, StatusAction, StepDetail, StepInput,
        StepOutput, TurnRecord,
    };
    use parking_lot::Mutex;
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::Value;

    use super::*;

    #[derive(Default)]
    struct MemoryStore {
        runs: Mutex<HashMap<String, WorkflowRun>>,
        heads: Mutex<HashMap<String, RunHead>>,
        objects: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl RunStore for MemoryStore {
        fn save_run(&self, run: &WorkflowRun) -> Result<()> {
            self.runs.lock().insert(run.id.clone(), run.clone());
            Ok(())
        }

        fn load_run(&self, run_id: &cowboy_workflow_core::RunId) -> Result<WorkflowRun> {
            self.runs
                .lock()
                .get(run_id)
                .cloned()
                .ok_or_else(|| WorkflowError::InvalidAction("missing run".to_string()))
        }

        fn list_runs(&self) -> Result<Vec<RunHead>> {
            Ok(self.heads.lock().values().cloned().collect())
        }

        fn put_object<T: Serialize>(&self, _kind: ObjectKind, value: &T) -> Result<ObjectHash> {
            let bytes = serde_json::to_vec(value).unwrap();
            let hash = format!("hash-{}", self.objects.lock().len() + 1);
            self.objects.lock().insert(hash.clone(), bytes);
            Ok(hash)
        }

        fn get_object<T: DeserializeOwned>(&self, hash: &ObjectHash) -> Result<T> {
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

        fn save_role_session(&self, _session: cowboy_workflow_core::RoleSession) -> Result<()> {
            Ok(())
        }

        fn load_role_session(
            &self,
            _run_id: &str,
            _role_id: &str,
        ) -> Result<Option<cowboy_workflow_core::RoleSession>> {
            Ok(None)
        }

        fn delete_role_sessions(&self, _run_id: &str) -> Result<()> {
            Ok(())
        }

        fn append_turn(&self, _run_id: &str, _turn: TurnRecord) -> Result<ObjectHash> {
            Ok("turn".to_string())
        }
    }

    struct NoopExecutor;

    #[async_trait]
    impl ActionExecutor for NoopExecutor {
        async fn execute(
            &self,
            _action: StepAction,
            context: cowboy_workflow_core::ExecutionContext,
        ) -> Result<ActionExecution> {
            let now = Utc::now();
            Ok(ActionExecution::StepCompleted(Box::new(StepRecord {
                id: context.step_record_id,
                prev: context.prev,
                step: context.step_id,
                action: "agent".to_string(),
                input: StepInput {
                    prompt: Some("agent prompt".to_string()),
                    context: Value::Null,
                },
                output: Some(StepOutput {
                    status: "success".to_string(),
                    fields: Value::Null,
                    body: "agent done".to_string(),
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

    struct StaticProvider(Vec<StepAction>);

    impl StepActionProvider for StaticProvider {
        fn step_action(
            &self,
            _definition: &WorkflowDefinition,
            run: &WorkflowRun,
            _step: &StepDefinition,
            _prev: Option<&StepRecord>,
        ) -> Result<StepAction> {
            let index = run.steps_executed.saturating_sub(1) as usize;
            self.0
                .get(index)
                .cloned()
                .ok_or_else(|| WorkflowError::InvalidAction("missing test action".to_string()))
        }
    }

    fn definition() -> WorkflowDefinition {
        use cowboy_workflow_core::{RoleDefinition, StepTransitions};

        let mut start = StepDefinition {
            id: "start".to_string(),
            role: None,
            transitions: StepTransitions::new(),
            properties: Value::Null,
        };
        start.transitions.insert("next", "agent");
        WorkflowDefinition {
            name: "wf".to_string(),
            description: None,
            source_hash: "source".to_string(),
            head: "start".to_string(),
            roles: BTreeMap::from([(
                "developer".to_string(),
                RoleDefinition {
                    id: "developer".to_string(),
                    instructions: "implement".to_string(),
                    agent: None,
                    properties: Value::Null,
                },
            )]),
            steps: BTreeMap::from([
                ("start".to_string(), start),
                (
                    "agent".to_string(),
                    StepDefinition {
                        id: "agent".to_string(),
                        role: Some("developer".to_string()),
                        transitions: cowboy_workflow_core::StepTransitions::new(),
                        properties: Value::Null,
                    },
                ),
            ]),
        }
    }

    fn run() -> WorkflowRun {
        let now = Utc::now();
        WorkflowRun {
            id: "run-1".to_string(),
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
    async fn runner_executes_until_terminal_status_and_emits_events() {
        let bus = Arc::new(EventBus::new(16));
        let mut events = bus.subscribe();
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            NoopExecutor,
            StaticProvider(vec![
                StepAction::Status(StatusAction {
                    status: "next".to_string(),
                    fields: Value::Null,
                    body: "first".to_string(),
                }),
                StepAction::Agent(cowboy_workflow_core::AgentAction {
                    role: "developer".to_string(),
                    prompt: "do it".to_string(),
                    output: None,
                }),
            ]),
            bus,
        );

        let run = runner
            .run_until_blocked(&definition(), run())
            .await
            .unwrap();
        assert_eq!(run.status, RunStatus::Completed);
        assert_eq!(run.steps_executed, 2);

        let mut kinds = Vec::new();
        while let Ok(event) = events.try_recv() {
            kinds.push(event.kind);
        }
        assert!(matches!(kinds[0], WorkflowEventKind::RunStarted { .. }));
        assert!(kinds.iter().any(|kind| matches!(
            kind,
            WorkflowEventKind::StepCompleted { step_id, .. } if step_id == "start"
        )));
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, WorkflowEventKind::RunCompleted))
        );
    }

    #[tokio::test]
    async fn step_once_executes_a_single_step() {
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            NoopExecutor,
            StaticProvider(vec![
                StepAction::Status(StatusAction {
                    status: "next".to_string(),
                    fields: Value::Null,
                    body: "first".to_string(),
                }),
                StepAction::Agent(cowboy_workflow_core::AgentAction {
                    role: "developer".to_string(),
                    prompt: "do it".to_string(),
                    output: None,
                }),
            ]),
            Arc::new(EventBus::new(16)),
        );
        let definition = definition();

        // First step runs `start`, routes to `agent`, run stays Running.
        let run = runner.step_once(&definition, run()).await.unwrap();
        assert_eq!(run.steps_executed, 1);
        assert_eq!(run.current_step, "agent");
        assert_eq!(run.status, RunStatus::Running);

        // Second step runs `agent`, which has no transition -> Completed.
        let run = runner.step_once(&definition, run).await.unwrap();
        assert_eq!(run.steps_executed, 2);
        assert_eq!(run.status, RunStatus::Completed);

        // Stepping a non-running run is a no-op.
        let run = runner.step_once(&definition, run).await.unwrap();
        assert_eq!(run.steps_executed, 2);
        assert_eq!(run.status, RunStatus::Completed);
    }

    #[tokio::test]
    async fn lua_provider_exposes_previous_step_output() {
        let source_bundle = WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".to_string(),
            files: BTreeMap::from([(
                "main.lua".to_string(),
                r#"
                local start = step("start")
                start.run = function(ctx)
                  return action.status {
                    status = "next",
                    fields = { summary = "planned", files = { "AGENTS.md" } },
                    body = "plan body"
                  }
                end

                local finish = step("finish")
                finish.run = function(ctx)
                  local prev = ctx.prev or {}
                  local fields = prev.fields or {}
                  return action.status {
                    status = "success",
                    fields = {
                      previous_step = prev.step,
                      previous_status = prev.status,
                      summary = fields.summary,
                      first_file = fields.files and fields.files[1] or nil,
                    },
                    body = tostring(prev.body)
                  }
                end

                start:on("next", finish)
                return workflow("wf", start)
                "#
                .to_string(),
            )]),
        };
        let definition = cowboy_workflow_lua::compile_snapshot(&source_bundle).unwrap();
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            NoopExecutor,
            LuaStepActionProvider::new(source_bundle),
            Arc::new(EventBus::new(16)),
        );

        let run = runner.run_until_blocked(&definition, run()).await.unwrap();

        assert_eq!(run.status, RunStatus::Completed);
        let head = run.head.as_ref().expect("final step should be persisted");
        let record = runner.store().get_object::<StepRecord>(head).unwrap();
        let output = record.output.expect("final step should have output");
        assert_eq!(output.status, "success");
        assert_eq!(output.fields["previous_step"], "start");
        assert_eq!(output.fields["previous_status"], "next");
        assert_eq!(output.fields["summary"], "planned");
        assert_eq!(output.fields["first_file"], "AGENTS.md");
        assert_eq!(output.body, "plan body");
    }

    #[test]
    fn lua_provider_returns_action_from_snapshot() {
        let provider = LuaStepActionProvider::new(WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".to_string(),
            files: BTreeMap::from([(
                "main.lua".to_string(),
                r#"
                local implement = step("implement")
                implement.run = function(ctx)
                  return action.status { status = "success", body = ctx.request }
                end
                return workflow("wf", implement)
                "#
                .to_string(),
            )]),
        });
        let mut run = run();
        run.current_step = "implement".to_string();
        let definition = cowboy_workflow_lua::compile_snapshot(provider.source_bundle()).unwrap();
        let action = provider
            .step_action(
                &definition,
                &run,
                definition.steps.get("implement").unwrap(),
                None,
            )
            .unwrap();

        let StepAction::Status(action) = action else {
            panic!("expected status action")
        };
        assert_eq!(action.status, "success");
        assert_eq!(action.body, "do it");
    }
}
