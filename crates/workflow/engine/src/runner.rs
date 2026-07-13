use chrono::Utc;
use std::sync::Arc;

use cowboy_workflow_core::{
    ActionDispatcher, Result, RunStatus, RunStore, StepAction, StepActionProvider, StepDefinition,
    StepRecord, WorkflowDefinition, WorkflowError, WorkflowRun, WorkflowSourceSnapshot,
    apply_run_status, execute_step, retry_current_step,
};
use serde_json::{Value, json};

use crate::{
    active_clock::ActiveRunClock,
    events::{EventBus, WorkflowEvent, WorkflowEventKind},
};

/// Runs one workflow until it completes, fails, waits for input, or hits a budget.
///
/// This is the TUI-facing orchestration seam over `cowboy-workflow-core`: the
/// core engine owns step semantics, while this runner owns event projection and
/// the outer loop the terminal will drive.
pub struct WorkflowRunner<S, E, P> {
    store: S,
    executor: E,
    provider: P,
    events: Arc<EventBus>,
    request_topic: Option<String>,
    active_clock: Option<ActiveRunClock>,
}

impl<S, E, P> WorkflowRunner<S, E, P> {
    pub fn new(store: S, executor: E, provider: P, events: Arc<EventBus>) -> Self {
        Self {
            store,
            executor,
            provider,
            events,
            request_topic: None,
            active_clock: None,
        }
    }

    pub fn with_request_topic(mut self, request_topic: Option<String>) -> Self {
        self.request_topic = request_topic;
        self
    }

