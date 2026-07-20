use std::env;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cowboy_agent_acp::Client as AcpClient;
use cowboy_agent_acp::transport::{StdioConfig, TransportConfig};
use cowboy_agent_client::ModelInfo;
use cowboy_workflow_agent::{
    AgentExecutionConfig, AgentExecutor, ClientFactory, ResolvedAgentClient,
};
use cowboy_workflow_core::{
    AbortAgentPromptWindowOutcome, AgentAction, AgentPromptWindow, AppendUserPromptOutcome,
    CompareAndSealPromptWindowOutcome, ObjectHash, ObjectKind, OpenAgentPromptWindowOutcome,
    RoleDefinition, RoleSession, RunHead, RunId, RunStatus, RunStore, RunUserPrompt, TurnRecord,
    WorkflowRun,
};
use cowboy_workflow_store::{Error as StoreError, RedbRunStore};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = parse_args()?;
    let store = if config.db.exists() {
        RedbRunStore::open(&config.db)?
    } else {
        RedbRunStore::create(&config.db)?
    };
    let run_state = ensure_standalone_run(&store, &config)?;
    let user_prompts = store.load_user_prompts(&run_state.id)?;
    let action = AgentAction {
        role: config.role.clone(),
        prompt: config.prompt.clone(),
        output: None,
    };
    let context = execution_context(&config, &run_state, user_prompts);
    let factory = AcpFactory {
        transport: TransportConfig::Stdio(StdioConfig {
            command: config.command,
            args: config.args,
            env: Vec::new(),
        }),
    };
    let executor = AgentExecutor::new(
        factory,
        StoreWithSessions { inner: store },
        AgentExecutionConfig {
            cwd: config.cwd,
            ..AgentExecutionConfig::default()
        },
    );
    let execution = executor.execute_agent(action, context).await?;

    println!(
        "record:\n{}",
        serde_json::to_string_pretty(&execution.record)?
    );
    println!(
        "turns:\n{}",
        serde_json::to_string_pretty(&execution.turns)?
    );
    Ok(())
}

fn execution_context(
    config: &Args,
    run: &WorkflowRun,
    user_prompts: Vec<RunUserPrompt>,
) -> cowboy_workflow_core::ExecutionContext {
    cowboy_workflow_core::ExecutionContext {
        run_id: run.id.clone(),
        step_id: config.step_id.clone(),
        step_record_id: config.record_id.clone(),
        prev: run.head.clone(),
        role: Some(RoleDefinition {
            id: config.role.clone(),
            instructions: String::new(),
            agent: None,
            properties: serde_json::Value::Null,
        }),
        attempt: 1,
        retry_reason: None,
        original_request: run.original_request.clone(),
        run_created_at: run.created_at,
        user_prompts,
    }
}

fn ensure_standalone_run(store: &RedbRunStore, config: &Args) -> Result<WorkflowRun, StoreError> {
    match store.load_run(&config.run_id) {
        Ok(run) => Ok(run),
        Err(StoreError::RunNotFound(_)) => {
            let now = Utc::now();
            let run = WorkflowRun {
                id: config.run_id.clone(),
                workflow_name: "execute-agent".to_string(),
                workflow_api_version: 1,
                workflow_hash: "execute-agent".to_string(),
                workflow_sources: Default::default(),
                original_request: config.prompt.clone(),
                request_topic: None,
                config_set: Default::default(),
                status: RunStatus::Running,
                current_step: config.step_id.clone(),
                head: None,
                resume: serde_json::Value::Null,
                retries_used: 0,
                step_retries_used: Default::default(),
                steps_executed: 0,
                step_visits: Default::default(),
                active_duration_ms: 0,
                created_at: now,
                updated_at: now,
            };
            store.save_run(&run)?;
            let has_head = store.list_runs()?.iter().any(|head| head.run_id == run.id);
            if !has_head {
                store.update_run_head(
                    &run.id,
                    RunHead::from_run(&run),
                )?;
            }
            Ok(run)
        }
        Err(err) => Err(err),
    }
}

#[derive(Debug)]
struct Args {
    db: PathBuf,
    command: String,
    args: Vec<String>,
    run_id: RunId,
    step_id: String,
    record_id: String,
    role: String,
    prompt: String,
    cwd: String,
}

