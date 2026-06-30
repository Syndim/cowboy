use std::env;
use std::path::PathBuf;

use async_trait::async_trait;
use cowboy_agent_acp::Client as AcpClient;
use cowboy_agent_acp::transport::{StdioConfig, TransportConfig};
use cowboy_agent_client::Client;
use cowboy_workflow_agent::{AgentExecutionConfig, AgentExecutor, ClientFactory};
use cowboy_workflow_core::{
    AgentAction, ObjectHash, ObjectKind, RoleSession, RunHead, RunId, RunStore, TurnRecord,
    WorkflowRun,
};
use cowboy_workflow_store::RedbRunStore;

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
            backend: "acp".to_string(),
            ..AgentExecutionConfig::default()
        },
    );
    let execution = executor
        .execute_agent(
            AgentAction {
                role: config.role,
                prompt: config.prompt,
                output: None,
            },
            cowboy_workflow_core::ExecutionContext {
                run_id: config.run_id,
                step_id: config.step_id,
                step_record_id: config.record_id,
                prev: None,
            },
        )
        .await?;

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
        _role_id: &str,
    ) -> cowboy_workflow_agent::Result<Box<dyn Client>> {
        Ok(Box::new(AcpClient::connect(self.transport.clone()).await?))
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
}
