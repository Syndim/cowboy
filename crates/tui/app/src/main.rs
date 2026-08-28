use anyhow::Result;
use cowboy_command_parser::{Cli, CliCommand, SharedCommand, resolve_fields_object};

fn main() {
    let result = cowboy::run_with_bounded_shutdown(
        || async { run_main().await },
        cowboy::DEFAULT_PROCESS_SHUTDOWN_TIMEOUT,
    );
    let result = match result {
        Ok(result) => result,
        Err(err) => Err(err.into()),
    };
    if let Err(err) = result {
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
    let runtime = cowboy_workflow_engine::WorkflowRuntime::new(config.runtime_config(cwd)).await?;

    match command {
        SharedCommand::Run(args) => {
            let cowboy_command_parser::RunArgs {
                step,
                workflow,
                session_ids,
                request,
            } = args;
            let options = cowboy_workflow_engine::RunStartOptions::with_role_session_ids(
                session_ids
                    .into_iter()
                    .map(|session| (session.role, session.session_id)),
            );
            let request = request.join(" ");
            let report = match (step, workflow) {
                (true, Some(workflow_id)) => {
                    runtime
                        .start_run_with_workflow_stepwise_and_options(workflow_id, request, options)
                        .await?
                }
                (false, Some(workflow_id)) => {
                    runtime
                        .start_run_with_workflow_and_options(workflow_id, request, options)
                        .await?
                }
                (true, None) => {
                    runtime
                        .start_run_stepwise_with_options(request, options)
                        .await?
                }
                (false, None) => runtime.start_run_with_options(request, options).await?,
            };
            print_report_with_terminal_export(&runtime, &report).await;
            print_agent_session_ids(&report.events);
            Ok(())
        }
        SharedCommand::Step(args) => {
            let report = runtime.step_run(&args.run_id).await?;
            print_report_with_terminal_export(&runtime, &report).await;
            Ok(())
        }
        SharedCommand::Resume(args) => {
            let report = runtime.resume_run(&args.run_id).await?;
            print_report_with_terminal_export(&runtime, &report).await;
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
            print_report_with_terminal_export(&runtime, &report).await;
            Ok(())
        }
        SharedCommand::Improve(args) => {
            let applied = runtime.improve_run(&args.run_id).await?;
            println!("improvement={applied:?}");
            Ok(())
        }
        SharedCommand::Runs(args) => {
            for run in runtime.list_runs(args.partial_run_id.as_deref()).await? {
                for line in cowboy::run_summary::render_run_summary_lines(&run) {
                    println!("{line}");
                }
            }
            Ok(())
        }
        SharedCommand::Export(args) => {
            let exported = cowboy::export_run(&runtime, &args.run_id).await?;
            println!(
                "run={} cards={} path={}",
                exported.run_id,
                exported.card_count,
                exported.path.display()
            );
            Ok(())
        }
        SharedCommand::Resolve(args) => {
            let cowboy_command_parser::ResolveArgs {
                run_id,
                status,
                fields,
                body,
            } = args;
            let fields = resolve_fields_object(fields)?;
            match status {
                None => {
                    let options = runtime.resolution_options(&run_id).await?;
                    print_resolution_options(&options);
                    Ok(())
                }
                Some(status) => {
                    let report = runtime.resolve_run(&run_id, &status, fields, body).await?;
                    print_report_with_terminal_export(&runtime, &report).await;
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
        println!(
            "    resolve with: {}",
            cowboy::resolution::resolution_command("cowboy resolve", &options.run_id, status)
        );
    }
}

fn print_report(report: &cowboy_workflow_engine::RunReport) {
    println!(
        "run={} workflow={} status={:?} step={}",
        report.run.id, report.run.workflow.name, report.run.status, report.run.step.current
    );
    for event in &report.events {
        println!("event={:?}", event.kind);
    }
}

async fn print_report_with_terminal_export(
    runtime: &cowboy_workflow_engine::WorkflowRuntime,
    report: &cowboy_workflow_engine::RunReport,
) {
    print_report(report);
    match cowboy::export_terminal_report(runtime, report).await {
        Ok(Some(exported)) => println!("terminal_transcript={}", exported.path.display()),
        Ok(None) => {}
        Err(_) => eprintln!("warning: terminal transcript export failed"),
    }
}

fn agent_session_id_lines(events: &[cowboy_workflow_engine::WorkflowEvent]) -> Vec<String> {
    let mut sessions =
        std::collections::BTreeMap::<String, std::collections::BTreeSet<String>>::new();
    for event in events {
        let cowboy_workflow_engine::WorkflowEventKind::AgentSessionReady {
            role, session_id, ..
        } = &event.kind
        else {
            continue;
        };

        sessions
            .entry(role.clone())
            .or_default()
            .insert(session_id.clone());
    }

    sessions
        .into_iter()
        .map(|(role, session_ids)| {
            format!(
                "{role}: {}",
                session_ids.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect()
}

fn print_agent_session_ids(events: &[cowboy_workflow_engine::WorkflowEvent]) {
    for line in agent_session_id_lines(events) {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

    use super::agent_session_id_lines;

    #[test]
    fn agent_session_id_lines_are_sorted_and_deduplicated() {
        let events = vec![
            WorkflowEvent::new(
                "run-1",
                WorkflowEventKind::AgentSessionReady {
                    step_id: "review".to_string(),
                    role: "reviewer".to_string(),
                    session_id: "session-2".to_string(),
                    descriptor: None,
                },
            ),
            WorkflowEvent::new(
                "run-1",
                WorkflowEventKind::AgentSessionReady {
                    step_id: "implement".to_string(),
                    role: "developer".to_string(),
                    session_id: "session-3".to_string(),
                    descriptor: None,
                },
            ),
            WorkflowEvent::new(
                "run-1",
                WorkflowEventKind::AgentSessionReady {
                    step_id: "implement".to_string(),
                    role: "developer".to_string(),
                    session_id: "session-1".to_string(),
                    descriptor: None,
                },
            ),
        ];

        assert_eq!(
            agent_session_id_lines(&events),
            vec![
                "developer: session-1, session-3".to_string(),
                "reviewer: session-2".to_string(),
            ]
        );
    }
}
