//! `engine-cli` — a playground for the `cowboy-workflow-engine` runtime.
//!
//! It drives a real `WorkflowRuntime` (catalog selection, redb persistence,
//! event projection, single-step / run-until-blocked, ask-user input) so the
//! engine logic can be exercised from the shell without the full TUI.
//!
//! ```text
//! engine-cli catalog                         list selectable workflows + descriptions
//! engine-cli runs                            list persisted runs
//! engine-cli run <request...>                start a run, execute until it blocks/finishes
//! engine-cli run-step <request...>           start a run, execute only the first step
//! engine-cli step <run-id>                   execute exactly one further step
//! engine-cli resume <run-id>                 run an existing run until it blocks/finishes
//! engine-cli answer <run-id> <prompt> <val>  answer an ask-user prompt and continue
//! engine-cli show <run-id>                   print a run's persisted state
//! engine-cli events <run-id>                 print a run's persisted event log
//! ```
//!
//! Config is read from the environment (defaults in parentheses):
//!   COWBOY_ENGINE_STATE      state dir                  (engine-state)
//!   COWBOY_ENGINE_WORKFLOWS  ':'-separated workflow dirs (engine-workflows)
//!   COWBOY_ENGINE_BACKEND    ACP backend preset          (copilot; also: omp)
//!   COWBOY_ENGINE_AGENT      override the agent command  (preset default)
//!   COWBOY_ENGINE_AGENT_ARGS override args, whitespace-separated (preset default)
//!   COWBOY_ENGINE_MODEL      override the model id       (preset default)
//!   COWBOY_ENGINE_PROVIDER   override the provider, "" clears (preset default)

use std::env;
use std::future::Future;
use std::path::PathBuf;

use cowboy_agent_acp::BackendPreset;
use cowboy_workflow_core::{Result as CoreResult, RunStatus};
use cowboy_workflow_engine::{
    AgentRuntimeConfig, RunReport, RunnerLimitsConfig, RuntimeConfig, WorkflowEvent,
    WorkflowEventKind, WorkflowRuntime,
};

type CliResult = Result<(), Box<dyn std::error::Error>>;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

/// Send `tracing` diagnostics to `<binary dir>/engine-cli.log`.
fn init_logging() {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    if let Ok(log_path) =
        cowboy_log::init_file_logging(dir, "engine-cli", cowboy_log::TEST_APP_DIRECTIVE)
    {
        tracing::info!(log_path = %log_path.display(), "engine-cli logging initialized");
    }
}

#[tokio::main(flavor = "current_thread")]
async fn run() -> CliResult {
    init_logging();
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else { usage() };
    let rest = args.collect::<Vec<_>>();
    let rt = build_runtime()?;

    match command.as_str() {
        "catalog" => catalog(&rt)?,
        "runs" => runs(&rt)?,
        "run" => {
            let request = joined_request(&rest);
            let report = run_with_live_events(&rt, || rt.start_run(request)).await?;
            print_report(&report);
        }
        "run-step" => {
            let request = joined_request(&rest);
            let report = run_with_live_events(&rt, || rt.start_run_stepwise(request)).await?;
            print_report(&report);
        }
        "step" => {
            let [run_id] = rest.as_slice() else { usage() };
            let report = run_with_live_events(&rt, || rt.step_run(run_id)).await?;
            print_report(&report);
        }
        "resume" => {
            let [run_id] = rest.as_slice() else { usage() };
            let report = run_with_live_events(&rt, || rt.resume_run(run_id)).await?;
            print_report(&report);
        }
        "answer" => {
            let [run_id, prompt_id, value] = rest.as_slice() else {
                usage()
            };
            let report =
                run_with_live_events(&rt, || rt.answer_run(run_id, prompt_id, value)).await?;
            print_report(&report);
        }
        "show" => {
            let [run_id] = rest.as_slice() else { usage() };
            show(&rt, run_id)?;
        }
        "events" => {
            let [run_id] = rest.as_slice() else { usage() };
            events(&rt, run_id)?;
        }
        _ => usage(),
    }
    Ok(())
}