    pub(crate) fn with_active_clock(mut self, active_clock: ActiveRunClock) -> Self {
        self.active_clock = Some(active_clock);
        self
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn events(&self) -> &Arc<EventBus> {
        &self.events
    }

    fn run_started_event(&self, run: &WorkflowRun) -> WorkflowEvent {
        match &self.active_clock {
            Some(clock) => clock.run_started_with_topic(run, self.request_topic.clone()),
            None => WorkflowEvent::run_started_with_topic(run, self.request_topic.clone()),
        }
    }

    fn workflow_event_for_run(&self, run: &WorkflowRun, kind: WorkflowEventKind) -> WorkflowEvent {
        match &self.active_clock {
            Some(clock) => clock.event_for_run(run, kind),
            None => WorkflowEvent::for_run(run, kind),
        }
    }

    fn step_completed_event(&self, run: &WorkflowRun, record: &StepRecord) -> WorkflowEvent {
        match &self.active_clock {
            Some(clock) => clock.step_completed_for_run(run, record),
            None => WorkflowEvent::step_completed_for_run(run, record),
        }
    }
}

impl<S, E, P> WorkflowRunner<S, E, P>
where
    S: RunStore,
    E: ActionDispatcher,
    P: StepActionProvider,
{
    /// Execute `run` from its current step until the core engine returns a
    /// non-running state. The returned run is the latest durable run snapshot.
    pub async fn run_until_blocked(
        &self,
        definition: &WorkflowDefinition,
        mut run: WorkflowRun,
    ) -> Result<WorkflowRun> {
        self.events.emit(self.run_started_event(&run));

        let mut execution = Ok(());
        while matches!(run.status, RunStatus::Running) {
            if let Err(err) = self.execute_one(definition, &mut run).await {
                execution = Err(err);
                break;
            }
        }

        self.close_active_window(&mut run)?;
        execution?;
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
        self.events.emit(self.run_started_event(&run));

        let mut execution = Ok(());
        if matches!(run.status, RunStatus::Running)
            && let Err(err) = self.execute_one(definition, &mut run).await
        {
            execution = Err(err);
        }

        self.close_active_window(&mut run)?;
        execution?;
        Ok(run)
    }

    fn close_active_window(&self, run: &mut WorkflowRun) -> Result<()> {
        if let Some(clock) = &self.active_clock {
            clock.close(&self.store, run)?;
        }

        Ok(())
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
        self.events.emit(self.workflow_event_for_run(
            run,
            WorkflowEventKind::StepStarted {
                step_id: step_id.clone(),
            },
        ));

        let limits = run.config_set.limits;
        let status = match execute_step(
            &self.store,
            &self.executor,
            &self.provider,
            definition,
            run,
            &limits,
        )
        .await
        {
            Ok(status) => status,
            Err(err) => match self.retry_step(definition, run, &step_id, err).await {
                Ok(status) => status,
                Err(err) => {
                    let reason = err.to_string();
                    let _ = apply_run_status(
                        &self.store,
                        run,
                        RunStatus::Failed {
                            reason: reason.clone(),
                        },
                    );
                    self.events.emit(
                        self.workflow_event_for_run(run, WorkflowEventKind::RunFailed { reason }),
                    );
                    return Err(err);
                }
            },
        };

        if run.head != previous_head
            && let Some(head) = &run.head
        {
            let record = self.store.get_object::<StepRecord>(head)?;
            self.events.emit(self.step_completed_event(run, &record));
        }
        self.events
            .emit(self.workflow_event_for_run(run, WorkflowEventKind::from(&status)));

        Ok(status)
    }

    /// Retry a recoverable failure within the run-wide and per-step cumulative
    /// budgets. Retry event numbering remains local to this step visit.
    async fn retry_step(
        &self,
        definition: &WorkflowDefinition,
        run: &mut WorkflowRun,
        step_id: &str,
        first_error: WorkflowError,
    ) -> Result<RunStatus> {
        if !first_error.recoverable() {
            return Err(first_error);
        }

        let limits = run.config_set.limits;
        let run_remaining = limits.max_retries_per_run.saturating_sub(run.retries_used);
        let step_remaining = limits
            .max_retries_per_step
            .saturating_sub(run.step_retries_used.get(step_id).copied().unwrap_or(0));
        let (allowance, max_attempts) = retry_visit_bounds(run_remaining, step_remaining);
        let mut last_error = first_error;

        for retry_index in 1..=allowance {
            if let Some(exhausted) = self.retry_exhaustion_error(run, step_id, &last_error) {
                return Err(exhausted);
            }

            run.retries_used += 1;
            *run.step_retries_used
                .entry(step_id.to_string())
                .or_default() += 1;
            run.updated_at = Utc::now();
            self.store.save_run(run)?;

            let attempt = u64::from(retry_index) + 1;
            self.events.emit(self.workflow_event_for_run(
                run,
                WorkflowEventKind::StepRetrying {
                    step_id: step_id.to_string(),
                    attempt,
                    max_attempts,
                    reason: last_error.to_string(),
                },
            ));
            match retry_current_step(
                &self.store,
                &self.executor,
                &self.provider,
                definition,
                run,
                attempt,
                Some(last_error.to_string()),
            )
            .await
            {
                Ok(status) => return Ok(status),
                Err(err) if !err.recoverable() => return Err(err),
                Err(err) => last_error = err,
            }
        }

        Err(self
            .retry_exhaustion_error(run, step_id, &last_error)
            .unwrap_or(last_error))
    }

    fn retry_exhaustion_error(
        &self,
        run: &WorkflowRun,
        step_id: &str,
        last_error: &WorkflowError,
    ) -> Option<WorkflowError> {
        let limits = run.config_set.limits;
        if run.retries_used >= limits.max_retries_per_run {
            return Some(WorkflowError::InvalidAction(format!(
                "config set {:?} exhausted run retry budget: {}/{} retries used; last recoverable error: {last_error}",
                run.config_set.name, run.retries_used, limits.max_retries_per_run
            )));
        }

        let step_retries_used = run.step_retries_used.get(step_id).copied().unwrap_or(0);
        if step_retries_used >= limits.max_retries_per_step {
            return Some(WorkflowError::InvalidAction(format!(
                "config set {:?} exhausted retry budget for step {step_id:?}: {step_retries_used}/{} retries used; last recoverable error: {last_error}",
                run.config_set.name, limits.max_retries_per_step
            )));
        }

        None
    }
}

fn retry_visit_bounds(run_remaining: u32, step_remaining: u32) -> (u32, u64) {
    let allowance = run_remaining.min(step_remaining);
    let max_attempts = u64::from(allowance) + 1;
    (allowance, max_attempts)
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
            "resume": Value::Null,
            "prev": previous_step_context(prev),
            "steps_executed": run.steps_executed,
            "system": crate::system::system_context(),
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
        ActionDispatcher, ActionResult, ObjectHash, ObjectKind, RunHead, RunnerLimits,
        StatusAction, StepDetail, StepInput, StepOutput, TurnRecord,
    };
    use parking_lot::Mutex;
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::Value;

