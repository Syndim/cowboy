use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use chrono::Utc;
use cowboy_agent_client::ModelInfo;
#[cfg(test)]
use cowboy_agent_client::PromptTurnCancellation;
#[cfg(feature = "test-support")]
use cowboy_workflow_agent::PromptWindowHandoffObserver;
use cowboy_workflow_agent::{
    AgentExecutionConfig, AgentExecutor, AgentProgress, AgentProgressKind,
    PromptTurnControlRegistry,
};
use cowboy_workflow_catalog::{
    AppliedWorkflowImprovement, WorkflowCatalogLoader, apply_improvement, load_source_ref,
};
use cowboy_workflow_core::{
    ActionResult, AppendUserPromptOutcome, ConfigSetRef, DEFAULT_CONFIG_SET_NAME, ExecutionContext,
    ObjectKind, Result, RunHead, RunStatus, RunUserPrompt, RunnerLimits, StatusAction, StepAction,
    StepActionProvider, StepRecord, WorkflowCatalog, WorkflowDefinition, WorkflowError,
    WorkflowRun, WorkflowSelector, WorkflowSourceRef, WorkflowSourceSnapshot, WorkflowSummarizer,
    apply_run_status, apply_step_record,
};
use cowboy_workflow_store::{RedbRunStore, StoreWaitCancellation, StoreWaitObserver};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::active_clock::ActiveRunClock;
use crate::agent_resolver::AgentResolver;
use crate::events::WORKFLOW_STORE_WAITING_MESSAGE;
use crate::run_lock::RunExecutionLocks;
use crate::runtime_dependencies::{
    AcpConnector, ProductionAcpConnector, ProductionRuntimeDependencies, RuntimeDependencies,
    transport_for, watchdog_options_for,
};
use crate::workflow::DeterministicSelector;
use crate::{
    EngineActionDispatcher, EventBus, LuaStepActionProvider, ResolvedRuntimePolicy, ResumeRouter,
    WorkflowEvent, WorkflowEventKind, WorkflowRunner,
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
    pub model: Option<ModelInfo>,
    #[serde(default)]
    pub watchdog: AgentWatchdogRuntimeConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentWatchdogRuntimeConfig {
    pub response_timeout_seconds: u64,
    pub cancel_timeout_seconds: u64,
    pub recovery_operation_timeout_seconds: u64,
}

impl Default for AgentWatchdogRuntimeConfig {
    fn default() -> Self {
        Self {
            response_timeout_seconds: 100,
            cancel_timeout_seconds: 10,
            recovery_operation_timeout_seconds: 30,
        }
    }
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
        model: Option<ModelInfo>,
    ) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args,
            model,
            watchdog: AgentWatchdogRuntimeConfig::default(),
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

/// Durable result of attempting to send an on-the-fly prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserPromptSubmission {
    Accepted(RunUserPrompt),
    Rejected(UserPromptRejection),
}

/// Stable rejection reason used by the TUI to retain and explain a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserPromptRejection {
    Empty,
    MissingRun,
    TerminalRun,
    NoAgentWindow,
    StaleWindow,
    SealedWindow,
}

impl UserPromptRejection {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "prompt is empty",
            Self::MissingRun => "workflow run no longer exists",
            Self::TerminalRun => "workflow run is no longer running",
            Self::NoAgentWindow => "no agent is currently accepting prompts",
            Self::StaleWindow => {
                "the agent prompt window was replaced; wait for the current window"
            }
            Self::SealedWindow => "the agent already finalized this step",
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
    store: RedbRunStore,
    store_wait_generation: Arc<AtomicU64>,
    events: Arc<EventBus>,
    run_locks: RunExecutionLocks,
    selector: SelectorMode,
    dependencies: Arc<dyn RuntimeDependencies>,
    acp_connector: Arc<dyn AcpConnector>,
    prompt_turn_controls: PromptTurnControlRegistry,
    #[cfg(feature = "test-support")]
    handoff_observer: Option<Arc<dyn PromptWindowHandoffObserver>>,
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

        if let Err(err) = self.store.clear_agent_prompt_window(&self.run_id) {
            tracing::warn!(run_id = %self.run_id, error = %err, "failed to clear agent prompt window during cancellation");
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
        let acp_connector = Arc::new(ProductionAcpConnector);
        Self::with_dependencies_and_connector(
            config,
            Arc::new(ProductionRuntimeDependencies::new(acp_connector.clone())),
            acp_connector,
        )
    }

    #[cfg(test)]
    fn with_dependencies(
        config: RuntimeConfig,
        dependencies: Arc<dyn RuntimeDependencies>,
    ) -> Self {
        Self::with_dependencies_and_connector(
            config,
            dependencies,
            Arc::new(ProductionAcpConnector),
        )
    }

    fn with_dependencies_and_connector(
        config: RuntimeConfig,
        dependencies: Arc<dyn RuntimeDependencies>,
        acp_connector: Arc<dyn AcpConnector>,
    ) -> Self {
        let store = RedbRunStore::lazy(&config.workflow_store);

        Self {
            run_locks: RunExecutionLocks::new(config.workflow_store.clone()),
            config,
            store,
            store_wait_generation: Arc::new(AtomicU64::new(0)),
            events: Arc::new(EventBus::default()),
            selector: SelectorMode::Agent,
            dependencies,
            acp_connector,
            prompt_turn_controls: PromptTurnControlRegistry::default(),
            #[cfg(feature = "test-support")]
            handoff_observer: None,
        }
    }

    /// Use the deterministic (first-by-id) selector instead of the agent-backed
    /// one. Intended for tests that have no live agent backend.
    pub fn with_deterministic_selector(mut self) -> Self {
        self.selector = SelectorMode::Deterministic;
        self
    }

    /// Interrupt database availability waits started by current runtime operations.
    pub fn cancel_store_waits(&self) {
        let generation = self.store_wait_generation.fetch_add(1, Ordering::AcqRel) + 1;
        tracing::debug!(generation, "cancelled pending workflow store waits");
    }