fn parse_args() -> Result<Args, Box<dyn std::error::Error>> {
    let mut db = None;
    let mut command = None;
    let mut command_args = Vec::new();
    let mut run_id = "run-1".to_string();
    let mut step_id = "step-1".to_string();
    let mut record_id = "record-1".to_string();
    let mut role = "agent".to_string();
    let mut prompt = None;
    let mut cwd = ".".to_string();

    let mut args = env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db = args.next().map(PathBuf::from),
            "--cmd" => command = args.next(),
            "--arg" => {
                let Some(value) = args.next() else { usage() };
                command_args.push(value);
            }
            "--run" => run_id = args.next().unwrap_or_else(|| usage()),
            "--step" => step_id = args.next().unwrap_or_else(|| usage()),
            "--record" => record_id = args.next().unwrap_or_else(|| usage()),
            "--role" => role = args.next().unwrap_or_else(|| usage()),
            "--prompt" => prompt = args.next(),
            "--cwd" => cwd = args.next().unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }

    Ok(Args {
        db: db.unwrap_or_else(|| usage()),
        command: command.unwrap_or_else(|| usage()),
        args: command_args,
        run_id,
        step_id,
        record_id,
        role,
        prompt: prompt.unwrap_or_else(|| usage()),
        cwd,
    })
}

fn usage() -> ! {
    eprintln!(
        "usage: execute-agent --db <store.redb> --cmd <agent-cmd> [--arg <arg> ...] --prompt <text> [--run <id>] [--step <id>] [--record <id>] [--role <id>] [--cwd <path>]"
    );
    std::process::exit(2)
}

#[derive(Clone)]
struct AcpFactory {
    transport: TransportConfig,
}

#[async_trait]
impl ClientFactory for AcpFactory {
    async fn create_client(
        &self,
        _role: &RoleDefinition,
    ) -> cowboy_workflow_agent::Result<ResolvedAgentClient> {
        Ok(ResolvedAgentClient {
            client: Box::new(AcpClient::connect(self.transport.clone()).await?),
            model: ModelInfo::default(),
            backend: "acp".to_string(),
        })
    }
}

struct StoreWithSessions {
    inner: RedbRunStore,
}

impl RunStore for StoreWithSessions {
    fn save_run(&self, run: &WorkflowRun) -> cowboy_workflow_core::Result<()> {
        self.inner.save_run(run).map_err(Into::into)
    }

    fn load_run(&self, run_id: &RunId) -> cowboy_workflow_core::Result<WorkflowRun> {
        self.inner.load_run(run_id).map_err(Into::into)
    }

    fn list_runs(&self) -> cowboy_workflow_core::Result<Vec<RunHead>> {
        self.inner.list_runs().map_err(Into::into)
    }

    fn put_object<T: serde::Serialize>(
        &self,
        kind: ObjectKind,
        value: &T,
    ) -> cowboy_workflow_core::Result<ObjectHash> {
        self.inner.put_object(kind, value).map_err(Into::into)
    }

    fn get_object<T: serde::de::DeserializeOwned>(
        &self,
        hash: &ObjectHash,
    ) -> cowboy_workflow_core::Result<T> {
        self.inner.get_object(hash).map_err(Into::into)
    }

    fn update_run_head(&self, run_id: &str, head: RunHead) -> cowboy_workflow_core::Result<()> {
        self.inner.update_run_head(run_id, head).map_err(Into::into)
    }

    fn load_run_head(&self, run_id: &str) -> cowboy_workflow_core::Result<RunHead> {
        self.inner.load_run_head(run_id).map_err(Into::into)
    }

    fn save_role_session(&self, session: RoleSession) -> cowboy_workflow_core::Result<()> {
        self.inner.save_role_session(session).map_err(Into::into)
    }

    fn load_role_session(
        &self,
        run_id: &str,
        role_id: &str,
    ) -> cowboy_workflow_core::Result<Option<RoleSession>> {
        self.inner
            .load_role_session(run_id, role_id)
            .map_err(Into::into)
    }

    fn delete_role_sessions(&self, run_id: &str) -> cowboy_workflow_core::Result<()> {
        self.inner.delete_role_sessions(run_id).map_err(Into::into)
    }

    fn append_turn(
        &self,
        run_id: &str,
        turn: TurnRecord,
    ) -> cowboy_workflow_core::Result<ObjectHash> {
        self.inner.append_turn(run_id, turn).map_err(Into::into)
    }

    fn load_user_prompts(&self, run_id: &str) -> cowboy_workflow_core::Result<Vec<RunUserPrompt>> {
        self.inner.load_user_prompts(run_id).map_err(Into::into)
    }

    fn open_agent_prompt_window(
        &self,
        window: AgentPromptWindow,
    ) -> cowboy_workflow_core::Result<OpenAgentPromptWindowOutcome> {
        self.inner
            .open_agent_prompt_window(window)
            .map_err(Into::into)
    }

    fn append_user_prompt(
        &self,
        run_id: &str,
        window_id: &str,
        content: String,
    ) -> cowboy_workflow_core::Result<AppendUserPromptOutcome> {
        self.inner
            .append_user_prompt(run_id, window_id, content)
            .map_err(Into::into)
    }