    use super::*;

    #[derive(Clone, Default)]
    struct MemoryStore {
        runs: Arc<Mutex<HashMap<String, WorkflowRun>>>,
        heads: Arc<Mutex<HashMap<String, RunHead>>>,
        objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
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

    struct NoopDispatcher;

    #[async_trait]
    impl ActionDispatcher for NoopDispatcher {
        async fn dispatch(
            &self,
            action: StepAction,
            context: cowboy_workflow_core::ExecutionContext,
        ) -> Result<ActionResult> {
            let now = Utc::now();
            let (action_name, prompt, status, fields, body) = match action {
                StepAction::Status(action) => (
                    "status".to_string(),
                    None,
                    action.status,
                    action.fields,
                    action.body,
                ),
                StepAction::Command(action) => (
                    "command".to_string(),
                    None,
                    action.success_status,
                    Value::Null,
                    "command done".to_string(),
                ),
                _ => (
                    "agent".to_string(),
                    Some("agent prompt".to_string()),
                    "success".to_string(),
                    Value::Null,
                    "agent done".to_string(),
                ),
            };
            Ok(ActionResult::completed(StepRecord {
                id: context.step_record_id,
                prev: context.prev,
                step: context.step_id,
                action: action_name,
                input: StepInput {
                    prompt,
                    context: Value::Null,
                },
                output: Some(StepOutput {
                    status,
                    fields,
                    body,
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
            }))
        }
    }

    /// Dispatcher that fails its first `remaining_failures` calls (recoverably
    /// or not) and then completes. Records the `attempt` seen on each call so
    /// tests can assert retry numbering and budget behaviour.
    struct FlakyDispatcher {
        remaining_failures: Mutex<u32>,
        recoverable: bool,
        dispatches: Arc<Mutex<u32>>,
        seen_attempts: Arc<Mutex<Vec<u64>>>,
    }

    #[async_trait]
    impl ActionDispatcher for FlakyDispatcher {
        async fn dispatch(
            &self,
            _action: StepAction,
            context: cowboy_workflow_core::ExecutionContext,
        ) -> Result<ActionResult> {
            *self.dispatches.lock() += 1;
            self.seen_attempts.lock().push(context.attempt);
            {
                let mut remaining = self.remaining_failures.lock();
                if *remaining > 0 {
                    *remaining -= 1;
                    return Err(if self.recoverable {
                        WorkflowError::RecoverableAction("needs frontmatter".to_string())
                    } else {
                        WorkflowError::InvalidAction("fatal".to_string())
                    });
                }
            }
            let now = Utc::now();
            Ok(ActionResult::completed(StepRecord {
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
            }))
        }
    }

    struct PersistedRetryDispatcher {
        store: MemoryStore,
        dispatches: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl ActionDispatcher for PersistedRetryDispatcher {
        async fn dispatch(
            &self,
            _action: StepAction,
            context: cowboy_workflow_core::ExecutionContext,
        ) -> Result<ActionResult> {
            *self.dispatches.lock() += 1;
            if context.attempt > 1 {
                let persisted = self.store.load_run(&context.run_id)?;
                assert_eq!(persisted.retries_used, 1);
                assert_eq!(persisted.step_retries_used[&context.step_id], 1);
            }

            Err(WorkflowError::RecoverableAction(
                "still failing".to_string(),
            ))
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
            config_set: None,
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
            request_topic: None,
            config_set: Default::default(),
            status: RunStatus::Running,
            retries_used: 0,
            step_retries_used: Default::default(),
            current_step: "start".to_string(),
            head: None,
            resume: Value::Null,
            steps_executed: 0,
            step_visits: BTreeMap::new(),
            active_duration_ms: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn runner_executes_until_terminal_status_and_emits_events() {
        let bus = Arc::new(EventBus::new(16));
        let mut events = bus.subscribe();
        let initial_run = run();
        let run_started_at = initial_run.created_at;
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            NoopDispatcher,
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
        )
        .with_request_topic(Some("Add health route".to_string()))
        .with_active_clock(ActiveRunClock::open_at(&initial_run, Utc::now()));
        let run = runner
            .run_until_blocked(&definition(), initial_run)
            .await
            .unwrap();
        assert_eq!(run.status, RunStatus::Completed);
        assert_eq!(run.steps_executed, 2);

        let mut collected_events = Vec::new();
        while let Ok(event) = events.try_recv() {
            collected_events.push(event);
        }
        assert!(
            collected_events
                .iter()
                .all(|event| event.run_started_at == Some(run_started_at)),
            "{collected_events:#?}"
        );
        assert!(
            collected_events
                .iter()
                .all(|event| event.run_active_duration_ms.is_some()),
            "{collected_events:#?}"
        );
        assert!(collected_events.iter().any(|event| matches!(
            &event.kind,
            WorkflowEventKind::StepStarted { step_id } if step_id == "start"
        )));
        let kinds = collected_events
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds[0],
            WorkflowEventKind::RunStarted {
                workflow_name: "wf".to_string(),
                current_step: "start".to_string(),
                request_topic: Some("Add health route".to_string()),
            }
        );
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
        let bus = Arc::new(EventBus::new(16));
        let mut events = bus.subscribe();
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            NoopDispatcher,
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
            bus.clone(),
        )
        .with_request_topic(Some("Single step topic".to_string()));
        let definition = definition();

        // First step runs `start`, routes to `agent`, run stays Running.
        let run = runner.step_once(&definition, run()).await.unwrap();
        assert_eq!(run.steps_executed, 1);
        assert_eq!(run.current_step, "agent");
        assert_eq!(run.status, RunStatus::Running);
        let first_event = events.try_recv().unwrap();
        assert_eq!(
            first_event.kind,
            WorkflowEventKind::RunStarted {
                workflow_name: "wf".to_string(),
                current_step: "start".to_string(),
                request_topic: Some("Single step topic".to_string()),
            }
        );

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
            NoopDispatcher,
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

    #[tokio::test]
    async fn lua_provider_exposes_system_context() {
        let source_bundle = WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".to_string(),
            files: BTreeMap::from([(
                "main.lua".to_string(),
                r#"
                local implement = step("start")
                implement.run = function(ctx)
                  return action.status { status = "success", body = ctx.system.os }
                end
                return workflow("wf", implement)
                "#
                .to_string(),
            )]),
        };
        let definition = cowboy_workflow_lua::compile_snapshot(&source_bundle).unwrap();
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            NoopDispatcher,
            LuaStepActionProvider::new(source_bundle),
            Arc::new(EventBus::new(16)),
        );

        let run = runner.run_until_blocked(&definition, run()).await.unwrap();

        assert_eq!(run.status, RunStatus::Completed);
        let head = run.head.as_ref().expect("final step should be persisted");
        let record = runner.store().get_object::<StepRecord>(head).unwrap();
        let output = record.output.expect("final step should have output");
        assert_eq!(output.body, std::env::consts::OS);
    }

