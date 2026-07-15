use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use cowboy_agent_acp::Client as AcpClient;
use cowboy_agent_client::ModelInfo;
use cowboy_workflow_agent::{
    AgentExecutionConfig, AgentExecutor, AgentProgress, AgentProgressKind,
};
use cowboy_workflow_catalog::{
    AppliedWorkflowImprovement, WorkflowCatalogLoader, apply_improvement, load_source_ref,
};
use cowboy_workflow_core::{
    ActionResult, DEFAULT_CONFIG_SET_NAME, ExecutionContext, ObjectKind, ResolvedConfigSet, Result,
    RunHead, RunStatus, RunnerLimits, StatusAction, StepAction, StepActionProvider, StepRecord,
    WorkflowCatalog, WorkflowDefinition, WorkflowError, WorkflowRun, WorkflowSelector,
    WorkflowSourceRef, WorkflowSourceSnapshot, WorkflowSummarizer, apply_run_status,
    apply_step_record,
};
use cowboy_workflow_store::RedbRunStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::active_clock::ActiveRunClock;
use crate::agent_resolver::AgentResolver;
use crate::run_lock::RunExecutionLocks;
use crate::runtime_dependencies::{
    ProductionRuntimeDependencies, RuntimeDependencies, transport_for,
};
use crate::workflow::DeterministicSelector;
use crate::{
    EngineActionDispatcher, EventBus, LuaStepActionProvider, ResumeRouter, WorkflowEvent,
    WorkflowEventKind, WorkflowRunner,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub cwd: PathBuf,
    pub state_dir: PathBuf,
    pub workflow_store: PathBuf,
    #[serde(default)]
    pub workflow_dirs: Vec<PathBuf>,
    pub agents: Vec<AgentRuntimeConfig>,
    pub config_sets: BTreeMap<String, RunnerLimitsConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRuntimeConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub model: ModelInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerLimitsConfig {
    pub max_steps_per_run: u32,
    pub max_visits_per_step: u32,
    pub max_retries_per_run: u32,
    pub max_retries_per_step: u32,
}

impl Default for RunnerLimitsConfig {
    fn default() -> Self {
        let limits = RunnerLimits::default();
        Self {
            max_steps_per_run: limits.max_steps_per_run,
            max_visits_per_step: limits.max_visits_per_step,
            max_retries_per_run: limits.max_retries_per_run,
            max_retries_per_step: limits.max_retries_per_step,
        }
    }
}

impl AgentRuntimeConfig {
    pub fn new(
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        model_id: impl Into<String>,
        provider: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args,
            model: ModelInfo {
                id: model_id.into(),
                provider,
            },
        }
    }
}

impl RuntimeConfig {
    pub fn new(
        cwd: PathBuf,
        state_dir: PathBuf,
        workflow_store: PathBuf,
        workflow_dirs: Vec<PathBuf>,
        agents: Vec<AgentRuntimeConfig>,
        config_sets: BTreeMap<String, RunnerLimitsConfig>,
    ) -> Self {
        Self {
            cwd,
            state_dir,
            workflow_store,
            workflow_dirs,
            agents,
            config_sets,
        }
    }
}

impl From<RunnerLimitsConfig> for RunnerLimits {
    fn from(value: RunnerLimitsConfig) -> Self {
        Self {
            max_steps_per_run: value.max_steps_per_run,
            max_visits_per_step: value.max_visits_per_step,
            max_retries_per_run: value.max_retries_per_run,
            max_retries_per_step: value.max_retries_per_step,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub run: WorkflowRun,
    pub events: Vec<WorkflowEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummaryLine {
    pub run_id: String,
    pub workflow_name: String,
    pub topic: Option<String>,
    pub status: RunStatus,
    pub status_detail: RunStatusDetail,
    pub current_step: String,
    pub head_step: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatusState {
    Running,
    WaitingForInput,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatusState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::WaitingForInput => "waiting_for_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStatusDetail {
    pub state: RunStatusState,
    pub reason: Option<String>,
    pub waiting_step: Option<String>,
    pub prompt_id: Option<String>,
    pub message: Option<String>,
    pub choices: Vec<String>,
}

impl RunStatusDetail {
    pub fn from_status(status: &RunStatus) -> Self {
        match status {
            RunStatus::Running => Self {
                state: RunStatusState::Running,
                reason: None,
                waiting_step: None,
                prompt_id: None,
                message: None,
                choices: Vec::new(),
            },
            RunStatus::WaitingForInput {
                step,
                prompt_id,
                message,
                choices,
                ..
            } => Self {
                state: RunStatusState::WaitingForInput,
                reason: None,
                waiting_step: Some(step.clone()),
                prompt_id: Some(prompt_id.clone()),
                message: Some(message.clone()),
                choices: choices.clone(),
            },
            RunStatus::Completed => Self {
                state: RunStatusState::Completed,
                reason: None,
                waiting_step: None,
                prompt_id: None,
                message: None,
                choices: Vec::new(),
            },
            RunStatus::Failed { reason } => Self {
                state: RunStatusState::Failed,
                reason: Some(reason.clone()),
                waiting_step: None,
                prompt_id: None,
                message: None,
                choices: Vec::new(),
            },
            RunStatus::Cancelled => Self {
                state: RunStatusState::Cancelled,
                reason: None,
                waiting_step: None,
                prompt_id: None,
                message: None,
                choices: Vec::new(),
            },
        }
    }
}

/// Guided choices for manually resolving a run stopped on a failed step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionOptions {
    /// Run being resolved.
    pub run_id: String,
    /// Step the run is currently stopped on.
    pub failed_step: String,
    /// Failure reason recorded on the run, when the run is `Failed`.
    pub failure_reason: Option<String>,
    /// Statuses the failed step can be resolved to, with the info each needs.
    pub statuses: Vec<ResolutionStatus>,
}

/// One resolvable status for a failed step and the information it requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionStatus {
    /// Status the user can resolve the failed step to.
    pub status: String,
    /// Step the run routes to for this status, or `None` when the run completes.
    pub target_step: Option<String>,
    /// Output fields that must be provided to resolve with this status.
    pub required_fields: Vec<String>,
    /// Output fields that may optionally be provided.
    pub optional_fields: Vec<String>,
    /// Whether a human-readable body is expected/useful for this status.
    pub body_expected: bool,
}

#[derive(Clone)]
pub struct WorkflowRuntime {
    config: RuntimeConfig,
    events: Arc<EventBus>,
    run_locks: RunExecutionLocks,
    selector: SelectorMode,
    dependencies: Arc<dyn RuntimeDependencies>,
}

/// How far [`WorkflowRuntime`] drives a run in a single call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Execute steps until the run blocks, fails, or completes.
    UntilBlocked,
    /// Execute exactly one workflow step, then return.
    SingleStep,
}
struct ActiveRunExecution {
    request_topic: Option<String>,
    events: Vec<WorkflowEvent>,
    active_clock: ActiveRunClock,
}

struct ActiveRunCancellationGuard {
    store: RedbRunStore,
    run_id: String,
    active_clock: ActiveRunClock,
    armed: bool,
}

impl ActiveRunCancellationGuard {
    fn new(store: RedbRunStore, run_id: String, active_clock: ActiveRunClock) -> Self {
        Self {
            store,
            run_id,
            active_clock,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ActiveRunCancellationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        match self.store.load_run(&self.run_id) {
            Ok(mut run) => {
                if let Err(err) = self.active_clock.close(&self.store, &mut run) {
                    tracing::warn!(run_id = %self.run_id, error = %err, "failed to close active run clock during cancellation");
                }
            }
            Err(err) => {
                tracing::warn!(run_id = %self.run_id, error = %err, "failed to load run during active clock cancellation cleanup");
            }
        }
    }
}

/// Workflow selection strategy used by [`WorkflowRuntime`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectorMode {
    /// Ask the configured ACP agent to choose a workflow from the catalog.
    Agent,
    /// Pick the first catalog workflow by id; used by tests with no live agent.
    Deterministic,
}

impl WorkflowRuntime {
    pub fn new(config: RuntimeConfig) -> Self {
        Self::with_dependencies(config, Arc::new(ProductionRuntimeDependencies))
    }

    fn with_dependencies(
        config: RuntimeConfig,
        dependencies: Arc<dyn RuntimeDependencies>,
    ) -> Self {
        Self {
            run_locks: RunExecutionLocks::new(config.workflow_store.clone()),
            config,
            events: Arc::new(EventBus::default()),
            selector: SelectorMode::Agent,
            dependencies,
        }
    }

    /// Use the deterministic (first-by-id) selector instead of the agent-backed
    /// one. Intended for tests that have no live agent backend.
    pub fn with_deterministic_selector(mut self) -> Self {
        self.selector = SelectorMode::Deterministic;
        self
    }

    pub fn events(&self) -> Arc<EventBus> {
        self.events.clone()
    }

    pub fn catalog(&self) -> Result<WorkflowCatalog> {
        let mut catalog = self
            .catalog_loader()
            .load_catalog()
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        // Descriptions for filesystem workflows are declared in Lua, so they are
        // only available after compiling the source. Compilation is non-fatal
        // here: an invalid workflow stays listed, just without a description.
        for source_ref in catalog.workflows.values_mut() {
            if source_ref.description.is_some() || source_ref.root.is_none() {
                continue;
            }
            if let Ok(compiled) = cowboy_workflow_lua::load(source_ref) {
                source_ref.description = compiled.definition.description;
            }
        }
        Ok(catalog)
    }

    pub fn list_runs(&self) -> Result<Vec<RunSummaryLine>> {
        let store = self.store()?;
        let mut runs = Vec::new();
        for head in store.list_runs()? {
            if let Ok(run) = store.load_run(&head.run_id) {
                let topic = self.summary_topic(&run);
                let status_detail = RunStatusDetail::from_status(&head.status);
                runs.push(RunSummaryLine {
                    run_id: run.id,
                    workflow_name: run.workflow_name,
                    topic,
                    status: head.status,
                    status_detail,
                    current_step: run.current_step,
                    head_step: head.head_step,
                });
            }
        }
        Ok(runs)
    }

    fn summary_topic(&self, run: &WorkflowRun) -> Option<String> {
        run.request_topic.clone().or_else(|| {
            self.load_events(&run.id).ok().and_then(|events| {
                events.into_iter().find_map(|event| match event.kind {
                    WorkflowEventKind::RunStarted { request_topic, .. } => request_topic,
                    _ => None,
                })
            })
        })
    }

    pub fn load_run(&self, run_id: &str) -> Result<WorkflowRun> {
        Ok(self.store()?.load_run(&run_id.to_string())?)
    }

    pub async fn start_run(&self, request: impl Into<String>) -> Result<RunReport> {
        self.start_with(request, RunMode::UntilBlocked).await
    }

    /// Start a new run for the requested catalog workflow id and execute until
    /// the run blocks, fails, suspends, or completes.
    pub async fn start_run_with_workflow(
        &self,
        workflow_id: impl Into<String>,
        request: impl Into<String>,
    ) -> Result<RunReport> {
        self.start_with_workflow(workflow_id.into(), request, RunMode::UntilBlocked)
            .await
    }

    /// Start a new run and execute exactly one workflow step, leaving the run
    /// ready to be advanced with [`step_run`].
    pub async fn start_run_stepwise(&self, request: impl Into<String>) -> Result<RunReport> {
        self.start_with(request, RunMode::SingleStep).await
    }

    /// Start a new run for the requested catalog workflow id and execute exactly
    /// one workflow step, leaving the run ready to be advanced with [`step_run`].
    pub async fn start_run_with_workflow_stepwise(
        &self,
        workflow_id: impl Into<String>,
        request: impl Into<String>,
    ) -> Result<RunReport> {
        self.start_with_workflow(workflow_id.into(), request, RunMode::SingleStep)
            .await
    }

    async fn start_with(&self, request: impl Into<String>, mode: RunMode) -> Result<RunReport> {
        let request = request.into();
        tracing::info!(request = %request, mode = ?mode, "starting workflow run");
        let catalog = self.catalog()?;
        tracing::debug!(
            workflow_count = catalog.workflows.len(),
            "workflow catalog loaded"
        );
        let selection = self.select_workflow(&request, &catalog).await?;
        self.start_catalog_workflow(request, mode, &catalog, &selection.workflow_id)
            .await
    }

    async fn start_with_workflow(
        &self,
        workflow_id: String,
        request: impl Into<String>,
        mode: RunMode,
    ) -> Result<RunReport> {
        let request = request.into();
        tracing::info!(request = %request, workflow_id = %workflow_id, mode = ?mode, "starting requested workflow run");
        let catalog = self.catalog()?;
        tracing::debug!(
            workflow_count = catalog.workflows.len(),
            "workflow catalog loaded"
        );
        self.ensure_workflow_exists(&catalog, &workflow_id)?;
        self.start_catalog_workflow(request, mode, &catalog, &workflow_id)
            .await
    }

    fn ensure_workflow_exists(&self, catalog: &WorkflowCatalog, workflow_id: &str) -> Result<()> {
        if catalog.workflows.contains_key(workflow_id) {
            return Ok(());
        }

        let available = catalog
            .workflows
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        Err(WorkflowError::InvalidAction(format!(
            "unknown workflow id {workflow_id:?}; use a catalog id from /workflows or catalog listings; available workflow ids: {available}"
        )))
    }

    async fn start_catalog_workflow(
        &self,
        request: String,
        mode: RunMode,
        catalog: &WorkflowCatalog,
        workflow_id: &str,
    ) -> Result<RunReport> {
        let run_id = format!("run-{}", Uuid::new_v4());
        let _run_guard = self.run_locks.acquire(&run_id)?;
        let source_ref = catalog
            .workflows
            .get(workflow_id)
            .ok_or_else(|| WorkflowError::InvalidAction("selected workflow missing".to_string()))?;
        let (definition, snapshot, workflow_hash) = self.compile_source(source_ref)?;
        let config_set = self.resolve_config_set(&definition)?;
        tracing::debug!(
            workflow_id = %workflow_id,
            source_entry = %source_ref.entry,
            source_root = ?source_ref.root,
            workflow_hash = %workflow_hash,
            "workflow source compiled"
        );
        let now = Utc::now();
        let mut run = WorkflowRun {
            id: run_id,
            workflow_name: definition.name.clone(),
            workflow_api_version: 1,
            workflow_hash,
            workflow_sources: snapshot.files.clone(),
            original_request: request,
            request_topic: None,
            config_set,
            status: RunStatus::Running,
            current_step: definition.head.clone(),
            head: None,
            resume: Value::Null,
            retries_used: 0,
            step_retries_used: BTreeMap::new(),
            steps_executed: 0,
            step_visits: BTreeMap::new(),
            active_duration_ms: 0,
            created_at: now,
            updated_at: now,
        };
        let store = self.store()?;
        store.save_run(&run)?;
        store.update_run_head(&run.id, run_head(&run))?;
        let active_clock = ActiveRunClock::open(&run);
        tracing::info!(run_id = %run.id, workflow = %run.workflow_name, "created workflow run");
        let request_topic = self.generate_request_topic(&run.original_request).await;
        if let Some(topic) = &request_topic {
            run.request_topic = Some(topic.clone());
            run.updated_at = Utc::now();
            store.save_run(&run)?;
            store.update_run_head(&run.id, run_head(&run))?;
        }

        self.run_existing(run, definition, snapshot, mode, request_topic, active_clock)
            .await
    }

    fn resolve_config_set(&self, definition: &WorkflowDefinition) -> Result<ResolvedConfigSet> {
        let name = definition
            .config_set
            .as_deref()
            .unwrap_or(DEFAULT_CONFIG_SET_NAME);
        let Some(config_set) = self.config.config_sets.get(name).copied() else {
            let available = self
                .config
                .config_sets
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(WorkflowError::InvalidAction(format!(
                "workflow {:?} requested unknown config set {name:?}; available config sets: {available}",
                definition.name
            )));
        };

        Ok(ResolvedConfigSet {
            name: name.to_string(),
            limits: config_set.into(),
        })
    }