fn build_runtime() -> Result<WorkflowRuntime, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let state_dir = PathBuf::from(env_or("COWBOY_ENGINE_STATE", "engine-state"));
    let workflow_store = state_dir.join("workflow.redb");
    let workflow_dirs: Vec<PathBuf> = env::var("COWBOY_ENGINE_WORKFLOWS")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| value.split(':').map(PathBuf::from).collect())
        .unwrap_or_else(|| vec![PathBuf::from("engine-workflows")]);
    let agent = resolve_agent()?;
    let limits = RunnerLimitsConfig {
        max_steps_per_run: 100,
        max_visits_per_step: 20,
    };
    eprintln!(
        "# engine-cli  state={}  workflows={:?}  agent=`{} {}`  model={:?}",
        state_dir.display(),
        workflow_dirs,
        agent.command,
        agent.args.join(" "),
        agent.model.id,
    );
    tracing::info!(
        cwd = %cwd.display(),
        state_dir = %state_dir.display(),
        workflow_store = %workflow_store.display(),
        workflow_dirs = ?workflow_dirs,
        agent_name = %agent.name,
        agent_command = %agent.command,
        agent_args = ?agent.args,
        model_id = %agent.model.id,
        provider = ?agent.model.provider,
        max_steps_per_run = limits.max_steps_per_run,
        max_visits_per_step = limits.max_visits_per_step,
        "engine-cli runtime configured"
    );
    Ok(WorkflowRuntime::new(RuntimeConfig::new(
        cwd,
        state_dir,
        workflow_store,
        workflow_dirs,
        vec![agent],
        limits,
    )))
}

/// Resolve the ACP agent config from a backend preset selected by
/// `COWBOY_ENGINE_BACKEND` (default `copilot`, also `omp`), with optional
/// per-field overrides.
fn resolve_agent() -> Result<AgentRuntimeConfig, Box<dyn std::error::Error>> {
    let backend = env_or("COWBOY_ENGINE_BACKEND", "copilot");
    let preset = BackendPreset::lookup(&backend).ok_or_else(|| {
        format!(
            "unknown backend {backend:?}; known backends: {}",
            BackendPreset::known_names()
        )
    })?;
    let command = env::var("COWBOY_ENGINE_AGENT").unwrap_or_else(|_| preset.command.to_string());
    let args = match env::var("COWBOY_ENGINE_AGENT_ARGS") {
        Ok(value) => value.split_whitespace().map(str::to_string).collect(),
        Err(_) => preset.owned_args(),
    };
    let model = env::var("COWBOY_ENGINE_MODEL").unwrap_or_else(|_| preset.model.to_string());
    let provider = match env::var("COWBOY_ENGINE_PROVIDER") {
        Ok(value) if value.is_empty() => None,
        Ok(value) => Some(value),
        Err(_) => Some(preset.provider.to_string()),
    };
    Ok(AgentRuntimeConfig::new(
        "default", command, args, model, provider,
    ))
}

/// List the workflows the engine would select from. Unlike the catalog crate,
/// the engine compiles each `.lua` workflow, so descriptions declared in Lua
/// are resolved here.
fn catalog(rt: &WorkflowRuntime) -> CliResult {
    let catalog = rt.catalog()?;
    println!("workflows ({})", catalog.workflows.len());
    for (id, source_ref) in &catalog.workflows {
        println!(
            "- {id}  {}",
            source_ref
                .description
                .as_deref()
                .unwrap_or("<no description>")
        );
    }
    println!("\nnote: `run` selects the first workflow by id (deterministic selector).");
    Ok(())
}