    fn agent_run() -> WorkflowRun {
        let mut run = run();
        run.current_step = "agent".to_string();
        run
    }

    fn agent_run_with_retry_limits(
        max_retries_per_run: u32,
        max_retries_per_step: u32,
    ) -> WorkflowRun {
        let mut run = agent_run();
        run.config_set.limits = RunnerLimits {
            max_steps_per_run: 5,
            max_visits_per_step: 3,
            max_retries_per_run,
            max_retries_per_step,
        };
        run
    }

    fn agent_action() -> StepAction {
        StepAction::Agent(cowboy_workflow_core::AgentAction {
            role: "developer".to_string(),
            prompt: "do it".to_string(),
            output: None,
        })
    }

    #[tokio::test]
    async fn retries_recoverable_failure_then_succeeds() {
        let dispatches = Arc::new(Mutex::new(0));
        let seen_attempts = Arc::new(Mutex::new(Vec::new()));
        let bus = Arc::new(EventBus::new(32));
        let mut events = bus.subscribe();
        let initial_run = agent_run_with_retry_limits(200, 2);
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            FlakyDispatcher {
                remaining_failures: Mutex::new(2),
                recoverable: true,
                dispatches: dispatches.clone(),
                seen_attempts: seen_attempts.clone(),
            },
            StaticProvider(vec![agent_action()]),
            bus,
        )
        .with_active_clock(ActiveRunClock::open_at(&initial_run, Utc::now()));