    async fn select_workflow(
        &self,
        request: &str,
        catalog: &WorkflowCatalog,
    ) -> Result<cowboy_workflow_core::WorkflowSelection> {
        tracing::debug!(selector = ?self.selector, request, "selecting workflow");
        match self.selector {
            SelectorMode::Deterministic => {
                DeterministicSelector::new().select(request, catalog).await
            }
            SelectorMode::Agent => {
                let resolver = AgentResolver::new(self.config.agents.clone())?;
                let agent = resolver.resolve_default()?;
                let client = AcpClient::connect(transport_for(agent))
                    .await
                    .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
                let selector = crate::AgentWorkflowSelector::new(
                    client,
                    self.config.cwd.to_string_lossy().to_string(),
                    agent.model.clone(),
                );
                selector.select(request, catalog).await
            }
        }
    }

    async fn generate_request_topic(&self, request: &str) -> Option<String> {
        self.dependencies
            .generate_request_topic(&self.config, self.selector, request)
            .await
    }

    pub async fn resume_run(&self, run_id: &str) -> Result<RunReport> {
        self.resume_with(run_id, RunMode::UntilBlocked).await
    }

    /// Execute exactly one workflow step for an existing run, then return. The
    /// run may remain `Running`, advanced to its next step.
    pub async fn step_run(&self, run_id: &str) -> Result<RunReport> {
        self.resume_with(run_id, RunMode::SingleStep).await
    }

    async fn resume_with(&self, run_id: &str, mode: RunMode) -> Result<RunReport> {
        tracing::debug!(run_id, mode = ?mode, "resuming workflow run");
        let _run_guard = self.run_locks.acquire(run_id)?;
        let run = self.load_run(run_id)?;
        tracing::debug!(
            run_id = %run.id,
            status = ?run.status,
            current_step = %run.current_step,
            steps_executed = run.steps_executed,
            "loaded workflow run"
        );
        if !matches!(run.status, RunStatus::Running) {
            tracing::debug!(run_id = %run.id, status = ?run.status, "workflow run is not running; returning without execution");
            return Ok(RunReport {
                run,
                events: Vec::new(),
            });
        }
        let active_clock = ActiveRunClock::open(&run);
        let snapshot = snapshot_from_run(&run);
        let mut definition = cowboy_workflow_lua::compile_snapshot(&snapshot)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        definition.name = run.workflow_name.clone();
        definition.source_hash = run.workflow_hash.clone();
        self.run_existing(run, definition, snapshot, mode, None, active_clock)
            .await
    }

    pub async fn answer_run(
        &self,
        run_id: &str,
        prompt_id: &str,
        answer: &str,
    ) -> Result<RunReport> {
        tracing::info!(
            run_id,
            prompt_id,
            answer_chars = answer.chars().count(),
            "answering workflow prompt"
        );
        let _run_guard = self.run_locks.acquire(run_id)?;
        let store = self.store()?;
        let mut run = store.load_run(&run_id.to_string())?;
        let router = ResumeRouter::default();
        let answer = router.validate_answer(&run, prompt_id, answer)?;
        let active_clock = ActiveRunClock::open(&run);
        let result = router.dispatch_validated_answer(answer)?;
        let snapshot = snapshot_from_run(&run);
        let mut definition = cowboy_workflow_lua::compile_snapshot(&snapshot)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        definition.name = run.workflow_name.clone();
        definition.source_hash = run.workflow_hash.clone();
        let mut events = Vec::new();
        let status = match result {
            ActionResult::Completed(record) => {
                let record = *record;
                let status = apply_step_record(&store, &definition, &mut run, record.clone())?;
                events.push(active_clock.step_completed_for_run(&run, &record));
                events.push(active_clock.run_status_for_run(&run, &status));
                status
            }
            ActionResult::Blocked(status) => {
                let status = apply_run_status(&store, &mut run, status)?;
                events.push(active_clock.run_status_for_run(&run, &status));
                status
            }
        };
        for event in &events {
            self.events.emit(event.clone());
        }
        tracing::debug!(run_id = %run.id, prompt_id, status = ?run.status, "workflow prompt answer completed");

        if matches!(status, RunStatus::Running) {
            self.run_existing_with_events(
                run,
                definition,
                snapshot,
                RunMode::UntilBlocked,
                ActiveRunExecution {
                    request_topic: None,
                    events,
                    active_clock,
                },
            )
            .await
        } else {
            active_clock.close(&store, &mut run)?;
            self.persist_events(&run.id, &events)?;
            Ok(RunReport { run, events })
        }
    }

    /// Inspect a failed run and return the statuses it can be resolved to along
    /// with the information each status requires. See [`resolve_run`].
    pub fn resolution_options(&self, run_id: &str) -> Result<ResolutionOptions> {
        let store = self.store()?;
        let run = store.load_run(&run_id.to_string())?;
        self.build_resolution_options(&run, &store)
    }

    fn build_resolution_options(
        &self,
        run: &WorkflowRun,
        store: &RedbRunStore,
    ) -> Result<ResolutionOptions> {
        ensure_resolvable(run)?;
        let snapshot = snapshot_from_run(run);
        let mut definition = cowboy_workflow_lua::compile_snapshot(&snapshot)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        definition.name = run.workflow_name.clone();
        definition.source_hash = run.workflow_hash.clone();

        let failed_step = run.current_step.clone();
        let step = definition
            .steps
            .get(&failed_step)
            .ok_or_else(|| WorkflowError::UnknownStep {
                step: failed_step.clone(),
            })?
            .clone();

        // Recompute the failed step's action to recover its output shape; a
        // failed step persists no StepRecord, so the schema must be re-evaluated.
        let prev_record = run
            .head
            .as_ref()
            .map(|head| store.get_object::<StepRecord>(head))
            .transpose()?;
        let provider = LuaStepActionProvider::new(snapshot);
        let action = provider
            .step_action(&definition, run, &step, prev_record.as_ref())
            .ok();
        let (required_fields, optional_fields, body_expected) =
            action_output_shape(action.as_ref());

        let mut statuses = Vec::new();
        for (status, target) in &step.transitions.by_status {
            statuses.push(ResolutionStatus {
                status: status.clone(),
                target_step: Some(target.clone()),
                required_fields: required_fields.clone(),
                optional_fields: optional_fields.clone(),
                body_expected,
            });
        }
        if !step.transitions.by_status.contains_key("success") {
            statuses.push(ResolutionStatus {
                status: "success".to_string(),
                target_step: None,
                required_fields: required_fields.clone(),
                optional_fields: optional_fields.clone(),
                body_expected,
            });
        }

        let failure_reason = match &run.status {
            RunStatus::Failed { reason } => Some(reason.clone()),
            _ => None,
        };

        Ok(ResolutionOptions {
            run_id: run.id.clone(),
            failed_step,
            failure_reason,
            statuses,
        })
    }

    /// Manually resolve a failed run by synthesizing a completed step record for
    /// the failed step with the chosen `status`, then route and continue the run.
    pub async fn resolve_run(
        &self,
        run_id: &str,
        status: &str,
        fields: Option<Value>,
        body: Option<String>,
    ) -> Result<RunReport> {
        tracing::info!(run_id, status, "resolving failed workflow run");
        let _run_guard = self.run_locks.acquire(run_id)?;
        let store = self.store()?;
        let mut run = store.load_run(&run_id.to_string())?;
        ensure_resolvable(&run)?;

        let options = self.build_resolution_options(&run, &store)?;
        let Some(chosen) = options.statuses.iter().find(|s| s.status == status) else {
            return Err(WorkflowError::InvalidAction(format!(
                "status {status:?} cannot resolve step {:?}. {}",
                run.current_step,
                describe_resolution_options(&options)
            )));
        };

        let fields_value = fields.unwrap_or(Value::Null);
        let missing: Vec<String> = chosen
            .required_fields
            .iter()
            .filter(|field| !field_present(&fields_value, field))
            .cloned()
            .collect();
        if !missing.is_empty() {
            let required_fields = missing
                .iter()
                .map(|field| format!("{field:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(WorkflowError::InvalidAction(format!(
                "status {status:?} requires field(s): {required_fields}. Provide them via {}.",
                resolution_field_arguments(&missing)
            )));
        }

        let active_clock = ActiveRunClock::open(&run);
        let snapshot = snapshot_from_run(&run);
        let mut definition = cowboy_workflow_lua::compile_snapshot(&snapshot)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        definition.name = run.workflow_name.clone();
        definition.source_hash = run.workflow_hash.clone();

        let record_id = format!("{}-{}", run.id, run.steps_executed.max(1));
        let context = ExecutionContext {
            run_id: run.id.clone(),
            step_id: run.current_step.clone(),
            step_record_id: record_id,
            prev: run.head.clone(),
            role: None,
            attempt: 1,
            retry_reason: None,
        };
        let ActionResult::Completed(record) = crate::StatusActionRunner.run(
            StatusAction {
                status: status.to_string(),
                fields: fields_value,
                body: body.unwrap_or_default(),
            },
            context,
        ) else {
            return Err(WorkflowError::InvalidAction(
                "manual resolution did not produce a completed record".to_string(),
            ));
        };
        let mut record = *record;
        record.action = "manual_resolution".to_string();

        let mut events = Vec::new();
        let status_result = apply_step_record(&store, &definition, &mut run, record.clone())?;
        events.push(active_clock.step_completed_for_run(&run, &record));
        events.push(active_clock.event_for_run(
            &run,
            WorkflowEventKind::ManuallyResolved {
                step_id: record.step.clone(),
                status: status.to_string(),
            },
        ));
        events.push(active_clock.run_status_for_run(&run, &status_result));
        for event in &events {
            self.events.emit(event.clone());
        }

        if matches!(status_result, RunStatus::Running) {
            self.run_existing_with_events(
                run,
                definition,
                snapshot,
                RunMode::UntilBlocked,
                ActiveRunExecution {
                    request_topic: None,
                    events,
                    active_clock,
                },
            )
            .await
        } else {
            active_clock.close(&store, &mut run)?;
            self.persist_events(&run.id, &events)?;
            Ok(RunReport { run, events })
        }
    }

    pub async fn improve_run(&self, run_id: &str) -> Result<AppliedWorkflowImprovement> {
        tracing::info!(run_id, "improving workflow from run");
        let run = self.load_run(run_id)?;
        let resolver = AgentResolver::new(self.config.agents.clone())?;
        let agent = resolver.resolve_default()?;
        let client = AcpClient::connect(transport_for(agent))
            .await
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        let summarizer = crate::AgentWorkflowSummarizer::new(
            client,
            self.config.cwd.to_string_lossy().to_string(),
            agent.model.clone(),
        );
        let summary = summarizer.summarize(&run).await?;
        let catalog = self.catalog()?;
        apply_improvement(self.workflow_update_root(), &catalog, &summary.improvement)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))
    }

    fn catalog_loader(&self) -> WorkflowCatalogLoader {
        let mut loader = WorkflowCatalogLoader::new();
        for dir in &self.config.workflow_dirs {
            loader = loader.with_project_dir(dir);
        }
        loader
    }

    fn workflow_update_root(&self) -> PathBuf {
        self.config
            .workflow_dirs
            .first()
            .cloned()
            .unwrap_or_else(|| self.config.state_dir.join("workflows"))
    }

    fn compile_source(
        &self,
        source_ref: &WorkflowSourceRef,
    ) -> Result<(
        cowboy_workflow_core::WorkflowDefinition,
        WorkflowSourceSnapshot,
        String,
    )> {
        tracing::debug!(
            workflow_id = %source_ref.id,
            source_entry = %source_ref.entry,
            source_root = ?source_ref.root,
            "compiling workflow source"
        );
        let (mut definition, snapshot, workflow_name) = if source_ref.root.is_some() {
            let compiled = cowboy_workflow_lua::load(source_ref)
                .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
            (
                compiled.definition,
                compiled.source_bundle,
                source_ref.id.clone(),
            )
        } else {
            let loaded = load_source_ref(source_ref)
                .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
            let snapshot = WorkflowSourceSnapshot {
                root: loaded.source_ref.root.clone(),
                entry: loaded.source_ref.entry.clone(),
                files: BTreeMap::from([(loaded.source_ref.entry.clone(), loaded.source)]),
            };
            let definition = cowboy_workflow_lua::compile_snapshot(&snapshot)
                .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
            (definition, snapshot, loaded.source_ref.id)
        };
        let store = self.store()?;
        let workflow_hash = store.put_object(ObjectKind::WorkflowSourceSnapshot, &snapshot)?;
        tracing::debug!(
            workflow_id = %workflow_name,
            files = snapshot.files.len(),
            workflow_hash = %workflow_hash,
            "workflow source snapshot persisted"
        );
        definition.name = workflow_name;
        definition.source_hash = workflow_hash.clone();
        Ok((definition, snapshot, workflow_hash))
    }
    async fn run_existing(
        &self,
        run: WorkflowRun,
        definition: cowboy_workflow_core::WorkflowDefinition,
        snapshot: WorkflowSourceSnapshot,
        mode: RunMode,
        request_topic: Option<String>,
        active_clock: ActiveRunClock,
    ) -> Result<RunReport> {
        self.run_existing_with_events(
            run,
            definition,
            snapshot,
            mode,
            ActiveRunExecution {
                request_topic,
                events: Vec::new(),
                active_clock,
            },
        )
        .await
    }

    async fn run_existing_with_events(
        &self,
        run: WorkflowRun,
        definition: cowboy_workflow_core::WorkflowDefinition,
        snapshot: WorkflowSourceSnapshot,
        mode: RunMode,
        execution: ActiveRunExecution,
    ) -> Result<RunReport> {
        let ActiveRunExecution {
            request_topic,
            mut events,
            active_clock,
        } = execution;
        tracing::debug!(
            run_id = %run.id,
            workflow = %definition.name,
            mode = ?mode,
            current_step = %run.current_step,
            steps_executed = run.steps_executed,
            "running workflow"
        );
        let run_id = run.id.clone();
        let progress_clock = active_clock.clone();
        let mut rx = self.events.subscribe();
        let store = self.store()?;
        let mut cancellation_guard =
            ActiveRunCancellationGuard::new(store.clone(), run_id.clone(), active_clock.clone());
        let agent_store = store.clone();
        let progress_events = self.events.clone();
        let agent_config = AgentExecutionConfig {
            cwd: self.config.cwd.to_string_lossy().to_string(),
            mcp_servers: Vec::new(),
            progress: Some(Arc::new(move |progress| {
                progress_events.emit(Self::workflow_event_from_agent_progress(
                    progress,
                    &progress_clock,
                ));
            })),
        };
        let factory = self.dependencies.agent_factory(&self.config)?;
        let executor = AgentExecutor::new(factory, agent_store, agent_config);
        let dispatcher = EngineActionDispatcher::new(executor, self.config.cwd.clone());
        let provider = LuaStepActionProvider::new(snapshot);
        let runner = WorkflowRunner::new(store, dispatcher, provider, self.events.clone())
            .with_request_topic(request_topic)
            .with_active_clock(active_clock);
        let run_future = async {
            match mode {
                RunMode::UntilBlocked => runner.run_until_blocked(&definition, run).await,
                RunMode::SingleStep => runner.step_once(&definition, run).await,
            }
        };
        tokio::pin!(run_future);
        let prefix_len = events.len();
        if prefix_len > 0 {
            self.persist_events(&run_id, &events)?;
        }
        let run_result = loop {
            tokio::select! {
                result = &mut run_future => break result,
                received = rx.recv() => match received {
                    Ok(event) => events.push(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(run_id = %run_id, skipped, "workflow event collector lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!(run_id = %run_id, "workflow event collector closed");
                        break (&mut run_future).await;
                    }
                }
            }
        };
        drain_available_workflow_events(&mut rx, &mut events);
        match &run_result {
            Ok(run) => {
                tracing::debug!(run_id = %run.id, event_count = events.len(), prefix_events = prefix_len, "workflow events collected");
                self.persist_events(&run.id, &events)?;
            }
            Err(err) => {
                tracing::debug!(run_id = %run_id, event_count = events.len(), prefix_events = prefix_len, error = %err, "workflow events collected before run error");
                self.persist_events(&run_id, &events)?;
            }
        }
        cancellation_guard.disarm();
        let run = run_result?;
        Ok(RunReport { run, events })
    }

    fn workflow_event_from_agent_progress(
        progress: AgentProgress,
        active_clock: &ActiveRunClock,
    ) -> WorkflowEvent {
        let run_id = progress.run_id.clone();
        let kind = Self::workflow_event_kind_from_agent_progress(progress);
        active_clock.event(run_id, kind)
    }

    fn workflow_event_kind_from_agent_progress(progress: AgentProgress) -> WorkflowEventKind {
        let step_id = progress.step_id;
        match progress.kind {
            AgentProgressKind::SessionReady { role, session_id } => {
                WorkflowEventKind::AgentSessionReady {
                    step_id,
                    role,
                    session_id,
                }
            }
            AgentProgressKind::Prompt {
                role,
                session_id,
                prompt,
            } => WorkflowEventKind::AgentPrompt {
                step_id,
                role,
                session_id,
                prompt,
            },
            AgentProgressKind::Response { content } => {
                WorkflowEventKind::AgentResponse { step_id, content }
            }
            AgentProgressKind::Thought { content } => {
                WorkflowEventKind::AgentThought { step_id, content }
            }
            AgentProgressKind::ToolCall {
                tool_call_id,
                title,
                tool_kind,
                status,
            } => WorkflowEventKind::AgentToolCall {
                step_id,
                tool_call_id,
                title,
                tool_kind,
                status,
            },
            AgentProgressKind::ToolCallUpdate {
                tool_call_id,
                title,
                status,
                content,
            } => WorkflowEventKind::AgentToolCallUpdate {
                step_id,
                tool_call_id,
                title,
                status,
                content,
            },
            AgentProgressKind::Plan { entries } => {
                WorkflowEventKind::AgentPlan { step_id, entries }
            }
        }
    }

    fn store(&self) -> Result<RedbRunStore> {
        tracing::debug!(path = %self.config.workflow_store.display(), "opening workflow store");
        let store =
            RedbRunStore::create(&self.config.workflow_store).map_err(WorkflowError::from)?;
        tracing::debug!(path = %self.config.workflow_store.display(), "workflow store ready");
        Ok(store)
    }

    fn persist_events(&self, run_id: &str, events: &[WorkflowEvent]) -> Result<()> {
        let dir = self.config.state_dir.join("events");
        fs::create_dir_all(&dir).map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        let path = dir.join(format!("{run_id}.json"));
        let raw = serde_json::to_string_pretty(events)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        fs::write(&path, raw).map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        tracing::debug!(run_id, path = %path.display(), event_count = events.len(), "workflow events persisted");
        Ok(())
    }

    pub fn load_events(&self, run_id: &str) -> Result<Vec<WorkflowEvent>> {
        let path = self
            .config
            .state_dir
            .join("events")
            .join(format!("{run_id}.json"));
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(path)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        serde_json::from_str(&raw).map_err(|err| WorkflowError::InvalidAction(err.to_string()))
    }
}
fn drain_available_workflow_events(
    rx: &mut tokio::sync::broadcast::Receiver<WorkflowEvent>,
    events: &mut Vec<WorkflowEvent>,
) {
    loop {
        match rx.try_recv() {
            Ok(event) => events.push(event),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "workflow event final drain lagged");
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
    }
}

