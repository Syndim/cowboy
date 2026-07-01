use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

fn default_config_path() -> PathBuf {
    cowboy::default_config_path()
}

/// Cowboy workflow terminal UI.
#[derive(Debug, Parser)]
#[command(
    name = "cowboy",
    version,
    about = "Workflow-first AI agent orchestrator"
)]
struct Cli {
    /// Path to config file.
    #[arg(short, long, default_value_os_t = default_config_path(), global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Launch the interactive terminal UI.
    #[command(alias = "start")]
    Tui,
    /// Start a new workflow run. With --step, execute only the first step.
    Run {
        /// Execute only the first workflow step instead of running until blocked.
        #[arg(long)]
        step: bool,
        request: Vec<String>,
    },
    /// Execute exactly one further step of an existing workflow run.
    Step { run_id: String },
    /// Answer a workflow input prompt and continue the run.
    Answer {
        run_id: String,
        prompt_id: String,
        answer: String,
    },
    /// Summarize a completed run and apply proposed workflow file updates.
    Improve { run_id: String },
    /// List workflow runs.
    Runs,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run_main().await {
        tracing::error!(error = ?err, "cowboy exited with error");
        eprintln!("cowboy error: {err:?}");
        std::process::exit(1);
    }
}

async fn run_main() -> Result<()> {
    let cli = Cli::parse();
    let config = cowboy::load_config(&cli.config)?;
    if let Ok(log_path) = cowboy_log::init_file_logging(
        config.state_dir.join("logs"),
        "cowboy",
        cowboy_log::DEFAULT_DIRECTIVE,
    ) {
        tracing::info!(
            config_path = %cli.config.display(),
            log_path = %log_path.display(),
            state_dir = %config.state_dir.display(),
            workflow_store = %config.workflow_store.display(),
            workflow_dirs = ?config.workflow_dirs,
            agent_command = %config.agent.command,
            agent_args = ?config.agent.args,
            model_id = %config.agent.model.id,
            provider = ?config.agent.model.provider,
            "cowboy logging initialized"
        );
        cowboy_log::install_panic_hook();
    }
    let cwd = std::env::current_dir()?;

    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => cowboy::run_tui(config).await,
        Command::Run { step, request } => {
            let runtime = cowboy_workflow_engine::WorkflowRuntime::new(config.runtime_config(cwd));
            let request = request.join(" ");
            let report = if step {
                runtime.start_run_stepwise(request).await?
            } else {
                runtime.start_run(request).await?
            };
            print_report(&report);
            Ok(())
        }
        Command::Step { run_id } => {
            let runtime = cowboy_workflow_engine::WorkflowRuntime::new(config.runtime_config(cwd));
            let report = runtime.step_run(&run_id).await?;
            print_report(&report);
            Ok(())
        }
        Command::Answer {
            run_id,
            prompt_id,
            answer,
        } => {
            let runtime = cowboy_workflow_engine::WorkflowRuntime::new(config.runtime_config(cwd));
            let report = runtime.answer_run(&run_id, &prompt_id, &answer).await?;
            print_report(&report);
            Ok(())
        }
        Command::Improve { run_id } => {
            let runtime = cowboy_workflow_engine::WorkflowRuntime::new(config.runtime_config(cwd));
            let applied = runtime.improve_run(&run_id).await?;
            println!("improvement={applied:?}");
            Ok(())
        }
        Command::Runs => {
            let runtime = cowboy_workflow_engine::WorkflowRuntime::new(config.runtime_config(cwd));
            for run in runtime.list_runs()? {
                println!(
                    "{} workflow={} status={:?} step={} head={}",
                    run.run_id,
                    run.workflow_name,
                    run.status,
                    run.current_step,
                    run.head_step.as_deref().unwrap_or("<none>")
                );
            }
            Ok(())
        }
    }
}

fn print_report(report: &cowboy_workflow_engine::RunReport) {
    println!(
        "run={} workflow={} status={:?} step={}",
        report.run.id, report.run.workflow_name, report.run.status, report.run.current_step
    );
    for event in &report.events {
        println!("event={:?}", event.kind);
    }
}
