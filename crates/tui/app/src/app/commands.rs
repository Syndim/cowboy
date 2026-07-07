use anyhow::Result;
use cowboy_command_parser::{
    SLASH_COMMANDS, SlashCommand, SlashParseError, parse_slash_command, slash_command_metadata,
    slash_suggestions,
};
use cowboy_workflow_engine::WorkflowRuntime;

use super::state::AppState;

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
    let status = match err {
        SlashParseError::UnmatchedQuote => "usage: unmatched quote in slash command".to_string(),
        SlashParseError::Validation { command, message } => command
            .as_deref()
            .and_then(slash_command_metadata)
            .map(|command| format!("usage: {}", command.usage))
            .unwrap_or_else(|| first_error_line(&message)),
    };

    state.set_status(status);
    state.push_card("Usage", [state.status().to_string()]);
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
    let Some(input) = state.take_submitted_input() else {
        return;
    };

    let result = dispatch_submitted_input(state, runtime, &input).await;
    if let Err(err) = result {
        state.set_status(format!("error: {err}"));
        state.push_card("Error", [state.status().to_string()]);
    }
}

async fn dispatch_submitted_input(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    input: &str,
) -> Result<()> {
    if input.starts_with('/') {
        let command = match parse_slash_command(input) {
            Ok(command) => command,
            Err(err) => {
                show_slash_parse_error(state, err);
                return Ok(());
            }
        };

        dispatch_slash_command(state, runtime, command).await?;
    } else if let Some((run_id, prompt_id)) = state.pending_prompt_answer_target() {
        spawn_answer_task(state, runtime, run_id, prompt_id, input.to_string());
    } else {
        spawn_start_run(state, runtime, input.to_string());
    }

    Ok(())
}