        let run = runner
            .run_until_blocked(&definition(), initial_run)
            .await
            .unwrap();

        assert_eq!(run.status, RunStatus::Completed);
        // Two failures + one success.
        assert_eq!(*dispatches.lock(), 3);
        // Attempts are numbered 1, 2, 3 across the retries.
        assert_eq!(*seen_attempts.lock(), vec![1, 2, 3]);
        // Retries reuse the same step budget: the step is only visited once.
        assert_eq!(run.step_visits.get("agent").copied().unwrap_or(0), 1);
        assert_eq!(run.retries_used, 2);
        assert_eq!(run.step_retries_used["agent"], 2);

        let mut collected_events = Vec::new();
        while let Ok(event) = events.try_recv() {
            collected_events.push(event);
        }
        assert!(
            collected_events
                .iter()
                .all(|event| event.run_active_duration_ms.is_some()),
            "{collected_events:#?}"
        );
        let kinds = collected_events
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>();
        let retrying = kinds
            .iter()
            .filter_map(|kind| match kind {
                WorkflowEventKind::StepRetrying {
                    attempt,
                    max_attempts,
                    ..
                } => Some((*attempt, *max_attempts)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(retrying, vec![(2, 3), (3, 3)]);
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, WorkflowEventKind::RunCompleted))
        );
    }

    #[tokio::test]
    async fn exhausts_recoverable_retries_and_persists_failed() {
        let dispatches = Arc::new(Mutex::new(0));
        let seen_attempts = Arc::new(Mutex::new(Vec::new()));
        let bus = Arc::new(EventBus::new(32));
        let mut events = bus.subscribe();
        let initial_run = agent_run_with_retry_limits(200, 2);
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            FlakyDispatcher {
                remaining_failures: Mutex::new(u32::MAX),
                recoverable: true,
                dispatches: dispatches.clone(),
                seen_attempts: seen_attempts.clone(),
            },
            StaticProvider(vec![agent_action()]),
            bus,
        )
        .with_active_clock(ActiveRunClock::open_at(&initial_run, Utc::now()));

        let err = runner
            .run_until_blocked(&definition(), initial_run)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("exhausted retry budget for step \"agent\"")
        );

        // Initial attempt + 2 retries.
        assert_eq!(*dispatches.lock(), 3);

        let stored = runner.store().load_run(&"run-1".to_string()).unwrap();
        assert!(matches!(stored.status, RunStatus::Failed { .. }));
        // The failed step stays current so it can be resolved manually.
        assert_eq!(stored.current_step, "agent");
        assert_eq!(stored.retries_used, 2);
        assert_eq!(stored.step_retries_used["agent"], 2);

