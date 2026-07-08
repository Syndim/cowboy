use anyhow::{Context, Result};
use cowboy_command_parser::{Cli, CliCommand, SharedCommand};

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
    let config_path = cli.config_path_or_else(cowboy::default_config_path);
    let config = cowboy::load_config(&config_path)?;
    if let Ok(log_path) = cowboy_log::init_file_logging(
        config.state_dir.join("logs"),
        "cowboy",
        cowboy_log::DEFAULT_DIRECTIVE,
    ) {
        tracing::info!(
            config_path = %config_path.display(),
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

    match cli.command.unwrap_or(CliCommand::Tui) {
        CliCommand::Tui => cowboy::run_tui(config).await,
        CliCommand::Shared(command) => run_shared_command(command, config, cwd).await,
    }
}

async fn run_shared_command(
    command: SharedCommand,
    config: cowboy::AppConfig,
    cwd: std::path::PathBuf,
) -> Result<()> {
    let runtime = cowboy_workflow_engine::WorkflowRuntime::new(config.runtime_config(cwd));

    match command {
        SharedCommand::Run(args) => {
            let cowboy_command_parser::RunArgs {
                step,
                workflow,
                request,
            } = args;
            let request = request.join(" ");
            let report = match (step, workflow) {
                (true, Some(workflow_id)) => {
                    runtime
                        .start_run_with_workflow_stepwise(workflow_id, request)
                        .await?
                }
                (false, Some(workflow_id)) => {
                    runtime
                        .start_run_with_workflow(workflow_id, request)
                        .await?
                }
                (true, None) => runtime.start_run_stepwise(request).await?,
                (false, None) => runtime.start_run(request).await?,
            };
            print_report(&report);
            Ok(())
        }
        SharedCommand::Step(args) => {
            let report = runtime.step_run(&args.run_id).await?;
            print_report(&report);
            Ok(())
        }
        SharedCommand::Resume(args) => {
            let report = runtime.resume_run(&args.run_id).await?;
            print_report(&report);
            Ok(())
        }
        SharedCommand::Answer(args) => {
            let cowboy_command_parser::AnswerArgs {
                run_id,
                prompt_id,
                answer,
            } = args;
            let answer = answer.join(" ");
            let report = runtime.answer_run(&run_id, &prompt_id, &answer).await?;
            print_report(&report);
            Ok(())
        }
        SharedCommand::Improve(args) => {
            let applied = runtime.improve_run(&args.run_id).await?;
            println!("improvement={applied:?}");
            Ok(())
        }
        SharedCommand::Runs => {
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
        SharedCommand::Resolve(args) => {
            let cowboy_command_parser::ResolveArgs {
                run_id,
                status,
                fields,
                body,
                fields_json,
            } = args;
            match status {
                None => {
                    let options = runtime.resolution_options(&run_id)?;
                    print_resolution_options(&options);
                    Ok(())
                }
                Some(status) => {
                    let fields = match fields.or(fields_json) {
                        Some(raw) => Some(
                            serde_json::from_str(&raw)
                                .with_context(|| format!("invalid fields JSON: {raw}"))?,
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
