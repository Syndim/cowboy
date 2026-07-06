use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cowboy_agent_acp::Client as AcpClient;
use cowboy_agent_acp::transport::{StdioConfig, TransportConfig};
use cowboy_agent_client::ModelInfo;
use cowboy_workflow_agent::{
    AgentExecutionConfig, AgentExecutor, AgentProgress, AgentProgressKind, ClientFactory,
    ResolvedAgentClient,
};
use cowboy_workflow_catalog::{
    AppliedWorkflowImprovement, WorkflowCatalogLoader, apply_improvement, load_source_ref,
};
use cowboy_workflow_core::{
    ActionResult, ExecutionContext, ObjectKind, Result, RoleDefinition, RunHead, RunStatus,
    RunnerLimits, StatusAction, StepAction, StepActionProvider, StepRecord, WorkflowCatalog,
    WorkflowError, WorkflowRun, WorkflowSelector, WorkflowSourceRef, WorkflowSourceSnapshot,
    WorkflowSummarizer, apply_run_status, apply_step_record,
};
use cowboy_workflow_store::RedbRunStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::agent_resolver::AgentResolver;
use crate::run_lock::RunExecutionLocks;
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
    pub limits: RunnerLimitsConfig,
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
    #[serde(default = "default_max_retries_per_step")]
    pub max_retries_per_step: u32,
}

fn default_max_retries_per_step() -> u32 {
    2
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
        limits: RunnerLimitsConfig,
    ) -> Self {
        Self {
            cwd,
            state_dir,
            workflow_store,
            workflow_dirs,
            agents,
            limits,
        }
    }
}

impl From<RunnerLimitsConfig> for RunnerLimits {
    fn from(value: RunnerLimitsConfig) -> Self {
        Self {
            max_steps_per_run: value.max_steps_per_run,
            max_visits_per_step: value.max_visits_per_step,
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
    pub status: RunStatus,
    pub current_step: String,
    pub head_step: Option<String>,
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
}

/// How far [`WorkflowRuntime`] drives a run in a single call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Execute steps until the run blocks, fails, or completes.
    UntilBlocked,
    /// Execute exactly one workflow step, then return.
    SingleStep,
}

/// Workflow selection strategy used by [`WorkflowRuntime`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectorMode {
    /// Ask the configured ACP agent to choose a workflow from the catalog.
    Agent,
    /// Pick the first catalog workflow by id; used by tests with no live agent.
    Deterministic,
}

