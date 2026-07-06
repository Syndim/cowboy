use std::path::PathBuf;

use anyhow::{Context, Result};
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
    /// Continue an existing workflow run until it blocks, fails, or completes.
    Resume { run_id: String },
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
    /// Resolve a failed run. Without <status>, lists resolvable statuses.
    Resolve {
        run_id: String,
        /// Status to resolve the failed step to. Omit to list options.
        status: Option<String>,
        /// JSON object of output fields for the chosen status.
        #[arg(long)]
        fields: Option<String>,
        /// Human-readable body for the synthesized step record.
        #[arg(long)]
        body: Option<String>,
    },
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
            agents = ?config.agents,
            agent_count = config.agents.len(),
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
        Command::Resume { run_id } => {
            let runtime = cowboy_workflow_engine::WorkflowRuntime::new(config.runtime_config(cwd));
            let report = runtime.resume_run(&run_id).await?;
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
        Command::Resolve {
            run_id,
            status,
            fields,
            body,
        } => {
            let runtime = cowboy_workflow_engine::WorkflowRuntime::new(config.runtime_config(cwd));
            match status {
                None => {
                    let options = runtime.resolution_options(&run_id)?;
                    print_resolution_options(&options);
                    Ok(())
                }
                Some(status) => {
                    let fields = match fields {
                        Some(raw) => Some(
                            serde_json::from_str(&raw)
                                .with_context(|| format!("invalid --fields JSON: {raw}"))?,
                        ),
                        None => None,
                    };
                    let report = runtime.resolve_run(&run_id, &status, fields, body).await?;
                    print_report(&report);
                    Ok(())
                }
            }
        }
    }
}

fn print_resolution_options(options: &cowboy_workflow_engine::ResolutionOptions) {
    println!(
        "run={} failed_step={} reason={}",
        options.run_id,
        options.failed_step,
        options.failure_reason.as_deref().unwrap_or("<none>")
    );
    println!("resolvable statuses:");
    for status in &options.statuses {
        let target = status.target_step.as_deref().unwrap_or("<run completes>");
        println!(
            "  {} -> {} required=[{}] optional=[{}] body_expected={}",
            status.status,
            target,
            status.required_fields.join(", "),
            status.optional_fields.join(", "),
            status.body_expected
        );
    }
    println!(
        "resolve with: cowboy resolve {} <status> [--fields '<json>'] [--body <text>]",
        options.run_id
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_without_status_parses_as_list_form() {
        let cli = Cli::parse_from(["cowboy", "resolve", "run-1"]);
        match cli.command {
            Some(Command::Resolve {
                run_id,
                status,
                fields,
                body,
            }) => {
                assert_eq!(run_id, "run-1");
                assert_eq!(status, None);
                assert_eq!(fields, None);
                assert_eq!(body, None);
            }
            other => panic!("expected resolve command, got {other:?}"),
        }
    }

    #[test]
    fn resume_with_run_id_parses() {
        let cli = Cli::parse_from(["cowboy", "resume", "run-1"]);
        match cli.command {
            Some(Command::Resume { run_id }) => assert_eq!(run_id, "run-1"),
            other => panic!("expected resume command, got {other:?}"),
        }
    }

    #[test]
    fn resume_requires_run_id() {
        let err = Cli::try_parse_from(["cowboy", "resume"]).unwrap_err();

        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn resolve_with_status_fields_and_body_parses() {
        let cli = Cli::parse_from([
            "cowboy",
            "resolve",
            "run-1",
            "approved",
            "--fields",
            r#"{"summary":"done"}"#,
            "--body",
            "looks good",
        ]);
        match cli.command {
            Some(Command::Resolve {
                run_id,
                status,
                fields,
                body,
            }) => {
                assert_eq!(run_id, "run-1");
                assert_eq!(status.as_deref(), Some("approved"));
                assert_eq!(fields.as_deref(), Some(r#"{"summary":"done"}"#));
                assert_eq!(body.as_deref(), Some("looks good"));
            }
            other => panic!("expected resolve command, got {other:?}"),
        }
    }
}
