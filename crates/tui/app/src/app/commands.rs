use crate::resolution::resolution_command;
use anyhow::Result;
use cowboy_command_parser::{
    SharedCommand, SlashCommand, SlashParseError, parse_slash_command, resolve_fields_object,
    slash_command_usage, slash_help_rows, slash_suggestions,
};
use cowboy_workflow_engine::{UserPromptSubmission, WorkflowRuntime};

use super::state::{AppState, ComposerSubmissionMode};

pub(in crate::app) fn complete_slash_suggestion(state: &mut AppState) {
    let Some(command) = slash_suggestions(state.input()).into_iter().next() else {
        return;
    };

    let input = if command.takes_arguments {
        format!("{} ", command.name)
    } else {
        command.name.to_string()
    };
    state.replace_input_from_completion(input);
}

fn show_slash_parse_error(state: &mut AppState, err: SlashParseError) {
    let (status, details) = match err {
        SlashParseError::UnmatchedQuote => {
            let status = "usage: unmatched quote in slash command".to_string();
            (status.clone(), vec![status])
        }
        SlashParseError::Validation { command, message } => {
            let reason = first_error_line(&message);
            match command.as_deref().and_then(slash_command_usage) {
                Some(usage) => {
                    let usage = format!("usage: {usage}");
                    (usage.clone(), vec![reason, usage])
                }
                None => (reason.clone(), vec![reason]),
            }
        }
    };

    state.set_status(status);
    state.push_card("Usage", details);
}

fn first_error_line(message: &str) -> String {
    message
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .unwrap_or("invalid slash command")
        .to_string()
}

pub(in crate::app) async fn submit_input(state: &mut AppState, runtime: &WorkflowRuntime) {
    let Some(input) = state.submitted_input() else {
        return;
    };
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        match state.composer_submission_mode() {
            ComposerSubmissionMode::AgentPrompt => {
                let Some(window) = state.agent_prompt_window().cloned() else {
                    return;
                };
                match runtime.submit_user_prompt(&window.run_id, &window.window_id, input.clone()) {
                    Ok(UserPromptSubmission::Accepted(prompt)) => {
                        state.commit_submitted_input(&input);
                        state.set_status(format!(
                            "prompt accepted for step {} as sequence {}",
                            window.step_id, prompt.sequence
                        ));
                        state.push_card("Prompt", [input]);
                    }
                    Ok(UserPromptSubmission::Rejected(reason)) => {
                        state.set_status(format!(
                            "prompt not sent: {}; draft retained",
                            reason.message()
                        ));
                        state.push_card("Notice", [state.status().to_string()]);
                    }
                    Err(err) => {
                        state.set_status(format!("prompt not sent: {err}; draft retained"));
                        state.push_card("Error", [state.status().to_string()]);
                    }
                }
                return;
            }
            ComposerSubmissionMode::ExecutionBlocked => {
                state.set_status(
                    "prompt not sent: no agent is currently accepting prompts; draft retained",
                );
                state.push_card("Notice", [state.status().to_string()]);
                return;
            }
            ComposerSubmissionMode::PendingAnswer | ComposerSubmissionMode::Idle => {}
        }
    }

    match dispatch_submitted_input(state, runtime, trimmed).await {
        Ok(true) => state.commit_submitted_input(trimmed),
        Ok(false) => {}
        Err(err) => {
            state.commit_submitted_input(trimmed);
            state.set_status(format!("error: {err}"));
            state.push_card("Error", [state.status().to_string()]);
        }
    }
}

async fn dispatch_submitted_input(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    input: &str,
) -> Result<bool> {
    if input.starts_with('/') {
        let command = match parse_slash_command(input) {
            Ok(command) => command,
            Err(err) => {
                show_slash_parse_error(state, err);
                return Ok(true);
            }
        };
        if state.workflow_execution_running() && command_conflicts_with_execution(&command) {
            state.set_status("command not run: workflow execution is active; draft retained");
            state.push_card("Notice", [state.status().to_string()]);
            return Ok(false);
        }

        dispatch_slash_command(state, runtime, command).await?;
    } else if let Some((run_id, prompt_id)) = state.pending_prompt_answer_target() {
        spawn_answer_task(state, runtime, run_id, prompt_id, input.to_string());
    } else {
        spawn_start_run(state, runtime, input.to_string());
    }

    Ok(true)
}

fn command_conflicts_with_execution(command: &SlashCommand) -> bool {
    match command {
        SlashCommand::Cancel
        | SlashCommand::Exit
        | SlashCommand::Help
        | SlashCommand::Workflows
        | SlashCommand::Shared(SharedCommand::Runs(_)) => false,
        SlashCommand::Shared(SharedCommand::Resolve(args)) => args.status.is_some(),
        _ => true,
    }
}

async fn dispatch_slash_command(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    command: SlashCommand,
) -> Result<()> {
    match command {
        SlashCommand::Shared(command) => dispatch_shared_command(state, runtime, command).await?,
        SlashCommand::Workflows => show_workflows(state, runtime)?,
        SlashCommand::Cancel => state.cancel_background_tasks(),
        SlashCommand::Exit => {
            state.mark_exit_requested();
            state.set_status("exiting");
            state.push_card("Exit", ["exiting".to_string()]);
        }
        SlashCommand::Help => show_help(state),
    }

    Ok(())
}

async fn dispatch_shared_command(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    command: SharedCommand,
) -> Result<()> {
    match command {
        SharedCommand::Run(args) => spawn_start_run_from_args(state, runtime, args),
        SharedCommand::Step(args) => spawn_step_run(state, runtime, args.run_id),
        SharedCommand::Resume(args) => spawn_resume_run(state, runtime, args.run_id),
        SharedCommand::Answer(args) => {
            let cowboy_command_parser::AnswerArgs {
                run_id,
                prompt_id,
                answer,
            } = args;
            spawn_answer_task(state, runtime, run_id, prompt_id, answer.join(" "));
        }
        SharedCommand::Runs(args) => spawn_runs_list(state, runtime, args.partial_run_id),
        SharedCommand::Improve(args) => improve_run(state, runtime, &args.run_id).await?,
        SharedCommand::Resolve(args) => {
            let cowboy_command_parser::ResolveArgs {
                run_id,
                status,
                fields,
                body,
            } = args;
            let fields = match resolve_fields_object(fields) {
                Ok(fields) => fields,
                Err(err) => {
                    show_slash_parse_error(
                        state,
                        SlashParseError::Validation {
                            command: Some("resolve".to_string()),
                            message: err.to_string(),
                        },
                    );
                    return Ok(());
                }
            };
            resolve_run(state, runtime, run_id, status, fields, body).await?;
        }
    }

    Ok(())
}

fn spawn_start_run_from_args(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    args: cowboy_command_parser::RunArgs,
) {
    let cowboy_command_parser::RunArgs {
        step,
        workflow,
        request,
    } = args;
    let request = request.join(" ");

    match (step, workflow) {
        (true, Some(workflow_id)) => {
            spawn_start_run_with_workflow_stepwise(state, runtime, workflow_id, request);
        }
        (false, Some(workflow_id)) => {
            spawn_start_run_with_workflow(state, runtime, workflow_id, request);
        }
        (true, None) => spawn_start_run_stepwise(state, runtime, request),
        (false, None) => spawn_start_run(state, runtime, request),
    }
}