    #[cfg(all(test, feature = "test-support"))]
    fn with_handoff_observer(mut self, observer: Arc<dyn PromptWindowHandoffObserver>) -> Self {
        self.handoff_observer = Some(observer);
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

    pub fn list_runs(&self, partial_run_id: Option<&str>) -> Result<Vec<RunSummaryLine>> {
        let store = self.store()?;
        let mut runs = Vec::new();
        for head in store.list_runs()? {
            if partial_run_id.is_some_and(|partial| !head.run_id.contains(partial)) {
                continue;
            }

            let RunHead {
                run_id,
                workflow_hash: _,
                head_step,
                status,
                updated_at: _,
                summary,
            } = head;
            let status_detail = RunStatusDetail::from_status(&status);

            if let Some(summary) = summary {
                runs.push(RunSummaryLine {
                    run_id,
                    workflow_name: summary.workflow_name,
                    topic: summary.request_topic,
                    status,
                    status_detail,
                    current_step: summary.current_step,
                    head_step,
                });
                continue;
            }

            if let Ok(run) = store.load_run(&run_id) {
                let topic = self.summary_topic(&run);
                runs.push(RunSummaryLine {
                    run_id: run.id,
                    workflow_name: run.workflow_name,
                    topic,
                    status,
                    status_detail,
                    current_step: run.current_step,
                    head_step,
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

    pub fn submit_user_prompt(
        &self,
        run_id: &str,
        window_id: &str,
        content: String,
    ) -> Result<UserPromptSubmission> {
        if content.trim().is_empty() {
            return Ok(UserPromptSubmission::Rejected(UserPromptRejection::Empty));
        }
        let outcome = self
            .store()?
            .append_user_prompt(run_id, window_id, content)?;
        Ok(match outcome {
            AppendUserPromptOutcome::Accepted(prompt) => {
                self.prompt_turn_controls
                    .publish(run_id, window_id, prompt.sequence);
                UserPromptSubmission::Accepted(prompt)
            }
            AppendUserPromptOutcome::MissingRun => {
                UserPromptSubmission::Rejected(UserPromptRejection::MissingRun)
            }
            AppendUserPromptOutcome::TerminalRun => {
                UserPromptSubmission::Rejected(UserPromptRejection::TerminalRun)
            }
            AppendUserPromptOutcome::NoWindow => {
                UserPromptSubmission::Rejected(UserPromptRejection::NoAgentWindow)
            }
            AppendUserPromptOutcome::StaleWindow => {
                UserPromptSubmission::Rejected(UserPromptRejection::StaleWindow)
            }
            AppendUserPromptOutcome::SealedWindow => {
                UserPromptSubmission::Rejected(UserPromptRejection::SealedWindow)
            }
        })
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
        let store = self.store_for_run(&run.id)?;
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

    /// Resolve the config-set **name** a workflow selects, validating it against
    /// live config before a run is persisted. An unknown/misspelled name is a
    /// hard error at start (only the durable pointer is stored; limits are
    /// resolved live afterward).
    fn resolve_config_set(&self, definition: &WorkflowDefinition) -> Result<ConfigSetRef> {
        let name = definition
            .config_set
            .as_deref()
            .unwrap_or(DEFAULT_CONFIG_SET_NAME);
        if !self.config.config_sets.contains_key(name) {
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
        }

        Ok(ConfigSetRef {
            name: name.to_string(),
        })
    }

    /// Resolve effective runner limits for an existing run from current process
    /// configuration. Infallible by construction so it can run at any point in a
    /// lifecycle path without stranding a durable mutation: three exhaustive
    /// branches cover every case and none index `config_sets`.
    fn resolve_limits(&self, name: &str) -> ResolvedRuntimePolicy {
        if let Some(config_set) = self.config.config_sets.get(name).copied() {
            return ResolvedRuntimePolicy {
                name: name.to_string(),
                limits: config_set.into(),
            };
        }

        if let Some(config_set) = self
            .config
            .config_sets
            .get(DEFAULT_CONFIG_SET_NAME)
            .copied()
        {
            tracing::warn!(
                requested = %name,
                "config set missing from current config; falling back to default config set"
            );
            return ResolvedRuntimePolicy {
                name: DEFAULT_CONFIG_SET_NAME.to_string(),
                limits: config_set.into(),
            };
        }

        tracing::warn!(
            requested = %name,
            "config set and default config set both missing from current config; falling back to built-in default limits"
        );
        ResolvedRuntimePolicy {
            name: DEFAULT_CONFIG_SET_NAME.to_string(),
            limits: RunnerLimits::default(),
        }
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
                let client = self
                    .acp_connector
                    .connect(transport_for(agent), watchdog_options_for(agent))
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
        let store = self.store_for_run(run_id)?;
        let mut run = store.load_run(&run_id.to_string())?;
        tracing::debug!(
            run_id = %run.id,
            status = ?run.status,
            current_step = %run.current_step,
            steps_executed = run.steps_executed,
            "loaded workflow run"
        );
        match run.status {
            RunStatus::Running => {}
            RunStatus::Failed { .. } | RunStatus::WaitingForInput { .. } => {
                // Every non-terminal run retains its current step and must be
                // re-executed on resume; only Completed and Cancelled runs are
                // non-resumable no-ops. A Failed run gave up after exhausting its
                // recoverable retry budget, and a WaitingForInput run is blocked
                // on its retained ask_user step. Flip the status back to Running
                // and persist it so the retained current step is re-executed
                // through the normal execution path; the runner persists the
                // resulting terminal status. Re-executing an ask_user step mints
                // a fresh record id and overwrites the prior WaitingForInput
                // status, so the durable pending resume callback is safely
                // replaced rather than duplicated or orphaned.
                tracing::debug!(
                    run_id = %run.id,
                    status = ?run.status,
                    current_step = %run.current_step,
                    "resuming non-terminal run; re-executing the retained current step"
                );
                apply_run_status(&store, &mut run, RunStatus::Running)?;
            }
            RunStatus::Completed | RunStatus::Cancelled => {
                tracing::debug!(run_id = %run.id, status = ?run.status, "workflow run is not resumable; returning without execution");
                return Ok(RunReport {
                    run,
                    events: Vec::new(),
                });
            }
        }
        let active_clock = ActiveRunClock::open(&run);
        let snapshot = snapshot_from_run(&run);
        let mut definition = cowboy_workflow_lua::compile_snapshot(&snapshot)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        definition.name = run.workflow_name.clone();
        definition.source_hash = run.workflow_hash.clone();
        let request_topic = run.request_topic.clone();
        self.run_existing(run, definition, snapshot, mode, request_topic, active_clock)
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
        let store = self.store_for_run(run_id)?;
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
            let request_topic = run.request_topic.clone();
            self.run_existing_with_events(
                run,
                definition,
                snapshot,
                RunMode::UntilBlocked,
                ActiveRunExecution {
                    request_topic,
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
        let store = self.store_for_run(run_id)?;
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
        let user_prompts = store.load_user_prompts(&run.id)?;
        let provider = LuaStepActionProvider::new(snapshot);
        let action = provider
            .step_action(&definition, run, &step, prev_record.as_ref(), &user_prompts)
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
        let store = self.store_for_run(run_id)?;
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
            original_request: run.original_request.clone(),
            run_created_at: run.created_at,
            user_prompts: store.load_user_prompts(&run.id)?,
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
            let request_topic = run.request_topic.clone();
            self.run_existing_with_events(
                run,
                definition,
                snapshot,
                RunMode::UntilBlocked,
                ActiveRunExecution {
                    request_topic,
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
        let client = self
            .acp_connector
            .connect(transport_for(agent), watchdog_options_for(agent))
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
        let store = self.store_for_run(&run_id)?;
        store.clear_agent_prompt_window(&run_id)?;
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
            #[cfg(feature = "test-support")]
            handoff_observer: self.handoff_observer.clone(),
        };
        let factory = self.dependencies.agent_factory(&self.config)?;
        let executor = AgentExecutor::new(factory, agent_store, agent_config)
            .with_prompt_turn_controls(self.prompt_turn_controls.clone());
        let dispatcher = EngineActionDispatcher::new(executor, self.config.cwd.clone());
        let provider = LuaStepActionProvider::new(snapshot);
        let policy = self.resolve_limits(&run.config_set.name);
        let runner = WorkflowRunner::new(store, dispatcher, provider, self.events.clone(), policy)
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
            AgentProgressKind::SessionReady {
                role,
                session_id,
                descriptor,
            } => WorkflowEventKind::AgentSessionReady {
                step_id,
                role,
                session_id,
                descriptor,
            },
            AgentProgressKind::PromptWindowOpened { role, window_id } => {
                WorkflowEventKind::AgentPromptWindowOpened {
                    step_id,
                    role,
                    window_id,
                }
            }
            AgentProgressKind::PromptWindowClosed { role, window_id } => {
                WorkflowEventKind::AgentPromptWindowClosed {
                    step_id,
                    role,
                    window_id,
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
        Ok(self.store_with_cancellation(self.store.clone()))
    }

    fn store_for_run(&self, run_id: &str) -> Result<RedbRunStore> {
        let run_id = run_id.to_string();
        let events = self.events.clone();
        let observer: StoreWaitObserver = Arc::new(move |_| {
            events.emit(WorkflowEvent::new(
                run_id.clone(),
                WorkflowEventKind::WorkflowStoreWaiting {
                    message: WORKFLOW_STORE_WAITING_MESSAGE.to_string(),
                },
            ));
        });
        Ok(self.store_with_cancellation(self.store.with_wait_observer(observer)))
    }

    fn store_with_cancellation(&self, store: RedbRunStore) -> RedbRunStore {
        let expected_generation = self.store_wait_generation.load(Ordering::Acquire);
        let cancellation =
            StoreWaitCancellation::new(self.store_wait_generation.clone(), expected_generation);
        store.with_wait_cancellation(cancellation)
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
            let required_fields = agent
                .output
                .as_ref()
                .map(|output| output.required_fields.clone())
                .unwrap_or_default();
            let optional_fields = agent
                .output
                .as_ref()
                .and_then(|output| output.fields.as_object())
                .map(|map| {
                    map.keys()
                        .filter(|field| !required_fields.iter().any(|required| required == *field))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            (required_fields, optional_fields, true)
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
    RunHead::from_run(run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_dependencies::{MockRuntimeDependencies, SharedClientFactory};
    use async_trait::async_trait;
    use cowboy_agent_acp::transport::TransportConfig;
    use cowboy_agent_acp::{AgentWatchdogOptions, Client as AcpClient};
    use cowboy_agent_client::{AgentInfo, Client, Event, PromptContent, StopReason};
    #[cfg(feature = "test-support")]
    use cowboy_workflow_agent::PromptWindowHandoffPoint;
    use cowboy_workflow_agent::{ClientFactory, ResolvedAgentClient};
    use cowboy_workflow_core::{
        AgentPromptWindow, ResumeCallback, RoleDefinition, RunStatus, StepAction,
    };
    use parking_lot::Mutex as SyncMutex;
    use std::collections::VecDeque;
    use std::os::unix::fs::PermissionsExt;
    use std::thread;
    use std::time::Duration;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RecordedRuntimeAcpConnection {
        command: String,
        watchdog: AgentWatchdogOptions,
    }

    #[derive(Clone, Default)]
    struct RecordingRuntimeAcpConnector {
        connections: Arc<SyncMutex<Vec<RecordedRuntimeAcpConnection>>>,
    }

    impl RecordingRuntimeAcpConnector {
        fn connections(&self) -> Vec<RecordedRuntimeAcpConnection> {
            self.connections.lock().clone()
        }
    }

    #[async_trait]
    impl AcpConnector for RecordingRuntimeAcpConnector {
        async fn connect(
            &self,
            transport: TransportConfig,
            watchdog: AgentWatchdogOptions,
        ) -> anyhow::Result<AcpClient> {
            let TransportConfig::Stdio(transport) = transport else {
                panic!("workflow runtime agents must use stdio")
            };
            self.connections.lock().push(RecordedRuntimeAcpConnection {
                command: transport.command,
                watchdog,
            });
            Err(anyhow::anyhow!("recording runtime connector"))
        }
    }

    fn agent(name: &str, command: &str) -> AgentRuntimeConfig {
        AgentRuntimeConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            model: Some(ModelInfo::default()),
            watchdog: AgentWatchdogRuntimeConfig::default(),
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

        fn session_descriptor(&self) -> Option<&cowboy_agent_client::AgentSessionDescriptor> {
            None
        }

        fn session_id(&self) -> Option<&str> {
            self.session_id.as_deref()
        }

        async fn new_session(
            &mut self,
            _cwd: &str,
            _mcp_servers: &[Value],
            _model: Option<&ModelInfo>,
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
            _cancellation: PromptTurnCancellation,
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
                model: Some(ModelInfo {
                    id: "scripted-model".to_string(),
                    provider: Some("test".to_string()),
                }),
                backend: "scripted-agent".to_string(),
            })
        }
    }

    #[cfg(feature = "test-support")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum HandoffPause {
        BeforeCompareAndSeal,
        AfterSealed,
    }

    #[cfg(feature = "test-support")]
    #[derive(Debug)]
    struct BoundaryObserver {
        pause: HandoffPause,
        reached: tokio::sync::Notify,
        release: tokio::sync::Notify,
        point: SyncMutex<Option<PromptWindowHandoffPoint>>,
        paused: std::sync::atomic::AtomicBool,
    }

    #[cfg(feature = "test-support")]
    impl BoundaryObserver {
        fn new(pause: HandoffPause) -> Self {
            Self {
                pause,
                reached: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                point: SyncMutex::new(None),
                paused: std::sync::atomic::AtomicBool::new(false),
            }
        }

        async fn wait_until_reached(&self) -> PromptWindowHandoffPoint {
            self.reached.notified().await;
            self.point
                .lock()
                .clone()
                .expect("handoff point recorded before notification")
        }

        fn resume(&self) {
            self.release.notify_one();
        }
    }

    #[cfg(feature = "test-support")]
    #[async_trait]
    impl PromptWindowHandoffObserver for BoundaryObserver {
        async fn observe(&self, point: PromptWindowHandoffPoint) {
            let should_pause = matches!(
                (&self.pause, &point),
                (
                    HandoffPause::BeforeCompareAndSeal,
                    PromptWindowHandoffPoint::BeforeCompareAndSeal { .. },
                ) | (
                    HandoffPause::AfterSealed,
                    PromptWindowHandoffPoint::AfterCompareAndSeal { pending: false, .. },
                )
            );
            if !should_pause || self.paused.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return;
            }

            *self.point.lock() = Some(point);
            self.reached.notify_one();
            self.release.notified().await;
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
                dependencies.expect_agent_factory().returning(|config| {
                    ProductionRuntimeDependencies::default().agent_factory(config)
                });
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

        let topic = ProductionRuntimeDependencies::default()
            .generate_request_topic(&config, SelectorMode::Agent, "summarize this request")
            .await;

        assert_eq!(topic, None);
    }

    fn production_connector_test_config(dir: &tempfile::TempDir) -> RuntimeConfig {
        RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            agents: vec![AgentRuntimeConfig {
                name: "default".to_string(),
                command: "default-runtime-agent".to_string(),
                args: Vec::new(),
                model: None,
                watchdog: AgentWatchdogRuntimeConfig {
                    response_timeout_seconds: 31,
                    cancel_timeout_seconds: 32,
                    recovery_operation_timeout_seconds: 33,
                },
            }],
            config_sets: BTreeMap::from([("default".to_string(), RunnerLimitsConfig::default())]),
        }
    }

    fn runtime_with_recording_connector(
        config: RuntimeConfig,
        connector: Arc<RecordingRuntimeAcpConnector>,
    ) -> WorkflowRuntime {
        WorkflowRuntime::with_dependencies_and_connector(
            config,
            Arc::new(MockRuntimeDependencies::new()),
            connector,
        )
    }

    fn expected_default_runtime_connection() -> RecordedRuntimeAcpConnection {
        RecordedRuntimeAcpConnection {
            command: "default-runtime-agent".to_string(),
            watchdog: AgentWatchdogOptions {
                response_timeout_seconds: 31,
                cancel_timeout_seconds: 32,
                recovery_operation_timeout_seconds: 33,
            },
        }
    }

    #[tokio::test]
    async fn workflow_selector_client_construction_uses_default_watchdog() {
        let dir = tempfile::tempdir().unwrap();
        let connector = Arc::new(RecordingRuntimeAcpConnector::default());
        let runtime = runtime_with_recording_connector(
            production_connector_test_config(&dir),
            connector.clone(),
        );
        let catalog = runtime.catalog().unwrap();

        let error = runtime
            .select_workflow("select a workflow", &catalog)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("recording runtime connector"));
        assert_eq!(
            connector.connections(),
            [expected_default_runtime_connection()]
        );
    }

    #[tokio::test]
    async fn workflow_improvement_client_construction_uses_default_watchdog() {
        let dir = tempfile::tempdir().unwrap();
        let connector = Arc::new(RecordingRuntimeAcpConnector::default());
        let runtime = runtime_with_recording_connector(
            production_connector_test_config(&dir),
            connector.clone(),
        );
        let run = summary_test_run("improve-connector", RunStatus::Completed, None);
        runtime.store().unwrap().save_run(&run).unwrap();

        let error = runtime.improve_run(&run.id).await.unwrap_err();

        assert!(error.to_string().contains("recording runtime connector"));
        assert_eq!(
            connector.connections(),
            [expected_default_runtime_connection()]
        );
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
                    output = {{ status = {{ "success" }}, fields = {{ summary = "string", validation_doc = "string", rca_doc = "string", repro_test = "string" }}, required_fields = {{ "summary" }} }}
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

    #[tokio::test]
    async fn accepted_prompt_cancels_active_turn_before_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let first_control = dir.path().join("first-control.json");
        let replacement_prompt = dir.path().join("replacement-prompt.json");
        let delivery_order = dir.path().join("delivery-order");
        let handoff_gate = dir.path().join("handoff-gate");
        let agent_script = dir.path().join("cancel-aware-agent.sh");
        fs::write(
            &agent_script,
            r#"#!/bin/sh
first_control="$1"
replacement_prompt="$2"
delivery_order="$3"
handoff_gate="$4"

IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false},"agentInfo":{"name":"cancel-aware-agent","version":"1"}}}'
IFS= read -r new_session
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"sessionId":"session-1"}}'
IFS= read -r initial_prompt
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"action-1","title":"Current action","kind":"other","status":"pending"}}}'
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"action-1","status":"completed"}}}'

(
  sleep 1
  if mkdir "$handoff_gate" 2>/dev/null; then
    printf '%s' 'after_current_turn' > "$delivery_order"
    printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"---\nstatus: success\nsummary: initial\n---\ninitial"}}}}'
    printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn"}}'
  fi
) &