        let mut collected_events = Vec::new();
        while let Ok(event) = events.try_recv() {
            collected_events.push(event);
        }
        assert!(
            collected_events
                .iter()
                .all(|event| event.run_active_duration_ms.is_some()),
            "{collected_events:#?}"
        );
        let kinds = collected_events
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>();
        let retrying = kinds
            .iter()
            .filter(|kind| matches!(kind, WorkflowEventKind::StepRetrying { .. }))
            .count();
        assert_eq!(retrying, 2);
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, WorkflowEventKind::RunFailed { .. }))
        );
    }

    #[tokio::test]
    async fn non_recoverable_failure_is_not_retried() {
        let dispatches = Arc::new(Mutex::new(0));
        let seen_attempts = Arc::new(Mutex::new(Vec::new()));
        let bus = Arc::new(EventBus::new(32));
        let mut events = bus.subscribe();
        let initial_run = agent_run_with_retry_limits(200, 2);
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            FlakyDispatcher {
                remaining_failures: Mutex::new(1),
                recoverable: false,
                dispatches: dispatches.clone(),
                seen_attempts: seen_attempts.clone(),
            },
            StaticProvider(vec![agent_action()]),
            bus,
        );

        let result = runner.run_until_blocked(&definition(), initial_run).await;
        assert!(result.is_err());
        // No retry attempted for a non-recoverable error.
        assert_eq!(*dispatches.lock(), 1);

        let stored = runner.store().load_run(&"run-1".to_string()).unwrap();
        assert!(matches!(stored.status, RunStatus::Failed { .. }));
        assert_eq!(stored.retries_used, 0);
        assert!(stored.step_retries_used.is_empty());

        let mut kinds = Vec::new();
        while let Ok(event) = events.try_recv() {
            kinds.push(event.kind);
        }
        assert!(
            !kinds
                .iter()
                .any(|kind| matches!(kind, WorkflowEventKind::StepRetrying { .. }))
        );
    }

    #[tokio::test]
    async fn zero_retry_limits_use_run_first_exhaustion_precedence() {
        let dispatches = Arc::new(Mutex::new(0));
        let initial_run = agent_run_with_retry_limits(0, 0);
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            FlakyDispatcher {
                remaining_failures: Mutex::new(u32::MAX),
                recoverable: true,
                dispatches: dispatches.clone(),
                seen_attempts: Arc::new(Mutex::new(Vec::new())),
            },
            StaticProvider(vec![agent_action()]),
            Arc::new(EventBus::new(8)),
        );

        let err = runner
            .run_until_blocked(&definition(), initial_run)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("exhausted run retry budget"));
        assert!(err.to_string().contains("0/0 retries used"));
        assert_eq!(*dispatches.lock(), 1);
        let stored = runner.store().load_run(&"run-1".to_string()).unwrap();
        assert_eq!(stored.retries_used, 0);
        assert!(stored.step_retries_used.is_empty());
    }

    #[tokio::test]
    async fn run_ceiling_fixes_visit_local_max_attempts() {
        let bus = Arc::new(EventBus::new(16));
        let mut events = bus.subscribe();
        let initial_run = agent_run_with_retry_limits(1, 4);
        let runner = WorkflowRunner::new(
            MemoryStore::default(),
            FlakyDispatcher {
                remaining_failures: Mutex::new(u32::MAX),
                recoverable: true,
                dispatches: Arc::new(Mutex::new(0)),
                seen_attempts: Arc::new(Mutex::new(Vec::new())),
            },
            StaticProvider(vec![agent_action()]),
            bus,
        );

        let err = runner
            .run_until_blocked(&definition(), initial_run)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exhausted run retry budget"));

        let retrying = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event.kind {
                WorkflowEventKind::StepRetrying {
                    attempt,
                    max_attempts,
                    ..
                } => Some((attempt, max_attempts)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(retrying, vec![(2, 2)]);
    }

    #[tokio::test]
    async fn repeated_visits_share_the_per_step_retry_budget() {
        let first_visit = agent_run_with_retry_limits(10, 2);
        let first_runner = WorkflowRunner::new(
            MemoryStore::default(),
            FlakyDispatcher {
                remaining_failures: Mutex::new(1),
                recoverable: true,
                dispatches: Arc::new(Mutex::new(0)),
                seen_attempts: Arc::new(Mutex::new(Vec::new())),
            },
            StaticProvider(vec![agent_action()]),
            Arc::new(EventBus::new(8)),
        );
        let mut second_visit = first_runner
            .step_once(&definition(), first_visit)
            .await
            .unwrap();
        assert_eq!(second_visit.step_retries_used["agent"], 1);
        second_visit.status = RunStatus::Running;
        second_visit.current_step = "agent".to_string();
        second_visit.head = None;

        let dispatches = Arc::new(Mutex::new(0));
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let second_runner = WorkflowRunner::new(
            MemoryStore::default(),
            FlakyDispatcher {
                remaining_failures: Mutex::new(u32::MAX),
                recoverable: true,
                dispatches: dispatches.clone(),
                seen_attempts: attempts.clone(),
            },
            StaticProvider(vec![agent_action(), agent_action()]),
            Arc::new(EventBus::new(8)),
        );

        let err = second_runner
            .step_once(&definition(), second_visit)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exhausted retry budget for step"));
        assert_eq!(*dispatches.lock(), 2);
        assert_eq!(*attempts.lock(), vec![1, 2]);
        let stored = second_runner
            .store()
            .load_run(&"run-1".to_string())
            .unwrap();
        assert_eq!(stored.step_retries_used["agent"], 2);
    }

    #[tokio::test]
    async fn retry_reservation_is_saved_before_dispatch() {
        let store = MemoryStore::default();
        let first_dispatches = Arc::new(Mutex::new(0));
        let initial_run = agent_run_with_retry_limits(1, 1);
        let runner = WorkflowRunner::new(
            store.clone(),
            PersistedRetryDispatcher {
                store: store.clone(),
                dispatches: first_dispatches.clone(),
            },
            StaticProvider(vec![agent_action()]),
            Arc::new(EventBus::new(8)),
        );

        runner
            .step_once(&definition(), initial_run)
            .await
            .unwrap_err();

        assert_eq!(*first_dispatches.lock(), 2);
        let persisted = store.load_run(&"run-1".to_string()).unwrap();
        assert_eq!(persisted.retries_used, 1);
        assert_eq!(persisted.step_retries_used["agent"], 1);
    }

    async fn reconstructed_retry_failure(
        limits: RunnerLimits,
        retries_used: u32,
        step_retries_used: u32,
    ) -> (WorkflowError, u32) {
        let store = MemoryStore::default();
        let mut persisted = agent_run();
        persisted.config_set.limits = limits;
        persisted.retries_used = retries_used;
        persisted
            .step_retries_used
            .insert("agent".to_string(), step_retries_used);
        store.save_run(&persisted).unwrap();
        let reloaded = store.load_run(&persisted.id).unwrap();

        let dispatches = Arc::new(Mutex::new(0));
        let reconstructed = WorkflowRunner::new(
            store,
            FlakyDispatcher {
                remaining_failures: Mutex::new(u32::MAX),
                recoverable: true,
                dispatches: dispatches.clone(),
                seen_attempts: Arc::new(Mutex::new(Vec::new())),
            },
            StaticProvider(vec![agent_action()]),
            Arc::new(EventBus::new(8)),
        );

        let error = reconstructed
            .step_once(&definition(), reloaded)
            .await
            .unwrap_err();
        let dispatch_count = *dispatches.lock();
        (error, dispatch_count)
    }

    #[tokio::test]
    async fn reconstructed_run_exhaustion_preserves_step_capacity() {
        let (error, dispatches) = reconstructed_retry_failure(
            RunnerLimits {
                max_steps_per_run: 5,
                max_visits_per_step: 3,
                max_retries_per_run: 1,
                max_retries_per_step: 2,
            },
            1,
            0,
        )
        .await;

        assert!(error.to_string().contains("exhausted run retry budget"));
        assert_eq!(dispatches, 1);
    }

    #[tokio::test]
    async fn reconstructed_step_exhaustion_preserves_run_capacity() {
        let (error, dispatches) = reconstructed_retry_failure(
            RunnerLimits {
                max_steps_per_run: 5,
                max_visits_per_step: 3,
                max_retries_per_run: 2,
                max_retries_per_step: 1,
            },
            0,
            1,
        )
        .await;

        assert!(
            error
                .to_string()
                .contains("exhausted retry budget for step \"agent\"")
        );
        assert_eq!(dispatches, 1);
    }
    #[test]
    fn maximum_u32_retry_allowance_has_representable_attempt_bounds() {
        let (allowance, max_attempts) = retry_visit_bounds(u32::MAX, u32::MAX);

        assert_eq!(allowance, u32::MAX);
        assert_eq!(max_attempts, u64::from(u32::MAX) + 1);
        assert_eq!(u64::from(allowance) + 1, max_attempts);
        assert!(max_attempts > u64::from(u32::MAX));
    }
}