fn runs(rt: &WorkflowRuntime) -> CliResult {
    let runs = rt.list_runs()?;
    println!("runs ({})", runs.len());
    for run in &runs {
        println!(
            "- {} workflow={} status={} step={} head={}",
            run.run_id,
            run.workflow_name,
            status_label(&run.status),
            run.current_step,
            run.head_step.as_deref().unwrap_or("<none>"),
        );
    }
    Ok(())
}

fn show(rt: &WorkflowRuntime, run_id: &str) -> CliResult {
    let run = rt.load_run(run_id)?;
    println!("id:             {}", run.id);
    println!("workflow:       {}", run.workflow_name);
    println!("status:         {}", status_label(&run.status));
    println!("current_step:   {}", run.current_step);
    println!("steps_executed: {}", run.steps_executed);
    println!(
        "head:           {}",
        run.head.as_deref().unwrap_or("<none>")
    );
    println!("request:        {}", run.original_request);
    if !run.resume.is_null() {
        println!("resume:         {}", run.resume);
    }
    if let RunStatus::WaitingForInput {
        prompt_id,
        message,
        choices,
        ..
    } = &run.status
    {
        println!("waiting prompt: {prompt_id} ({message}) choices={choices:?}");
    }
    Ok(())
}

async fn run_with_live_events<F, Fut>(rt: &WorkflowRuntime, run: F) -> CoreResult<RunReport>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = CoreResult<RunReport>>,
{
    let events = rt.events();
    let mut rx = events.subscribe();
    let future = run();
    tokio::pin!(future);

    loop {
        tokio::select! {
            result = &mut future => return result,
            event = rx.recv() => {
                if let Ok(event) = event {
                    eprintln!("  progress={}", render_workflow_event(&event));
                }
            }
        }
    }
}

fn events(rt: &WorkflowRuntime, run_id: &str) -> CliResult {
    let events = rt.load_events(run_id)?;
    println!("events ({})", events.len());
    for event in &events {
        println!("- {}", render_workflow_event(event));
    }
    Ok(())
}

fn render_workflow_event(event: &WorkflowEvent) -> String {
    match &event.kind {
        WorkflowEventKind::RunStarted {
            workflow_name,
            current_step,
        } => format!(
            "{} started workflow {workflow_name} at {current_step}",
            event.run_id
        ),
        WorkflowEventKind::StepStarted { step_id } => {
            format!("{} started step {step_id}", event.run_id)
        }
        WorkflowEventKind::StepProgress { step_id, message } => {
            format!("{} step {step_id}: {message}", event.run_id)
        }
        WorkflowEventKind::AgentSessionReady {
            step_id,
            role,
            session_id,
        } => format!(
            "{} step {step_id} agent session ready role={role} session={session_id}",
            event.run_id
        ),
        WorkflowEventKind::AgentPrompt {
            step_id,
            role,
            session_id,
            prompt,
        } => format!(
            "{} step {step_id} prompt role={role} session={session_id}:\n{prompt}",
            event.run_id
        ),
        WorkflowEventKind::AgentResponse { step_id, content } => {
            format!("{} step {step_id} agent response:\n{content}", event.run_id)
        }
        WorkflowEventKind::AgentThought { step_id, content } => {
            format!("{} step {step_id} agent thought:\n{content}", event.run_id)
        }
        WorkflowEventKind::AgentToolCall {
            step_id,
            tool_call_id,
            title,
            tool_kind,
            status,
        } => format!(
            "{} step {step_id} agent tool id={tool_call_id} kind={tool_kind} status={status}: {title}",
            event.run_id
        ),
        WorkflowEventKind::AgentToolCallUpdate {
            step_id,
            tool_call_id,
            title,
            status,
            content,
        } => {
            let content = content
                .as_ref()
                .map(|content| format!(" content={content}"))
                .unwrap_or_default();
            format!(
                "{} step {step_id} agent tool update id={tool_call_id} status={status}: {title}{content}",
                event.run_id
            )
        }
        WorkflowEventKind::AgentPlan { step_id, entries } => {
            format!(
                "{} step {step_id} agent plan: {}",
                event.run_id,
                serde_json::Value::Array(entries.clone())
            )
        }
        WorkflowEventKind::StepCompleted {
            step_id,
            action,
            status,
            body,
        } => format!(
            "{} completed step {step_id} via {action} status={} {}",
            event.run_id,
            status.as_deref().unwrap_or("<none>"),
            body
        ),
        WorkflowEventKind::WaitingForInput {
            step,
            prompt_id,
            message,
            choices,
        } => format!(
            "{} waiting for input {prompt_id} at {step}: {message} [{}]",
            event.run_id,
            choices.join(",")
        ),
        WorkflowEventKind::Suspended { step, reason } => {
            format!("{} suspended at {step}: {reason}", event.run_id)
        }
        WorkflowEventKind::RunCompleted => format!("{} completed", event.run_id),
        WorkflowEventKind::RunFailed { reason } => format!("{} failed: {reason}", event.run_id),
        WorkflowEventKind::RunCancelled => format!("{} cancelled", event.run_id),
        WorkflowEventKind::RunStatusChanged { status } => {
            format!("{} status {status}", event.run_id)
        }
    }
}