    fn compare_and_seal_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        applied_sequence: u64,
        sealed_at: DateTime<Utc>,
    ) -> cowboy_workflow_core::Result<CompareAndSealPromptWindowOutcome> {
        self.inner
            .compare_and_seal_agent_prompt_window(run_id, window_id, applied_sequence, sealed_at)
            .map_err(Into::into)
    }

    fn abort_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        aborted_at: DateTime<Utc>,
    ) -> cowboy_workflow_core::Result<AbortAgentPromptWindowOutcome> {
        self.inner
            .abort_agent_prompt_window(run_id, window_id, aborted_at)
            .map_err(Into::into)
    }

    fn clear_agent_prompt_window(
        &self,
        run_id: &str,
    ) -> cowboy_workflow_core::Result<Option<AgentPromptWindow>> {
        self.inner
            .clear_agent_prompt_window(run_id)
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_database_supports_prompt_windows_without_overwriting_existing_run() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("workflow.redb");
        let store = RedbRunStore::create(&db).unwrap();
        let config = Args {
            db,
            command: "unused".to_string(),
            args: Vec::new(),
            run_id: "diagnostic-run".to_string(),
            step_id: "diagnostic-step".to_string(),
            record_id: "diagnostic-record".to_string(),
            role: "agent".to_string(),
            prompt: "diagnose this".to_string(),
            cwd: ".".to_string(),
        };

        let seeded = ensure_standalone_run(&store, &config).unwrap();
        assert_eq!(seeded.status, RunStatus::Running);
        assert!(
            store
                .list_runs()
                .unwrap()
                .iter()
                .any(|head| { head.run_id == config.run_id && head.status == RunStatus::Running })
        );
        assert!(matches!(
            store
                .open_agent_prompt_window(AgentPromptWindow {
                    window_id: "window".to_string(),
                    run_id: config.run_id.clone(),
                    step_record_id: config.record_id.clone(),
                    step_id: config.step_id.clone(),
                    role_id: config.role.clone(),
                    baseline_sequence: 0,
                    applied_sequence: 0,
                    opened_at: Utc::now(),
                    sealed_at: None,
                })
                .unwrap(),
            OpenAgentPromptWindowOutcome::Opened(_)
        ));

        let mut existing = store.load_run(&config.run_id).unwrap();
        existing.status = RunStatus::Completed;
        store.save_run(&existing).unwrap();
        assert_eq!(ensure_standalone_run(&store, &config).unwrap(), existing);
    }

    #[test]
    fn existing_run_context_uses_durable_request_timestamp_and_follow_ups() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("workflow.redb");
        let store = RedbRunStore::create(&db).unwrap();
        let config = Args {
            db,
            command: "unused".to_string(),
            args: Vec::new(),
            run_id: "existing-run".to_string(),
            step_id: "diagnostic-step".to_string(),
            record_id: "diagnostic-record".to_string(),
            role: "agent".to_string(),
            prompt: "new CLI prompt".to_string(),
            cwd: ".".to_string(),
        };
        let mut existing = ensure_standalone_run(&store, &config).unwrap();
        existing.original_request = "durable original request".to_string();
        existing.created_at = Utc::now() - chrono::Duration::hours(1);
        store.save_run(&existing).unwrap();
        store
            .open_agent_prompt_window(AgentPromptWindow {
                window_id: "accept-follow-up".to_string(),
                run_id: existing.id.clone(),
                step_record_id: config.record_id.clone(),
                step_id: config.step_id.clone(),
                role_id: config.role.clone(),
                baseline_sequence: 0,
                applied_sequence: 0,
                opened_at: Utc::now(),
                sealed_at: None,
            })
            .unwrap();
        let accepted = store
            .append_user_prompt(
                &existing.id,
                "accept-follow-up",
                "durable follow-up".to_string(),
            )
            .unwrap();
        assert!(matches!(accepted, AppendUserPromptOutcome::Accepted(_)));
        store.clear_agent_prompt_window(&existing.id).unwrap();

        let loaded = ensure_standalone_run(&store, &config).unwrap();
        let prompts = store.load_user_prompts(&loaded.id).unwrap();
        let context = execution_context(&config, &loaded, prompts.clone());
        assert_eq!(context.original_request, existing.original_request);
        assert_eq!(context.run_created_at, existing.created_at);
        assert_eq!(context.user_prompts, prompts);
        assert_eq!(context.user_prompts[0].content, "durable follow-up");

        let opened = store
            .open_agent_prompt_window(AgentPromptWindow {
                window_id: "execute-existing".to_string(),
                run_id: context.run_id.clone(),
                step_record_id: context.step_record_id.clone(),
                step_id: context.step_id.clone(),
                role_id: config.role.clone(),
                baseline_sequence: 0,
                applied_sequence: 0,
                opened_at: Utc::now(),
                sealed_at: None,
            })
            .unwrap();
        assert!(matches!(
            opened,
            OpenAgentPromptWindowOutcome::Opened(window)
                if window.baseline_sequence == context.user_prompts[0].sequence
        ));
    }
}