async fn dispatch_slash_command(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    command: SlashCommand,
) -> Result<()> {
    match command {
        SlashCommand::Run { request } => spawn_start_run(state, runtime, request),
        SlashCommand::RunWorkflow {
            workflow_id,
            request,
        } => spawn_start_run_with_workflow(state, runtime, workflow_id, request),
        SlashCommand::RunStep { request } => spawn_start_run_stepwise(state, runtime, request),
        SlashCommand::Step { run_id } => spawn_step_run(state, runtime, run_id),
        SlashCommand::Resume { run_id } => submit_resume_run(state, runtime, run_id),
        SlashCommand::Answer {
            run_id,
            prompt_id,
            answer,
        } => spawn_answer_task(state, runtime, run_id, prompt_id, answer),
        SlashCommand::Runs => show_runs(state, runtime)?,
        SlashCommand::Workflows => show_workflows(state, runtime)?,
        SlashCommand::Improve { run_id } => improve_run(state, runtime, &run_id).await?,
        SlashCommand::Resolve {
            run_id,
            status,
            fields_json,
        } => resolve_run(state, runtime, run_id, status, fields_json).await?,
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

fn spawn_start_run(state: &mut AppState, runtime: &WorkflowRuntime, request: String) {
    let runtime = runtime.clone();
    state.spawn_report_task(format!("submitted run: {request}"), async move {
        runtime
            .start_run(request)
            .await
            .map_err(|err| err.to_string())
    });
}

fn spawn_start_run_stepwise(state: &mut AppState, runtime: &WorkflowRuntime, request: String) {
    let runtime = runtime.clone();
    state.spawn_report_task(format!("submitted run-step: {request}"), async move {
        runtime
            .start_run_stepwise(request)
            .await
            .map_err(|err| err.to_string())
    });
}

fn spawn_start_run_with_workflow(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    workflow_id: String,
    request: String,
) {
    let runtime = runtime.clone();
    state.spawn_report_task(
        format!("submitted run-workflow {workflow_id}: {request}"),
        async move {
            runtime
                .start_run_with_workflow(workflow_id, request)
                .await
                .map_err(|err| err.to_string())
        },
    );
}

fn spawn_step_run(state: &mut AppState, runtime: &WorkflowRuntime, run_id: String) {
    let runtime = runtime.clone();
    state.spawn_report_task(format!("submitted step: {run_id}"), async move {
        runtime
            .step_run(&run_id)
            .await
            .map_err(|err| err.to_string())
    });
}

fn submit_resume_run(state: &mut AppState, runtime: &WorkflowRuntime, run_id: Option<String>) {
    let run_id = run_id
        .filter(|run_id| !run_id.is_empty())
        .or_else(|| state.active_run_id().map(str::to_string));
    let Some(run_id) = run_id else {
        state.set_status("usage: /resume [run-id]");
        state.push_card("Usage", [state.status().to_string()]);
        return;
    };

    spawn_resume_run(state, runtime, run_id);
}

fn spawn_resume_run(state: &mut AppState, runtime: &WorkflowRuntime, run_id: String) {
    let runtime = runtime.clone();
    state.spawn_report_task(format!("submitted resume: {run_id}"), async move {
        runtime
            .resume_run(&run_id)
            .await
            .map_err(|err| err.to_string())
    });
}

fn spawn_answer_task(
    state: &mut AppState,
    runtime: &WorkflowRuntime,
    run_id: String,
    prompt_id: String,
    answer: String,
) {
    let runtime = runtime.clone();
    state.clear_pending_prompt();
    state.spawn_report_task(
        format!("submitted answer: {run_id} {prompt_id}"),
        async move {
            runtime
                .answer_run(&run_id, &prompt_id, &answer)
                .await
                .map_err(|err| err.to_string())
        },
    );
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
    fields_raw: Option<String>,
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
            }
            details.push(format!(
                "resolve with: cowboy resolve {} <status> [--fields '<json>']",
                options.run_id
            ));
            state.push_card("Resolve", details);
            Ok(())
        }
        Some(status) => {
            let fields = match fields_raw {
                Some(raw) => Some(
                    serde_json::from_str(&raw)
                        .map_err(|err| anyhow::anyhow!("invalid fields JSON: {err}"))?,
                ),
                None => None,
            };
            let runtime = runtime.clone();
            state.spawn_report_task(
                format!("submitted resolve: {run_id} {status}"),
                async move {
                    runtime
                        .resolve_run(&run_id, &status, fields, None)
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
        SLASH_COMMANDS
            .iter()
            .map(|command| format!("{:<28} {}", command.usage, command.description)),
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

fn show_runs(state: &mut AppState, runtime: &WorkflowRuntime) -> Result<()> {
    let runs = runtime.list_runs()?;
    state.set_status(format!("{} run(s)", runs.len()));
    let mut details = vec![format!("known runs: {}", runs.len())];
    for run in runs {
        details.push(run.run_id);
        details.push(format!("  workflow: {}", run.workflow_name));
        details.push(format!("  status: {:?}", run.status));
        details.push(format!("  step: {}", run.current_step));
    }
    state.push_card("Runs", details);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

    fn test_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(AppConfig {
            state_dir: dir.path().to_path_buf(),
            workflow_store: dir.path().join("workflow.redb"),
            max_steps_per_run: 1,
            max_visits_per_step: 1,
            ..AppConfig::default()
        })
    }

    fn test_runtime_state() -> (tempfile::TempDir, WorkflowRuntime, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            max_steps_per_run: 1,
            max_visits_per_step: 1,
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let state = AppState::new(config);
        (dir, runtime, state)
    }

    #[test]
    fn help_uses_slash_command_metadata() {
        let mut state = test_state();

        show_help(&mut state);
        let rendered = state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
            .collect::<Vec<_>>()
            .join("\n");

        for command in SLASH_COMMANDS {
            assert!(rendered.contains(command.usage));
            assert!(rendered.contains(command.description));
        }
    }

    #[test]
    fn workflows_command_renders_catalog_details() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            max_steps_per_run: 1,
            max_visits_per_step: 1,
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

    #[test]
    fn slash_suggestions_filter_by_command_prefix() {
        let suggestions = slash_suggestions("/run")
            .into_iter()
            .map(|command| command.usage)
            .collect::<Vec<_>>();

        assert!(suggestions.contains(&"/run <request>"));
        assert!(suggestions.contains(&"/run-step <request>"));
        assert!(suggestions.contains(&"/runs"));
        assert!(suggestions.contains(&"/run-workflow <workflow-id> <request>"));

        assert!(!suggestions.contains(&"/answer <run> <id> <answer>"));
    }

    #[test]
    fn slash_suggestions_include_resume_usage() {
        let suggestions = slash_suggestions("/res")
            .into_iter()
            .map(|command| command.usage)
            .collect::<Vec<_>>();

        assert!(suggestions.contains(&"/resume [run-id]"));
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
            max_steps_per_run: 5,
            max_visits_per_step: 5,
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()))
            .with_deterministic_selector();
        let mut state = AppState::new(config);

        state.push_input("/run-workflow review do work");
        submit_input(&mut state, &runtime).await;

        assert!(state.status().contains("run-workflow review"));
        assert_eq!(state.background_task_count(), 1);
        tokio::task::yield_now().await;
        assert!(state.drain_background_tasks().await);
        assert_eq!(state.background_task_count(), 0);
        assert_eq!(state.workflow_name(), Some("review"));
    }

    #[tokio::test]
    async fn missing_required_slash_args_show_usage_without_spawning_tasks() {
        for (input, usage) in [
            ("/run", "/run <request>"),
            ("/run-step", "/run-step <request>"),
            ("/step", "/step <run-id>"),
            ("/answer", "/answer <run> <id> <answer>"),
            ("/answer run-1 prompt-1", "/answer <run> <id> <answer>"),
            ("/improve", "/improve <run-id>"),
            ("/resolve", "/resolve <run-id> [status] [fields-json]"),
            ("/run-workflow", "/run-workflow <workflow-id> <request>"),
            (
                "/run-workflow review",
                "/run-workflow <workflow-id> <request>",
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
    async fn run_and_plain_text_keep_selector_backed_start_labels() {
        for (input, expected_status) in [
            ("/run do work", "submitted run: do work"),
            ("do work", "submitted run: do work"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let config = AppConfig {
                state_dir: dir.path().join("state"),
                workflow_store: dir.path().join("state/workflow.redb"),
                workflow_dirs: Vec::new(),
                max_steps_per_run: 1,
                max_visits_per_step: 1,
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
        assert!(
            state
                .event_entries()
                .last()
                .is_some_and(|entry| entry.contains("submitted answer: pending-run prompt-42"))
        );
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
        assert!(
            state.event_entries().last().is_some_and(
                |entry| entry.contains("submitted answer: explicit-run explicit-prompt")
            )
        );
    }
    #[test]
    fn complete_slash_suggestion_updates_input() {
        let mut state = test_state();
        state.push_input("/ru");

        complete_slash_suggestion(&mut state);

        assert_eq!(state.input(), "/run ");
    }

    #[tokio::test]
    async fn explicit_resume_spawns_resume_labeled_background_task() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            max_steps_per_run: 1,
            max_visits_per_step: 1,
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let mut state = AppState::new(config);

        state.push_input("/resume run-123");
        submit_input(&mut state, &runtime).await;

        assert_eq!(state.status(), "submitted resume: run-123");
        assert_eq!(state.background_task_count(), 1);
        assert!(
            state
                .event_entries()
                .last()
                .is_some_and(|entry| entry.contains("submitted resume: run-123"))
        );
    }

    #[tokio::test]
    async fn bare_resume_uses_active_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflows");
        std::fs::create_dir(&workflow_dir).unwrap();
        std::fs::write(
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
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: vec![workflow_dir],
            max_steps_per_run: 5,
            max_visits_per_step: 5,
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()))
            .with_deterministic_selector();
        let start = runtime.start_run_stepwise("request").await.unwrap();
        let run_id = start.run.id.clone();
        let mut state = AppState::new(config);
        state.spawn_report_task("seed active run".to_string(), async move { Ok(start) });
        tokio::task::yield_now().await;
        assert!(state.drain_background_tasks().await);
        assert_eq!(state.active_run_id(), Some(run_id.as_str()));

        state.push_input("/resume");
        submit_input(&mut state, &runtime).await;

        assert_eq!(state.status(), format!("submitted resume: {run_id}"));
        assert_eq!(state.background_task_count(), 1);
    }

    #[tokio::test]
    async fn bare_resume_without_active_run_shows_usage_without_spawning_task() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            max_steps_per_run: 1,
            max_visits_per_step: 1,
            ..AppConfig::default()
        };
        let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()));
        let mut state = AppState::new(config);

        state.push_input("/resume");
        submit_input(&mut state, &runtime).await;

        assert_eq!(state.status(), "usage: /resume [run-id]");
        assert_eq!(state.background_task_count(), 0);
        let rendered = state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Usage"));
        assert!(rendered.contains("usage: /resume [run-id]"));
    }

    #[tokio::test]
    async fn empty_submit_is_inert() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            workflow_dirs: Vec::new(),
            max_steps_per_run: 1,
            max_visits_per_step: 1,
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
}