fn print_report(report: &RunReport) {
    let run = &report.run;
    println!(
        "run={} workflow={} status={} step={} steps_executed={}",
        run.id,
        run.workflow_name,
        status_label(&run.status),
        run.current_step,
        run.steps_executed,
    );
    if let RunStatus::WaitingForInput {
        prompt_id,
        message,
        choices,
        ..
    } = &run.status
    {
        println!("  waiting: prompt={prompt_id:?} message={message:?} choices={choices:?}");
        println!("  -> engine-cli answer {} {} <value>", run.id, prompt_id);
    }
    for event in &report.events {
        println!("  event={}", render_workflow_event(event));
    }
}

fn status_label(status: &RunStatus) -> String {
    match status {
        RunStatus::Running => "Running".to_string(),
        RunStatus::WaitingForInput { prompt_id, .. } => format!("WaitingForInput({prompt_id})"),
        RunStatus::Suspended { reason, .. } => format!("Suspended({reason})"),
        RunStatus::Completed => "Completed".to_string(),
        RunStatus::Failed { reason } => format!("Failed({reason})"),
        RunStatus::Cancelled => "Cancelled".to_string(),
    }
}

fn joined_request(rest: &[String]) -> String {
    if rest.is_empty() {
        usage();
    }
    rest.join(" ")
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn usage() -> ! {
    eprintln!("engine-cli — exercise the cowboy-workflow-engine runtime");
    eprintln!();
    eprintln!("usage:");
    eprintln!("  engine-cli catalog                         list selectable workflows");
    eprintln!("  engine-cli runs                            list persisted runs");
    eprintln!("  engine-cli run <request...>                start + run until blocked/finished");
    eprintln!("  engine-cli run-step <request...>           start + run only the first step");
    eprintln!("  engine-cli step <run-id>                   run exactly one further step");
    eprintln!("  engine-cli resume <run-id>                 run an existing run until blocked");
    eprintln!("  engine-cli answer <run-id> <prompt> <val>  answer an ask-user prompt");
    eprintln!("  engine-cli show <run-id>                   print persisted run state");
    eprintln!("  engine-cli events <run-id>                 print persisted event log");
    eprintln!();
    eprintln!("env: COWBOY_ENGINE_STATE, COWBOY_ENGINE_WORKFLOWS,");
    eprintln!("     COWBOY_ENGINE_BACKEND (copilot|omp), COWBOY_ENGINE_AGENT,");
    eprintln!("     COWBOY_ENGINE_AGENT_ARGS, COWBOY_ENGINE_MODEL, COWBOY_ENGINE_PROVIDER");
    std::process::exit(2);
}