impl WorkflowRuntime {
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            run_locks: RunExecutionLocks::new(config.workflow_store.clone()),
            config,
            events: Arc::new(EventBus::default()),
            selector: SelectorMode::Agent,
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
                runs.push(RunSummaryLine {
                    run_id: run.id,
                    workflow_name: run.workflow_name,
                    status: head.status,
                    current_step: run.current_step,
                    head_step: head.head_step,
                });
            }
        }
        Ok(runs)
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
        tracing::debug!(
            workflow_id = %workflow_id,
            source_entry = %source_ref.entry,
            source_root = ?source_ref.root,
            workflow_hash = %workflow_hash,
            "workflow source compiled"
        );
        let now = Utc::now();
        let run = WorkflowRun {
            id: run_id,
            workflow_name: definition.name.clone(),
            workflow_api_version: 1,
            workflow_hash,
            workflow_sources: snapshot.files.clone(),
            original_request: request,
            status: RunStatus::Running,
            current_step: definition.head.clone(),
            head: None,
            resume: Value::Null,
            steps_executed: 0,
            step_visits: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        };
        let store = self.store()?;
        store.save_run(&run)?;
        store.update_run_head(&run.id, run_head(&run))?;
        tracing::info!(run_id = %run.id, workflow = %run.workflow_name, "created workflow run");
        self.run_existing(run, definition, snapshot, mode).await
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
        let snapshot = snapshot_from_run(&run);
        let mut definition = cowboy_workflow_lua::compile_snapshot(&snapshot)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        definition.name = run.workflow_name.clone();
        definition.source_hash = run.workflow_hash.clone();
        self.run_existing(run, definition, snapshot, mode).await
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
        let snapshot = snapshot_from_run(&run);
        let mut definition = cowboy_workflow_lua::compile_snapshot(&snapshot)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        definition.name = run.workflow_name.clone();
        definition.source_hash = run.workflow_hash.clone();

        let result = ResumeRouter::default().answer(&run, prompt_id, answer)?;
        let mut events = Vec::new();
        let status = match result {
            ActionResult::Completed(record) => {
                let record = *record;
                let status = apply_step_record(&store, &definition, &mut run, record.clone())?;
                events.push(WorkflowEvent::step_completed_for_run(&run, &record));
                events.push(WorkflowEvent::run_status_for_run(&run, &status));
                status
            }
            ActionResult::Blocked(status) => {
                let status = apply_run_status(&store, &mut run, status)?;
                events.push(WorkflowEvent::run_status_for_run(&run, &status));
                status
            }
        };
        for event in &events {
            self.events.emit(event.clone());
        }
        tracing::debug!(run_id = %run.id, prompt_id, status = ?run.status, "workflow prompt answer completed");

        if matches!(status, RunStatus::Running) {
            self.run_existing_with_events(run, definition, snapshot, RunMode::UntilBlocked, events)
                .await
        } else {
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
            return Err(WorkflowError::InvalidAction(format!(
                "status {status:?} requires field(s): {}. Provide them via --fields '<json object>'.",
                missing.join(", ")
            )));
        }

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
        events.push(WorkflowEvent::step_completed_for_run(&run, &record));
        events.push(WorkflowEvent::for_run(
            &run,
            WorkflowEventKind::ManuallyResolved {
                step_id: record.step.clone(),
                status: status.to_string(),
            },
        ));
        events.push(WorkflowEvent::run_status_for_run(&run, &status_result));
        for event in &events {
            self.events.emit(event.clone());
        }

        if matches!(status_result, RunStatus::Running) {
            self.run_existing_with_events(run, definition, snapshot, RunMode::UntilBlocked, events)
                .await
        } else {
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
    ) -> Result<RunReport> {
        self.run_existing_with_events(run, definition, snapshot, mode, Vec::new())
            .await
    }

    async fn run_existing_with_events(
        &self,
        run: WorkflowRun,
        definition: cowboy_workflow_core::WorkflowDefinition,
        snapshot: WorkflowSourceSnapshot,
        mode: RunMode,
        mut events: Vec<WorkflowEvent>,
    ) -> Result<RunReport> {
        tracing::debug!(
            run_id = %run.id,
            workflow = %definition.name,
            mode = ?mode,
            current_step = %run.current_step,
            steps_executed = run.steps_executed,
            "running workflow"
        );
        let run_id = run.id.clone();
        let run_started_at = run.created_at;
        let mut rx = self.events.subscribe();
        let store = self.store()?;
        let agent_store = store.clone();
        let progress_events = self.events.clone();
        let agent_config = AgentExecutionConfig {
            cwd: self.config.cwd.to_string_lossy().to_string(),
            mcp_servers: Vec::new(),
            progress: Some(Arc::new(move |progress| {
                progress_events.emit(Self::workflow_event_from_agent_progress(
                    progress,
                    run_started_at,
                ));
            })),
        };
        let executor = AgentExecutor::new(self.agent_factory()?, agent_store, agent_config);
        let dispatcher = EngineActionDispatcher::new(executor);
        let provider = LuaStepActionProvider::new(snapshot);
        let runner = WorkflowRunner::new(store, dispatcher, provider, self.events.clone())
            .with_limits(self.config.limits.into());
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
        let run = loop {
            tokio::select! {
                result = &mut run_future => break result?,
                received = rx.recv() => match received {
                    Ok(event) => events.push(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(run_id = %run_id, skipped, "workflow event collector lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!(run_id = %run_id, "workflow event collector closed");
                        break (&mut run_future).await?;
                    }
                }
            }
        };
        drain_available_workflow_events(&mut rx, &mut events);
        tracing::debug!(run_id = %run.id, event_count = events.len(), prefix_events = prefix_len, "workflow events collected");
        self.persist_events(&run.id, &events)?;
        Ok(RunReport { run, events })
    }

    fn workflow_event_from_agent_progress(
        progress: AgentProgress,
        run_started_at: DateTime<Utc>,
    ) -> WorkflowEvent {
        let run_id = progress.run_id.clone();
        let kind = Self::workflow_event_kind_from_agent_progress(progress);
        WorkflowEvent::with_run_started_at(run_id, run_started_at, kind)
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

    fn agent_factory(&self) -> Result<AcpClientFactory> {
        tracing::debug!(
            agents = self.config.agents.len(),
            "ACP client factory configured"
        );
        Ok(AcpClientFactory {
            resolver: AgentResolver::new(self.config.agents.clone())?,
        })
    }

    fn store(&self) -> Result<RedbRunStore> {
        tracing::debug!(path = %self.config.workflow_store.display(), "opening workflow store");
        let store = RedbRunStore::create(&self.config.workflow_store)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
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

#[derive(Debug, Clone)]
pub struct AcpClientFactory {
    resolver: AgentResolver,
}

fn transport_for(agent: &AgentRuntimeConfig) -> TransportConfig {
    TransportConfig::Stdio(StdioConfig {
        command: agent.command.clone(),
        args: agent.args.clone(),
        env: Vec::new(),
    })
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

/// Whether `field` is present and non-null in a supplied `--fields` value.
fn field_present(fields: &Value, field: &str) -> bool {
    fields
        .as_object()
        .and_then(|map| map.get(field))
        .map(|value| !value.is_null())
        .unwrap_or(false)
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

#[async_trait]
impl ClientFactory for AcpClientFactory {
    async fn create_client(
        &self,
        role: &RoleDefinition,
    ) -> cowboy_workflow_agent::Result<ResolvedAgentClient> {
        let agent = self.resolver.resolve(role)?;
        tracing::debug!(
            role = %role.id,
            agent = %agent.name,
            command = %agent.command,
            args = ?agent.args,
            model_id = %agent.model.id,
            provider = ?agent.model.provider,
            "resolving ACP client for role"
        );
        let client = AcpClient::connect(transport_for(agent)).await?;
        Ok(ResolvedAgentClient {
            client: Box::new(client),
            model: agent.model.clone(),
            backend: agent.name.clone(),
        })
    }
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
    use cowboy_workflow_core::{RunStatus, StepAction};
    fn agent(name: &str, command: &str) -> AgentRuntimeConfig {
        AgentRuntimeConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            model: ModelInfo::default(),
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
        })
        .with_deterministic_selector()
    }

    fn runtime_for_workflow_dir(dir: &tempfile::TempDir, workflow_dir: PathBuf) -> WorkflowRuntime {
        WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "unused-agent")],
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
        })
        .with_deterministic_selector()
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
        })
        .with_deterministic_selector();

        let err = runtime.start_run("do it").await.unwrap_err();
        // A missing agent command surfaces as a transient (recoverable) client
        // error; the runner retries it and gives up with a recoverable error.
        assert!(matches!(err, WorkflowError::RecoverableAction(_)));
        assert_eq!(runtime.list_runs().unwrap().len(), 1);
        // The give-up path persists a clean Failed status for later resolution.
        let run = runtime
            .load_run(&runtime.list_runs().unwrap()[0].run_id)
            .unwrap();
        assert!(matches!(run.status, RunStatus::Failed { .. }));
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
        assert!(err.to_string().contains("summary"), "{err}");

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
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "definitely-missing-agent")],
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
        })
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
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
            event.kind,
            WorkflowEventKind::StepStarted { ref step_id } if step_id == "finish"
        )));
        assert!(report.events.iter().any(|event| matches!(
            event.kind,
            WorkflowEventKind::StepCompleted { ref step_id, ref action, ref status, .. }
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
            event.kind,
            WorkflowEventKind::StepStarted { ref step_id } if step_id == "finish"
        )));
        assert!(persisted.iter().any(|event| matches!(
            event.kind,
            WorkflowEventKind::StepCompleted { ref step_id, ref action, ref status, .. }
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
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
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: vec![agent("default", "unused-agent")],
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
        })
        .with_deterministic_selector();

        let start = runtime.start_run("request").await.unwrap();
        assert!(matches!(
            start.run.status,
            RunStatus::WaitingForInput { .. }
        ));
        let steps_before_answer = start.run.steps_executed;
        let report = runtime
            .answer_run(&start.run.id, "approval", "yes")
            .await
            .unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        assert_eq!(report.run.steps_executed, steps_before_answer + 2);
        assert!(matches!(
            report.events[0].kind,
            WorkflowEventKind::StepCompleted { ref step_id, ref action, ref status, .. }
                if step_id == "ask" && action == "ask_user" && status.as_deref() == Some("answered")
        ));
        assert!(matches!(
            report.events[1].kind,
            WorkflowEventKind::RunStatusChanged { ref status } if status == "running"
        ));
        assert!(report.events.iter().skip(2).any(|event| matches!(
            event.kind,
            WorkflowEventKind::StepCompleted { ref step_id, .. } if step_id == "decide"
        )));
        assert!(
            report
                .events
                .iter()
                .all(|event| event.run_started_at == Some(start.run.created_at)),
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
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
            persisted[0].kind,
            WorkflowEventKind::StepCompleted { ref step_id, ref action, ref status, .. }
                if step_id == "ask" && action == "ask_user" && status.as_deref() == Some("answered")
        ));
        assert!(matches!(
            persisted[1].kind,
            WorkflowEventKind::RunStatusChanged { ref status } if status == "running"
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
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
            status: RunStatus::Running,
            current_step: "start".to_string(),
            head: None,
            resume: Value::Null,
            steps_executed: 0,
            step_visits: BTreeMap::new(),
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
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_step: 2,
            },
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
    fn agent_progress_workflow_events_use_run_creation_timestamp() {
        let run_started_at = Utc::now();
        let event = WorkflowRuntime::workflow_event_from_agent_progress(
            progress(AgentProgressKind::Response {
                content: "answer".to_string(),
            }),
            run_started_at,
        );

        assert_eq!(event.run_id, "run-1");
        assert_eq!(event.run_started_at, Some(run_started_at));
        assert!(matches!(
            event.kind,
            WorkflowEventKind::AgentResponse { ref content, .. } if content == "answer"
        ));
    }
}