/// A run can be manually resolved only when it is stopped on a step, i.e. it is
/// `Failed` (after giving up) or still `Running` on the failed step.
fn ensure_resolvable(run: &WorkflowRun) -> Result<()> {
    if matches!(run.status, RunStatus::Failed { .. } | RunStatus::Running) {
        Ok(())
    } else {
        Err(WorkflowError::InvalidAction(format!(
            "run {} is {:?}; only failed runs can be resolved",
            run.id, run.status
        )))
    }
}

/// Derive the required/optional output fields and body expectation for a step's
/// recomputed action. Agent actions expose their declared `OutputSpec` fields as
/// required information for manual resolution.
fn action_output_shape(action: Option<&StepAction>) -> (Vec<String>, Vec<String>, bool) {
    match action {
        Some(StepAction::Agent(agent)) => {
            let fields = agent
                .output
                .as_ref()
                .and_then(|output| output.fields.as_object())
                .map(|map| map.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            (fields, Vec::new(), true)
        }
        _ => (Vec::new(), Vec::new(), true),
    }
}

/// Whether `field` is present and non-null in supplied manual-resolution fields.
fn field_present(fields: &Value, field: &str) -> bool {
    fields
        .as_object()
        .and_then(|map| map.get(field))
        .map(|value| !value.is_null())
        .unwrap_or(false)
}

fn resolution_field_arguments(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| {
            format!(
                "--field {} {}",
                quote_command_argument(field),
                quote_command_argument("...")
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_command_argument(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Human-readable list of valid statuses and their required fields for errors.
fn describe_resolution_options(options: &ResolutionOptions) -> String {
    let mut parts = Vec::new();
    for status in &options.statuses {
        if status.required_fields.is_empty() {
            parts.push(status.status.clone());
        } else {
            parts.push(format!(
                "{} (requires: {})",
                status.status,
                status.required_fields.join(", ")
            ));
        }
    }
    format!("Valid statuses: {}", parts.join("; "))
}

fn snapshot_from_run(run: &WorkflowRun) -> WorkflowSourceSnapshot {
    let workflow_entry = format!("{}.lua", run.workflow_name);
    let entry = if run.workflow_sources.contains_key(&workflow_entry) {
        workflow_entry
    } else {
        run.workflow_sources
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "default.lua".to_string())
    };
    WorkflowSourceSnapshot {
        root: None,
        entry,
        files: run.workflow_sources.clone(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_dependencies::{MockRuntimeDependencies, SharedClientFactory};
    use async_trait::async_trait;
    use cowboy_agent_client::{AgentInfo, Client, Event, PromptContent, StopReason};
    use cowboy_workflow_agent::{ClientFactory, ResolvedAgentClient};
    use cowboy_workflow_core::{ResumeCallback, RoleDefinition, RunStatus, StepAction};
    use parking_lot::Mutex as SyncMutex;
    use std::collections::VecDeque;
    use std::os::unix::fs::PermissionsExt;

    fn agent(name: &str, command: &str) -> AgentRuntimeConfig {
        AgentRuntimeConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            model: ModelInfo::default(),
        }
    }

    #[derive(Debug)]
    struct ScriptedAgentState {
        responses: VecDeque<String>,
        prompts: Vec<String>,
        next_session: usize,
        created_roles: Vec<String>,
    }

    #[derive(Clone, Debug)]
    struct ScriptedAgentFactory {
        state: Arc<SyncMutex<ScriptedAgentState>>,
    }

    impl ScriptedAgentFactory {
        fn new(responses: Vec<String>) -> Self {
            Self {
                state: Arc::new(SyncMutex::new(ScriptedAgentState {
                    responses: responses.into(),
                    prompts: Vec::new(),
                    next_session: 0,
                    created_roles: Vec::new(),
                })),
            }
        }

        fn prompts(&self) -> Vec<String> {
            self.state.lock().prompts.clone()
        }

        fn created_roles(&self) -> Vec<String> {
            self.state.lock().created_roles.clone()
        }

        fn assert_exhausted(&self) {
            assert!(
                self.state.lock().responses.is_empty(),
                "scripted agent responses should all be consumed"
            );
        }
    }

    #[derive(Debug)]
    struct ScriptedAgentClient {
        state: Arc<SyncMutex<ScriptedAgentState>>,
        session_id: Option<String>,
    }

    #[async_trait]
    impl Client for ScriptedAgentClient {
        fn is_connected(&self) -> bool {
            true
        }

        fn agent_info(&self) -> Option<&AgentInfo> {
            None
        }

        fn session_id(&self) -> Option<&str> {
            self.session_id.as_deref()
        }

        async fn new_session(
            &mut self,
            _cwd: &str,
            _mcp_servers: &[Value],
            _model: &ModelInfo,
        ) -> anyhow::Result<String> {
            let mut state = self.state.lock();
            state.next_session += 1;
            let session_id = format!("scripted-session-{}", state.next_session);
            self.session_id = Some(session_id.clone());
            Ok(session_id)
        }

        fn supports_load_session(&self) -> bool {
            true
        }

        async fn load_session(
            &mut self,
            session_id: &str,
            _cwd: &str,
            _mcp_servers: &[Value],
        ) -> anyhow::Result<Vec<Event>> {
            self.session_id = Some(session_id.to_string());
            Ok(Vec::new())
        }

        async fn prompt(
            &mut self,
            _session_id: &str,
            prompt_content: Vec<PromptContent>,
            event_handler: &mut (dyn FnMut(Event) + Send),
        ) -> anyhow::Result<StopReason> {
            let prompt = prompt_content
                .into_iter()
                .map(|content| content.text)
                .collect::<Vec<_>>()
                .join("\n");
            let response = {
                let mut state = self.state.lock();
                state.prompts.push(prompt);
                state
                    .responses
                    .pop_front()
                    .ok_or_else(|| anyhow::anyhow!("scripted agent response queue exhausted"))?
            };

            event_handler(Event::MessageChunk {
                content: serde_json::json!({ "text": response }),
            });
            Ok(StopReason::EndTurn)
        }

        async fn close(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl ClientFactory for ScriptedAgentFactory {
        async fn create_client(
            &self,
            role: &RoleDefinition,
        ) -> cowboy_workflow_agent::Result<ResolvedAgentClient> {
            self.state.lock().created_roles.push(role.id.clone());
            Ok(ResolvedAgentClient {
                client: Box::new(ScriptedAgentClient {
                    state: self.state.clone(),
                    session_id: None,
                }),
                model: ModelInfo {
                    id: "scripted-model".to_string(),
                    provider: Some("test".to_string()),
                },
                backend: "scripted-agent".to_string(),
            })
        }
    }

    fn mock_runtime_dependencies(
        topic: Option<&str>,
        factory: Option<ScriptedAgentFactory>,
    ) -> Arc<dyn RuntimeDependencies> {
        let mut dependencies = MockRuntimeDependencies::new();
        let topic = topic.map(str::to_string);
        dependencies
            .expect_generate_request_topic()
            .times(1)
            .withf(|_, selector, request| {
                *selector == SelectorMode::Deterministic && !request.is_empty()
            })
            .returning(move |_, _, _| topic.clone());

        match factory {
            Some(factory) => {
                let factory = SharedClientFactory::new(factory);
                dependencies
                    .expect_agent_factory()
                    .returning(move |_| Ok(factory.clone()));
            }
            None => {
                dependencies
                    .expect_agent_factory()
                    .returning(|config| ProductionRuntimeDependencies.agent_factory(config));
            }
        }

        Arc::new(dependencies)
    }

    #[tokio::test]
    async fn shared_client_factory_forwards_role_and_resolved_client() {
        let scripted = ScriptedAgentFactory::new(Vec::new());
        let factory = SharedClientFactory::new(scripted.clone());
        let role = RoleDefinition {
            id: "reviewer".to_string(),
            instructions: "Review changes".to_string(),
            agent: None,
            properties: Value::Null,
        };

        let resolved = factory.create_client(&role).await.unwrap();

        assert_eq!(resolved.backend, "scripted-agent");
        assert_eq!(scripted.created_roles(), ["reviewer"]);
    }

    #[tokio::test]
    async fn production_request_topic_failure_falls_back_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let config = RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            agents: vec![agent("default", "definitely-missing-topic-agent")],
            config_sets: BTreeMap::new(),
        };

        let topic = ProductionRuntimeDependencies
            .generate_request_topic(&config, SelectorMode::Agent, "summarize this request")
            .await;

        assert_eq!(topic, None);
    }

    #[tokio::test]
    async fn workflow_runtime_propagates_dependency_factory_errors() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success" }
            end
            return workflow("aaa", start)
            "#,
        )
        .unwrap();
        let config = RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: Vec::new(),
            config_sets: BTreeMap::from([("default".to_string(), RunnerLimitsConfig::default())]),
        };
        let mut dependencies = MockRuntimeDependencies::new();
        dependencies
            .expect_generate_request_topic()
            .times(1)
            .return_const(None);
        dependencies.expect_agent_factory().times(1).returning(|_| {
            Err(WorkflowError::InvalidAction(
                "injected factory failure".to_string(),
            ))
        });
        let runtime = WorkflowRuntime::with_dependencies(config, Arc::new(dependencies))
            .with_deterministic_selector();

        let error = runtime.start_run("request").await.unwrap_err();

        assert!(error.to_string().contains("injected factory failure"));
    }

    fn runtime_for_example_workflow(
        dir: &tempfile::TempDir,
        factory: ScriptedAgentFactory,
    ) -> WorkflowRuntime {
        let examples_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("examples/workflows")
            .canonicalize()
            .unwrap();
        let config = RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![examples_root],
            agents: Vec::new(),
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 100,
                    max_visits_per_step: 20,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        };
        WorkflowRuntime::with_dependencies(config, mock_runtime_dependencies(None, Some(factory)))
            .with_deterministic_selector()
    }

    fn waiting_prompt_id(report: &RunReport) -> &str {
        match &report.run.status {
            RunStatus::WaitingForInput { prompt_id, .. } => prompt_id,
            status => panic!("expected waiting run, got {status:?}"),
        }
    }

    fn runtime_for_agent_workflow(
        dir: &tempfile::TempDir,
        role_agent: Option<&str>,
        agents: Vec<AgentRuntimeConfig>,
    ) -> WorkflowRuntime {
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        let agent_field = role_agent
            .map(|agent| format!(", agent = {:?}", agent))
            .unwrap_or_default();
        fs::write(
            workflow_dir.join("aaa.lua"),
            format!(
                r#"
                local developer = role("developer", {{ instructions = "Implement"{agent_field} }})
                local start = step("start", {{ role = developer }})
                start.run = function(ctx)
                  return action.agent {{
                    role = developer,
                    prompt = "Do work",
                    output = {{ status = {{ "success" }}, fields = {{ summary = "string" }} }}
                  }}
                end
                return workflow("aaa", start)
                "#
            ),
        )
        .unwrap();
        WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents,
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        })
        .with_deterministic_selector()
    }

    fn runtime_for_workflow_dir(dir: &tempfile::TempDir, workflow_dir: PathBuf) -> WorkflowRuntime {
        runtime_for_workflow_dir_with_config_sets(
            dir,
            workflow_dir,
            BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        )
    }

    fn runtime_for_workflow_dir_with_config_sets(
        dir: &tempfile::TempDir,
        workflow_dir: PathBuf,
        config_sets: BTreeMap<String, RunnerLimitsConfig>,
    ) -> WorkflowRuntime {
        WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "unused-agent")],
            config_sets,
        })
        .with_deterministic_selector()
    }

    fn runtime_for_empty_state(dir: &tempfile::TempDir) -> WorkflowRuntime {
        WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            agents: vec![agent("default", "unused-agent")],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        })
    }

    fn summary_test_run(
        run_id: &str,
        status: RunStatus,
        request_topic: Option<&str>,
    ) -> WorkflowRun {
        let now = Utc::now();
        WorkflowRun {
            id: run_id.to_string(),
            workflow_name: "aaa".to_string(),
            workflow_api_version: 1,
            workflow_hash: "hash".to_string(),
            workflow_sources: BTreeMap::new(),
            original_request: "do it".to_string(),
            request_topic: request_topic.map(str::to_string),
            config_set: Default::default(),
            status,
            current_step: "start".to_string(),
            head: None,
            resume: Value::Null,
            retries_used: 0,
            step_retries_used: BTreeMap::new(),
            steps_executed: 0,
            step_visits: BTreeMap::new(),
            active_duration_ms: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn runtime_for_topic_workflow(dir: &tempfile::TempDir, topic: &str) -> WorkflowRuntime {
        let workflow_dir = dir.path().join("topic-workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "next", body = "first" }
            end

            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = "done" }
            end

            start:on("next", finish)
            return workflow("aaa-declared", start)
            "#,
        )
        .unwrap();
        let config = RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "unused-agent")],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        };
        WorkflowRuntime::with_dependencies(config, mock_runtime_dependencies(Some(topic), None))
            .with_deterministic_selector()
    }

    fn command_script(dir: &tempfile::TempDir) -> PathBuf {
        let script = dir.path().join("command-helper.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
echo "command stdout: $PWD"
echo "command stderr: $1" >&2
if [ "$1" = "fail" ]; then
  exit 4
fi
exit 0
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        script
    }

    fn command_output_record(runtime: &WorkflowRuntime, report: &RunReport) -> StepRecord {
        let head = report.run.head.as_ref().expect("completed head");
        runtime
            .store()
            .unwrap()
            .get_object::<StepRecord>(head)
            .unwrap()
    }

    #[tokio::test]
    async fn command_action_runtime_routes_and_exposes_prev_fields() {
        let dir = tempfile::tempdir().unwrap();
        let script = command_script(&dir);
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            format!(
                r#"
                local run_command = step("run_command")
                run_command.run = function(ctx)
                  return action.command {{
                    program = {},
                    args = {{ ctx.request }},
                    success_status = "command_ok",
                    failure_status = "command_failed",
                  }}
                end

                local inspect = step("inspect")
                inspect.run = function(ctx)
                  return action.status {{
                    status = "success",
                    fields = {{
                      prev_action = ctx.prev.action,
                      prev_status = ctx.prev.status,
                      stdout = ctx.prev.fields.stdout,
                      stderr = ctx.prev.fields.stderr,
                      success = ctx.prev.fields.success,
                      exit_code = ctx.prev.fields.exit_code,
                    }}
                  }}
                end

                run_command:on("command_ok", inspect)
                run_command:on("command_failed", inspect)
                return workflow("aaa", run_command)
                "#,
                serde_json::to_string(&script.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        let runtime = runtime_for_workflow_dir(&dir, workflow_dir);

        let success = runtime
            .start_run_with_workflow("aaa", "success")
            .await
            .unwrap();
        assert_eq!(success.run.status, RunStatus::Completed);
        let output = command_output_record(&runtime, &success).output.unwrap();
        assert_eq!(output.fields["prev_action"], "command");
        assert_eq!(output.fields["prev_status"], "command_ok");
        assert_eq!(output.fields["success"], true);
        assert_eq!(output.fields["exit_code"], 0);
        assert!(
            output.fields["stdout"]
                .as_str()
                .unwrap()
                .contains("command stdout:")
        );
        assert!(
            output.fields["stderr"]
                .as_str()
                .unwrap()
                .contains("command stderr: success")
        );

        let failure = runtime
            .start_run_with_workflow("aaa", "fail")
            .await
            .unwrap();
        assert_eq!(failure.run.status, RunStatus::Completed);
        let output = command_output_record(&runtime, &failure).output.unwrap();
        assert_eq!(output.fields["prev_action"], "command");
        assert_eq!(output.fields["prev_status"], "command_failed");
        assert_eq!(output.fields["success"], false);
        assert_eq!(output.fields["exit_code"], 4);
        assert!(
            output.fields["stdout"]
                .as_str()
                .unwrap()
                .contains("command stdout:")
        );
        assert!(
            output.fields["stderr"]
                .as_str()
                .unwrap()
                .contains("command stderr: fail")
        );
    }

    fn runtime_for_inline_workflow(dir: &tempfile::TempDir, source: &str) -> WorkflowRuntime {
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(workflow_dir.join("aaa.lua"), source).unwrap();
        runtime_for_workflow_dir(dir, workflow_dir)
    }

    fn config_sets_with_careful(
        careful: Option<RunnerLimitsConfig>,
    ) -> BTreeMap<String, RunnerLimitsConfig> {
        let mut config_sets =
            BTreeMap::from([("default".to_string(), RunnerLimitsConfig::default())]);
        if let Some(careful) = careful {
            config_sets.insert("careful".to_string(), careful);
        }

        config_sets
    }

    fn successful_workflow(config: &str) -> String {
        format!(
            r#"
            local start = step("start")
            start.run = function(ctx) return action.status {{ status = "success" }} end
            return workflow("declared", start{config})
            "#
        )
    }

    #[tokio::test]
    async fn start_resolves_explicit_and_default_config_sets_once() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("explicit.lua"),
            successful_workflow(", { config_set = \"careful\" }"),
        )
        .unwrap();
        fs::write(workflow_dir.join("implicit.lua"), successful_workflow("")).unwrap();
        let careful = RunnerLimitsConfig {
            max_steps_per_run: 9,
            max_visits_per_step: 8,
            max_retries_per_run: 7,
            max_retries_per_step: 6,
        };
        let runtime = runtime_for_workflow_dir_with_config_sets(
            &dir,
            workflow_dir,
            config_sets_with_careful(Some(careful)),
        );

        let explicit = runtime
            .start_run_with_workflow("explicit", "do it")
            .await
            .unwrap();
        assert_eq!(explicit.run.config_set.name, "careful");
        assert_eq!(explicit.run.config_set.limits, careful.into());

        let implicit = runtime
            .start_run_with_workflow("implicit", "do it")
            .await
            .unwrap();
        assert_eq!(implicit.run.config_set.name, "default");
        assert_eq!(implicit.run.config_set.limits, RunnerLimits::default());
    }

    #[tokio::test]
    async fn unknown_config_set_fails_before_run_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("unknown.lua"),
            successful_workflow(", { config_set = \"missing\" }"),
        )
        .unwrap();
        let runtime = runtime_for_workflow_dir_with_config_sets(
            &dir,
            workflow_dir,
            config_sets_with_careful(Some(RunnerLimitsConfig::default())),
        );

        let err = runtime
            .start_run_with_workflow("unknown", "do it")
            .await
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("workflow \"unknown\""), "{message}");
        assert!(
            message.contains("unknown config set \"missing\""),
            "{message}"
        );
        assert!(message.contains("careful, default"), "{message}");
        assert!(runtime.list_runs().unwrap().is_empty());
    }

    #[tokio::test]
    async fn step_and_resume_use_snapshot_after_config_set_changes() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("multi.lua"),
            r#"
            local first = step("first")
            first.run = function(ctx) return action.status { status = "next" } end
            local second = step("second")
            second.run = function(ctx) return action.status { status = "next" } end
            local third = step("third")
            third.run = function(ctx) return action.status { status = "success" } end
            first:on("next", second)
            second:on("next", third)
            return workflow("multi", first, { config_set = "careful" })
            "#,
        )
        .unwrap();
        let original = RunnerLimitsConfig {
            max_steps_per_run: 5,
            max_visits_per_step: 5,
            max_retries_per_run: 5,
            max_retries_per_step: 2,
        };
        let creator = runtime_for_workflow_dir_with_config_sets(
            &dir,
            workflow_dir.clone(),
            config_sets_with_careful(Some(original)),
        );
        let started = creator
            .start_run_with_workflow_stepwise("multi", "do it")
            .await
            .unwrap();
        assert_eq!(started.run.steps_executed, 1);

        let changed = RunnerLimitsConfig {
            max_steps_per_run: 1,
            ..original
        };
        let resumed_runtime = runtime_for_workflow_dir_with_config_sets(
            &dir,
            workflow_dir,
            config_sets_with_careful(Some(changed)),
        );
        let stepped = resumed_runtime.step_run(&started.run.id).await.unwrap();
        assert_eq!(stepped.run.steps_executed, 2);
        assert_eq!(stepped.run.status, RunStatus::Running);
        assert_eq!(stepped.run.config_set.limits, original.into());

        let resumed = resumed_runtime.resume_run(&started.run.id).await.unwrap();
        assert_eq!(resumed.run.status, RunStatus::Completed);
        assert_eq!(resumed.run.steps_executed, 3);
        assert_eq!(resumed.run.config_set.limits, original.into());
    }

    #[tokio::test]
    async fn answer_resolve_and_options_use_snapshot_after_set_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("answer.lua"),
            r#"
            local ask = step("ask")
            ask.run = function(ctx)
              return action.ask_user { id = "approval", message = "Approve?", choices = { "yes" } }
            end
            local done = step("done")
            done.run = function(ctx) return action.status { status = "success" } end
            ask:on("answered", done)
            return workflow("answer", ask, { config_set = "careful" })
            "#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("resolve.lua"),
            r#"
            local broken = step("broken")
            broken.run = function(ctx) return action.fail { reason = "broken" } end
            local done = step("done")
            done.run = function(ctx) return action.status { status = "success" } end
            broken:on("fixed", done)
            return workflow("resolve", broken, { config_set = "careful" })
            "#,
        )
        .unwrap();
        let current_default = RunnerLimitsConfig {
            max_steps_per_run: 1,
            ..RunnerLimitsConfig::default()
        };
        let careful = RunnerLimitsConfig {
            max_steps_per_run: 2,
            ..RunnerLimitsConfig::default()
        };
        let creator = runtime_for_workflow_dir_with_config_sets(
            &dir,
            workflow_dir.clone(),
            BTreeMap::from([
                ("default".to_string(), current_default),
                ("careful".to_string(), careful),
            ]),
        );
        let waiting = creator
            .start_run_with_workflow("answer", "do it")
            .await
            .unwrap();
        assert!(matches!(
            waiting.run.status,
            RunStatus::WaitingForInput { .. }
        ));
        assert_eq!(waiting.run.steps_executed, 1);
        assert_eq!(waiting.run.config_set.limits.max_steps_per_run, 2);
        let failed = creator
            .start_run_with_workflow("resolve", "do it")
            .await
            .unwrap();
        assert!(matches!(failed.run.status, RunStatus::Failed { .. }));
        assert_eq!(failed.run.steps_executed, 1);
        assert_eq!(failed.run.config_set.limits.max_steps_per_run, 2);

        let without_careful = runtime_for_workflow_dir_with_config_sets(
            &dir,
            workflow_dir,
            BTreeMap::from([("default".to_string(), current_default)]),
        );
        let answered = without_careful
            .answer_run(&waiting.run.id, "approval", "yes")
            .await
            .unwrap();
        assert_eq!(answered.run.status, RunStatus::Completed);
        assert_eq!(answered.run.config_set.name, "careful");
        assert_eq!(answered.run.steps_executed, 2);
        assert_eq!(answered.run.config_set.limits.max_steps_per_run, 2);

        let options = without_careful.resolution_options(&failed.run.id).unwrap();
        assert!(
            options
                .statuses
                .iter()
                .any(|status| status.status == "fixed")
        );
        let resolved = without_careful
            .resolve_run(&failed.run.id, "fixed", None, None)
            .await
            .unwrap();
        assert_eq!(resolved.run.status, RunStatus::Completed);
        assert_eq!(resolved.run.config_set.name, "careful");
        assert_eq!(resolved.run.steps_executed, 2);
        assert_eq!(resolved.run.config_set.limits.max_steps_per_run, 2);
    }

    fn prompt_workflow_source() -> &'static str {
        r#"
        local ask = step("ask")
        ask.run = function(ctx)
          return action.ask_user { id = "approval", message = "Approve?", choices = { "yes", "no" }, fields = { carried = "ok" } }
        end

        local decide = step("decide")
        decide.run = function(ctx)
          local total = 0
          for i = 1, 5000000 do total = total + i end
          local fields = (ctx.prev and ctx.prev.fields) or {}
          return action.status { status = tostring(fields.answer), fields = { answer = fields.answer, carried = fields.carried, total = tostring(total) }, body = "decided" }
        end

        local done = step("done")
        done.run = function(ctx)
          return action.status { status = "success", body = "done" }
        end

        ask:on("answered", decide)
        decide:on("yes", done)
        return workflow("aaa", ask)
        "#
    }

    #[test]
    fn cancellation_guard_closes_active_window_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_empty_state(&dir);
        let store = runtime.store().unwrap();
        let mut run = summary_test_run("run-cancel", RunStatus::Running, None);
        let started_at = Utc::now() - chrono::Duration::hours(1);
        run.created_at = started_at;
        run.updated_at = started_at;
        run.active_duration_ms = 7;
        store.save_run(&run).unwrap();
        store.update_run_head(&run.id, run_head(&run)).unwrap();
        let window_started_at = Utc::now() - chrono::Duration::milliseconds(25);
        let active_clock = ActiveRunClock::open_at(&run, window_started_at);

        drop(ActiveRunCancellationGuard::new(
            store.clone(),
            run.id.clone(),
            active_clock,
        ));

        let stored = store.load_run(&run.id).unwrap();
        assert!(
            stored.active_duration_ms >= 32,
            "cancel cleanup should add the open active window: {stored:#?}"
        );
        assert!(
            stored.active_duration_ms < 60_000,
            "cancel cleanup should not charge the full wall-clock age: {stored:#?}"
        );
    }

    fn persist_run_active_duration(
        runtime: &WorkflowRuntime,
        run_id: &str,
        active_duration_ms: u64,
        created_at: chrono::DateTime<Utc>,
    ) -> WorkflowRun {
        let store = runtime.store().unwrap();
        let mut run = store.load_run(&run_id.to_string()).unwrap();
        run.created_at = created_at;
        run.updated_at = created_at;
        run.active_duration_ms = active_duration_ms;
        store.save_run(&run).unwrap();
        store.update_run_head(&run.id, run_head(&run)).unwrap();
        run
    }
    fn first_run_started_topic(report: &RunReport) -> Option<&str> {
        report.events.iter().find_map(|event| match &event.kind {
            WorkflowEventKind::RunStarted { request_topic, .. } => request_topic.as_deref(),
            _ => None,
        })
    }

    #[test]
    fn run_summary_legacy_workflow_run_json_defaults_missing_request_topic() {
        let mut raw = serde_json::to_value(summary_test_run(
            "legacy-run",
            RunStatus::Running,
            Some("discarded topic"),
        ))
        .unwrap();
        let object = raw.as_object_mut().unwrap();
        assert!(object.remove("request_topic").is_some());

        let run: WorkflowRun = serde_json::from_value(raw).unwrap();

        assert_eq!(run.id, "legacy-run");
        assert_eq!(run.request_topic, None);
        assert_eq!(run.status, RunStatus::Running);
    }

    #[test]
    fn run_summary_list_runs_projects_structured_status_detail_for_every_status() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_empty_state(&dir);
        let store = runtime.store().unwrap();
        let waiting = RunStatus::WaitingForInput {
            step: "review".to_string(),
            prompt_id: "approval".to_string(),
            message: "Approve deployment?".to_string(),
            choices: vec!["yes".to_string(), "no".to_string()],
            resume_callback: ResumeCallback::new(
                "ask_user",
                serde_json::json!({ "prompt_id": "approval" }),
            )
            .unwrap(),
        };
        let cases = vec![
            (
                "running-run",
                RunStatus::Running,
                serde_json::json!({
                    "state": "running",
                    "reason": null,
                    "waiting_step": null,
                    "prompt_id": null,
                    "message": null,
                    "choices": [],
                }),
            ),
            (
                "completed-run",
                RunStatus::Completed,
                serde_json::json!({
                    "state": "completed",
                    "reason": null,
                    "waiting_step": null,
                    "prompt_id": null,
                    "message": null,
                    "choices": [],
                }),
            ),
            (
                "failed-run",
                RunStatus::Failed {
                    reason: "agent exited 2".to_string(),
                },
                serde_json::json!({
                    "state": "failed",
                    "reason": "agent exited 2",
                    "waiting_step": null,
                    "prompt_id": null,
                    "message": null,
                    "choices": [],
                }),
            ),
            (
                "cancelled-run",
                RunStatus::Cancelled,
                serde_json::json!({
                    "state": "cancelled",
                    "reason": null,
                    "waiting_step": null,
                    "prompt_id": null,
                    "message": null,
                    "choices": [],
                }),
            ),
            (
                "waiting-run",
                waiting,
                serde_json::json!({
                    "state": "waiting_for_input",
                    "reason": null,
                    "waiting_step": "review",
                    "prompt_id": "approval",
                    "message": "Approve deployment?",
                    "choices": ["yes", "no"],
                }),
            ),
        ];
        for (run_id, status, _) in &cases {
            let run = summary_test_run(run_id, status.clone(), None);
            store.save_run(&run).unwrap();
            store.update_run_head(&run.id, run_head(&run)).unwrap();
        }

        let summaries = runtime.list_runs().unwrap();

        for (run_id, _, expected_detail) in cases {
            let summary = summaries
                .iter()
                .find(|summary| summary.run_id == run_id)
                .unwrap_or_else(|| panic!("missing summary for {run_id}"));
            assert_eq!(
                serde_json::to_value(&summary.status_detail).unwrap(),
                expected_detail,
                "{run_id}"
            );
            let rendered_detail = serde_json::to_string(&summary.status_detail).unwrap();
            for debug_fragment in ["WaitingForInput {", "Failed {", "resume_callback"] {
                assert!(
                    !rendered_detail.contains(debug_fragment),
                    "{run_id} rendered status detail with Rust Debug fragment {debug_fragment:?}: {rendered_detail}"
                );
            }
        }
    }

    #[tokio::test]
    async fn run_summary_start_run_persists_generated_topic_on_run_and_list_summary() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_topic_workflow(&dir, "Persisted generated topic");

        let report = runtime.start_run("add a health endpoint").await.unwrap();
        let run_id = report.run.id.clone();

        assert_eq!(
            report.run.request_topic.as_deref(),
            Some("Persisted generated topic")
        );
        let persisted = runtime.load_run(&run_id).unwrap();
        assert_eq!(
            persisted.request_topic.as_deref(),
            Some("Persisted generated topic")
        );
        runtime.persist_events(&run_id, &[]).unwrap();
        let summary = runtime
            .list_runs()
            .unwrap()
            .into_iter()
            .find(|summary| summary.run_id == run_id)
            .expect("run summary");
        assert_eq!(summary.topic.as_deref(), Some("Persisted generated topic"));
    }

    #[test]
    fn run_summary_list_runs_backfills_topic_from_persisted_run_started_event() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_empty_state(&dir);
        let run = summary_test_run("legacy-event-run", RunStatus::Completed, None);
        let run_id = run.id.clone();
        let store = runtime.store().unwrap();
        store.save_run(&run).unwrap();
        store.update_run_head(&run.id, run_head(&run)).unwrap();
        runtime
            .persist_events(
                &run.id,
                &[WorkflowEvent::run_started_with_topic(
                    &run,
                    Some("Recovered event topic".to_string()),
                )],
            )
            .unwrap();

        let summary = runtime
            .list_runs()
            .unwrap()
            .into_iter()
            .find(|summary| summary.run_id == run_id)
            .expect("run summary");

        assert_eq!(summary.topic.as_deref(), Some("Recovered event topic"));
        assert_eq!(runtime.load_run(&run_id).unwrap().request_topic, None);
    }

    #[tokio::test]
    async fn start_run_attaches_generated_topic_to_runner_event() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_topic_workflow(&dir, "Add health route");

        let report = runtime.start_run("add a /healthz route").await.unwrap();

        assert_eq!(first_run_started_topic(&report), Some("Add health route"));
    }

    #[tokio::test]
    async fn start_run_stepwise_attaches_generated_topic_to_runner_event() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_topic_workflow(&dir, "Plan implementation");

        let report = runtime
            .start_run_stepwise("plan implementation")
            .await
            .unwrap();

        assert_eq!(report.run.status, RunStatus::Running);
        assert_eq!(
            first_run_started_topic(&report),
            Some("Plan implementation")
        );
    }

    #[tokio::test]
    async fn start_run_with_workflow_attaches_generated_topic_to_runner_event() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_topic_workflow(&dir, "Review branch");

        let report = runtime
            .start_run_with_workflow("aaa", "review branch")
            .await
            .unwrap();

        assert_eq!(report.run.workflow_name, "aaa");
        assert_eq!(first_run_started_topic(&report), Some("Review branch"));
    }

    #[tokio::test]
    async fn start_run_with_workflow_stepwise_attaches_generated_topic_to_runner_event() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_topic_workflow(&dir, "Scoped fix");

        let report = runtime
            .start_run_with_workflow_stepwise("aaa", "fix scoped issue")
            .await
            .unwrap();

        assert_eq!(report.run.status, RunStatus::Running);
        assert_eq!(first_run_started_topic(&report), Some("Scoped fix"));
    }

    #[tokio::test]
    async fn existing_run_resume_does_not_attach_new_topic() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_topic_workflow(&dir, "Initial topic");
        let start = runtime.start_run_stepwise("do first step").await.unwrap();

        let report = runtime.resume_run(&start.run.id).await.unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        assert_eq!(first_run_started_topic(&report), None);
        assert!(report.events.iter().any(|event| matches!(
            &event.kind,
            WorkflowEventKind::RunStarted {
                request_topic: None,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn step_run_closes_active_window_without_counting_idle_before_next_step() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_inline_workflow(
            &dir,
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "next", body = "first" }
            end

            local middle = step("middle")
            middle.run = function(ctx)
              local total = 0
              for i = 1, 5000000 do total = total + i end
              return action.status { status = "more", fields = { total = tostring(total) }, body = "middle" }
            end

            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = "done" }
            end

            start:on("next", middle)
            middle:on("more", finish)
            return workflow("aaa", start)
            "#,
        );
        let start = runtime.start_run_stepwise("request").await.unwrap();
        assert_eq!(start.run.status, RunStatus::Running);
        assert_eq!(start.run.current_step, "middle");

        let active_before_idle_ms = 2_000;
        persist_run_active_duration(
            &runtime,
            &start.run.id,
            active_before_idle_ms,
            Utc::now() - chrono::Duration::hours(1),
        );

        let report = runtime.step_run(&start.run.id).await.unwrap();

        assert_eq!(report.run.status, RunStatus::Running);
        assert_eq!(report.run.current_step, "finish");
        let stored = runtime.load_run(&start.run.id).unwrap();
        assert_eq!(stored.active_duration_ms, report.run.active_duration_ms);
        assert!(
            stored.active_duration_ms > active_before_idle_ms,
            "active time must include the current step's execution window: {stored:#?}"
        );
        assert!(
            stored.active_duration_ms < active_before_idle_ms + 60_000,
            "idle wall-clock gap must not be charged as active time: {stored:#?}"
        );
        let max_event_active_ms = report
            .events
            .iter()
            .map(|event| event.run_active_duration_ms.expect("active event duration"))
            .max()
            .expect("step_run should emit lifecycle events");
        assert!(max_event_active_ms >= active_before_idle_ms);
        assert!(
            max_event_active_ms <= stored.active_duration_ms,
            "stored active duration should close after the emitted lifecycle events"
        );
    }

    #[tokio::test]
    async fn answer_run_counts_answer_execution_without_counting_prompt_wait() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_inline_workflow(&dir, prompt_workflow_source());
        let start = runtime.start_run("request").await.unwrap();
        assert!(matches!(
            start.run.status,
            RunStatus::WaitingForInput { .. }
        ));

        let active_before_wait_ms = 3_000;
        persist_run_active_duration(
            &runtime,
            &start.run.id,
            active_before_wait_ms,
            Utc::now() - chrono::Duration::hours(1),
        );

        let report = runtime
            .answer_run(&start.run.id, "approval", "yes")
            .await
            .unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        let stored = runtime.load_run(&start.run.id).unwrap();
        assert_eq!(stored.active_duration_ms, report.run.active_duration_ms);
        assert!(
            stored.active_duration_ms > active_before_wait_ms,
            "active time must include answer/resume execution: {stored:#?}"
        );
        assert!(
            stored.active_duration_ms < active_before_wait_ms + 60_000,
            "prompt wait gap must not be charged as active time: {stored:#?}"
        );
        for event in &report.events {
            let active_ms = event
                .run_active_duration_ms
                .expect("answer/resume event active duration");
            assert!(active_ms >= active_before_wait_ms, "{event:#?}");
            assert!(
                active_ms < active_before_wait_ms + 60_000,
                "prompt wait gap must not be charged to events: {event:#?}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_prompt_answers_do_not_advance_active_duration() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_inline_workflow(&dir, prompt_workflow_source());
        let start = runtime.start_run("request").await.unwrap();
        assert!(matches!(
            start.run.status,
            RunStatus::WaitingForInput { .. }
        ));
        let unchanged_active_ms = 4_000;
        persist_run_active_duration(
            &runtime,
            &start.run.id,
            unchanged_active_ms,
            Utc::now() - chrono::Duration::hours(1),
        );

        let err = runtime
            .answer_run(&start.run.id, "other-prompt", "yes")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("prompt"), "{err}");
        assert_eq!(
            runtime.load_run(&start.run.id).unwrap().active_duration_ms,
            unchanged_active_ms
        );

        let err = runtime
            .answer_run(&start.run.id, "approval", "maybe")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("choice"), "{err}");
        assert_eq!(
            runtime.load_run(&start.run.id).unwrap().active_duration_ms,
            unchanged_active_ms
        );
    }

    #[tokio::test]
    async fn invalid_manual_resolution_does_not_advance_active_duration() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_agent_workflow(
            &dir,
            None,
            vec![agent("default", "definitely-missing-agent")],
        );
        runtime.start_run("do it").await.unwrap_err();
        let run_id = runtime.list_runs().unwrap()[0].run_id.clone();
        let unchanged_active_ms = 5_000;
        persist_run_active_duration(
            &runtime,
            &run_id,
            unchanged_active_ms,
            Utc::now() - chrono::Duration::hours(1),
        );

        let err = runtime
            .resolve_run(
                &run_id,
                "nope",
                Some(serde_json::json!({ "summary": "done" })),
                None,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Valid statuses"), "{err}");
        assert_eq!(
            runtime.load_run(&run_id).unwrap().active_duration_ms,
            unchanged_active_ms
        );

        let err = runtime
            .resolve_run(&run_id, "success", None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("summary"), "{err}");
        assert_eq!(
            runtime.load_run(&run_id).unwrap().active_duration_ms,
            unchanged_active_ms
        );
    }

    #[tokio::test]
    async fn failed_runner_events_are_persisted_with_active_duration() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_agent_workflow(
            &dir,
            None,
            vec![agent("default", "definitely-missing-agent")],
        );

        let err = runtime.start_run("do it").await.unwrap_err();
        assert!(
            err.to_string().contains("definitely-missing-agent"),
            "{err}"
        );
        let run_id = runtime.list_runs().unwrap()[0].run_id.clone();
        let events = runtime.load_events(&run_id).unwrap();

        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, WorkflowEventKind::RunFailed { .. })),
            "{events:#?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, WorkflowEventKind::StepStarted { .. })),
            "{events:#?}"
        );
        assert!(
            events
                .iter()
                .all(|event| event.run_active_duration_ms.is_some()),
            "{events:#?}"
        );
    }

    #[tokio::test]
    async fn starts_builtin_workflow_until_agent_call_attempts_backend() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            agents: vec![AgentRuntimeConfig {
                name: "default".to_string(),
                command: "definitely-missing-agent-command".to_string(),
                args: Vec::new(),
                model: ModelInfo::default(),
            }],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        })
        .with_deterministic_selector();

        let err = runtime.start_run("do it").await.unwrap_err();
        // A missing agent command is retried until the snapshotted per-step
        // budget is exhausted, then reported as a distinct policy failure.
        assert!(err.to_string().contains("exhausted retry budget for step"));
        assert_eq!(runtime.list_runs().unwrap().len(), 1);
        // The give-up path persists a clean Failed status for later resolution.
        let run = runtime
            .load_run(&runtime.list_runs().unwrap()[0].run_id)
            .unwrap();
        assert!(matches!(run.status, RunStatus::Failed { .. }));
        assert_eq!(run.retries_used, 2);
        assert_eq!(run.step_retries_used.values().copied().sum::<u32>(), 2);
    }

    #[test]
    fn resolution_field_guidance_quotes_boundary_names() {
        let fields = vec![
            "foo=bar".to_string(),
            "-review".to_string(),
            " review ".to_string(),
            "quote ' $(printf unsafe)".to_string(),
        ];

        assert_eq!(
            resolution_field_arguments(&fields),
            r#"--field 'foo=bar' '...' --field '-review' '...' --field ' review ' '...' --field 'quote '"'"' $(printf unsafe)' '...'"#
        );
    }

    #[tokio::test]
    async fn resolution_options_discovers_statuses_and_required_fields() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_agent_workflow(
            &dir,
            None,
            vec![agent("default", "definitely-missing-agent")],
        );

        // Fails on the agent step and persists a resolvable Failed run.
        runtime.start_run("do it").await.unwrap_err();
        let run_id = runtime.list_runs().unwrap()[0].run_id.clone();

        let options = runtime.resolution_options(&run_id).unwrap();
        assert_eq!(options.failed_step, "start");
        assert!(options.failure_reason.is_some());
        // The step has no transitions, so only the implicit `success` is offered.
        let success = options
            .statuses
            .iter()
            .find(|s| s.status == "success")
            .expect("success option");
        // Required fields are recovered from the agent action's OutputSpec.
        assert_eq!(success.required_fields, vec!["summary".to_string()]);

        // Resolving without the required field is a clear, actionable error.
        let err = runtime
            .resolve_run(&run_id, "success", None, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains(
                "requires field(s): \"summary\". Provide them via --field 'summary' '...'"
            ),
            "{err}"
        );

        // An unroutable status lists the valid options.
        let err = runtime
            .resolve_run(&run_id, "nope", None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Valid statuses"), "{err}");
        assert!(err.to_string().contains("success"), "{err}");
    }

    #[tokio::test]
    async fn resolve_run_routes_and_exposes_fields_to_next_step() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local developer = role("developer", { instructions = "Implement" })
            local start = step("start", { role = developer })
            start.run = function(ctx)
              return action.agent {
                role = developer,
                prompt = "Do work",
                output = { status = { "planned" }, fields = { summary = "string" } }
              }
            end
            local finish = step("finish")
            finish.run = function(ctx)
              local prev = ctx.prev or {}
              local fields = prev.fields or {}
              return action.status {
                status = "success",
                fields = { prev_status = prev.status, summary = fields.summary },
                body = tostring(prev.body)
              }
            end
            start:on("planned", finish)
            return workflow("aaa", start)
            "#,
        )
        .unwrap();
        let config = RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "definitely-missing-agent")],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        };
        let runtime = WorkflowRuntime::with_dependencies(
            config,
            mock_runtime_dependencies(Some("Original topic"), None),
        )
        .with_deterministic_selector();

        runtime.start_run("do it").await.unwrap_err();
        let run_id = runtime.list_runs().unwrap()[0].run_id.clone();

        // "planned" routes to `finish`; supply the required field and a body.
        let report = runtime
            .resolve_run(
                &run_id,
                "planned",
                Some(serde_json::json!({ "summary": "did the thing" })),
                Some("manual body".to_string()),
            )
            .await
            .unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        let head = report.run.head.as_ref().expect("final head");
        let store = runtime.store().unwrap();
        let record = store.get_object::<StepRecord>(head).unwrap();
        let output = record.output.expect("finish output");
        // The synthesized fields/body are visible to the next step via ctx.prev.
        assert_eq!(output.fields["prev_status"], "planned");
        assert_eq!(output.fields["summary"], "did the thing");
        assert_eq!(output.body, "manual body");
        assert!(
            report.events.iter().any(|event| matches!(
                &event.kind,
                WorkflowEventKind::RunStarted {
                    request_topic: None,
                    ..
                }
            )),
            "{:#?}",
            report.events
        );

        // A ManuallyResolved event is persisted in the run's event log.
        let events = runtime.load_events(&run_id).unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, WorkflowEventKind::ManuallyResolved { .. }))
        );
    }

    #[tokio::test]
    async fn explicit_role_agent_uses_named_backend_before_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_agent_workflow(
            &dir,
            Some("other"),
            vec![
                agent("default", "definitely-missing-default-agent"),
                agent("other", "definitely-missing-other-agent"),
            ],
        );

        let err = runtime.start_run("do it").await.unwrap_err();

        assert!(err.to_string().contains("definitely-missing-other-agent"));
        assert!(!err.to_string().contains("definitely-missing-default-agent"));
    }

    #[tokio::test]
    async fn explicit_unknown_role_agent_fails_before_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_agent_workflow(
            &dir,
            Some("missing"),
            vec![agent("default", "definitely-missing-default-agent")],
        );

        let err = runtime.start_run("do it").await.unwrap_err();

        assert!(err.to_string().contains("unknown agent"));
        assert!(err.to_string().contains("missing"));
        assert!(!err.to_string().contains("Failed to spawn"));
    }

    #[tokio::test]
    async fn implicit_ambiguous_role_agent_fails_before_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_agent_workflow(
            &dir,
            None,
            vec![
                agent("planner", "definitely-missing-planner-agent"),
                agent("reviewer", "definitely-missing-reviewer-agent"),
            ],
        );

        let err = runtime.start_run("do it").await.unwrap_err();

        assert!(err.to_string().contains("ambiguous"));
        assert!(!err.to_string().contains("Failed to spawn"));
    }

    #[tokio::test]
    async fn runs_project_status_workflow_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success", body = "done " .. ctx.request }
            end
            return workflow("aaa", start)
            "#,
        )
        .unwrap();
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![AgentRuntimeConfig {
                name: "default".to_string(),
                command: "unused-agent".to_string(),
                args: Vec::new(),
                model: ModelInfo::default(),
            }],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        })
        .with_deterministic_selector();

        let report = runtime.start_run("request").await.unwrap();

        assert_eq!(report.run.workflow_name, "aaa");
        assert_eq!(report.run.status, RunStatus::Completed);
        assert_eq!(runtime.list_runs().unwrap().len(), 1);
        assert!(!runtime.load_events(&report.run.id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn start_run_with_workflow_uses_requested_catalog_id() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("alpha.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success", body = "alpha selected" }
            end
            return workflow("alpha-declared", start)
            "#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("review.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success", body = "review selected: " .. ctx.request }
            end
            return workflow("review-declared", start)
            "#,
        )
        .unwrap();
        let runtime = runtime_for_workflow_dir(&dir, workflow_dir);

        let report = runtime
            .start_run_with_workflow("review", "do work")
            .await
            .unwrap();

        assert_eq!(report.run.workflow_name, "review");
        assert_eq!(report.run.original_request, "do work");
        assert_eq!(report.run.status, RunStatus::Completed);
        let head = report.run.head.as_ref().expect("completed head");
        let record = runtime
            .store()
            .unwrap()
            .get_object::<StepRecord>(head)
            .unwrap();
        let output = record.output.expect("status output");
        assert_eq!(output.body, "review selected: do work");
        assert_eq!(runtime.list_runs().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn start_run_with_workflow_stepwise_uses_requested_catalog_id() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("alpha.lua"),
            r#"
            local start = step("alpha-start")
            start.run = function(ctx)
              return action.status { status = "success", body = "alpha done" }
            end
            return workflow("alpha-declared", start)
            "#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("review.lua"),
            r#"
            local start = step("review-start")
            start.run = function(ctx)
              return action.status { status = "next", body = "review first" }
            end

            local finish = step("review-finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = "review done" }
            end

            start:on("next", finish)
            return workflow("review-declared", start)
            "#,
        )
        .unwrap();
        let runtime = runtime_for_workflow_dir(&dir, workflow_dir);

        let report = runtime
            .start_run_with_workflow_stepwise("review", "do work")
            .await
            .unwrap();

        assert_eq!(report.run.workflow_name, "review");
        assert_eq!(report.run.status, RunStatus::Running);
        assert_eq!(report.run.current_step, "review-finish");
        assert_eq!(report.run.steps_executed, 1);
        assert_eq!(runtime.list_runs().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn start_run_with_unknown_workflow_id_creates_no_run() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("review.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success", body = "review selected" }
            end
            return workflow("review-declared", start)
            "#,
        )
        .unwrap();
        let runtime = runtime_for_workflow_dir(&dir, workflow_dir);

        let err = runtime
            .start_run_with_workflow("review-declared", "do work")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("unknown workflow id"), "{err}");
        assert!(err.to_string().contains("review-declared"), "{err}");
        assert!(err.to_string().contains("review"), "{err}");
        assert!(runtime.list_runs().unwrap().is_empty());
    }

    #[tokio::test]
    async fn resume_run_continues_stepwise_status_workflow_and_persists_resumed_events() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "next", body = "first" }
            end

            local finish = step("finish")
            finish.run = function(ctx)
              local prev = ctx.prev or {}
              return action.status { status = "success", fields = { prev_status = prev.status }, body = "finished" }
            end

            start:on("next", finish)
            return workflow("aaa", start)
            "#,
        )
        .unwrap();
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "unused-agent")],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        })
        .with_deterministic_selector();

        let start = runtime.start_run_stepwise("request").await.unwrap();
        assert_eq!(start.run.status, RunStatus::Running);
        assert_eq!(start.run.current_step, "finish");
        assert_eq!(start.run.steps_executed, 1);

        let report = runtime.resume_run(&start.run.id).await.unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        assert_eq!(report.run.current_step, "finish");
        assert_eq!(report.run.steps_executed, 2);
        assert!(report.events.iter().any(|event| matches!(
            &event.kind,
            WorkflowEventKind::StepStarted { step_id } if step_id == "finish"
        )));
        assert!(report.events.iter().any(|event| matches!(
            &event.kind,
            WorkflowEventKind::StepCompleted { step_id, action, status, .. }
                if step_id == "finish" && action == "status" && status.as_deref() == Some("success")
        )));
        assert!(
            report
                .events
                .iter()
                .any(|event| matches!(event.kind, WorkflowEventKind::RunCompleted))
        );

        let persisted = runtime.load_events(&start.run.id).unwrap();
        assert_eq!(persisted, report.events);
        assert!(persisted.iter().any(|event| matches!(
            &event.kind,
            WorkflowEventKind::StepStarted { step_id } if step_id == "finish"
        )));
        assert!(persisted.iter().any(|event| matches!(
            &event.kind,
            WorkflowEventKind::StepCompleted { step_id, action, status, .. }
                if step_id == "finish" && action == "status" && status.as_deref() == Some("success")
        )));
    }

    #[tokio::test]
    async fn two_runtimes_start_independent_runs_against_one_store() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success", body = "done " .. ctx.request }
            end
            return workflow("aaa", start)
            "#,
        )
        .unwrap();
        let config = RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "unused-agent")],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        };
        let runtime_a = WorkflowRuntime::new(config.clone()).with_deterministic_selector();
        let runtime_b = WorkflowRuntime::new(config).with_deterministic_selector();

        let first = runtime_a.start_run("first").await.unwrap();
        let second = runtime_b.start_run("second").await.unwrap();

        assert_eq!(first.run.status, RunStatus::Completed);
        assert_eq!(second.run.status, RunStatus::Completed);
        assert_ne!(first.run.id, second.run.id);
        let mut run_ids = runtime_a
            .list_runs()
            .unwrap()
            .into_iter()
            .map(|run| run.run_id)
            .collect::<Vec<_>>();
        run_ids.sort();
        let mut expected = vec![first.run.id, second.run.id];
        expected.sort();
        assert_eq!(run_ids, expected);
    }

    #[tokio::test]
    async fn invalid_step_run_id_rejects_before_lock_path_creation() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: state_dir.clone(),
            workflow_store: state_dir.join("workflow.redb"),
            workflow_dirs: Vec::new(),
            agents: vec![agent("default", "unused-agent")],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        })
        .with_deterministic_selector();

        let err = runtime
            .step_run("../run-00000000-0000-0000-0000-000000000000")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("invalid run id"));
        assert!(!state_dir.join("workflow.redb.locks").exists());
        assert!(!state_dir.join("locks").exists());
        assert!(
            !dir.path()
                .join("run-00000000-0000-0000-0000-000000000000.lock")
                .exists()
        );
    }

    #[tokio::test]
    async fn contended_step_run_returns_active_error_without_redb_wording() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "next", body = "ready" }
            end

            local done = step("done")
            done.run = function(ctx)
              return action.status { status = "success", body = "done" }
            end

            start:on("next", done)
            return workflow("aaa", start)
            "#,
        )
        .unwrap();
        let config = RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state-a"),
            workflow_store: dir.path().join("shared/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "unused-agent")],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        };
        let runtime = WorkflowRuntime::new(config.clone()).with_deterministic_selector();
        let report = runtime.start_run_stepwise("request").await.unwrap();
        assert_eq!(report.run.status, RunStatus::Running);

        let locks = RunExecutionLocks::new(config.workflow_store.clone());
        let _held = locks.acquire(&report.run.id).unwrap();
        let runtime_with_other_state = WorkflowRuntime::new(RuntimeConfig {
            state_dir: dir.path().join("state-b"),
            ..config
        })
        .with_deterministic_selector();
        let err = runtime_with_other_state
            .step_run(&report.run.id)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("already active"));
        assert!(!err.to_string().contains("redb"));
    }

    #[tokio::test]
    async fn answer_run_persists_ask_user_completion_before_resumed_events() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local ask = step("ask")
            ask.run = function(ctx)
              return action.ask_user { id = "approval", message = "Approve?", choices = { "yes", "no" }, fields = { carried = "ok" } }
            end

            local decide = step("decide")
            decide.run = function(ctx)
              local fields = (ctx.prev and ctx.prev.fields) or {}
              return action.status { status = tostring(fields.answer), fields = { answer = fields.answer, carried = fields.carried }, body = "decided" }
            end

            local done = step("done")
            done.run = function(ctx)
              return action.status { status = "success", body = "done" }
            end

            ask:on("answered", decide)
            decide:on("yes", done)
            return workflow("aaa", ask)
            "#,
        )
        .unwrap();
        let config = RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "unused-agent")],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        };
        let runtime = WorkflowRuntime::with_dependencies(
            config,
            mock_runtime_dependencies(Some("Initial prompt topic"), None),
        )
        .with_deterministic_selector();

        let start = runtime.start_run("request").await.unwrap();
        assert!(matches!(
            start.run.status,
            RunStatus::WaitingForInput { .. }
        ));
        let steps_before_answer = start.run.steps_executed;
        assert_eq!(
            first_run_started_topic(&start),
            Some("Initial prompt topic")
        );
        let report = runtime
            .answer_run(&start.run.id, "approval", "yes")
            .await
            .unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        assert_eq!(report.run.steps_executed, steps_before_answer + 2);
        assert!(matches!(
            &report.events[0].kind,
            WorkflowEventKind::StepCompleted { step_id, action, status, .. }
                if step_id == "ask" && action == "ask_user" && status.as_deref() == Some("answered")
        ));
        assert!(matches!(
            &report.events[1].kind,
            WorkflowEventKind::RunStatusChanged { status } if status == "running"
        ));
        assert!(report.events.iter().skip(2).any(|event| matches!(
            &event.kind,
            WorkflowEventKind::StepCompleted { step_id, .. } if step_id == "decide"
        )));
        assert!(
            report
                .events
                .iter()
                .all(|event| event.run_started_at == Some(start.run.created_at)),
            "{:#?}",
            report.events
        );
        assert!(
            report.events.iter().any(|event| matches!(
                &event.kind,
                WorkflowEventKind::RunStarted {
                    request_topic: None,
                    ..
                }
            )),
            "{:#?}",
            report.events
        );

        let persisted = runtime.load_events(&report.run.id).unwrap();
        assert_eq!(persisted, report.events);
    }

    #[tokio::test]
    async fn answer_run_persists_ask_user_completion_when_resumed_step_fails() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local ask = step("ask")
            ask.run = function(ctx)
              return action.ask_user { id = "approval", message = "Approve?", choices = { "yes", "no" } }
            end

            local broken = step("broken")
            broken.run = function(ctx)
              return action.status { body = "missing status" }
            end

            ask:on("answered", broken)
            return workflow("aaa", ask)
            "#,
        )
        .unwrap();
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "unused-agent")],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        })
        .with_deterministic_selector();

        let start = runtime.start_run("request").await.unwrap();
        let err = runtime
            .answer_run(&start.run.id, "approval", "yes")
            .await
            .unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
        let persisted = runtime.load_events(&start.run.id).unwrap();
        assert!(matches!(
            &persisted[0].kind,
            WorkflowEventKind::StepCompleted { step_id, action, status, .. }
                if step_id == "ask" && action == "ask_user" && status.as_deref() == Some("answered")
        ));
        assert!(matches!(
            &persisted[1].kind,
            WorkflowEventKind::RunStatusChanged { status } if status == "running"
        ));
    }

    #[test]
    fn agent_feature_unclear_step_requests_and_reuses_clarification() {
        let source = include_str!("../test_files/agent/00-feature.lua");
        let bundle = WorkflowSourceSnapshot {
            root: None,
            entry: "00-feature.lua".to_string(),
            files: BTreeMap::from([("00-feature.lua".to_string(), source.to_string())]),
        };

        let definition = cowboy_workflow_lua::compile_snapshot(&bundle).unwrap();
        assert_eq!(
            definition.steps["unclear"].transitions.by_status["answered"],
            "unclear_answer"
        );
        assert_eq!(
            definition.steps["unclear_answer"].transitions.by_status["clarified"],
            "plan"
        );

        let result = cowboy_workflow_lua::run_step(
            &bundle,
            "unclear",
            serde_json::json!({ "steps_executed": 2, "resume": {} }),
        )
        .unwrap();
        let StepAction::AskUser(action) = result.action else {
            panic!("expected unclear step to ask the user")
        };
        assert_eq!(action.id, "clarification_2");
        assert!(action.message.contains("acceptance criteria"));
        assert!(action.choices.is_empty());

        let result = cowboy_workflow_lua::run_step(
            &bundle,
            "unclear_answer",
            serde_json::json!({
                "steps_executed": 3,
                "prev": {
                    "step": "unclear",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": { "answer": "Add a status command" },
                },
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("expected clarified status after answer")
        };
        assert_eq!(action.status, "clarified");

        for step in ["plan", "implement", "review", "revise"] {
            let result = cowboy_workflow_lua::run_step(
                &bundle,
                step,
                serde_json::json!({
                    "request": "Let's implement a feature",
                    "prev": {
                        "step": "unclear",
                        "status": "clarified",
                        "fields": { "clarification": "Add a status command" },
                    },
                }),
            )
            .unwrap();
            let StepAction::Agent(action) = result.action else {
                panic!("expected {step} step to call its agent")
            };
            assert!(action.prompt.contains("Let's implement a feature"));
            assert!(
                action.prompt.contains("Additional user context:"),
                "{step} prompt should include the clarification heading"
            );
            assert!(
                action.prompt.contains("- Add a status command"),
                "{step} prompt should include the clarification answer"
            );
            match step {
                "plan" => {
                    assert!(action.prompt.contains("Markdown plan document"));
                    assert!(action.prompt.contains("Tests to be added/updated"));
                    assert!(action.prompt.contains("- [ ]"));
                    assert!(action.prompt.contains("plan_doc"));
                    assert!(action.prompt.contains("docs/plans/"));
                    assert!(action.prompt.contains("snake_case"));
                    assert!(action.prompt.contains("Create `docs/plans`"));
                    let fields = &action.output.as_ref().unwrap().fields;
                    assert_eq!(fields["plan_doc"], "string");
                }
                "implement" => {
                    assert!(action.prompt.contains("mark each completed TODO item"));
                    assert!(action.prompt.contains("- [x]"));
                    let fields = &action.output.as_ref().unwrap().fields;
                    assert_eq!(fields["plan_doc"], "string");
                }
                "review" => {
                    assert!(action.prompt.contains("Verify every checked TODO item"));
                    assert!(action.prompt.contains("unfinished work items"));
                    let fields = &action.output.as_ref().unwrap().fields;
                    assert_eq!(fields["plan_doc"], "string");
                }
                "revise" => {
                    assert!(
                        action
                            .prompt
                            .contains("update the approved plan document's TODO list")
                    );
                    let fields = &action.output.as_ref().unwrap().fields;
                    assert_eq!(fields["plan_doc"], "string");
                }
                _ => unreachable!(),
            }
        }

        let result = cowboy_workflow_lua::run_step(
            &bundle,
            "implement",
            serde_json::json!({
                "request": "Let's implement a feature",
                "prev": {
                    "step": "plan",
                    "status": "ready",
                    "fields": {
                        "summary": "Update AGENTS.md",
                        "files": ["AGENTS.md"],
                        "plan_doc": "docs/plans/update_agents.md",
                    },
                    "body": "Plan body",
                },
            }),
        )
        .unwrap();
        let StepAction::Agent(action) = result.action else {
            panic!("expected implement step to call its agent")
        };
        assert!(action.prompt.contains("Previous plan:"));
        assert!(action.prompt.contains("Step: plan"));
        assert!(action.prompt.contains("Status: ready"));
        assert!(action.prompt.contains("Summary: Update AGENTS.md"));
        assert!(action.prompt.contains("- AGENTS.md"));
        assert!(
            action
                .prompt
                .contains("Plan doc: docs/plans/update_agents.md")
        );
        assert!(action.prompt.contains("Plan body"));

        let result = cowboy_workflow_lua::run_step(
            &bundle,
            "review",
            serde_json::json!({
                "request": "Let's implement a feature",
                "prev": {
                    "step": "implement",
                    "status": "implemented",
                    "fields": {
                        "summary": "Changed AGENTS.md",
                        "files": ["AGENTS.md"],
                        "plan_doc": "docs/plans/update_agents.md",
                    },
                    "body": "Implementation body",
                },
            }),
        )
        .unwrap();
        let StepAction::Agent(action) = result.action else {
            panic!("expected review step to call its agent")
        };
        assert!(action.prompt.contains("Implementation result:"));
        assert!(action.prompt.contains("Summary: Changed AGENTS.md"));
        assert!(
            action
                .prompt
                .contains("Plan doc: docs/plans/update_agents.md")
        );
        assert!(action.prompt.contains("Implementation body"));

        let result = cowboy_workflow_lua::run_step(
            &bundle,
            "revise",
            serde_json::json!({
                "request": "Let's implement a feature",
                "prev": {
                    "step": "review",
                    "status": "changes_requested",
                    "fields": {
                        "feedback": "Remove generated state from the change set",
                        "plan_doc": "docs/plans/update_agents.md",
                    },
                    "body": "Review body",
                },
            }),
        )
        .unwrap();
        let StepAction::Agent(action) = result.action else {
            panic!("expected revise step to call its agent")
        };
        assert!(action.prompt.contains("Reviewer feedback:"));
        assert!(action.prompt.contains("Status: changes_requested"));
        assert!(
            action
                .prompt
                .contains("Feedback: Remove generated state from the change set")
        );
        assert!(
            action
                .prompt
                .contains("Plan doc: docs/plans/update_agents.md")
        );
        assert!(
            action
                .prompt
                .contains("Address only the reviewer feedback above")
        );
    }

    #[test]
    fn example_workflows_enforce_plan_document_todo_contract() {
        let examples_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("examples/workflows")
            .canonicalize()
            .unwrap();
        let catalog = WorkflowCatalogLoader::new()
            .without_builtin()
            .with_project_dir(&examples_root)
            .load_catalog()
            .unwrap();
        let source_ref = catalog.workflows.get("workflows/feature").unwrap();
        let compiled = cowboy_workflow_lua::load(source_ref).unwrap();
        let plan_doc = "docs/plans/example.md";
        let reviewed_plan = "## Plan\nDo it\n\n## Changes\n- Update code\n\n## Tests to be added/updated\n- Add coverage\n\n## How to verify\n- Run tests\n\n## TODO\n- [ ] Update code";

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "plan",
            serde_json::json!({ "request": "add a status command" }),
        )
        .unwrap();
        let StepAction::Agent(plan_action) = result.action else {
            panic!("expected plan step to call its agent")
        };
        assert!(plan_action.prompt.contains("docs/plans/"));
        assert!(plan_action.prompt.contains("snake_case"));
        assert!(plan_action.prompt.contains("Create `docs/plans`"));
        assert!(plan_action.prompt.contains("Tests to be added/updated"));
        assert!(plan_action.prompt.contains("- [ ]"));
        let fields = &plan_action.output.as_ref().unwrap().fields;
        assert_eq!(fields["plan_doc"], "string");
        assert_eq!(fields.get("todo"), None);

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "review_plan",
            serde_json::json!({
                "request": "add a status command",
                "prev": {
                    "step": "plan",
                    "status": "ready",
                    "fields": {
                        "summary": "Example",
                        "plan_doc": plan_doc,
                        "files": [plan_doc],
                    },
                    "body": reviewed_plan,
                },
            }),
        )
        .unwrap();
        let StepAction::Agent(review_plan_action) = result.action else {
            panic!("expected review_plan step to call its agent")
        };
        assert!(
            review_plan_action
                .prompt
                .contains("Plan doc: docs/plans/example.md")
        );
        assert!(
            review_plan_action
                .prompt
                .contains("docs/plans/<snake_case_summary>.md")
        );
        let fields = &review_plan_action.output.as_ref().unwrap().fields;
        assert_eq!(fields["plan_doc"], "string");

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "confirm_plan_answer",
            serde_json::json!({
                "steps_executed": 6,
                "prev": {
                    "step": "confirm_plan",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": { "answer": "yes", "plan": reviewed_plan, "plan_doc": plan_doc },
                },
            }),
        )
        .unwrap();
        let StepAction::Status(confirm_action) = result.action else {
            panic!("expected confirm_plan to preserve the approved plan")
        };
        assert_eq!(confirm_action.status, "confirmed");
        assert_eq!(confirm_action.fields["plan"], reviewed_plan);
        assert_eq!(confirm_action.fields["plan_doc"], plan_doc);
        assert_eq!(confirm_action.body, reviewed_plan);

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "implement",
            serde_json::json!({
                "request": "add a status command",
                "prev": {
                    "step": "confirm_plan",
                    "status": "confirmed",
                    "fields": { "plan": reviewed_plan, "plan_doc": plan_doc },
                    "body": reviewed_plan,
                },
            }),
        )
        .unwrap();
        let StepAction::Agent(implement_action) = result.action else {
            panic!("expected implement step to call its agent")
        };
        assert!(implement_action.prompt.contains("Approved plan:"));
        assert!(
            implement_action
                .prompt
                .contains("Plan doc: docs/plans/example.md")
        );
        assert!(implement_action.prompt.contains("## TODO"));
        assert!(implement_action.prompt.contains("- [ ] Update code"));
        assert!(
            implement_action
                .prompt
                .contains("changing each completed `- [ ]` item to `- [x]`")
        );
        let fields = &implement_action.output.as_ref().unwrap().fields;
        assert_eq!(fields["plan_doc"], "string");

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "test",
            serde_json::json!({
                "request": "add a status command",
                "prev": {
                    "step": "implement",
                    "status": "implemented",
                    "fields": {
                        "summary": "Changed code",
                        "plan_doc": plan_doc,
                        "files": ["src/main.rs"],
                    },
                },
            }),
        )
        .unwrap();
        let StepAction::Agent(test_action) = result.action else {
            panic!("expected test step to call its agent")
        };
        assert!(
            test_action
                .prompt
                .contains("Plan doc: docs/plans/example.md")
        );
        let fields = &test_action.output.as_ref().unwrap().fields;
        assert_eq!(fields["plan_doc"], "string");

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "review",
            serde_json::json!({
                "request": "add a status command",
                "prev": {
                    "step": "test",
                    "status": "passed",
                    "fields": {
                        "summary": "Tests passed",
                        "plan_doc": plan_doc,
                        "commands": ["cargo test"],
                        "failures": [],
                    },
                },
            }),
        )
        .unwrap();
        let StepAction::Agent(review_action) = result.action else {
            panic!("expected review step to call its agent")
        };
        assert!(
            review_action
                .prompt
                .contains("Plan doc: docs/plans/example.md")
        );
        assert!(
            review_action
                .prompt
                .contains("Verify every checked TODO item is actually completed")
        );
        assert!(review_action.prompt.contains("unfinished work items"));
        let fields = &review_action.output.as_ref().unwrap().fields;
        assert_eq!(fields["plan_doc"], "string");

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "revise",
            serde_json::json!({
                "request": "add a status command",
                "prev": {
                    "step": "review",
                    "status": "changes_requested",
                    "fields": { "feedback": "Fix one TODO", "plan_doc": plan_doc },
                },
            }),
        )
        .unwrap();
        let StepAction::Agent(revise_action) = result.action else {
            panic!("expected revise step to call its agent")
        };
        assert!(
            revise_action
                .prompt
                .contains("Plan doc: docs/plans/example.md")
        );
        let fields = &revise_action.output.as_ref().unwrap().fields;
        assert_eq!(fields["plan_doc"], "string");

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "confirm_result_answer",
            serde_json::json!({
                "steps_executed": 9,
                "prev": {
                    "step": "confirm_result",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": { "answer": "fix one more thing", "plan_doc": plan_doc },
                },
            }),
        )
        .unwrap();
        let StepAction::Status(confirm_result_action) = result.action else {
            panic!("expected confirm_result to preserve plan_doc with feedback")
        };
        assert_eq!(confirm_result_action.status, "changes_requested");
        assert_eq!(
            confirm_result_action.fields["feedback"],
            "fix one more thing"
        );
        assert_eq!(confirm_result_action.fields["plan_doc"], plan_doc);
    }

    #[tokio::test]
    async fn workflow_runtime_plan_reviewer_receives_persisted_user_feedback() {
        let responses = vec![
            r#"---
status: ready
summary: Initial plan
user_feedback: []
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
files: [docs/plans/example.md]
---
Initial plan body"#
                .to_string(),
            r#"---
status: approved
plan: Initial reviewed plan
user_feedback: []
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
---
Initial review body"#
                .to_string(),
            r#"---
status: ready
summary: Revised plan keeps the command syntax
user_feedback:
  - "Plan confirmation: Keep the existing command syntax"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
files: [docs/plans/example.md]
---
Revised plan body"#
                .to_string(),
            r#"---
status: approved
plan: Revised reviewed plan
user_feedback:
  - "Plan confirmation: Keep the existing command syntax"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
---
Revised review body"#
                .to_string(),
        ];
        let dir = tempfile::tempdir().unwrap();
        let factory = ScriptedAgentFactory::new(responses);
        let runtime = runtime_for_example_workflow(&dir, factory.clone());

        let start = runtime
            .start_run_with_workflow("workflows/feature", "preserve feedback for plan reviewers")
            .await
            .unwrap();
        let run_id = start.run.id.clone();
        let prompt_id = waiting_prompt_id(&start).to_string();
        let revised = runtime
            .answer_run(&run_id, &prompt_id, "Keep the existing command syntax")
            .await
            .unwrap();

        assert!(matches!(
            revised.run.status,
            RunStatus::WaitingForInput { .. }
        ));
        let persisted_review = command_output_record(&runtime, &revised);
        assert_eq!(persisted_review.step, "review_plan");
        assert_eq!(
            persisted_review.output.unwrap().fields["user_feedback"],
            serde_json::json!(["Plan confirmation: Keep the existing command syntax"])
        );

        let prompts = factory.prompts();
        assert_eq!(prompts.len(), 4);
        let revised_review_prompt = &prompts[3];
        assert!(
            revised_review_prompt.contains("- Plan confirmation: Keep the existing command syntax")
        );
        assert!(revised_review_prompt.contains("Plan doc: docs/plans/example.md"));
        assert!(
            revised_review_prompt
                .contains("Evaluate the revised work against the complete user feedback history")
        );
        factory.assert_exhausted();
    }

    #[tokio::test]
    async fn workflow_runtime_preserves_result_feedback_through_commit_recovery() {
        let result_feedback = "The TUI help still omits the flag";
        let responses = vec![
            r#"---
status: ready
summary: Initial plan
user_feedback: []
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
files: [docs/plans/example.md]
---
Initial plan body"#
                .to_string(),
            r#"---
status: approved
plan: Initial reviewed plan
user_feedback: []
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
---
Initial review body"#
                .to_string(),
            r#"---
status: implemented
summary: Initial implementation
user_feedback: []
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
files: [src/main.rs]
---
Initial implementation body"#
                .to_string(),
            r#"---
status: passed
summary: Initial tests passed
user_feedback: []
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
commands: [cargo test -p cowboy-workflow-engine]
failures: []
---
Initial test body"#
                .to_string(),
            r#"---
status: approved
user_feedback: []
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
---
Initial implementation review"#
                .to_string(),
            r#"---
status: changes_requested
feedback: Update the TUI help
user_feedback:
  - "Result confirmation: The TUI help still omits the flag"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
---
Result feedback review"#
                .to_string(),
            r#"---
status: implemented
summary: Updated the TUI help
user_feedback:
  - "Result confirmation: The TUI help still omits the flag"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
files: [src/main.rs]
---
Revised implementation"#
                .to_string(),
            r#"---
status: passed
summary: Focused TUI tests passed
user_feedback:
  - "Result confirmation: The TUI help still omits the flag"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
commands: [cargo test -p cowboy]
failures: []
---
Revised tests"#
                .to_string(),
            r#"---
status: approved
user_feedback:
  - "Result confirmation: The TUI help still omits the flag"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
work_dir: docs/plans/example
rca_doc: docs/plans/example/rca.md
repro_test: crates/workflow/engine/src/runtime.rs::workflow_runtime_preserves_result_feedback_through_commit_recovery
---
Revised implementation review"#
                .to_string(),
            r#"---
status: blocked
summary: Commit backend unavailable
user_feedback:
  - "Result confirmation: The TUI help still omits the flag"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
work_dir: docs/plans/example
rca_doc: docs/plans/example/rca.md
repro_test: crates/workflow/engine/src/runtime.rs::workflow_runtime_preserves_result_feedback_through_commit_recovery
---
Commit blocked"#
                .to_string(),
            r#"---
status: implemented
summary: Retried implementation after commit recovery
user_feedback:
  - "Result confirmation: The TUI help still omits the flag"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
work_dir: docs/plans/example
rca_doc: docs/plans/example/rca.md
repro_test: crates/workflow/engine/src/runtime.rs::workflow_runtime_preserves_result_feedback_through_commit_recovery
files: [src/main.rs]
---
Recovered implementation"#
                .to_string(),
            r#"---
status: passed
summary: Recovery tests passed
user_feedback:
  - "Result confirmation: The TUI help still omits the flag"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
work_dir: docs/plans/example
rca_doc: docs/plans/example/rca.md
repro_test: crates/workflow/engine/src/runtime.rs::workflow_runtime_preserves_result_feedback_through_commit_recovery
commands: [cargo test -p cowboy]
failures: []
---
Recovery tests"#
                .to_string(),
            r#"---
status: approved
user_feedback:
  - "Result confirmation: The TUI help still omits the flag"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
work_dir: docs/plans/example
rca_doc: docs/plans/example/rca.md
repro_test: crates/workflow/engine/src/runtime.rs::workflow_runtime_preserves_result_feedback_through_commit_recovery
---
Recovery implementation review"#
                .to_string(),
        ];
        let dir = tempfile::tempdir().unwrap();
        let factory = ScriptedAgentFactory::new(responses);
        let runtime = runtime_for_example_workflow(&dir, factory.clone());

        let mut report = runtime
            .start_run_with_workflow(
                "workflows/feature",
                "preserve feedback for implementation reviewers",
            )
            .await
            .unwrap();
        let run_id = report.run.id.clone();
        let prompt_id = waiting_prompt_id(&report).to_string();
        report = runtime
            .answer_run(&run_id, &prompt_id, "yes")
            .await
            .unwrap();

        let prompt_id = waiting_prompt_id(&report).to_string();
        report = runtime
            .answer_run(&run_id, &prompt_id, result_feedback)
            .await
            .unwrap();

        let persisted_review = command_output_record(&runtime, &report);
        assert_eq!(persisted_review.step, "review");
        let pre_recovery_prompt = persisted_review.input.prompt.as_deref().unwrap();
        assert!(pre_recovery_prompt.contains("Goal: Preserve reviewer feedback context"));
        assert!(pre_recovery_prompt.contains("Validation: cargo test -p cowboy-workflow-engine"));
        let review_output = persisted_review.output.unwrap();
        assert_eq!(
            review_output.fields["user_feedback"],
            serde_json::json!([format!("Result confirmation: {result_feedback}")])
        );
        assert_eq!(
            review_output.fields["goal"],
            "Preserve reviewer feedback context"
        );
        assert_eq!(
            review_output.fields["validation"],
            "cargo test -p cowboy-workflow-engine"
        );

        let prompt_id = waiting_prompt_id(&report).to_string();
        report = runtime
            .answer_run(&run_id, &prompt_id, "yes")
            .await
            .unwrap();
        assert!(matches!(
            report.run.status,
            RunStatus::WaitingForInput { ref step, .. } if step == "blocked"
        ));
        let persisted_commit = command_output_record(&runtime, &report);
        assert_eq!(persisted_commit.step, "commit");
        let commit_output = persisted_commit.output.unwrap();
        assert_eq!(commit_output.fields["work_dir"], "docs/plans/example");
        assert_eq!(commit_output.fields["plan_doc"], "docs/plans/example.md");
        assert_eq!(commit_output.fields["rca_doc"], "docs/plans/example/rca.md");
        assert_eq!(
            commit_output.fields["repro_test"],
            "crates/workflow/engine/src/runtime.rs::workflow_runtime_preserves_result_feedback_through_commit_recovery"
        );

        let prompts_before_recovery = factory.prompts();
        let commit_prompt = &prompts_before_recovery[9];
        assert!(commit_prompt.contains(&format!("- Result confirmation: {result_feedback}")));
        assert!(
            commit_prompt
                .contains("Preserve `user_feedback` exactly in output fields when present.")
        );
        for label in ["Work dir", "Plan doc", "RCA doc", "Repro test"] {
            assert!(
                commit_prompt.contains(&format!("{label}:")),
                "commit prompt should contain {label}"
            );
        }

        let prompt_id = waiting_prompt_id(&report).to_string();
        report = runtime
            .answer_run(
                &run_id,
                &prompt_id,
                "Credentials restored; continue implementation",
            )
            .await
            .unwrap();

        let persisted_recovery_review = command_output_record(&runtime, &report);
        assert_eq!(persisted_recovery_review.step, "review");
        let post_recovery_prompt = persisted_recovery_review.input.prompt.as_deref().unwrap();
        assert!(post_recovery_prompt.contains("Goal: Preserve reviewer feedback context"));
        assert!(post_recovery_prompt.contains("Validation: cargo test -p cowboy-workflow-engine"));
        let recovery_output = persisted_recovery_review.output.unwrap();
        assert_eq!(
            recovery_output.fields["user_feedback"],
            serde_json::json!([format!("Result confirmation: {result_feedback}")])
        );
        assert_eq!(
            recovery_output.fields["goal"],
            "Preserve reviewer feedback context"
        );
        assert_eq!(
            recovery_output.fields["validation"],
            "cargo test -p cowboy-workflow-engine"
        );
        assert_eq!(recovery_output.fields["work_dir"], "docs/plans/example");
        assert_eq!(recovery_output.fields["plan_doc"], "docs/plans/example.md");
        assert_eq!(
            recovery_output.fields["rca_doc"],
            "docs/plans/example/rca.md"
        );
        assert_eq!(
            recovery_output.fields["repro_test"],
            "crates/workflow/engine/src/runtime.rs::workflow_runtime_preserves_result_feedback_through_commit_recovery"
        );

        let prompts = factory.prompts();
        assert_eq!(prompts.len(), 13);
        let result_review_prompt = &prompts[8];
        assert!(
            result_review_prompt.contains(&format!("- Result confirmation: {result_feedback}"))
        );
        assert!(result_review_prompt.contains("Commands:\n- cargo test -p cowboy"));
        assert!(result_review_prompt.contains("Plan doc: docs/plans/example.md"));
        assert!(result_review_prompt.contains("Goal: Preserve reviewer feedback context"));
        assert!(result_review_prompt.contains("Validation: cargo test -p cowboy-workflow-engine"));

        let recovery_review_prompt = &prompts[12];
        assert!(
            recovery_review_prompt.contains(&format!("- Result confirmation: {result_feedback}"))
        );
        assert!(recovery_review_prompt.contains("Commands:\n- cargo test -p cowboy"));
        assert!(recovery_review_prompt.contains("Goal: Preserve reviewer feedback context"));
        assert!(
            recovery_review_prompt.contains("Validation: cargo test -p cowboy-workflow-engine")
        );
        assert!(recovery_review_prompt.contains("Work dir: docs/plans/example"));
        assert!(recovery_review_prompt.contains("Plan doc: docs/plans/example.md"));
        assert!(recovery_review_prompt.contains("RCA doc: docs/plans/example/rca.md"));
        assert!(recovery_review_prompt.contains(
            "Repro test: crates/workflow/engine/src/runtime.rs::workflow_runtime_preserves_result_feedback_through_commit_recovery"
        ));
        factory.assert_exhausted();
    }

    #[test]
    fn example_blocked_step_requests_user_direction() {
        let examples_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("examples/workflows")
            .canonicalize()
            .unwrap();
        let catalog = WorkflowCatalogLoader::new()
            .without_builtin()
            .with_project_dir(&examples_root)
            .load_catalog()
            .unwrap();
        let source_ref = catalog.workflows.get("workflows/feature").unwrap();
        let compiled = cowboy_workflow_lua::load(source_ref).unwrap();
        assert_eq!(
            compiled.definition.steps["blocked"].transitions.by_status["answered"],
            "blocked_answer"
        );
        assert_eq!(
            compiled.definition.steps["blocked_answer"]
                .transitions
                .by_status["triaged"],
            "triage_blocked"
        );
        assert_eq!(
            compiled.definition.steps["triage_blocked"]
                .transitions
                .by_status["plan"],
            "plan"
        );
        assert_eq!(
            compiled.definition.steps["triage_blocked"]
                .transitions
                .by_status["implement"],
            "implement"
        );
        assert_eq!(
            compiled.definition.steps["triage_blocked"]
                .transitions
                .by_status["revise"],
            "revise"
        );

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "blocked",
            serde_json::json!({
                "steps_executed": 10,
                "resume": {},
                "prev": {
                    "step": "implement",
                    "status": "blocked",
                    "fields": { "summary": "Need credentials" },
                    "body": "Cannot continue without access",
                },
            }),
        )
        .unwrap();
        let StepAction::AskUser(action) = result.action else {
            panic!("expected blocked step to ask the user")
        };
        assert_eq!(action.id, "blocked_10");
        assert!(action.message.contains("What should Cowboy do next?"));
        assert!(action.message.contains("feature workflow blocked"));
        assert!(action.choices.is_empty());

        let blocked_response = "Credentials are available now; continue implementation.";
        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "blocked_answer",
            serde_json::json!({
                "steps_executed": 11,
                "prev": {
                    "step": "blocked",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": {
                        "answer": blocked_response,
                        "summary": "Need credentials",
                        "plan_doc": "docs/plans/example.md",
                        "blocked_from_step": "implement",
                        "blocked_from_status": "blocked"
                    },
                    "body": blocked_response,
                },
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("expected blocked answer to be recorded")
        };
        assert_eq!(action.status, "triaged");
        assert_eq!(action.fields["summary"], "Need credentials");
        assert_eq!(action.fields["plan_doc"], "docs/plans/example.md");
        assert_eq!(action.fields["blocked_response"], blocked_response);
        assert_eq!(action.fields["blocked_from_step"], "implement");
        assert_eq!(action.fields["blocked_from_status"], "blocked");
        assert!(action.body.contains("User response:"));

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "triage_blocked",
            serde_json::json!({
                "prev": {
                    "step": "blocked",
                    "status": "triaged",
                    "fields": {
                        "summary": "Need credentials",
                        "plan_doc": "docs/plans/example.md",
                        "blocked_response": blocked_response,
                        "blocked_from_step": "implement",
                        "blocked_from_status": "blocked"
                    },
                    "body": "Workflow was blocked. User response:\nCredentials are available now; continue implementation.",
                },
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("expected triage to route back to implementation")
        };
        assert_eq!(action.status, "implement");
        assert_eq!(action.fields["feedback"], blocked_response);
        assert_eq!(action.fields["plan_doc"], "docs/plans/example.md");
        assert_eq!(action.fields["blocked_from_step"], "implement");

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "implement",
            serde_json::json!({
                "request": "add a status command",
                "prev": {
                    "step": "triage_blocked",
                    "status": "implement",
                    "fields": {
                        "summary": "Blocked workflow triaged to implement",
                        "feedback": blocked_response,
                        "plan_doc": "docs/plans/example.md",
                        "files": [],
                        "blocked_from_step": "implement",
                        "blocked_from_status": "blocked"
                    },
                    "body": "Blocked workflow user response:\nCredentials are available now; continue implementation.",
                },
            }),
        )
        .unwrap();
        let StepAction::Agent(action) = result.action else {
            panic!("expected implement step to receive triage context")
        };
        assert!(action.prompt.contains(blocked_response));

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "triage_blocked",
            serde_json::json!({
                "prev": {
                    "step": "blocked",
                    "status": "answered",
                    "fields": {
                        "blocked_response": "Change the plan to reduce scope first.",
                        "blocked_from_step": "implement"
                    },
                },
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("expected triage to route to planning")
        };
        assert_eq!(action.status, "plan");

        let result = cowboy_workflow_lua::run_step(
            &compiled.source_bundle,
            "triage_blocked",
            serde_json::json!({
                "prev": {
                    "step": "blocked",
                    "status": "answered",
                    "fields": {
                        "blocked_response": "The dependency is fixed; continue.",
                        "blocked_from_step": "revise"
                    },
                },
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("expected triage to route back to revision")
        };
        assert_eq!(action.status, "revise");
    }

    #[test]
    fn catalog_loads_filesystem_workflow_description() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("review.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("review", start, { description = "reviews code" })
            "#,
        )
        .unwrap();
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![AgentRuntimeConfig {
                name: "default".to_string(),
                command: "unused-agent".to_string(),
                args: Vec::new(),
                model: ModelInfo::default(),
            }],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        });

        let catalog = runtime.catalog().unwrap();
        assert_eq!(
            catalog.workflows["review"].description.as_deref(),
            Some("reviews code")
        );
        // The built-in workflow keeps its own hard-coded description.
        assert!(catalog.workflows["default"].description.is_some());
    }

    #[test]
    fn snapshot_from_run_uses_workflow_entry_not_first_import() {
        let now = Utc::now();
        let run = WorkflowRun {
            id: "run-1".to_string(),
            workflow_name: "workflows/feature".to_string(),
            workflow_api_version: 1,
            workflow_hash: "hash".to_string(),
            workflow_sources: BTreeMap::from([
                (
                    "roles/planner.lua".to_string(),
                    r#"return role("planner", "Plan work")"#.to_string(),
                ),
                (
                    "workflows/feature.lua".to_string(),
                    r#"
                local planner = require("roles/planner.lua")
                local start = step("start", { role = planner })
                start.run = function(ctx) return action.status { status = "success" } end
                return workflow("feature", start)
                "#
                    .to_string(),
                ),
            ]),
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
        };

        let snapshot = snapshot_from_run(&run);
        assert_eq!(snapshot.entry, "workflows/feature.lua");
        cowboy_workflow_lua::compile_snapshot(&snapshot).unwrap();
    }

    #[test]
    fn lists_no_runs_for_fresh_store() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            agents: vec![AgentRuntimeConfig {
                name: "default".to_string(),
                command: "agent".to_string(),
                args: Vec::new(),
                model: ModelInfo::default(),
            }],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            )]),
        });
        assert!(runtime.list_runs().unwrap().is_empty());
    }

    fn progress(kind: AgentProgressKind) -> AgentProgress {
        AgentProgress {
            run_id: "run-1".to_string(),
            step_id: "implement".to_string(),
            kind,
        }
    }

    #[test]
    fn maps_every_agent_progress_kind_to_typed_workflow_event() {
        let cases = vec![
            (
                progress(AgentProgressKind::SessionReady {
                    role: "developer".to_string(),
                    session_id: "session-1".to_string(),
                }),
                WorkflowEventKind::AgentSessionReady {
                    step_id: "implement".to_string(),
                    role: "developer".to_string(),
                    session_id: "session-1".to_string(),
                },
            ),
            (
                progress(AgentProgressKind::Prompt {
                    role: "developer".to_string(),
                    session_id: "session-1".to_string(),
                    prompt: "Do work".to_string(),
                }),
                WorkflowEventKind::AgentPrompt {
                    step_id: "implement".to_string(),
                    role: "developer".to_string(),
                    session_id: "session-1".to_string(),
                    prompt: "Do work".to_string(),
                },
            ),
            (
                progress(AgentProgressKind::Response {
                    content: "answer".to_string(),
                }),
                WorkflowEventKind::AgentResponse {
                    step_id: "implement".to_string(),
                    content: "answer".to_string(),
                },
            ),
            (
                progress(AgentProgressKind::Thought {
                    content: "thinking".to_string(),
                }),
                WorkflowEventKind::AgentThought {
                    step_id: "implement".to_string(),
                    content: "thinking".to_string(),
                },
            ),
            (
                progress(AgentProgressKind::ToolCall {
                    tool_call_id: "call_1".to_string(),
                    title: "Read file".to_string(),
                    tool_kind: "read".to_string(),
                    status: "pending".to_string(),
                }),
                WorkflowEventKind::AgentToolCall {
                    step_id: "implement".to_string(),
                    tool_call_id: "call_1".to_string(),
                    title: "Read file".to_string(),
                    tool_kind: "read".to_string(),
                    status: "pending".to_string(),
                },
            ),
            (
                progress(AgentProgressKind::ToolCallUpdate {
                    tool_call_id: "call_1".to_string(),
                    title: "Read file".to_string(),
                    status: "completed".to_string(),
                    content: Some(serde_json::json!({"text":"done"})),
                }),
                WorkflowEventKind::AgentToolCallUpdate {
                    step_id: "implement".to_string(),
                    tool_call_id: "call_1".to_string(),
                    title: "Read file".to_string(),
                    status: "completed".to_string(),
                    content: Some(serde_json::json!({"text":"done"})),
                },
            ),
            (
                progress(AgentProgressKind::Plan {
                    entries: vec![serde_json::json!({"content":"first"})],
                }),
                WorkflowEventKind::AgentPlan {
                    step_id: "implement".to_string(),
                    entries: vec![serde_json::json!({"content":"first"})],
                },
            ),
        ];

        for (progress, expected) in cases {
            let mapped = WorkflowRuntime::workflow_event_kind_from_agent_progress(progress);
            assert_eq!(mapped, expected);
            assert!(
                !matches!(mapped, WorkflowEventKind::StepProgress { .. }),
                "typed agent progress must not map to generic step progress"
            );
        }
    }

    #[test]
    fn agent_progress_workflow_events_use_run_creation_timestamp_and_active_duration() {
        let run_started_at = Utc::now() - chrono::Duration::hours(1);
        let mut run = summary_test_run("run-1", RunStatus::Running, None);
        run.created_at = run_started_at;
        run.active_duration_ms = 6_000;
        let active_clock = ActiveRunClock::open_at(&run, Utc::now());
        let event = WorkflowRuntime::workflow_event_from_agent_progress(
            progress(AgentProgressKind::Response {
                content: "answer".to_string(),
            }),
            &active_clock,
        );

        assert_eq!(event.run_id, "run-1");
        assert_eq!(event.run_started_at, Some(run_started_at));
        let active_ms = event
            .run_active_duration_ms
            .expect("agent progress event active duration");
        assert!(active_ms >= 6_000, "{event:#?}");
        assert!(active_ms < 7_000, "{event:#?}");
        assert!(matches!(
            &event.kind,
            WorkflowEventKind::AgentResponse { content, .. } if content == "answer"
        ));
    }
}