IFS= read -r control
printf '%s' "$control" > "$first_control"
case "$control" in
  *'"method":"session/cancel"'*)
    if mkdir "$handoff_gate" 2>/dev/null; then
      printf '%s' 'before_replacement' > "$delivery_order"
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"stopReason":"cancelled"}}'
    fi
    IFS= read -r replacement
    ;;
  *)
    replacement="$control"
    ;;
esac
wait
printf '%s' "$replacement" > "$replacement_prompt"
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"---\nstatus: success\nsummary: corrected\n---\ncorrected"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&agent_script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&agent_script, permissions).unwrap();
        let runtime = runtime_for_agent_workflow(
            &dir,
            None,
            vec![AgentRuntimeConfig {
                name: "default".to_string(),
                command: agent_script.to_string_lossy().to_string(),
                args: vec![
                    first_control.to_string_lossy().to_string(),
                    replacement_prompt.to_string_lossy().to_string(),
                    delivery_order.to_string_lossy().to_string(),
                    handoff_gate.to_string_lossy().to_string(),
                ],
                model: Some(ModelInfo::default()),
                watchdog: AgentWatchdogRuntimeConfig::default(),
            }],
        );
        let submit_runtime = runtime.clone();
        let mut events = runtime.events().subscribe();
        let executing_runtime = runtime.clone();
        let run_task = tokio::spawn(async move { executing_runtime.start_run("original").await });
        let mut window_id = None;
        let mut run_id = None;
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                .await
                .expect("agent action boundary should be observed")
                .expect("workflow event channel should remain open");
            let event_run_id = event.run_id.clone();
            match event.kind {
                WorkflowEventKind::AgentPromptWindowOpened { window_id: id, .. } => {
                    window_id = Some(id);
                    run_id = Some(event_run_id);
                }
                WorkflowEventKind::AgentToolCallUpdate { status, .. } if status == "completed" => {
                    break;
                }
                _ => {}
            }
        }
        let run_id = run_id.expect("prompt window event should identify the active run");
        let window_id = window_id.expect("prompt window should open before the action boundary");
        let accepted_content = "steer by cancelling the active turn";
        let accepted = submit_runtime
            .submit_user_prompt(&run_id, &window_id, accepted_content.to_string())
            .unwrap();
        assert!(matches!(accepted, UserPromptSubmission::Accepted(_)));

        let report = run_task.await.unwrap().unwrap();
        let store = runtime.store().unwrap();
        let record = store
            .get_object::<StepRecord>(report.run.head.as_ref().unwrap())
            .unwrap();
        assert_eq!(record.output.as_ref().unwrap().body, "corrected");

        let replacement: Value =
            serde_json::from_str(&fs::read_to_string(&replacement_prompt).unwrap()).unwrap();
        assert_eq!(replacement["method"], "session/prompt");
        assert_eq!(replacement["params"]["sessionId"], "session-1");
        assert_eq!(replacement["params"]["prompt"][2]["type"], "text");
        assert_eq!(replacement["params"]["prompt"][2]["text"], accepted_content);

        let control: Value =
            serde_json::from_str(&fs::read_to_string(&first_control).unwrap()).unwrap();
        assert_eq!(
            control["method"], "session/cancel",
            "accepted prompt did not cancel the active ACP turn"
        );
        assert_eq!(control["params"]["sessionId"], "session-1");
        assert_eq!(
            fs::read_to_string(&delivery_order).unwrap(),
            "before_replacement"
        );
    }

    fn write_descriptor_stub(path: &std::path::Path) {
        // Line-oriented ACP stub: one JSON-RPC message per line in, exactly one
        // JSON object per line out. Answers by method so any session (topic
        // generation plus the workflow step) is served. `session/new` returns
        // agent-owned configOptions for model, context_size, and thought_level.
        fs::write(
            path,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false},"agentInfo":{"name":"descriptor-stub","version":"1"}}}\n' "$id"
      ;;
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"s1","configOptions":[{"id":"model","category":"model","currentValue":"gpt-5.6-sol","options":[{"value":"gpt-5.6-sol"}]},{"id":"context_size","category":"model_config","currentValue":"1m","options":[{"value":"1m"}]},{"id":"thought_level","category":"thought_level","currentValue":"high","options":[{"value":"high"}]}]}}\n' "$id"
      ;;
    *'"method":"session/prompt"'*)
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"---\nstatus: success\nsummary: ok\n---\ndone"}}}}'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id"
      ;;
    *'"method":"session/cancel"'*)
      : ;;
    *'"id":'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[tokio::test]
    async fn agent_session_ready_event_carries_returned_descriptor_case_a() {
        // Case A: no model configured, so ACP model-selection enforcement never
        // runs. The descriptor on the real emitted event must equal the
        // aggregation of only the stub's returned currentValues.
        let dir = tempfile::tempdir().unwrap();
        let agent_script = dir.path().join("descriptor-stub.sh");
        write_descriptor_stub(&agent_script);
        let runtime = runtime_for_agent_workflow(
            &dir,
            None,
            vec![AgentRuntimeConfig {
                name: "default".to_string(),
                command: agent_script.to_string_lossy().to_string(),
                args: Vec::new(),
                model: None,
                watchdog: AgentWatchdogRuntimeConfig::default(),
            }],
        );

        let mut events = runtime.events().subscribe();
        let executing_runtime = runtime.clone();
        let run_task = tokio::spawn(async move { executing_runtime.start_run("smoke").await });

        let descriptor = loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
                .await
                .expect("session-ready event should be observed")
                .expect("workflow event channel should remain open");
            if let WorkflowEventKind::AgentSessionReady {
                descriptor: token, ..
            } = event.kind
            {
                break token;
            }
        };

        run_task.await.unwrap().unwrap();
        assert_eq!(descriptor, Some("gpt-5.6-sol-1m-high".to_string()));
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn append_at_pre_seal_boundary_forces_same_session_correction() {
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
                output = { status = { "success" }, fields = { summary = "string" } }
              }
            end
            local inspect = step("inspect")
            inspect.run = function(ctx)
              return action.status {
                status = "success",
                fields = {
                  count = #ctx.user_inputs,
                  initial = ctx.user_inputs[1].content,
                  follow_up = ctx.user_inputs[2].content,
                }
              }
            end
            local verify = step("verify", { role = developer })
            verify.run = function(ctx)
              return action.agent {
                role = developer,
                prompt = "Verify all cumulative direction",
                output = { status = { "success" }, fields = { summary = "string" } }
              }
            end
            start:on("success", inspect)
            inspect:on("success", verify)
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
        let submit_runtime = WorkflowRuntime::new(config.clone());
        let factory = ScriptedAgentFactory::new(vec![
            "---\nstatus: success\nsummary: initial\n---\ninitial".to_string(),
            "---\nstatus: success\nsummary: corrected\n---\ncorrected".to_string(),
            "---\nstatus: success\nsummary: verified\n---\nverified".to_string(),
        ]);
        let observer = Arc::new(BoundaryObserver::new(HandoffPause::BeforeCompareAndSeal));
        let runtime = WorkflowRuntime::with_dependencies(
            config,
            mock_runtime_dependencies(None, Some(factory.clone())),
        )
        .with_deterministic_selector()
        .with_handoff_observer(observer.clone());
        let executing_runtime = runtime.clone();
        let run_task = tokio::spawn(async move { executing_runtime.start_run("original").await });
        let PromptWindowHandoffPoint::BeforeCompareAndSeal {
            run_id, window_id, ..
        } = observer.wait_until_reached().await
        else {
            panic!("expected pre-seal boundary")
        };

        let accepted = submit_runtime
            .submit_user_prompt(&run_id, &window_id, "  correct\nnow  ".to_string())
            .unwrap();
        assert!(matches!(accepted, UserPromptSubmission::Accepted(_)));
        observer.resume();
        let report = run_task.await.unwrap().unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        let store = runtime.store().unwrap();
        let final_record = store
            .get_object::<StepRecord>(report.run.head.as_ref().unwrap())
            .unwrap();
        assert_eq!(final_record.step, "verify");
        assert_eq!(final_record.output.as_ref().unwrap().body, "verified");
        let inspect_record = store
            .get_object::<StepRecord>(final_record.prev.as_ref().unwrap())
            .unwrap();
        assert_eq!(inspect_record.step, "inspect");
        assert_eq!(inspect_record.output.as_ref().unwrap().fields["count"], 2);
        assert_eq!(
            inspect_record.output.as_ref().unwrap().fields["initial"],
            "original"
        );
        assert_eq!(
            inspect_record.output.as_ref().unwrap().fields["follow_up"],
            "  correct\nnow  "
        );
        let first_record = store
            .get_object::<StepRecord>(inspect_record.prev.as_ref().unwrap())
            .unwrap();
        assert_eq!(first_record.output.as_ref().unwrap().body, "corrected");
        assert_eq!(factory.created_roles(), ["developer"]);
        let prompts = factory.prompts();
        assert_eq!(prompts.len(), 3);
        assert!(prompts[1].contains("  correct\nnow  "));
        assert_eq!(prompts[2].matches("\"sequence\": 0").count(), 1);
        assert_eq!(prompts[2].matches("\"sequence\": 1").count(), 1);
        assert_eq!(prompts[2].matches(r#"  correct\nnow  "#).count(), 1);
        assert_eq!(
            final_record.input.context["user_inputs"],
            serde_json::json!([
                {
                    "sequence": 0,
                    "kind": "initial",
                    "content": "original",
                    "submitted_at": report.run.created_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                },
                {
                    "sequence": 1,
                    "kind": "follow_up",
                    "content": "  correct\nnow  ",
                    "submitted_at": submit_runtime.store().unwrap().load_user_prompts(&run_id).unwrap()[0]
                        .submitted_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                }
            ])
        );
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn append_at_post_seal_boundary_is_rejected_before_step_application() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows-seal");
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
                output = { status = { "success" }, fields = { summary = "string" }, required_fields = { "summary" } }
              }
            end
            return workflow("aaa", start)
            "#,
        )
        .unwrap();
        let config = RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state-seal"),
            workflow_store: dir.path().join("state-seal/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: Vec::new(),
            config_sets: BTreeMap::from([("default".to_string(), RunnerLimitsConfig::default())]),
        };
        let submit_runtime = WorkflowRuntime::new(config.clone());
        let factory = ScriptedAgentFactory::new(vec![
            "---\nstatus: success\nsummary: done\n---\ndone".to_string(),
        ]);
        let observer = Arc::new(BoundaryObserver::new(HandoffPause::AfterSealed));
        let runtime = WorkflowRuntime::with_dependencies(
            config,
            mock_runtime_dependencies(None, Some(factory)),
        )
        .with_deterministic_selector()
        .with_handoff_observer(observer.clone());
        let executing_runtime = runtime.clone();
        let run_task = tokio::spawn(async move { executing_runtime.start_run("original").await });
        let PromptWindowHandoffPoint::AfterCompareAndSeal {
            run_id,
            window_id,
            pending,
            ..
        } = observer.wait_until_reached().await
        else {
            panic!("expected post-seal boundary")
        };
        assert!(!pending);

        let rejected = submit_runtime
            .submit_user_prompt(&run_id, &window_id, "too late".to_string())
            .unwrap();
        assert_eq!(
            rejected,
            UserPromptSubmission::Rejected(UserPromptRejection::SealedWindow)
        );
        assert!(matches!(
            submit_runtime.load_run(&run_id).unwrap().status,
            RunStatus::Running
        ));
        observer.resume();
        let report = run_task.await.unwrap().unwrap();
        assert_eq!(report.run.status, RunStatus::Completed);
        assert!(
            submit_runtime
                .store()
                .unwrap()
                .load_user_prompts(&run_id)
                .unwrap()
                .is_empty()
        );
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

    #[test]
    fn submit_user_prompt_validates_empty_and_preserves_run_counters_and_content() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_empty_state(&dir);
        let store = runtime.store().unwrap();
        let run = summary_test_run("run-prompt", RunStatus::Running, None);
        store.save_run(&run).unwrap();
        store
            .open_agent_prompt_window(AgentPromptWindow {
                window_id: "window-1".to_string(),
                run_id: run.id.clone(),
                step_record_id: "record-1".to_string(),
                step_id: run.current_step.clone(),
                role_id: "developer".to_string(),
                baseline_sequence: 0,
                applied_sequence: 0,
                opened_at: Utc::now(),
                sealed_at: None,
            })
            .unwrap();

        assert_eq!(
            runtime
                .submit_user_prompt(&run.id, "window-1", " \n\t ".to_string())
                .unwrap(),
            UserPromptSubmission::Rejected(UserPromptRejection::Empty)
        );
        let exact = "  adjust\nthis  ".to_string();
        let accepted = runtime
            .clone()
            .submit_user_prompt(&run.id, "window-1", exact.clone())
            .unwrap();
        let UserPromptSubmission::Accepted(prompt) = accepted else {
            panic!("expected durable acceptance")
        };
        assert_eq!(prompt.sequence, 1);
        assert_eq!(prompt.content, exact);
        assert_eq!(store.load_user_prompts(&run.id).unwrap(), vec![prompt]);
        assert_eq!(store.load_run(&run.id).unwrap(), run);
    }

    #[test]
    fn cancellation_guard_clears_active_prompt_window() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_empty_state(&dir);
        let store = runtime.store().unwrap();
        let run = summary_test_run("run-cancel", RunStatus::Running, None);
        store.save_run(&run).unwrap();
        store
            .open_agent_prompt_window(AgentPromptWindow {
                window_id: "window-cancel".to_string(),
                run_id: run.id.clone(),
                step_record_id: "record-cancel".to_string(),
                step_id: run.current_step.clone(),
                role_id: "developer".to_string(),
                baseline_sequence: 0,
                applied_sequence: 0,
                opened_at: Utc::now(),
                sealed_at: None,
            })
            .unwrap();

        drop(ActiveRunCancellationGuard::new(
            store.clone(),
            run.id.clone(),
            ActiveRunClock::open(&run),
        ));

        assert!(store.clear_agent_prompt_window(&run.id).unwrap().is_none());
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

    #[test]
    fn idle_runtime_does_not_keep_workflow_store_locked() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_empty_state(&dir);
        assert!(runtime.list_runs(None).unwrap().is_empty());
        let store_path = dir.path().join("state/workflow.redb");

        let database = redb::Database::create(&store_path).unwrap_or_else(|err| {
            panic!(
                "idle runtime retained the workflow store lock at {}: {err}",
                store_path.display()
            )
        });

        drop(database);
    }

    #[test]
    fn cancelling_runtime_store_wait_interrupts_contended_operation() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_inline_workflow(
            &dir,
            r#"
            local first = step("first")
            first.run = function(ctx) return action.status { status = "next" } end
            local second = step("second")
            second.run = function(ctx) return action.status { status = "success" } end
            first:on("next", second)
            return workflow("declared", first)
            "#,
        );
        let async_runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let started = async_runtime
            .block_on(runtime.start_run_with_workflow_stepwise("aaa", "do it"))
            .unwrap();
        let store_path = dir.path().join("state/workflow.redb");
        let database = redb::Database::create(store_path).unwrap();
        let mut events = runtime.events().subscribe();
        let executing_runtime = runtime.clone();
        let run_id = started.run.id.clone();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let async_runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let result = async_runtime.block_on(executing_runtime.resume_run(&run_id));
            result_tx.send(result).unwrap();
        });
        let wait_started = std::time::Instant::now();
        'waiting: loop {
            while let Ok(event) = events.try_recv() {
                if matches!(event.kind, WorkflowEventKind::WorkflowStoreWaiting { .. }) {
                    break 'waiting;
                }
            }

            assert!(
                wait_started.elapsed() < Duration::from_secs(1),
                "runtime did not report the contended store wait"
            );
            thread::sleep(Duration::from_millis(5));
        }

        runtime.cancel_store_waits();
        let result = result_rx.recv_timeout(Duration::from_millis(250));
        drop(database);
        worker.join().unwrap();
        let error = result
            .expect("cancelled runtime store wait should finish promptly")
            .expect_err("cancelled store wait should fail the runtime operation");

        assert!(
            error.to_string().contains("workflow store wait cancelled"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn workflow_store_wait_event_emits_once_during_contended_resume() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_inline_workflow(
            &dir,
            r#"
            local first = step("first")
            first.run = function(ctx) return action.status { status = "next" } end
            local second = step("second")
            second.run = function(ctx) return action.status { status = "success" } end
            first:on("next", second)
            return workflow("declared", first)
            "#,
        );
        let started = runtime
            .start_run_with_workflow_stepwise("aaa", "do it")
            .await
            .unwrap();
        assert_eq!(started.run.status, RunStatus::Running);
        let store_path = dir.path().join("state/workflow.redb");
        let holder_path = store_path.clone();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let holder = thread::spawn(move || {
            let database = redb::Database::create(holder_path).unwrap();
            locked_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(750));
            drop(database);
        });
        locked_rx.recv().unwrap();
        let mut rx = runtime.events().subscribe();

        let report = runtime.resume_run(&started.run.id).await.unwrap();

        holder.join().unwrap();
        assert_eq!(report.run.status, RunStatus::Completed);
        let mut wait_events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let WorkflowEventKind::WorkflowStoreWaiting { message } = event.kind {
                wait_events.push((event.run_id, message));
            }
        }
        assert_eq!(wait_events.len(), 1, "{wait_events:?}");
        assert_eq!(wait_events[0].0, report.run.id);
        assert_eq!(wait_events[0].1, WORKFLOW_STORE_WAITING_MESSAGE);
        assert!(
            !wait_events[0]
                .1
                .contains(dir.path().to_string_lossy().as_ref())
        );
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

        let implicit = runtime
            .start_run_with_workflow("implicit", "do it")
            .await
            .unwrap();
        assert_eq!(implicit.run.config_set.name, "default");
    }

    fn runtime_with_config_sets(
        dir: &tempfile::TempDir,
        config_sets: BTreeMap<String, RunnerLimitsConfig>,
    ) -> WorkflowRuntime {
        WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            agents: vec![agent("default", "unused-agent")],
            config_sets,
        })
    }

    #[test]
    fn resolve_limits_uses_requested_set_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let careful = RunnerLimitsConfig {
            max_steps_per_run: 9,
            max_visits_per_step: 8,
            max_retries_per_run: 7,
            max_retries_per_step: 6,
        };
        let runtime = runtime_with_config_sets(
            &dir,
            BTreeMap::from([
                ("default".to_string(), RunnerLimitsConfig::default()),
                ("careful".to_string(), careful),
            ]),
        );

        let policy = runtime.resolve_limits("careful");
        assert_eq!(policy.name, "careful");
        assert_eq!(policy.limits, careful.into());
    }

    #[test]
    fn resolve_limits_falls_back_to_default_when_requested_missing() {
        let dir = tempfile::tempdir().unwrap();
        let default = RunnerLimitsConfig {
            max_steps_per_run: 3,
            ..RunnerLimitsConfig::default()
        };
        let runtime =
            runtime_with_config_sets(&dir, BTreeMap::from([("default".to_string(), default)]));

        let policy = runtime.resolve_limits("careful");
        assert_eq!(policy.name, "default");
        assert_eq!(policy.limits, default.into());
    }

    #[test]
    fn resolve_limits_falls_back_to_builtin_default_when_all_sets_missing() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_with_config_sets(&dir, BTreeMap::new());

        let policy = runtime.resolve_limits("careful");
        assert_eq!(policy.name, "default");
        assert_eq!(policy.limits, RunnerLimits::default());
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
        assert!(runtime.list_runs(None).unwrap().is_empty());
    }

    #[tokio::test]
    async fn changed_config_set_limits_apply_live_on_resume_and_step() {
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
        let run_a = creator
            .start_run_with_workflow_stepwise("multi", "do it")
            .await
            .unwrap();
        assert_eq!(run_a.run.steps_executed, 1);
        assert_eq!(run_a.run.current_step, "second");
        let run_b = creator
            .start_run_with_workflow_stepwise("multi", "do it")
            .await
            .unwrap();
        assert_eq!(run_b.run.steps_executed, 1);
        assert_eq!(run_b.run.current_step, "second");

        // A second runtime whose `careful` set lowers the step ceiling to 1.
        let changed = RunnerLimitsConfig {
            max_steps_per_run: 1,
            ..original
        };
        let resumed_runtime = runtime_for_workflow_dir_with_config_sets(
            &dir,
            workflow_dir,
            config_sets_with_careful(Some(changed)),
        );

        // Run A: `step_run` applies the lowered live limit and is blocked.
        let stepped_err = resumed_runtime.step_run(&run_a.run.id).await.unwrap_err();
        assert!(
            stepped_err
                .to_string()
                .contains("run exceeded max step count (1)"),
            "{stepped_err}"
        );
        let loaded_a = resumed_runtime.load_run(&run_a.run.id).unwrap();
        assert!(matches!(loaded_a.status, RunStatus::Failed { .. }));
        assert_eq!(loaded_a.current_step, "second");
        assert_eq!(loaded_a.steps_executed, 1);

        // Run B: `resume_run` applies the same lowered live limit and is blocked.
        let resumed_err = resumed_runtime.resume_run(&run_b.run.id).await.unwrap_err();
        assert!(
            resumed_err
                .to_string()
                .contains("run exceeded max step count (1)"),
            "{resumed_err}"
        );
        let loaded_b = resumed_runtime.load_run(&run_b.run.id).unwrap();
        assert!(matches!(loaded_b.status, RunStatus::Failed { .. }));
        assert_eq!(loaded_b.current_step, "second");
        assert_eq!(loaded_b.steps_executed, 1);
    }

    #[tokio::test]
    async fn deleted_set_answer_and_resolve_fall_back_to_default_limits() {
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
            max_steps_per_run: 2,
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
        let waiting_head = waiting.run.head.clone();
        let failed = creator
            .start_run_with_workflow("resolve", "do it")
            .await
            .unwrap();
        assert!(matches!(failed.run.status, RunStatus::Failed { .. }));
        assert_eq!(failed.run.steps_executed, 1);
        let failed_head = failed.run.head.clone();

        // A second runtime that DELETES `careful` and lowers `default` to a
        // one-step ceiling. Live resolution falls back to `default` limit 1,
        // which blocks executing the next `done` step (careful's 2 would have
        // completed both).
        let lowered_default = RunnerLimitsConfig {
            max_steps_per_run: 1,
            ..RunnerLimitsConfig::default()
        };
        let without_careful = runtime_for_workflow_dir_with_config_sets(
            &dir,
            workflow_dir,
            BTreeMap::from([("default".to_string(), lowered_default)]),
        );
        let store = without_careful.store().unwrap();

        let answered_err = without_careful
            .answer_run(&waiting.run.id, "approval", "yes")
            .await
            .unwrap_err();
        assert!(
            answered_err
                .to_string()
                .contains("run exceeded max step count (1)"),
            "{answered_err}"
        );
        let loaded_answer = without_careful.load_run(&waiting.run.id).unwrap();
        assert!(matches!(loaded_answer.status, RunStatus::Failed { .. }));
        assert_ne!(loaded_answer.status, RunStatus::Completed);
        assert_eq!(loaded_answer.current_step, "done");
        assert_eq!(loaded_answer.steps_executed, 1);
        assert_ne!(loaded_answer.head, waiting_head);
        let answer_record = store
            .get_object::<StepRecord>(loaded_answer.head.as_ref().unwrap())
            .unwrap();
        assert_eq!(answer_record.step, "ask");
        assert_eq!(answer_record.output.unwrap().fields["answer"], "yes");

        let options = without_careful.resolution_options(&failed.run.id).unwrap();
        assert!(
            options
                .statuses
                .iter()
                .any(|status| status.status == "fixed")
        );
        let resolved_err = without_careful
            .resolve_run(&failed.run.id, "fixed", None, None)
            .await
            .unwrap_err();
        assert!(
            resolved_err
                .to_string()
                .contains("run exceeded max step count (1)"),
            "{resolved_err}"
        );
        let loaded_resolve = without_careful.load_run(&failed.run.id).unwrap();
        assert!(matches!(loaded_resolve.status, RunStatus::Failed { .. }));
        assert_ne!(loaded_resolve.status, RunStatus::Completed);
        assert_eq!(loaded_resolve.current_step, "done");
        assert_eq!(loaded_resolve.steps_executed, 1);
        assert_ne!(loaded_resolve.head, failed_head);
        let resolve_record = store
            .get_object::<StepRecord>(loaded_resolve.head.as_ref().unwrap())
            .unwrap();
        assert_eq!(resolve_record.action, "manual_resolution");
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

        let summaries = runtime.list_runs(None).unwrap();

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

    #[test]
    fn list_runs_filters_by_partial_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_empty_state(&dir);
        let store = runtime.store().unwrap();
        for run in [
            summary_test_run(
                "alpha-wait-run",
                RunStatus::WaitingForInput {
                    step: "review".to_string(),
                    prompt_id: "approval".to_string(),
                    message: "Approve?".to_string(),
                    choices: Vec::new(),
                    resume_callback: ResumeCallback::new(
                        "ask_user",
                        serde_json::json!({ "prompt_id": "approval" }),
                    )
                    .unwrap(),
                },
                Some("Approve release"),
            ),
            summary_test_run(
                "beta-completed-run",
                RunStatus::Completed,
                Some("Ship release"),
            ),
            summary_test_run(
                "gamma-running-run",
                RunStatus::Running,
                Some("Keep working"),
            ),
        ] {
            store.save_run(&run).unwrap();
            store.update_run_head(&run.id, run_head(&run)).unwrap();
        }

        let mut all_ids = runtime
            .list_runs(None)
            .unwrap()
            .into_iter()
            .map(|summary| summary.run_id)
            .collect::<Vec<_>>();
        all_ids.sort();
        assert_eq!(
            all_ids,
            vec!["alpha-wait-run", "beta-completed-run", "gamma-running-run"]
        );

        let filtered = runtime.list_runs(Some("wait")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id, "alpha-wait-run");
        assert_eq!(filtered[0].topic.as_deref(), Some("Approve release"));
        assert_eq!(filtered[0].workflow_name, "aaa");
        assert_eq!(
            filtered[0].status_detail.state,
            RunStatusState::WaitingForInput
        );
        assert_eq!(filtered[0].current_step, "start");
        assert_eq!(filtered[0].head_step, None);

        let no_matches = runtime.list_runs(Some("WAIT")).unwrap();
        assert!(no_matches.is_empty());
    }

    #[test]
    fn list_runs_reads_persisted_head_summaries_without_full_runs() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_empty_state(&dir);
        let store = runtime.store().unwrap();

        for index in 0..100 {
            let run = summary_test_run(
                &format!("run-bulk-{index:04}"),
                RunStatus::Completed,
                Some(&format!("Bulk run {index}")),
            );
            store.update_run_head(&run.id, run_head(&run)).unwrap();
        }

        let summaries = runtime.list_runs(None).unwrap();

        assert_eq!(summaries.len(), 100);
        let summary = summaries
            .iter()
            .find(|summary| summary.run_id == "run-bulk-0042")
            .expect("persisted run-head summary");
        assert_eq!(summary.topic.as_deref(), Some("Bulk run 42"));
        assert_eq!(summary.workflow_name, "aaa");
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
            .list_runs(None)
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
        let mut head = run_head(&run);
        head.summary = None;
        store.update_run_head(&run.id, head).unwrap();
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
            .list_runs(None)
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
    async fn resume_run_restores_persisted_request_topic_in_run_started_event() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_topic_workflow(&dir, "Initial topic");
        let start = runtime.start_run_stepwise("do first step").await.unwrap();

        assert_eq!(start.run.request_topic.as_deref(), Some("Initial topic"));
        assert_eq!(
            runtime
                .load_run(&start.run.id)
                .unwrap()
                .request_topic
                .as_deref(),
            Some("Initial topic")
        );

        let report = runtime.resume_run(&start.run.id).await.unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        assert_eq!(first_run_started_topic(&report), Some("Initial topic"));
    }

    #[tokio::test]
    async fn step_run_restores_persisted_request_topic_in_run_started_event() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_topic_workflow(&dir, "Step topic");
        let start = runtime.start_run_stepwise("do first step").await.unwrap();

        assert_eq!(start.run.request_topic.as_deref(), Some("Step topic"));

        let report = runtime.step_run(&start.run.id).await.unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        assert_eq!(first_run_started_topic(&report), Some("Step topic"));
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
        let run_id = runtime.list_runs(None).unwrap()[0].run_id.clone();
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
        let run_id = runtime.list_runs(None).unwrap()[0].run_id.clone();
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
                model: Some(ModelInfo::default()),
                watchdog: AgentWatchdogRuntimeConfig::default(),
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
        assert_eq!(runtime.list_runs(None).unwrap().len(), 1);
        // The give-up path persists a clean Failed status for later resolution.
        let run = runtime
            .load_run(&runtime.list_runs(None).unwrap()[0].run_id)
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
        let run_id = runtime.list_runs(None).unwrap()[0].run_id.clone();

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
        assert_eq!(
            success.optional_fields,
            vec![
                "rca_doc".to_string(),
                "repro_test".to_string(),
                "validation_doc".to_string(),
            ]
        );

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

        // Optional fields may be omitted during manual resolution.
        let resolved = runtime
            .resolve_run(
                &run_id,
                "success",
                Some(serde_json::json!({ "summary": "manually resolved" })),
                None,
            )
            .await
            .unwrap();
        assert_eq!(resolved.run.status, RunStatus::Completed);
    }

    #[tokio::test]
    async fn resolve_run_restores_persisted_request_topic_and_exposes_fields_to_next_step() {
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
        let run_id = runtime.list_runs(None).unwrap()[0].run_id.clone();

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
        assert_eq!(first_run_started_topic(&report), Some("Original topic"));

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
                model: Some(ModelInfo::default()),
                watchdog: AgentWatchdogRuntimeConfig::default(),
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
        assert_eq!(runtime.list_runs(None).unwrap().len(), 1);
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
        assert_eq!(runtime.list_runs(None).unwrap().len(), 1);
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
        assert_eq!(runtime.list_runs(None).unwrap().len(), 1);
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
        assert!(runtime.list_runs(None).unwrap().is_empty());
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
    async fn resume_run_uses_persisted_workflow_source_after_filesystem_change() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        let workflow_path = workflow_dir.join("aaa.lua");
        fs::write(
            &workflow_path,
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "next" }
            end

            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = "original" }
            end

            start:on("next", finish)
            return workflow("aaa", start)
            "#,
        )
        .unwrap();
        let runtime = runtime_for_workflow_dir(&dir, workflow_dir);

        let started = runtime
            .start_run_with_workflow_stepwise("aaa", "request")
            .await
            .unwrap();
        assert_eq!(started.run.status, RunStatus::Running);
        assert_eq!(started.run.current_step, "finish");
        assert!(started.run.workflow_sources["aaa.lua"].contains("body = \"original\""));

        fs::write(
            &workflow_path,
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "next" }
            end

            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = "replacement" }
            end

            start:on("next", finish)
            return workflow("aaa", start)
            "#,
        )
        .unwrap();

        let resumed = runtime.resume_run(&started.run.id).await.unwrap();

        assert_eq!(resumed.run.status, RunStatus::Completed);
        assert_eq!(resumed.run.current_step, "finish");
        assert!(resumed.events.iter().any(|event| matches!(
            &event.kind,
            WorkflowEventKind::StepCompleted { step_id, status, body, .. }
                if step_id == "finish"
                    && status.as_deref() == Some("success")
                    && body == "original"
        )));
        assert!(!resumed.events.iter().any(|event| matches!(
            &event.kind,
            WorkflowEventKind::StepCompleted { body, .. } if body == "replacement"
        )));
    }

    #[tokio::test]
    async fn resume_retries_current_step_when_run_failed_by_exhausted_retries() {
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
                output = { status = { "success" }, fields = { summary = "string" } }
              }
            end
            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = "finished" }
            end
            start:on("success", finish)
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

        // The initial attempt plus both recoverable retries return
        // frontmatter-less bodies, exhausting the per-step retry budget. The
        // valid response is only reached if resume retries the failed step.
        let factory = ScriptedAgentFactory::new(vec![
            "no frontmatter here".to_string(),
            "still no frontmatter".to_string(),
            "again no frontmatter".to_string(),
            "---\nstatus: success\nsummary: done\n---\nrecovered".to_string(),
        ]);
        let runtime = WorkflowRuntime::with_dependencies(
            config,
            mock_runtime_dependencies(None, Some(factory.clone())),
        )
        .with_deterministic_selector();

        // Exhausted recoverable retries persist the run as Failed while keeping
        // the failing step as the current step.
        let start_err = runtime.start_run("do work").await.unwrap_err();
        assert!(
            start_err.to_string().contains("exhausted retry budget"),
            "unexpected start error: {start_err}"
        );

        let runs = runtime.list_runs(None).unwrap();
        assert_eq!(runs.len(), 1);
        let run_id = runs[0].run_id.clone();
        let failed = runtime.load_run(&run_id).unwrap();
        assert!(
            matches!(failed.status, RunStatus::Failed { .. }),
            "expected Failed run, got {:?}",
            failed.status
        );
        assert_eq!(failed.current_step, "start");

        // Resume must retry the current step and drive the run forward. Before
        // the fix, resume short-circuits on the Failed status and returns the
        // run unchanged with no events.
        let report = runtime.resume_run(&run_id).await.unwrap();

        assert_eq!(
            report.run.status,
            RunStatus::Completed,
            "resume should retry the failed current step and complete the run"
        );
        assert!(
            report.events.iter().any(|event| matches!(
                &event.kind,
                WorkflowEventKind::StepStarted { step_id } if step_id == "start"
            )),
            "resume should re-run the failed current step"
        );
        factory.assert_exhausted();
    }

    /// Build a runtime whose only workflow is a single agent step that exhausts
    /// its per-step recoverable retry budget, then hands control to a `finish`
    /// status step on `success`. The scripted `responses` drive the agent
    /// backend deterministically.
    fn agent_exhaustion_runtime(
        dir: &tempfile::TempDir,
        responses: Vec<String>,
    ) -> (WorkflowRuntime, ScriptedAgentFactory) {
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
                output = { status = { "success" }, fields = { summary = "string" } }
              }
            end
            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = "finished" }
            end
            start:on("success", finish)
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
        let factory = ScriptedAgentFactory::new(responses);
        let runtime = WorkflowRuntime::with_dependencies(
            config,
            mock_runtime_dependencies(None, Some(factory.clone())),
        )
        .with_deterministic_selector();
        (runtime, factory)
    }

    /// Drive `agent_exhaustion_runtime` until the run gives up as Failed with the
    /// failing `start` step retained as the current step, and return its id.
    async fn failed_agent_run(runtime: &WorkflowRuntime) -> String {
        let start_err = runtime.start_run("do work").await.unwrap_err();
        assert!(
            start_err.to_string().contains("exhausted retry budget"),
            "unexpected start error: {start_err}"
        );
        let runs = runtime.list_runs(None).unwrap();
        assert_eq!(runs.len(), 1);
        let run_id = runs[0].run_id.clone();
        let failed = runtime.load_run(&run_id).unwrap();
        assert!(
            matches!(failed.status, RunStatus::Failed { .. }),
            "expected Failed run, got {:?}",
            failed.status
        );
        assert_eq!(failed.current_step, "start");
        assert_eq!(failed.retries_used, 2);
        assert_eq!(failed.step_retries_used.get("start").copied(), Some(2));
        run_id
    }

    #[tokio::test]
    async fn resume_is_noop_for_completed_run() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success", body = "done" }
            end
            return workflow("aaa", start)
            "#,
        )
        .unwrap();
        let runtime = runtime_for_workflow_dir(&dir, workflow_dir);

        let started = runtime.start_run("do work").await.unwrap();
        assert_eq!(started.run.status, RunStatus::Completed);
        let before = runtime.load_run(&started.run.id).unwrap();

        let report = runtime.resume_run(&started.run.id).await.unwrap();

        assert_eq!(report.run.status, RunStatus::Completed);
        assert!(
            report.events.is_empty(),
            "resuming a completed run should emit no events"
        );
        assert_eq!(runtime.load_run(&started.run.id).unwrap(), before);
    }

    #[tokio::test]
    async fn resume_is_noop_for_cancelled_run() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_for_empty_state(&dir);
        let store = runtime.store().unwrap();
        let run = summary_test_run(
            "run-00000000-0000-0000-0000-000000000000",
            RunStatus::Cancelled,
            None,
        );
        store.save_run(&run).unwrap();
        store.update_run_head(&run.id, run_head(&run)).unwrap();

        let report = runtime.resume_run(&run.id).await.unwrap();

        assert_eq!(report.run.status, RunStatus::Cancelled);
        assert!(
            report.events.is_empty(),
            "resuming a cancelled run should emit no events"
        );
        assert_eq!(runtime.load_run(&run.id).unwrap(), run);
    }

    /// Extract the durable ask-user resume-callback `record_id` from a
    /// `WaitingForInput` status. Each fresh execution of the ask-user step mints
    /// a new record id, so this uniquely identifies the pending callback.
    fn waiting_callback_record_id(status: &RunStatus) -> String {
        match status {
            RunStatus::WaitingForInput {
                resume_callback, ..
            } => resume_callback
                .payload()
                .get("record_id")
                .and_then(|value| value.as_str())
                .expect("ask_user resume callback payload carries a record_id")
                .to_string(),
            other => panic!("expected WaitingForInput status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            r#"
            local ask = step("ask")
            ask.run = function(ctx)
              return action.ask_user { id = "approval", message = "Approve?", choices = { "yes" } }
            end
            local done = step("done")
            done.run = function(ctx) return action.status { status = "success" } end
            ask:on("answered", done)
            return workflow("aaa", ask)
            "#,
        )
        .unwrap();
        let runtime = runtime_for_workflow_dir(&dir, workflow_dir);

        let started = runtime.start_run("do work").await.unwrap();
        // The ask_user step blocks the run, retaining "ask" as the current step
        // with a durable pending resume callback.
        assert!(matches!(
            started.run.status,
            RunStatus::WaitingForInput { .. }
        ));
        assert_eq!(started.run.current_step, "ask");
        let first_callback_record = waiting_callback_record_id(&started.run.status);

        // Resume must re-execute (re-prompt) the retained ask_user step rather
        // than treating a WaitingForInput run as a no-op. Only Completed and
        // Cancelled runs are non-resumable.
        let report = runtime.resume_run(&started.run.id).await.unwrap();

        // The run is re-prompted: still WaitingForInput on the same step, and
        // the ask_user step ran again (a fresh StepStarted event was emitted).
        assert!(matches!(
            &report.run.status,
            RunStatus::WaitingForInput { step, prompt_id, .. }
                if step == "ask" && prompt_id == "approval"
        ));
        assert!(
            report.events.iter().any(|event| matches!(
                &event.kind,
                WorkflowEventKind::StepStarted { step_id } if step_id == "ask"
            )),
            "resume must re-execute the waiting ask_user step, emitting StepStarted for it"
        );

        // The durable pending callback is safely replaced by the fresh
        // execution's callback, not left dangling or duplicated.
        let reloaded = runtime.load_run(&started.run.id).unwrap();
        let second_callback_record = waiting_callback_record_id(&reloaded.status);
        assert_ne!(
            first_callback_record, second_callback_record,
            "re-executing the ask_user step must replace the durable pending resume callback"
        );

        // The freshly re-prompted run stays answerable to completion, and
        // answering routes through the replaced callback.
        let answered = runtime
            .answer_run(&started.run.id, "approval", "yes")
            .await
            .unwrap();
        assert_eq!(answered.run.status, RunStatus::Completed);
    }

    #[tokio::test]
    async fn step_run_takes_one_fresh_attempt_at_failed_current_step() {
        let dir = tempfile::tempdir().unwrap();
        // Three frontmatter-less bodies exhaust the per-step retry budget; the
        // valid fourth body is only reached by the fresh step attempt.
        let (runtime, factory) = agent_exhaustion_runtime(
            &dir,
            vec![
                "no frontmatter here".to_string(),
                "still no frontmatter".to_string(),
                "again no frontmatter".to_string(),
                "---\nstatus: success\nsummary: done\n---\nrecovered".to_string(),
            ],
        );
        let run_id = failed_agent_run(&runtime).await;

        let report = runtime.step_run(&run_id).await.unwrap();

        // The single fresh attempt succeeds and advances the run to `finish`,
        // leaving it Running (step mode executes exactly one step).
        assert_eq!(report.run.status, RunStatus::Running);
        assert_eq!(report.run.current_step, "finish");
        assert!(report.events.iter().any(|event| matches!(
            &event.kind,
            WorkflowEventKind::StepStarted { step_id } if step_id == "start"
        )));

        let loaded = runtime.load_run(&run_id).unwrap();
        assert_eq!(loaded.status, RunStatus::Running);
        assert_eq!(loaded.current_step, "finish");
        // The fresh initial attempt does not consume retry budget.
        assert_eq!(loaded.retries_used, 2);
        assert_eq!(loaded.step_retries_used.get("start").copied(), Some(2));
        factory.assert_exhausted();
    }

    #[tokio::test]
    async fn resume_refails_when_fresh_attempt_fails_with_exhausted_step_budget() {
        let dir = tempfile::tempdir().unwrap();
        // Every body lacks frontmatter, so the fresh resume attempt fails while
        // the per-step budget is already exhausted.
        let (runtime, factory) = agent_exhaustion_runtime(
            &dir,
            vec![
                "no frontmatter here".to_string(),
                "still no frontmatter".to_string(),
                "again no frontmatter".to_string(),
                "yet again no frontmatter".to_string(),
            ],
        );
        let run_id = failed_agent_run(&runtime).await;

        let resume_err = runtime.resume_run(&run_id).await.unwrap_err();
        assert!(
            resume_err.to_string().contains("exhausted retry budget"),
            "unexpected resume error: {resume_err}"
        );

        let loaded = runtime.load_run(&run_id).unwrap();
        assert!(
            matches!(loaded.status, RunStatus::Failed { .. }),
            "expected Failed run, got {:?}",
            loaded.status
        );
        assert_eq!(loaded.current_step, "start");
        // No budget was available to consume, so counters are unchanged.
        assert_eq!(loaded.retries_used, 2);
        assert_eq!(loaded.step_retries_used.get("start").copied(), Some(2));
        factory.assert_exhausted();
    }

    #[tokio::test]
    async fn resume_applies_increased_step_retry_budget_to_existing_failed_run() {
        // Regression: a run that failed with the per-step retry budget exhausted
        // (the "2/2 retries used" the user saw) must be able to benefit from a
        // raised `max_retries_per_step` after the user edits their config and
        // resumes. Today resume reuses the durable per-run config snapshot, so
        // the raised limit never reaches an already-started run and it re-fails
        // immediately at the stale 2/2 boundary.
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
                output = { status = { "success" }, fields = { summary = "string" } }
              }
            end
            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = "finished" }
            end
            start:on("success", finish)
            return workflow("aaa", start)
            "#,
        )
        .unwrap();

        let config_for = |limit: u32| RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir.clone()],
            agents: Vec::new(),
            config_sets: BTreeMap::from([(
                "default".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: limit,
                },
            )]),
        };

        // First runtime: default `max_retries_per_step = 2`. The initial attempt
        // plus both recoverable retries return frontmatter-less bodies, so the
        // run gives up as Failed at the 2/2 per-step boundary.
        let low_factory = ScriptedAgentFactory::new(vec![
            "no frontmatter here".to_string(),
            "still no frontmatter".to_string(),
            "again no frontmatter".to_string(),
        ]);
        let low = WorkflowRuntime::with_dependencies(
            config_for(2),
            mock_runtime_dependencies(None, Some(low_factory.clone())),
        )
        .with_deterministic_selector();

        let start_err = low.start_run("do work").await.unwrap_err();
        assert!(
            start_err.to_string().contains("2/2 retries used"),
            "unexpected start error: {start_err}"
        );
        low_factory.assert_exhausted();

        let runs = low.list_runs(None).unwrap();
        assert_eq!(runs.len(), 1);
        let run_id = runs[0].run_id.clone();
        let failed = low.load_run(&run_id).unwrap();
        assert!(
            matches!(failed.status, RunStatus::Failed { .. }),
            "expected Failed run, got {:?}",
            failed.status
        );
        assert_eq!(failed.step_retries_used.get("start").copied(), Some(2));

        // The user raises `max_retries_per_step` to 20 and resumes the same run.
        // The fresh attempt fails once more, but with the raised budget a single
        // retry reaches the scripted success and completes the run.
        let high_factory = ScriptedAgentFactory::new(vec![
            "no frontmatter yet".to_string(),
            "---\nstatus: success\nsummary: done\n---\nrecovered".to_string(),
        ]);
        let mut high_deps = MockRuntimeDependencies::new();
        let high_shared = SharedClientFactory::new(high_factory.clone());
        high_deps
            .expect_agent_factory()
            .returning(move |_| Ok(high_shared.clone()));
        let high = WorkflowRuntime::with_dependencies(config_for(20), Arc::new(high_deps))
            .with_deterministic_selector();

        let report = high.resume_run(&run_id).await.expect(
            "raising max_retries_per_step should let the resumed run retry instead of re-failing at the stale 2/2",
        );

        assert_eq!(
            report.run.status,
            RunStatus::Completed,
            "resumed run should apply the raised max_retries_per_step and complete"
        );
    }

    fn agent_retry_workflow(config_set_suffix: &str) -> String {
        format!(
            r#"
            local developer = role("developer", {{ instructions = "Implement" }})
            local start = step("start", {{ role = developer }})
            start.run = function(ctx)
              return action.agent {{
                role = developer,
                prompt = "Do work",
                output = {{ status = {{ "success" }}, fields = {{ summary = "string" }} }}
              }}
            end
            local finish = step("finish")
            finish.run = function(ctx)
              return action.status {{ status = "success", body = "finished" }}
            end
            start:on("success", finish)
            return workflow("aaa", start{config_set_suffix})
            "#
        )
    }

    fn single_default_set(max_retries_per_step: u32) -> BTreeMap<String, RunnerLimitsConfig> {
        BTreeMap::from([(
            "default".to_string(),
            RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                max_retries_per_run: 200,
                max_retries_per_step,
            },
        )])
    }

    fn agent_retry_config(
        dir: &tempfile::TempDir,
        workflow_dir: PathBuf,
        config_sets: BTreeMap<String, RunnerLimitsConfig>,
    ) -> RuntimeConfig {
        RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            agents: Vec::new(),
            config_sets,
        }
    }

    /// Runtime dependencies for a resume/step path that never generates a
    /// request topic (only the agent factory is exercised).
    fn agent_factory_deps(factory: ScriptedAgentFactory) -> Arc<dyn RuntimeDependencies> {
        let mut deps = MockRuntimeDependencies::new();
        let shared = SharedClientFactory::new(factory);
        deps.expect_agent_factory()
            .returning(move |_| Ok(shared.clone()));
        Arc::new(deps)
    }

    #[tokio::test]
    async fn resume_of_failed_run_advances_cumulative_retry_counters() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(workflow_dir.join("aaa.lua"), agent_retry_workflow("")).unwrap();

        let low = WorkflowRuntime::with_dependencies(
            agent_retry_config(&dir, workflow_dir.clone(), single_default_set(2)),
            mock_runtime_dependencies(
                None,
                Some(ScriptedAgentFactory::new(vec![
                    "no frontmatter here".to_string(),
                    "still no frontmatter".to_string(),
                    "again no frontmatter".to_string(),
                ])),
            ),
        )
        .with_deterministic_selector();

        let start_err = low.start_run("do work").await.unwrap_err();
        assert!(
            start_err.to_string().contains("2/2 retries used"),
            "{start_err}"
        );
        let run_id = low.list_runs(None).unwrap()[0].run_id.clone();
        let failed = low.load_run(&run_id).unwrap();
        assert_eq!(failed.step_retries_used.get("start").copied(), Some(2));
        assert_eq!(failed.retries_used, 2);

        let high = WorkflowRuntime::with_dependencies(
            agent_retry_config(&dir, workflow_dir, single_default_set(20)),
            agent_factory_deps(ScriptedAgentFactory::new(vec![
                "no frontmatter yet".to_string(),
                "---\nstatus: success\nsummary: done\n---\nrecovered".to_string(),
            ])),
        )
        .with_deterministic_selector();

        let report = high.resume_run(&run_id).await.unwrap();
        assert_eq!(report.run.status, RunStatus::Completed);
        assert_eq!(report.run.retries_used, 3);
        assert_eq!(report.run.step_retries_used.get("start"), Some(&3));
    }

    #[tokio::test]
    async fn step_run_applies_increased_step_retry_budget_to_existing_failed_run() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(workflow_dir.join("aaa.lua"), agent_retry_workflow("")).unwrap();

        let low = WorkflowRuntime::with_dependencies(
            agent_retry_config(&dir, workflow_dir.clone(), single_default_set(2)),
            mock_runtime_dependencies(
                None,
                Some(ScriptedAgentFactory::new(vec![
                    "no frontmatter here".to_string(),
                    "still no frontmatter".to_string(),
                    "again no frontmatter".to_string(),
                ])),
            ),
        )
        .with_deterministic_selector();

        low.start_run("do work").await.unwrap_err();
        let run_id = low.list_runs(None).unwrap()[0].run_id.clone();
        assert_eq!(
            low.load_run(&run_id)
                .unwrap()
                .step_retries_used
                .get("start")
                .copied(),
            Some(2)
        );

        let high = WorkflowRuntime::with_dependencies(
            agent_retry_config(&dir, workflow_dir, single_default_set(20)),
            agent_factory_deps(ScriptedAgentFactory::new(vec![
                "no frontmatter yet".to_string(),
                "---\nstatus: success\nsummary: done\n---\nrecovered".to_string(),
            ])),
        )
        .with_deterministic_selector();

        let report = high.step_run(&run_id).await.unwrap();
        assert_eq!(report.run.status, RunStatus::Running);
        assert_eq!(report.run.current_step, "finish");
        assert_eq!(report.run.step_retries_used.get("start"), Some(&3));
    }

    #[tokio::test]
    async fn resume_of_failed_run_applies_lowered_whole_set_limits() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(workflow_dir.join("aaa.lua"), agent_retry_workflow("")).unwrap();

        // Fail at the per-step ceiling under `max_retries_per_step = 5`: initial
        // attempt plus five recoverable retries all return frontmatter-less bodies.
        let high = WorkflowRuntime::with_dependencies(
            agent_retry_config(&dir, workflow_dir.clone(), single_default_set(5)),
            mock_runtime_dependencies(
                None,
                Some(ScriptedAgentFactory::new(vec![
                    "no frontmatter 1".to_string(),
                    "no frontmatter 2".to_string(),
                    "no frontmatter 3".to_string(),
                    "no frontmatter 4".to_string(),
                    "no frontmatter 5".to_string(),
                    "no frontmatter 6".to_string(),
                ])),
            ),
        )
        .with_deterministic_selector();

        high.start_run("do work").await.unwrap_err();
        let run_id = high.list_runs(None).unwrap()[0].run_id.clone();
        assert_eq!(
            high.load_run(&run_id)
                .unwrap()
                .step_retries_used
                .get("start")
                .copied(),
            Some(5)
        );

        // Resume through a runtime whose set lowers `max_retries_per_step` to 3.
        let low = WorkflowRuntime::with_dependencies(
            agent_retry_config(&dir, workflow_dir, single_default_set(3)),
            agent_factory_deps(ScriptedAgentFactory::new(vec![
                "still no frontmatter".to_string(),
            ])),
        )
        .with_deterministic_selector();

        let err = low.resume_run(&run_id).await.unwrap_err();
        assert!(err.to_string().contains("5/3 retries used"), "{err}");
        let reloaded = low.load_run(&run_id).unwrap();
        assert_eq!(reloaded.retries_used, 5);
        assert_eq!(reloaded.step_retries_used.get("start").copied(), Some(5));
    }

    #[tokio::test]
    async fn resume_of_failed_run_uses_default_limits_when_selected_set_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("aaa.lua"),
            agent_retry_workflow(", { config_set = \"careful\" }"),
        )
        .unwrap();

        let with_careful = BTreeMap::from([
            ("default".to_string(), RunnerLimitsConfig::default()),
            (
                "careful".to_string(),
                RunnerLimitsConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    max_retries_per_run: 200,
                    max_retries_per_step: 2,
                },
            ),
        ]);
        let low = WorkflowRuntime::with_dependencies(
            agent_retry_config(&dir, workflow_dir.clone(), with_careful),
            mock_runtime_dependencies(
                None,
                Some(ScriptedAgentFactory::new(vec![
                    "no frontmatter here".to_string(),
                    "still no frontmatter".to_string(),
                    "again no frontmatter".to_string(),
                ])),
            ),
        )
        .with_deterministic_selector();

        let start_err = low.start_run("do work").await.unwrap_err();
        assert!(
            start_err.to_string().contains("2/2 retries used"),
            "{start_err}"
        );
        let run_id = low.list_runs(None).unwrap()[0].run_id.clone();
        assert_eq!(
            low.load_run(&run_id)
                .unwrap()
                .step_retries_used
                .get("start")
                .copied(),
            Some(2)
        );

        // Resume through a runtime that DELETES `careful` but keeps a widened
        // `default`. Live resolution falls back to `default` limit 20.
        let high = WorkflowRuntime::with_dependencies(
            agent_retry_config(&dir, workflow_dir, single_default_set(20)),
            agent_factory_deps(ScriptedAgentFactory::new(vec![
                "no frontmatter yet".to_string(),
                "---\nstatus: success\nsummary: done\n---\nrecovered".to_string(),
            ])),
        )
        .with_deterministic_selector();

        let report = high.resume_run(&run_id).await.unwrap();
        assert_eq!(report.run.status, RunStatus::Completed);
        // The durable pointer is unchanged even though `careful` no longer exists.
        assert_eq!(report.run.config_set.name, "careful");
        assert_eq!(report.run.step_retries_used.get("start"), Some(&3));
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
            .list_runs(None)
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
    async fn answer_run_restores_persisted_request_topic_before_resumed_events() {
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
        assert_eq!(
            first_run_started_topic(&report),
            Some("Initial prompt topic")
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
                .contains("changing each completed `- [ ] TODO-NN` item to `- [x] TODO-NN`")
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
                        "implementation_commands": [],
                        "implementation_evidence": [],
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
                        "implementation_commands": [],
                        "implementation_evidence": [],
                        "tester_commands": [],
                        "tester_evidence": [],
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
                .contains("complete Pass 1 for every required `TODO-NN` in plan order")
        );
        assert!(
            review_action
                .prompt
                .contains("An unchecked required TODO must remain visible")
        );
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
                    "fields": {
                        "feedback": "Fix one TODO",
                        "plan_doc": plan_doc,
                        "implementation_commands": [],
                        "implementation_evidence": [],
                    },
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
implementation_commands: []
implementation_evidence: []
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
implementation_commands: []
implementation_evidence: []
tester_commands: []
tester_evidence: []
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
implementation_commands: []
implementation_evidence: []
tester_commands: []
tester_evidence: []
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
implementation_commands: []
implementation_evidence: []
tester_commands: []
tester_evidence: []
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
implementation_commands: []
implementation_evidence: []
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
implementation_commands: []
implementation_evidence: []
tester_commands: []
tester_evidence: []
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
implementation_commands: []
implementation_evidence: []
tester_commands: []
tester_evidence: []
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
status: user_required
blocker_reason: The commit backend requires credentials that are unavailable to the agent
blocker_resolution: Restore the commit backend credentials
blocker_statement: Commit blocked
blocked_from_step: commit
blocked_from_status: blocked
user_feedback:
  - "Result confirmation: The TUI help still omits the flag"
goal: Preserve reviewer feedback context
validation: cargo test -p cowboy-workflow-engine
plan_doc: docs/plans/example.md
work_dir: docs/plans/example
rca_doc: docs/plans/example/rca.md
repro_test: crates/workflow/engine/src/runtime.rs::workflow_runtime_preserves_result_feedback_through_commit_recovery
---
Commit requires user action"#
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
implementation_commands: []
implementation_evidence: []
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
implementation_commands: []
implementation_evidence: []
tester_commands: []
tester_evidence: []
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
implementation_commands: []
implementation_evidence: []
tester_commands: []
tester_evidence: []
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
        let persisted_blocker_review = command_output_record(&runtime, &report);
        assert_eq!(persisted_blocker_review.step, "review_blocker");
        let commit_output = persisted_blocker_review.output.unwrap();
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
            .answer_run(&run_id, &prompt_id, "/route implement")
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
        assert_eq!(prompts.len(), 14);
        let result_review_prompt = &prompts[8];
        assert!(
            result_review_prompt.contains(&format!("- Result confirmation: {result_feedback}"))
        );
        assert!(result_review_prompt.contains("Tester command records: array(empty)"));
        assert!(result_review_prompt.contains("Plan doc: docs/plans/example.md"));
        assert!(result_review_prompt.contains("Goal: Preserve reviewer feedback context"));
        assert!(result_review_prompt.contains("Validation: cargo test -p cowboy-workflow-engine"));

        let recovery_review_prompt = &prompts[13];
        assert!(
            recovery_review_prompt.contains(&format!("- Result confirmation: {result_feedback}"))
        );
        assert!(recovery_review_prompt.contains("Tester command records: array(empty)"));
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
        assert!(
            action
                .message
                .contains("The blocker reviewer determined that user action is required.")
        );
        assert!(action.message.contains("feature workflow blocked"));
        assert!(action.message.contains("Required user action:"));
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
                        "blocked_response": "/route plan",
                        "blocked_from_step": "implement"
                    },
                },
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("expected explicit route to return to planning")
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
                model: Some(ModelInfo::default()),
                watchdog: AgentWatchdogRuntimeConfig::default(),
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
                model: Some(ModelInfo::default()),
                watchdog: AgentWatchdogRuntimeConfig::default(),
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
        assert!(runtime.list_runs(None).unwrap().is_empty());
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
                    descriptor: Some("gpt-5.6-sol-1m-high".to_string()),
                }),
                WorkflowEventKind::AgentSessionReady {
                    step_id: "implement".to_string(),
                    role: "developer".to_string(),
                    session_id: "session-1".to_string(),
                    descriptor: Some("gpt-5.6-sol-1m-high".to_string()),
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