fn spawn_start_run(state: &mut AppState, runtime: &WorkflowRuntime, request: String) {
    let runtime = runtime.clone();
    let label = format!("submitted run: {request}");
    let body = request.clone();
    state.spawn_card_report_task(
        "Run",
        ["00:00:00".to_string()],
        ["submitted run".to_string()],
        label,
        [body],
        async move {
            runtime
                .start_run(request)
                .await
                .map_err(|err| err.to_string())
        },
    );
}

fn spawn_start_run_stepwise(state: &mut AppState, runtime: &WorkflowRuntime, request: String) {
    let runtime = runtime.clone();
    let label = format!("submitted run --step: {request}");
    let body = request.clone();
    state.spawn_card_report_task(
        "Run",
        ["00:00:00".to_string()],
        ["submitted run --step".to_string()],
        label,
        [body],
        async move {
            runtime
                .start_run_stepwise(request)
                .await
                .map_err(|err| err.to_string())
        },
    );
}

fn spawn_start_run_with_workflow(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    workflow_id: String,
    request: String,
) {
    let runtime = runtime.clone();
    let label = format!("submitted run --workflow {workflow_id}: {request}");
    let title_suffix = format!("submitted run --workflow {workflow_id}");
    let body = request.clone();
    state.spawn_card_report_task(
        "Run",
        ["00:00:00".to_string()],
        [title_suffix],
        label,
        [body],
        async move {
            runtime
                .start_run_with_workflow(workflow_id, request)
                .await
                .map_err(|err| err.to_string())
        },
    );
}

fn spawn_start_run_with_workflow_stepwise(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    workflow_id: String,
    request: String,
) {
    let runtime = runtime.clone();
    let label = format!("submitted run --step --workflow {workflow_id}: {request}");
    let title_suffix = format!("submitted run --step --workflow {workflow_id}");
    let body = request.clone();
    state.spawn_card_report_task(
        "Run",
        ["00:00:00".to_string()],
        [title_suffix],
        label,
        [body],
        async move {
            runtime
                .start_run_with_workflow_stepwise(workflow_id, request)
                .await
                .map_err(|err| err.to_string())
        },
    );
}

fn spawn_step_run(state: &mut AppState, runtime: &WorkflowRuntime, run_id: String) {
    let runtime = runtime.clone();
    let label = format!("submitted step: {run_id}");
    let body = run_id.clone();
    state.spawn_card_report_task(
        "Step",
        [],
        ["submitted step".to_string()],
        label,
        [body],
        async move {
            runtime
                .step_run(&run_id)
                .await
                .map_err(|err| err.to_string())
        },
    );
}

fn spawn_resume_run(state: &mut AppState, runtime: &WorkflowRuntime, run_id: String) {
    let runtime = runtime.clone();
    let label = format!("submitted resume: {run_id}");
    let body = run_id.clone();
    state.spawn_card_report_task(
        "Resume",
        [],
        ["submitted resume".to_string()],
        label,
        [body],
        async move {
            runtime
                .resume_run(&run_id)
                .await
                .map_err(|err| err.to_string())
        },
    );
}

fn spawn_answer_task(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    run_id: String,
    prompt_id: String,
    answer: String,
) {
    let runtime = runtime.clone();
    let label = format!("submitted answer: {run_id} {prompt_id}");
    let details = [run_id.clone(), prompt_id.clone()];
    state.clear_pending_prompt();
    state.spawn_card_report_task(
        "Answer",
        [],
        ["submitted answer".to_string()],
        label,
        details,
        async move {
            runtime
                .answer_run(&run_id, &prompt_id, &answer)
                .await
                .map_err(|err| err.to_string())
        },
    );
}

fn spawn_runs_list(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    partial_run_id: Option<String>,
) {
    let runtime = runtime.clone();
    let filter_for_task = partial_run_id.clone();
    state.spawn_runs_list_task("loading runs".to_string(), partial_run_id, async move {
        let runs = tokio::task::spawn_blocking(move || {
            runtime
                .list_runs(filter_for_task.as_deref())
                .map_err(|err| err.to_string())
        })
        .await
        .map_err(|err| err.to_string())??;
        Ok(runs)
    });
}

async fn improve_run(state: &mut AppState, runtime: &WorkflowRuntime, run_id: &str) -> Result<()> {
    let applied = runtime.improve_run(run_id).await?;
    state.set_status(format!("improvement={applied:?}"));
    state.push_card("Improve", [state.status().to_string()]);
    Ok(())
}

async fn resolve_run(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    run_id: String,
    status: Option<String>,
    fields: Option<serde_json::Value>,
    body: Option<String>,
) -> Result<()> {
    match status {
        None => {
            let options = runtime.resolution_options(&run_id)?;
            state.set_status(format!("resolve options for {}", options.run_id));
            let mut details = vec![
                format!("failed step: {}", options.failed_step),
                format!(
                    "reason: {}",
                    options.failure_reason.as_deref().unwrap_or("<none>")
                ),
                "resolvable statuses:".to_string(),
            ];
            for status in &options.statuses {
                let target = status.target_step.as_deref().unwrap_or("<run completes>");
                details.push(format!(
                    "  {} -> {} required=[{}]",
                    status.status,
                    target,
                    status.required_fields.join(", ")
                ));
                details.push(format!(
                    "    resolve with: {}",
                    resolution_command("/resolve", &options.run_id, status)
                ));
            }
            state.push_card("Resolve", details);
            Ok(())
        }
        Some(status) => {
            let runtime = runtime.clone();
            let label = format!("submitted resolve: {run_id} {status}");
            let details = [run_id.clone(), status.clone()];
            state.spawn_card_report_task(
                "Resolve",
                [],
                ["submitted resolve".to_string()],
                label,
                details,
                async move {
                    runtime
                        .resolve_run(&run_id, &status, fields, body)
                        .await
                        .map_err(|err| err.to_string())
                },
            );
            Ok(())
        }
    }
}

pub(in crate::app) fn show_help(state: &mut AppState) {
    state.set_status("built-in commands");
    let mut details =
        vec!["Plain text starts a workflow run. Slash commands control runs.".to_string()];
    details.extend(
        slash_help_rows()
            .into_iter()
            .map(|command| format!("{:<42} {}", command.usage, command.description)),
    );
    state.push_card("Help", details);
}

