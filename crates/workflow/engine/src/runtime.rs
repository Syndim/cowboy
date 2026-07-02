use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use cowboy_agent_acp::Client as AcpClient;
use cowboy_agent_acp::transport::{StdioConfig, TransportConfig};
use cowboy_agent_client::{Client, ModelInfo};
use cowboy_workflow_agent::{
    AgentExecutionConfig, AgentExecutor, AgentProgress, AgentProgressKind, ClientFactory,
};
use cowboy_workflow_catalog::{
    AppliedWorkflowImprovement, WorkflowCatalogLoader, apply_improvement, load_source_ref,
};
use cowboy_workflow_core::{
    ObjectKind, Result, RunHead, RunStatus, RunnerLimits, WorkflowCatalog, WorkflowError,
    WorkflowRun, WorkflowSelector, WorkflowSourceRef, WorkflowSourceSnapshot, WorkflowSummarizer,
};
use cowboy_workflow_store::RedbRunStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::workflow::DeterministicSelector;
use crate::{
    EventBus, InputRouter, LuaStepActionProvider, WorkflowEvent, WorkflowEventKind, WorkflowRunner,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub cwd: PathBuf,
    pub state_dir: PathBuf,
    pub workflow_store: PathBuf,
    #[serde(default)]
    pub workflow_dirs: Vec<PathBuf>,
    pub agent: AgentRuntimeConfig,
    pub limits: RunnerLimitsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRuntimeConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub model: ModelInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerLimitsConfig {
    pub max_steps_per_run: u32,
    pub max_visits_per_step: u32,
}

impl AgentRuntimeConfig {
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        model_id: impl Into<String>,
        provider: Option<String>,
    ) -> Self {
        Self {
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
        agent: AgentRuntimeConfig,
        limits: RunnerLimitsConfig,
    ) -> Self {
        Self {
            cwd,
            state_dir,
            workflow_store,
            workflow_dirs,
            agent,
            limits,
        }
    }
}

impl From<RunnerLimitsConfig> for RunnerLimits {
    fn from(value: RunnerLimitsConfig) -> Self {
        Self {
            max_steps_per_run: value.max_steps_per_run,
            max_visits_per_step: value.max_visits_per_step,
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

#[derive(Clone)]
pub struct WorkflowRuntime {
    config: RuntimeConfig,
    events: Arc<EventBus>,
    store: Arc<Mutex<Option<RedbRunStore>>>,
    selector: SelectorMode,
}

/// How far [`WorkflowRuntime`] drives a run in a single call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Execute steps until the run blocks, suspends, fails, or completes.
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
            config,
            events: Arc::new(EventBus::default()),
            store: Arc::new(Mutex::new(None)),
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
        let store = self.open_store()?;
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
        Ok(self.open_store()?.load_run(&run_id.to_string())?)
    }

    pub async fn start_run(&self, request: impl Into<String>) -> Result<RunReport> {
        self.start_with(request, RunMode::UntilBlocked).await
    }

    /// Start a new run and execute exactly one workflow step, leaving the run
    /// ready to be advanced with [`step_run`].
    pub async fn start_run_stepwise(&self, request: impl Into<String>) -> Result<RunReport> {
        self.start_with(request, RunMode::SingleStep).await
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
        let source_ref = catalog
            .workflows
            .get(&selection.workflow_id)
            .ok_or_else(|| WorkflowError::InvalidAction("selected workflow missing".to_string()))?;
        let (definition, snapshot, workflow_hash) = self.compile_source(source_ref)?;
        tracing::debug!(
            workflow_id = %selection.workflow_id,
            confidence = selection.confidence,
            source_entry = %source_ref.entry,
            source_root = ?source_ref.root,
            workflow_hash = %workflow_hash,
            "workflow source compiled"
        );
        let now = Utc::now();
        let run_id = format!("run-{}", Uuid::new_v4());
        let run = WorkflowRun {
            id: run_id.clone(),
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
        let store = self.open_store()?;
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
                let client = AcpClient::connect(self.agent_factory().transport)
                    .await
                    .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
                let selector = crate::AgentWorkflowSelector::new(
                    client,
                    self.config.cwd.to_string_lossy().to_string(),
                    self.config.agent.model.clone(),
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
        let store = self.open_store()?;
        let mut run = store.load_run(&run_id.to_string())?;
        InputRouter::new().answer(&mut run, prompt_id, answer)?;
        store.save_run(&run)?;
        store.update_run_head(&run.id, run_head(&run))?;
        tracing::debug!(run_id = %run.id, prompt_id, status = ?run.status, "workflow prompt answer persisted");
        self.resume_run(run_id).await
    }

    pub async fn improve_run(&self, run_id: &str) -> Result<AppliedWorkflowImprovement> {
        tracing::info!(run_id, "improving workflow from run");
        let run = self.load_run(run_id)?;
        let client = AcpClient::connect(self.agent_factory().transport)
            .await
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        let summarizer = crate::AgentWorkflowSummarizer::new(
            client,
            self.config.cwd.to_string_lossy().to_string(),
            self.config.agent.model.clone(),
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
        let store = self.open_store()?;
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
        tracing::debug!(
            run_id = %run.id,
            workflow = %definition.name,
            mode = ?mode,
            current_step = %run.current_step,
            steps_executed = run.steps_executed,
            "running workflow"
        );
        let run_id = run.id.clone();
        let mut rx = self.events.subscribe();
        let store = self.open_store()?;
        let agent_store = store.clone();
        let progress_events = self.events.clone();
        let agent_config = AgentExecutionConfig {
            cwd: self.config.cwd.to_string_lossy().to_string(),
            mcp_servers: Vec::new(),
            model: self.config.agent.model.clone(),
            backend: "acp".to_string(),
            progress: Some(Arc::new(move |progress| {
                progress_events.emit(WorkflowEvent::new(
                    progress.run_id.clone(),
                    Self::workflow_event_from_agent_progress(progress),
                ));
            })),
        };
        let executor = AgentExecutor::new(self.agent_factory(), agent_store, agent_config);
        let provider = LuaStepActionProvider::new(snapshot);
        let runner = WorkflowRunner::new(store, executor, provider, self.events.clone())
            .with_limits(self.config.limits.into());
        let run_future = async {
            match mode {
                RunMode::UntilBlocked => runner.run_until_blocked(&definition, run).await,
                RunMode::SingleStep => runner.step_once(&definition, run).await,
            }
        };
        tokio::pin!(run_future);
        let mut events = Vec::new();
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
        tracing::debug!(run_id = %run.id, event_count = events.len(), "workflow events collected");
        self.persist_events(&run.id, &events)?;
        Ok(RunReport { run, events })
    }

    fn workflow_event_from_agent_progress(progress: AgentProgress) -> WorkflowEventKind {
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

    fn agent_factory(&self) -> AcpClientFactory {
        tracing::debug!(
            command = %self.config.agent.command,
            args = ?self.config.agent.args,
            model_id = %self.config.agent.model.id,
            provider = ?self.config.agent.model.provider,
            "ACP client factory configured"
        );
        AcpClientFactory {
            transport: TransportConfig::Stdio(StdioConfig {
                command: self.config.agent.command.clone(),
                args: self.config.agent.args.clone(),
                env: Vec::new(),
            }),
        }
    }

    fn open_store(&self) -> Result<RedbRunStore> {
        let mut cached = self
            .store
            .lock()
            .map_err(|_| WorkflowError::InvalidAction("store cache lock poisoned".to_string()))?;
        if let Some(store) = cached.as_ref() {
            return Ok(store.clone());
        }
        if let Some(parent) = self.config.workflow_store.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        }
        tracing::debug!(path = %self.config.workflow_store.display(), "opening workflow store");
        let store = if self.config.workflow_store.exists() {
            RedbRunStore::open(&self.config.workflow_store)
        } else {
            RedbRunStore::create(&self.config.workflow_store)
        }
        .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        *cached = Some(store.clone());
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
    transport: TransportConfig,
}

#[async_trait]
impl ClientFactory for AcpClientFactory {
    async fn create_client(
        &self,
        _role_id: &str,
    ) -> cowboy_workflow_agent::Result<Box<dyn Client>> {
        Ok(Box::new(AcpClient::connect(self.transport.clone()).await?))
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

    #[tokio::test]
    async fn starts_builtin_workflow_until_agent_call_attempts_backend() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = WorkflowRuntime::new(RuntimeConfig {
            cwd: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            agent: AgentRuntimeConfig {
                command: "definitely-missing-agent-command".to_string(),
                args: Vec::new(),
                model: ModelInfo::default(),
            },
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
            },
        })
        .with_deterministic_selector();

        let err = runtime.start_run("do it").await.unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidAction(_)));
        assert_eq!(runtime.list_runs().unwrap().len(), 1);
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
            agent: AgentRuntimeConfig {
                command: "unused-agent".to_string(),
                args: Vec::new(),
                model: ModelInfo::default(),
            },
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
            },
        })
        .with_deterministic_selector();

        let report = runtime.start_run("request").await.unwrap();

        assert_eq!(report.run.workflow_name, "aaa");
        assert_eq!(report.run.status, RunStatus::Completed);
        assert_eq!(runtime.list_runs().unwrap().len(), 1);
        assert!(!runtime.load_events(&report.run.id).unwrap().is_empty());
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
            definition.steps["unclear"].transitions.by_status["clarified"],
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
            "unclear",
            serde_json::json!({
                "steps_executed": 3,
                "resume": { "clarification_2": "Add a status command" },
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
                    "resume": { "clarification_2": "Add a status command" },
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
            "confirm_plan",
            serde_json::json!({
                "steps_executed": 6,
                "resume": { "plan_confirmation_5": "yes" },
                "prev": {
                    "step": "review_plan",
                    "status": "approved",
                    "fields": { "plan": reviewed_plan, "plan_doc": plan_doc },
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
            "confirm_result",
            serde_json::json!({
                "steps_executed": 9,
                "resume": { "result_confirmation_8": "fix one more thing" },
                "prev": {
                    "step": "review",
                    "status": "approved",
                    "fields": { "plan_doc": plan_doc },
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
            "blocked",
            serde_json::json!({
                "steps_executed": 11,
                "resume": { "blocked_10": blocked_response },
                "prev": {
                    "step": "implement",
                    "status": "blocked",
                    "fields": { "summary": "Need credentials", "plan_doc": "docs/plans/example.md" },
                    "body": "Cannot continue without access",
                },
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("expected blocked answer to be recorded")
        };
        assert_eq!(action.status, "answered");
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
                    "status": "answered",
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
            agent: AgentRuntimeConfig {
                command: "unused-agent".to_string(),
                args: Vec::new(),
                model: ModelInfo::default(),
            },
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
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
            agent: AgentRuntimeConfig {
                command: "agent".to_string(),
                args: Vec::new(),
                model: ModelInfo::default(),
            },
            limits: RunnerLimitsConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
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
            let mapped = WorkflowRuntime::workflow_event_from_agent_progress(progress);
            assert_eq!(mapped, expected);
            assert!(
                !matches!(mapped, WorkflowEventKind::StepProgress { .. }),
                "typed agent progress must not map to generic step progress"
            );
        }
    }
}