pub(in crate::app) fn show_workflows(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
) -> Result<()> {
    let catalog = runtime.catalog()?;
    let count = catalog.workflows.len();
    state.set_status(format!("known workflows ({count})"));
    let mut details = vec![format!("known workflows: {count}")];
    for (id, workflow) in catalog.workflows {
        let description = workflow.description.unwrap_or_else(|| "<none>".to_string());
        let root = workflow.root.unwrap_or_else(|| "<built-in>".to_string());
        details.push(id);
        details.push(format!("  description: {description}"));
        details.push(format!("  entry: {}", workflow.entry));
        details.push(format!("  root: {root}"));
    }
    state.push_card("Workflows", details);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, AppConfig};
    use chrono::Utc;
    use cowboy_workflow_core::{
        AgentPromptWindow, ResumeCallback, RunHead, RunStatus, StepRecord, WorkflowRun,
    };
    use cowboy_workflow_engine::{RunReport, WorkflowEvent, WorkflowEventKind};
    use cowboy_workflow_store::RedbRunStore;
    use serde_json::Value;

    fn test_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(AppConfig {
            state_dir: dir.path().to_path_buf(),
            workflow_store: dir.path().join("workflow.redb"),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        })
    }

    fn test_runtime_state() -> (tempfile::TempDir, WorkflowRuntime, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let state = AppState::new(config);
        (dir, runtime, state)
    }

    fn rendered_entries(state: &AppState) -> String {
        state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assert_last_entry_is_card(
        state: &AppState,
        expected_title: &str,
        expected_body: &[&str],
    ) -> String {
        let rendered = state
            .event_entries()
            .last()
            .expect("submission should append a transcript entry")
            .plain_text();
        assert_eq!(rendered.lines().next(), Some(expected_title), "{rendered}");
        for border in ['╭', '╮', '╰', '╯'] {
            assert!(rendered.contains(border), "{rendered}");
        }
        for detail in expected_body {
            assert!(rendered.contains(&format!("│{detail}")), "{rendered}");
        }
        assert!(
            !rendered.lines().any(|line| line.starts_with("submitted ")),
            "{rendered}"
        );
        rendered
    }

    fn workflow_run(
        id: &str,
        topic: Option<&str>,
        status: RunStatus,
        current_step: &str,
        head: Option<&str>,
    ) -> WorkflowRun {
        let now = Utc::now();
        WorkflowRun {
            id: id.to_string(),
            workflow_name: "deploy".to_string(),
            workflow_api_version: 1,
            workflow_hash: format!("hash-{id}"),
            workflow_sources: Default::default(),
            original_request: format!("request for {id}"),
            request_topic: topic.map(ToString::to_string),
            config_set: Default::default(),
            status,
            current_step: current_step.to_string(),
            head: head.map(ToString::to_string),
            resume: Value::Null,
            retries_used: 0,
            step_retries_used: Default::default(),
            steps_executed: 0,
            step_visits: Default::default(),
            active_duration_ms: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn seed_run(store: &RedbRunStore, run: WorkflowRun) {
        let head = RunHead {
            run_id: run.id.clone(),
            workflow_hash: run.workflow_hash.clone(),
            head_step: run.head.clone(),
            status: run.status.clone(),
            updated_at: run.updated_at,
        };
        store.save_run(&run).unwrap();
        store.update_run_head(&run.id, head).unwrap();
    }

    fn assert_rendered_contains(rendered: &str, expected: &str) {
        assert!(
            rendered.contains(expected),
            "rendered /runs card was missing {expected:?}:\n{rendered}"
        );
    }
    async fn drain_finished_background_task(state: &mut AppState) {
        for _ in 0..100 {
            tokio::task::yield_now().await;
            if state.drain_background_tasks().await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        panic!("background task did not finish");
    }

    #[tokio::test]
    async fn plain_request_submission_renders_initial_input_as_card() {
        let (_dir, runtime, mut state) = test_runtime_state();
        state.push_input("build health route");

        submit_input(&mut state, &runtime).await;

        let rendered = assert_last_entry_is_card(
            &state,
            "00:00:00 · ◌ Run · submitted run",
            &["build health route"],
        );
        assert!(!rendered.contains("│submitted run:"), "{rendered}");
        assert_eq!(state.status(), "submitted run: build health route");
        assert_eq!(state.background_task_count(), 1);
        state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn slash_run_variants_render_initial_input_as_cards() {
        for (input, expected_title, expected_status) in [
            (
                "/run build health route",
                "00:00:00 · ◌ Run · submitted run",
                "submitted run: build health route",
            ),
            (
                "/run --step build health route",
                "00:00:00 · ◌ Run · submitted run --step",
                "submitted run --step: build health route",
            ),
            (
                "/run --workflow test-failure-fix build health route",
                "00:00:00 · ◌ Run · submitted run --workflow test-failure-fix",
                "submitted run --workflow test-failure-fix: build health route",
            ),
            (
                "/run --step --workflow test-failure-fix build health route",
                "00:00:00 · ◌ Run · submitted run --step --workflow test-failure-fix",
                "submitted run --step --workflow test-failure-fix: build health route",
            ),
        ] {
            let (_dir, runtime, mut state) = test_runtime_state();
            state.push_input(input);

            submit_input(&mut state, &runtime).await;

            let rendered =
                assert_last_entry_is_card(&state, expected_title, &["build health route"]);
            assert!(!rendered.contains("│submitted run"), "{rendered}");
            assert_eq!(state.status(), expected_status);
            assert_eq!(state.background_task_count(), 1);
            state.cancel_background_tasks();
        }
    }

    #[tokio::test]
    async fn run_control_submissions_render_action_cards() {
        for (input, expected_title, expected_status, expected_body) in [
            (
                "/step run-123",
                "○ Step · submitted step",
                "submitted step: run-123",
                vec!["run-123"],
            ),
            (
                "/resume run-123",
                "○ Resume · submitted resume",
                "submitted resume: run-123",
                vec!["run-123"],
            ),
            (
                "/resolve run-123 accepted",
                "● Resolve · submitted resolve",
                "submitted resolve: run-123 accepted",
                vec!["run-123", "accepted"],
            ),
        ] {
            let (_dir, runtime, mut state) = test_runtime_state();
            state.push_input(input);

            submit_input(&mut state, &runtime).await;

            assert_last_entry_is_card(&state, expected_title, &expected_body);
            assert_eq!(state.status(), expected_status);
            assert_eq!(state.background_task_count(), 1);
            state.cancel_background_tasks();
        }
    }

    #[tokio::test]
    async fn runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive() {
        let (_dir, runtime, mut state) = test_runtime_state();
        state.push_input("/runs");

        submit_input(&mut state, &runtime).await;

        assert_eq!(
            state.background_task_count(),
            1,
            "/runs must run in a background task so the TUI event loop can keep processing keys"
        );
        state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn runs_submission_does_not_mark_workflow_execution_running() {
        let (_dir, runtime, mut state) = test_runtime_state();
        state.push_input("/runs");

        submit_input(&mut state, &runtime).await;

        assert_eq!(state.background_task_count(), 1);
        assert!(!state.workflow_execution_running());
        assert_eq!(
            state.composer_submission_mode(),
            ComposerSubmissionMode::Idle
        );

        state.push_input("new workflow request");
        submit_input(&mut state, &runtime).await;

        assert_eq!(state.background_task_count(), 2);
        assert!(state.workflow_execution_running());
        state.cancel_background_tasks();
    }

    #[test]
    fn help_uses_generated_slash_command_rows() {
        let mut state = test_state();

        show_help(&mut state);
        let rendered = state
            .event_entries()
            .iter()
            .flat_map(|entry| entry.render_lines_for_width(160))
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        for command in slash_help_rows() {
            assert!(rendered.contains(&command.usage));
            assert!(rendered.contains(&command.description));
        }
    }

    #[test]
    fn workflows_command_renders_catalog_details() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let mut state = AppState::new(config);

        show_workflows(&mut state, &runtime).unwrap();
        let rendered = state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(state.status().contains("workflow"));
        assert!(rendered.contains("known workflows"));
        assert!(rendered.contains("default"));
        assert!(rendered.contains("description:"));
        assert!(rendered.contains("entry:"));
        assert!(rendered.contains("root:"));
    }

    #[tokio::test]
    async fn runs_submission_eventually_renders_structured_runtime_summaries_after_background_drain()
     {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let store = RedbRunStore::create(&config.workflow_store).unwrap();
        let mut state = AppState::new(config);

        seed_run(
            &store,
            workflow_run(
                "run-completed",
                Some("Ship deployment"),
                RunStatus::Completed,
                "done",
                Some("record-completed"),
            ),
        );
        seed_run(
            &store,
            workflow_run(
                "run-waiting",
                Some("Approve release"),
                RunStatus::WaitingForInput {
                    step: "approval".to_string(),
                    prompt_id: "prompt-42".to_string(),
                    message: "Approve the deployment?".to_string(),
                    choices: vec!["yes".to_string(), "no".to_string()],
                    resume_callback: ResumeCallback::new(
                        "ask_user",
                        serde_json::json!({ "prompt_id": "prompt-42" }),
                    )
                    .unwrap(),
                },
                "approval",
                Some("record-waiting"),
            ),
        );
        seed_run(
            &store,
            workflow_run(
                "run-failed",
                Some("Diagnose failure"),
                RunStatus::Failed {
                    reason: "agent command exited 2".to_string(),
                },
                "deploy",
                Some("record-failed"),
            ),
        );

        state.push_input("/runs");
        submit_input(&mut state, &runtime).await;
        assert_eq!(state.background_task_count(), 1);

        drain_finished_background_task(&mut state).await;

        assert_eq!(state.status(), "3 run(s)");
        let run_cards = state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
            .collect::<Vec<_>>();
        assert_eq!(
            run_cards.len(),
            4,
            "expected loading card plus one card per run: {run_cards:#?}"
        );
        assert_rendered_contains(&run_cards[0], "● Runs");
        assert_rendered_contains(&run_cards[0], "Loading runs");

        let completed_card = run_cards
            .iter()
            .find(|card| card.contains("run-completed"))
            .map(String::as_str)
            .expect("completed run card missing");
        let waiting_card = run_cards
            .iter()
            .find(|card| card.contains("run-waiting"))
            .map(String::as_str)
            .expect("waiting run card missing");
        let failed_card = run_cards
            .iter()
            .find(|card| card.contains("run-failed"))
            .map(String::as_str)
            .expect("failed run card missing");

        for expected in [
            "run-completed",
            "topic: Ship deployment",
            "workflow: deploy",
            "current_step: done",
            "head: record-completed",
            "status: completed",
        ] {
            assert_rendered_contains(completed_card, expected);
        }

        for unexpected in ["run-waiting", "run-failed"] {
            assert!(
                !completed_card.contains(unexpected),
                "completed card leaked {unexpected:?}:\n{completed_card}"
            );
        }

        for expected in [
            "run-waiting",
            "topic: Approve release",
            "workflow: deploy",
            "current_step: approval",
            "head: record-waiting",
            "status: waiting_for_input",
            "status.waiting_step: approval",
            "status.prompt_id: prompt-42",
            "status.message: Approve the deployment?",
            "status.choices: yes, no",
        ] {
            assert_rendered_contains(waiting_card, expected);
        }

        for unexpected in ["run-completed", "run-failed"] {
            assert!(
                !waiting_card.contains(unexpected),
                "waiting card leaked {unexpected:?}:\n{waiting_card}"
            );
        }

        for expected in [
            "run-failed",
            "topic: Diagnose failure",
            "workflow: deploy",
            "current_step: deploy",
            "head: record-failed",
            "status: failed",
            "status.reason: agent command exited 2",
        ] {
            assert_rendered_contains(failed_card, expected);
        }

        for unexpected in ["run-completed", "run-waiting"] {
            assert!(
                !failed_card.contains(unexpected),
                "failed card leaked {unexpected:?}:\n{failed_card}"
            );
        }

        for card in [completed_card, waiting_card, failed_card] {
            for debug_fragment in ["WaitingForInput {", "Failed {", "resume_callback:"] {
                assert!(
                    !card.contains(debug_fragment),
                    "rendered /runs card leaked Rust debug fragment {debug_fragment:?}:\n{card}"
                );
            }
        }
    }

    #[tokio::test]
    async fn runs_submission_filters_by_partial_run_id_after_background_drain() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let store = RedbRunStore::create(&config.workflow_store).unwrap();
        let mut matching_state = AppState::new(config.clone());

        seed_run(
            &store,
            workflow_run(
                "run-completed",
                Some("Ship deployment"),
                RunStatus::Completed,
                "done",
                Some("record-completed"),
            ),
        );
        seed_run(
            &store,
            workflow_run(
                "run-waiting",
                Some("Approve release"),
                RunStatus::WaitingForInput {
                    step: "approval".to_string(),
                    prompt_id: "prompt-42".to_string(),
                    message: "Approve the deployment?".to_string(),
                    choices: vec!["yes".to_string(), "no".to_string()],
                    resume_callback: ResumeCallback::new(
                        "ask_user",
                        serde_json::json!({ "prompt_id": "prompt-42" }),
                    )
                    .unwrap(),
                },
                "approval",
                Some("record-waiting"),
            ),
        );

        matching_state.push_input("/runs waiting");
        submit_input(&mut matching_state, &runtime).await;
        assert_eq!(matching_state.background_task_count(), 1);

        drain_finished_background_task(&mut matching_state).await;

        assert_eq!(matching_state.status(), "1 run(s)");
        let rendered = rendered_entries(&matching_state);
        assert_rendered_contains(&rendered, "run-waiting");
        assert_rendered_contains(&rendered, "topic: Approve release");
        assert!(
            !rendered.contains("run-completed"),
            "filtered /runs leaked a nonmatching run:\n{rendered}"
        );

        let mut empty_state = AppState::new(config);
        empty_state.push_input("/runs missing");
        submit_input(&mut empty_state, &runtime).await;
        assert_eq!(empty_state.background_task_count(), 1);

        drain_finished_background_task(&mut empty_state).await;

        assert_eq!(empty_state.status(), "0 run(s)");
        let rendered = rendered_entries(&empty_state);
        assert_rendered_contains(&rendered, "matching runs for missing: 0");
        assert!(
            !rendered.contains("known runs: 0"),
            "filtered empty state reused unfiltered empty text:\n{rendered}"
        );
        assert!(
            !rendered.contains("run-completed") && !rendered.contains("run-waiting"),
            "filtered empty state leaked run ids:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn runs_submission_eventually_renders_empty_state_card_after_background_drain() {
        let (_dir, runtime, mut state) = test_runtime_state();

        state.push_input("/runs");
        submit_input(&mut state, &runtime).await;
        assert_eq!(state.background_task_count(), 1);

        drain_finished_background_task(&mut state).await;

        assert_eq!(state.status(), "0 run(s)");
        assert_eq!(state.event_entries().len(), 2);
        let rendered = rendered_entries(&state);
        assert_rendered_contains(&rendered, "Runs");
        assert_rendered_contains(&rendered, "known runs: 0");
        assert!(
            !rendered.contains("run-"),
            "empty runs card should not render a per-run card:\n{rendered}"
        );
    }

    #[test]
    fn slash_suggestions_filter_by_command_prefix() {
        let suggestions = slash_suggestions("/run")
            .into_iter()
            .map(|command| command.usage)
            .collect::<Vec<_>>();

        assert!(
            suggestions.contains(&"/run [--step] [--workflow <workflow-id>] <request>".to_string())
        );
        assert!(suggestions.contains(&"/runs [partial-run-id]".to_string()));

        assert!(!suggestions.iter().any(|usage| usage.contains("run-step")));
        assert!(
            !suggestions
                .iter()
                .any(|usage| usage.contains("run-workflow"))
        );
        assert!(!suggestions.iter().any(|usage| usage.starts_with("/answer")));
    }

    #[test]
    fn slash_suggestions_include_resume_usage() {
        let suggestions = slash_suggestions("/res")
            .into_iter()
            .map(|command| command.usage)
            .collect::<Vec<_>>();

        assert!(suggestions.contains(&"/resume <run-id>".to_string()));
    }

    #[tokio::test]
    async fn run_workflow_spawns_named_workflow_task() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        std::fs::create_dir(&workflow_dir).unwrap();
        std::fs::write(
            workflow_dir.join("alpha.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success", body = "alpha " .. ctx.request }
            end
            return workflow("alpha-declared", start)
            "#,
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("review.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success", body = "reviewed " .. ctx.request }
            end
            return workflow("review-declared", start)
            "#,
        )
        .unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()))
            .with_deterministic_selector();
        let mut state = AppState::new(config);

        state.push_input("/run --workflow review do work");
        submit_input(&mut state, &runtime).await;

        assert!(state.status().contains("run --workflow review"));
        assert_eq!(state.background_task_count(), 1);
        tokio::task::yield_now().await;
        assert!(state.drain_background_tasks().await);
        assert_eq!(state.background_task_count(), 0);
        assert_eq!(state.workflow_name(), Some("review"));
    }

    #[tokio::test]
    async fn missing_required_slash_args_show_usage_without_spawning_tasks() {
        for (input, usage) in [
            ("/run", "/run [--step] [--workflow <workflow-id>] <request>"),
            ("/step", "/step <run-id>"),
            ("/resume", "/resume <run-id>"),
            ("/answer", "/answer <run-id> <prompt-id> <answer>"),
            (
                "/answer run-1 prompt-1",
                "/answer <run-id> <prompt-id> <answer>",
            ),
            ("/improve", "/improve <run-id>"),
            (
                "/resolve",
                "/resolve <run-id> [status] [--field <name> <value>]... [--body <text>]",
            ),
            (
                "/run --workflow review",
                "/run [--step] [--workflow <workflow-id>] <request>",
            ),
        ] {
            let (_dir, runtime, mut state) = test_runtime_state();

            state.push_input(input);
            submit_input(&mut state, &runtime).await;

            assert_eq!(state.status(), format!("usage: {usage}"));
            assert_eq!(state.background_task_count(), 0);
            let rendered = state
                .event_entries()
                .iter()
                .map(|entry| entry.plain_text())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(rendered.contains("Usage"));
            assert!(rendered.contains(&format!("usage: {usage}")));
        }
    }

    #[tokio::test]
    async fn slash_resolve_forwards_typed_fields_and_renders_commands() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        std::fs::create_dir(&workflow_dir).unwrap();
        std::fs::write(
            workflow_dir.join("resolve-smoke.lua"),
            r#"
            local developer = role("developer", { instructions = "Implement" })
            local start = step("start", { role = developer })
            start.run = function(ctx)
              return action.agent {
                role = developer,
                prompt = "Do work",
                output = {
                  status = { "planned" },
                  fields = {
                    summary = "string",
                    retry = "boolean",
                    files = "array",
                    ["foo=bar"] = "string",
                    ["-review"] = "string",
                    [" review "] = "string"
                  },
                  required_fields = {
                    "summary", "retry", "files", "foo=bar", "-review", " review "
                  }
                }
              }
            end
            local finish = step("finish")
            finish.run = function(ctx)
              local fields = ctx.prev.fields
              return action.status {
                status = "success",
                fields = {
                  summary = fields.summary,
                  retry = fields.retry,
                  first_file = fields.files[1],
                  equals_name = fields["foo=bar"],
                  hyphen_name = fields["-review"],
                  spaced_name = fields[" review "]
                },
                body = ctx.prev.body
              }
            end
            start:on("planned", finish)
            return workflow("resolve-smoke", start)
            "#,
        )
        .unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 5,
                    max_visits_per_step: 5,
                    ..Default::default()
                },
            )]),
            agents: vec![AgentConfig {
                command: "definitely-missing-agent".to_string(),
                args: Vec::new(),
                ..AgentConfig::default()
            }],
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let mut state = AppState::new(config);

        runtime
            .start_run_with_workflow("resolve-smoke", "do it")
            .await
            .unwrap_err();
        let run_id = runtime.list_runs(None).unwrap()[0].run_id.clone();
        assert!(!state.workflow_execution_running());

        let options_input = format!("/resolve {run_id}");
        state.push_input(&options_input);
        submit_input(&mut state, &runtime).await;
        let rendered = state
            .event_entries()
            .iter()
            .flat_map(|entry| entry.render_lines_for_width(240))
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rendered.lines().next(), Some("✓ Resolve"), "{rendered}");
        assert!(
            rendered.contains(&format!("/resolve '{run_id}'")),
            "{rendered}"
        );
        assert!(rendered.contains("'planned'"), "{rendered}");
        for field in [
            "files", "retry", "summary", "foo=bar", "-review", " review ",
        ] {
            assert!(
                rendered.contains(&format!("field '{field}' '...'")),
                "{rendered}"
            );
        }

        let resolve_input = format!(
            "/resolve {run_id} planned --field summary \"manual resolution\" \
             --field retry false --field files '[\"src/a.rs\"]' \
             --field foo=bar equals-value --field -review -declined \
             --field \" review \" \" spaced value \" --body \"manual body\""
        );

        assert!(!state.workflow_execution_running());
        state.push_input(&resolve_input);
        submit_input(&mut state, &runtime).await;
        assert_eq!(state.background_task_count(), 1);
        assert_eq!(
            state.status(),
            format!("submitted resolve: {run_id} planned")
        );
        assert_last_entry_is_card(
            &state,
            "● Resolve · submitted resolve",
            &[&run_id, "planned"],
        );
        tokio::task::yield_now().await;
        assert!(state.drain_background_tasks().await);
        assert_eq!(runtime.list_runs(None).unwrap().len(), 1);

        let run = runtime.load_run(&run_id).unwrap();
        assert_eq!(run.status, RunStatus::Completed);
        let store = RedbRunStore::create(dir.path().join("state/workflow.redb")).unwrap();
        let record = store
            .get_object::<StepRecord>(run.head.as_ref().unwrap())
            .unwrap();
        let output = record.output.unwrap();
        assert_eq!(output.fields["summary"], "manual resolution");
        assert_eq!(output.fields["retry"], false);
        assert_eq!(output.fields["first_file"], "src/a.rs");
        assert_eq!(output.fields["equals_name"], "equals-value");
        assert_eq!(output.fields["hyphen_name"], "-declined");
        assert_eq!(output.fields["spaced_name"], " spaced value ");
        assert_eq!(output.body, "manual body");
    }

    #[tokio::test]
    async fn parser_errors_show_usage_without_starting_plain_text_run() {
        let (_dir, runtime, mut state) = test_runtime_state();

        state.push_input("/run \"unterminated");
        submit_input(&mut state, &runtime).await;

        assert_eq!(state.status(), "usage: unmatched quote in slash command");
        assert_eq!(state.background_task_count(), 0);
        let rendered = state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Usage"));
        assert!(rendered.contains("usage: unmatched quote in slash command"));
        assert!(!rendered.contains("submitted run:"));
    }

    #[tokio::test]
    async fn malformed_resolve_field_shows_reason_and_usage() {
        let (_dir, runtime, mut state) = test_runtime_state();

        state
            .push_input(r#"/resolve run-1 success --field credentials '{"token":"private-token"'"#);
        submit_input(&mut state, &runtime).await;

        assert_eq!(
            state.status(),
            "usage: /resolve <run-id> [status] [--field <name> <value>]... [--body <text>]"
        );
        assert_eq!(state.background_task_count(), 0);
        let rendered = rendered_entries(&state);
        assert!(
            rendered.contains("field \"credentials\" has malformed JSON value:"),
            "{rendered}"
        );
        assert!(
            rendered.contains("EOF while parsing an object"),
            "{rendered}"
        );
        assert!(rendered.contains("usage: /resolve"), "{rendered}");
        assert!(!rendered.contains("private-token"), "{rendered}");
    }

    #[tokio::test]
    async fn resolve_payload_without_status_is_rejected_before_dispatch() {
        for input in [
            "/resolve run-1 --field summary one --field summary two",
            "/resolve run-1 --body details",
        ] {
            let (_dir, runtime, mut state) = test_runtime_state();

            state.push_input(input);
            submit_input(&mut state, &runtime).await;

            assert_eq!(
                state.status(),
                "usage: /resolve <run-id> [status] [--field <name> <value>]... [--body <text>]"
            );
            assert_eq!(state.background_task_count(), 0);
            let rendered = rendered_entries(&state);
            assert!(rendered.contains("required arguments"), "{rendered}");
            assert!(rendered.contains("usage: /resolve"), "{rendered}");
        }
    }

    #[tokio::test]
    async fn duplicate_resolve_fields_with_status_are_actionable() {
        let (_dir, runtime, mut state) = test_runtime_state();

        state.push_input("/resolve run-1 success --field summary one --field summary two");
        submit_input(&mut state, &runtime).await;

        assert_eq!(
            state.status(),
            "usage: /resolve <run-id> [status] [--field <name> <value>]... [--body <text>]"
        );
        assert_eq!(state.background_task_count(), 0);
        let rendered = rendered_entries(&state);
        assert!(rendered.contains("provided more than once"), "{rendered}");
        assert!(rendered.contains("usage: /resolve"), "{rendered}");
    }

    #[tokio::test]
    async fn run_and_plain_text_keep_selector_backed_start_labels() {
        for (input, expected_status) in [
            ("/run do work", "submitted run: do work"),
            ("/run --step do work", "submitted run --step: do work"),
            ("do work", "submitted run: do work"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let config = AppConfig {
                state_dir: dir.path().join("state"),
                workflow_store: dir.path().join("state/workflow.redb"),
                workflow_dirs: Vec::new(),
                config_sets: std::collections::BTreeMap::from([(
                    "default".to_string(),
                    crate::config::ConfigSetConfig {
                        max_steps_per_run: 1,
                        max_visits_per_step: 1,
                        ..Default::default()
                    },
                )]),
                ..AppConfig::default()
            };
            let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()))
                .with_deterministic_selector();
            let mut state = AppState::new(config);

            state.push_input(input);
            submit_input(&mut state, &runtime).await;

            assert_eq!(state.status(), expected_status);
            assert_eq!(state.background_task_count(), 1);
            state.cancel_background_tasks();
        }
    }

    #[tokio::test]
    async fn run_command_paths_display_runtime_supplied_topic_only() {
        for (input, expected_status) in [
            ("do work", "submitted run: do work"),
            ("/run do work", "submitted run: do work"),
            ("/run --step do work", "submitted run --step: do work"),
            (
                "/run --workflow review do work",
                "submitted run --workflow review: do work",
            ),
        ] {
            let (_dir, runtime, mut state) = test_runtime_state();

            state.push_input(input);
            submit_input(&mut state, &runtime).await;

            assert_eq!(state.status(), expected_status);
            assert_eq!(state.background_task_count(), 1);
            assert_eq!(crate::app::header::text(&state, 120), "Cowboy");
            assert_eq!(state.current_run_topic(), None);

            state.apply_workflow_event(WorkflowEvent::new(
                "run-topic",
                WorkflowEventKind::RunStarted {
                    workflow_name: "default".to_string(),
                    current_step: "start".to_string(),
                    request_topic: Some("Agent supplied topic".to_string()),
                },
            ));

            assert_eq!(state.current_run_topic(), Some("Agent supplied topic"));
            assert_eq!(
                crate::app::header::text(&state, 120),
                "Cowboy · Agent supplied topic"
            );
            state.cancel_background_tasks();
        }
    }

    #[tokio::test]
    async fn pending_prompt_answer_fallback_spawns_answer_task_and_clears_target() {
        let (_dir, runtime, mut state) = test_runtime_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "pending-run",
            WorkflowEventKind::WaitingForInput {
                step: "approve".to_string(),
                prompt_id: "prompt-42".to_string(),
                message: "Approve?".to_string(),
                choices: vec![],
            },
        ));
        assert_eq!(
            state.pending_prompt_answer_target(),
            Some(("pending-run".to_string(), "prompt-42".to_string()))
        );

        state.push_input("answer with spaces");
        submit_input(&mut state, &runtime).await;

        assert_eq!(state.status(), "submitted answer: pending-run prompt-42");
        assert_eq!(state.background_task_count(), 1);
        assert_eq!(state.pending_prompt_answer_target(), None);
        let rendered = assert_last_entry_is_card(
            &state,
            "○ Answer · submitted answer",
            &["pending-run", "prompt-42"],
        );
        assert!(!rendered.contains("answer with spaces"), "{rendered}");
        state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn explicit_answer_slash_command_preempts_pending_prompt_fallback() {
        let (_dir, runtime, mut state) = test_runtime_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "pending-run",
            WorkflowEventKind::WaitingForInput {
                step: "approve".to_string(),
                prompt_id: "pending-prompt".to_string(),
                message: "Approve?".to_string(),
                choices: vec![],
            },
        ));

        state.push_input("/answer explicit-run explicit-prompt \"answer with spaces\"");
        submit_input(&mut state, &runtime).await;

        assert_eq!(
            state.status(),
            "submitted answer: explicit-run explicit-prompt"
        );
        assert_eq!(state.background_task_count(), 1);
        assert_eq!(state.pending_prompt_answer_target(), None);
        let rendered = assert_last_entry_is_card(
            &state,
            "○ Answer · submitted answer",
            &["explicit-run", "explicit-prompt"],
        );
        assert!(!rendered.contains("answer with spaces"), "{rendered}");
        state.cancel_background_tasks();
    }
    #[test]
    fn complete_slash_suggestion_updates_input() {
        let mut state = test_state();
        state.push_input("/ru");

        complete_slash_suggestion(&mut state);

        assert_eq!(state.input(), "/run ");
    }

    #[tokio::test]
    async fn bare_resume_shows_required_run_id_usage_without_spawning_task() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let mut state = AppState::new(config);

        state.push_input("/resume");
        submit_input(&mut state, &runtime).await;

        assert_eq!(state.status(), "usage: /resume <run-id>");
        assert_eq!(state.background_task_count(), 0);
        let rendered = state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Usage"));
        assert!(rendered.contains("usage: /resume <run-id>"));
    }

    #[tokio::test]
    async fn empty_submit_is_inert() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let mut state = AppState::new(config);

        submit_input(&mut state, &runtime).await;

        assert!(state.event_entries().is_empty());
        assert!(state.history_is_empty());
        assert!(!dir.path().join("state/input_history").exists());
        assert!(!dir.path().join("state/input_history.lock").exists());
    }

    #[tokio::test]
    async fn valid_idle_lifecycle_states_dispatch_step_resume_answers_and_terminal_requests() {
        async fn apply_finished_report(state: &mut AppState, report: RunReport) {
            state.spawn_test_card_report_task("seed report".to_string(), async move { Ok(report) });
            tokio::task::yield_now().await;
            assert!(state.drain_background_tasks().await);
            assert!(!state.workflow_execution_running());
        }

        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        std::fs::create_dir(&workflow_dir).unwrap();
        std::fs::write(
            workflow_dir.join("two.lua"),
            r#"
            local first = step("first")
            first.run = function(ctx)
              return action.status { status = "next" }
            end
            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success" }
            end
            first:on("next", finish)
            return workflow("two", first)
            "#,
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("ask.lua"),
            r#"
            local ask = step("ask")
            ask.run = function(ctx)
              return action.ask_user { id = "approval", message = "Approve?", status = "answered" }
            end
            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = ctx.prev.fields.answer }
            end
            ask:on("answered", finish)
            return workflow("ask", ask)
            "#,
        )
        .unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 8,
                    max_visits_per_step: 8,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()))
            .with_deterministic_selector();

        let step_report = runtime
            .start_run_with_workflow_stepwise("two", "step request")
            .await
            .unwrap();
        let step_run_id = step_report.run.id.clone();
        assert_eq!(step_report.run.status, RunStatus::Running);
        let mut step_state = AppState::new(config.clone());
        apply_finished_report(&mut step_state, step_report).await;
        step_state.push_input(&format!("/step {step_run_id}"));
        submit_input(&mut step_state, &runtime).await;
        assert_eq!(step_state.background_task_count(), 1);
        tokio::task::yield_now().await;
        assert!(step_state.drain_background_tasks().await);
        assert_eq!(
            runtime.load_run(&step_run_id).unwrap().status,
            RunStatus::Completed
        );

        step_state.push_input("new request after completion");
        submit_input(&mut step_state, &runtime).await;
        assert_eq!(step_state.background_task_count(), 1);
        assert_eq!(step_state.input(), "");

        let resume_report = runtime
            .start_run_with_workflow_stepwise("two", "resume request")
            .await
            .unwrap();
        let resume_run_id = resume_report.run.id.clone();
        let mut resume_state = AppState::new(config.clone());
        apply_finished_report(&mut resume_state, resume_report).await;
        resume_state.push_input(&format!("/resume {resume_run_id}"));
        submit_input(&mut resume_state, &runtime).await;
        assert_eq!(resume_state.background_task_count(), 1);
        tokio::task::yield_now().await;
        assert!(resume_state.drain_background_tasks().await);
        assert_eq!(
            runtime.load_run(&resume_run_id).unwrap().status,
            RunStatus::Completed
        );

        let waiting = runtime
            .start_run_with_workflow("ask", "plain answer request")
            .await
            .unwrap();
        let plain_answer_run = waiting.run.id.clone();
        let mut answer_state = AppState::new(config.clone());
        apply_finished_report(&mut answer_state, waiting).await;
        answer_state.push_input("yes");
        submit_input(&mut answer_state, &runtime).await;
        assert_eq!(answer_state.background_task_count(), 1);
        tokio::task::yield_now().await;
        assert!(answer_state.drain_background_tasks().await);
        assert_eq!(
            runtime.load_run(&plain_answer_run).unwrap().status,
            RunStatus::Completed
        );

        let waiting = runtime
            .start_run_with_workflow("ask", "explicit answer request")
            .await
            .unwrap();
        let explicit_answer_run = waiting.run.id.clone();
        let prompt_id = match &waiting.run.status {
            RunStatus::WaitingForInput { prompt_id, .. } => prompt_id.clone(),
            status => panic!("expected waiting run, got {status:?}"),
        };
        let mut explicit_state = AppState::new(config.clone());
        apply_finished_report(&mut explicit_state, waiting).await;
        explicit_state.push_input(&format!(
            "/answer {explicit_answer_run} {prompt_id} explicit"
        ));
        submit_input(&mut explicit_state, &runtime).await;
        assert_eq!(explicit_state.background_task_count(), 1);
        tokio::task::yield_now().await;
        assert!(explicit_state.drain_background_tasks().await);
        assert_eq!(
            runtime.load_run(&explicit_answer_run).unwrap().status,
            RunStatus::Completed
        );

        let mut cancelled_state = AppState::new(config);
        cancelled_state.apply_workflow_event(WorkflowEvent::new(
            "cancelled-run",
            WorkflowEventKind::RunCancelled,
        ));
        cancelled_state.push_input("new request after cancellation");
        submit_input(&mut cancelled_state, &runtime).await;
        assert_eq!(cancelled_state.background_task_count(), 1);
        assert_eq!(cancelled_state.input(), "");

        step_state.cancel_background_tasks();
        cancelled_state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn idle_requests_answers_and_allowed_slash_history_remain_trimmed() {
        async fn apply_finished_report(state: &mut AppState, report: RunReport) {
            state.spawn_test_card_report_task("seed report".to_string(), async move { Ok(report) });
            tokio::task::yield_now().await;
            assert!(state.drain_background_tasks().await);
        }

        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        std::fs::create_dir(&workflow_dir).unwrap();
        std::fs::write(
            workflow_dir.join("aaa-idle.lua"),
            r#"
            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success" }
            end
            return workflow("idle", finish)
            "#,
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("ask.lua"),
            r#"
            local ask = step("ask")
            ask.run = function(ctx)
              return action.ask_user { id = "approval", message = "Approve?", status = "answered" }
            end
            local finish = step("finish")
            finish.run = function(ctx)
              return action.status { status = "success", body = ctx.prev.fields.answer }
            end
            ask:on("answered", finish)
            return workflow("ask", ask)
            "#,
        )
        .unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig::default(),
            )]),
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()))
            .with_deterministic_selector();
        let mut idle_state = AppState::new(config.clone());
        idle_state.push_input("  request  ");

        submit_input(&mut idle_state, &runtime).await;
        tokio::task::yield_now().await;
        assert!(idle_state.drain_background_tasks().await);

        let idle_run = runtime.list_runs(None).unwrap().remove(0);
        assert_eq!(
            runtime.load_run(&idle_run.run_id).unwrap().original_request,
            "request"
        );

        let waiting = runtime
            .start_run_with_workflow("ask", "ask request")
            .await
            .unwrap();
        let answered_run_id = waiting.run.id.clone();
        let mut answer_state = AppState::new(config.clone());
        apply_finished_report(&mut answer_state, waiting).await;
        answer_state.push_input("  yes  ");

        submit_input(&mut answer_state, &runtime).await;
        tokio::task::yield_now().await;
        assert!(answer_state.drain_background_tasks().await);

        let answered = runtime.load_run(&answered_run_id).unwrap();
        let store = RedbRunStore::create(dir.path().join("state/workflow.redb")).unwrap();
        let record = store
            .get_object::<StepRecord>(answered.head.as_ref().unwrap())
            .unwrap();
        assert_eq!(record.output.unwrap().body, "yes");

        answer_state.push_input("  /runs  ");
        submit_input(&mut answer_state, &runtime).await;
        assert_eq!(answer_state.background_task_count(), 1);
        drain_finished_background_task(&mut answer_state).await;

        let mut history_state = AppState::new(config);
        history_state.history_previous();
        assert_eq!(history_state.input(), "/runs");
        history_state.history_previous();
        assert_eq!(history_state.input(), "yes");
        history_state.history_previous();
        assert_eq!(history_state.input(), "request");
    }

    #[tokio::test]
    async fn synchronous_idle_dispatch_error_clears_and_records_trimmed_submission() {
        let (dir, runtime, mut state) = test_runtime_state();
        state.push_input("  /resolve missing-run  ");

        submit_input(&mut state, &runtime).await;

        assert_eq!(state.input(), "");
        assert_eq!(state.background_task_count(), 0);
        assert!(state.status().starts_with("error: "), "{}", state.status());
        assert!(state.status().contains("missing-run"), "{}", state.status());
        let error_card = state.event_entries().last().unwrap().plain_text();
        assert_eq!(error_card.lines().next(), Some("✗ Error"), "{error_card}");
        assert!(error_card.contains(state.status()), "{error_card}");

        let history = std::fs::read_to_string(dir.path().join("state/input_history")).unwrap();
        assert!(
            history.contains(r#""entry":"/resolve missing-run""#),
            "{history}"
        );
        assert!(!history.contains("  /resolve missing-run  "), "{history}");
    }

    #[tokio::test]
    async fn active_agent_prompt_is_persisted_verbatim_without_starting_another_task() {
        let (dir, runtime, mut state) = test_runtime_state();
        let store = RedbRunStore::create(dir.path().join("state/workflow.redb")).unwrap();
        let run = workflow_run("run-live", None, RunStatus::Running, "implement", None);
        seed_run(&store, run.clone());
        let window = AgentPromptWindow {
            window_id: "window-live".to_string(),
            run_id: run.id.clone(),
            step_record_id: "record-live".to_string(),
            step_id: "implement".to_string(),
            role_id: "developer".to_string(),
            baseline_sequence: 0,
            applied_sequence: 0,
            opened_at: Utc::now(),
            sealed_at: None,
        };
        store.open_agent_prompt_window(window.clone()).unwrap();
        state.spawn_test_card_report_task("running".to_string(), async {
            std::future::pending::<Result<RunReport, String>>().await
        });
        state.apply_workflow_event(WorkflowEvent::new(
            &run.id,
            WorkflowEventKind::RunStarted {
                workflow_name: run.workflow_name.clone(),
                current_step: run.current_step.clone(),
                request_topic: None,
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            &run.id,
            WorkflowEventKind::AgentPromptWindowOpened {
                step_id: "implement".to_string(),
                role: "developer".to_string(),
                window_id: window.window_id.clone(),
            },
        ));
        let prompt = "  revise this\nwithout trimming  ";
        state.push_input(prompt);

        submit_input(&mut state, &runtime).await;

        assert_eq!(state.input(), "");
        assert_eq!(state.background_task_count(), 1);
        assert!(!state.history_is_empty());
        let prompts = store.load_user_prompts(&run.id).unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].content, prompt);
        assert_eq!(prompts[0].sequence, 1);
        let rendered = rendered_entries(&state);
        assert!(rendered.contains("revise this"));
        assert!(rendered.contains("without trimming"));
        state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn rejected_active_prompt_retains_exact_draft_and_history() {
        let (dir, runtime, mut state) = test_runtime_state();
        let store = RedbRunStore::create(dir.path().join("state/workflow.redb")).unwrap();
        let run = workflow_run("run-live", None, RunStatus::Running, "implement", None);
        seed_run(&store, run.clone());
        store
            .open_agent_prompt_window(AgentPromptWindow {
                window_id: "current-window".to_string(),
                run_id: run.id.clone(),
                step_record_id: "record-live".to_string(),
                step_id: "implement".to_string(),
                role_id: "developer".to_string(),
                baseline_sequence: 0,
                applied_sequence: 0,
                opened_at: Utc::now(),
                sealed_at: None,
            })
            .unwrap();
        state.spawn_test_card_report_task("running".to_string(), async {
            std::future::pending::<Result<RunReport, String>>().await
        });
        state.apply_workflow_event(WorkflowEvent::new(
            &run.id,
            WorkflowEventKind::RunStarted {
                workflow_name: run.workflow_name.clone(),
                current_step: run.current_step.clone(),
                request_topic: None,
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            &run.id,
            WorkflowEventKind::AgentPromptWindowOpened {
                step_id: "implement".to_string(),
                role: "developer".to_string(),
                window_id: "stale-window".to_string(),
            },
        ));
        let draft = "  keep rejected draft  ";
        state.push_input(draft);

        submit_input(&mut state, &runtime).await;

        assert_eq!(state.input(), draft);
        assert!(state.history_is_empty());
        assert!(store.load_user_prompts(&run.id).unwrap().is_empty());
        assert!(state.status().contains("draft retained"));
        state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn active_execution_rejects_mutating_commands_but_allows_read_only_commands() {
        let (_dir, runtime, mut state) = test_runtime_state();
        state.spawn_test_card_report_task("running".to_string(), async {
            std::future::pending::<Result<RunReport, String>>().await
        });
        state.push_input("/step run-1");

        submit_input(&mut state, &runtime).await;

        assert_eq!(state.input(), "/step run-1");
        assert_eq!(state.background_task_count(), 1);
        assert!(state.history_is_empty());
        state.replace_input_from_completion("/runs".to_string());

        submit_input(&mut state, &runtime).await;

        assert_eq!(state.input(), "");
        assert_eq!(state.background_task_count(), 2);
        drain_finished_background_task(&mut state).await;
        assert_eq!(state.background_task_count(), 1);
        assert!(rendered_entries(&state).contains("known runs: 0"));
        state.cancel_background_tasks();
    }

    #[test]
    fn execution_command_policy_matches_control_matrix() {
        for input in [
            "/cancel",
            "/help",
            "/exit",
            "/runs",
            "/workflows",
            "/resolve run-1",
        ] {
            let command = parse_slash_command(input).unwrap();
            assert!(!command_conflicts_with_execution(&command), "{input}");
        }
        for input in [
            "/run do work",
            "/step run-1",
            "/resume run-1",
            "/answer run-1 prompt answer",
            "/improve run-1",
            "/resolve run-1 success",
        ] {
            let command = parse_slash_command(input).unwrap();
            assert!(command_conflicts_with_execution(&command), "{input}");
        }
    }
}
