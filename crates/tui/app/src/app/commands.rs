use crate::run_summary::render_run_summary_lines;
use anyhow::Result;
use cowboy_command_parser::{
    SharedCommand, SlashCommand, SlashParseError, parse_slash_command, slash_command_usage,
    slash_help_rows, slash_suggestions,
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
            .and_then(slash_command_usage)
            .map(|usage| format!("usage: {usage}"))
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
        SharedCommand::Runs => show_runs(state, runtime)?,
        SharedCommand::Improve(args) => improve_run(state, runtime, &args.run_id).await?,
        SharedCommand::Resolve(args) => {
            let cowboy_command_parser::ResolveArgs {
                run_id,
                status,
                fields,
                body,
                fields_json,
            } = args;
            resolve_run(state, runtime, run_id, status, fields.or(fields_json), body).await?;
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
    state.spawn_report_task(format!("submitted run: {request}"), async move {
        runtime
            .start_run(request)
            .await
            .map_err(|err| err.to_string())
    });
}

fn spawn_start_run_stepwise(state: &mut AppState, runtime: &WorkflowRuntime, request: String) {
    let runtime = runtime.clone();
    state.spawn_report_task(format!("submitted run --step: {request}"), async move {
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
        format!("submitted run --workflow {workflow_id}: {request}"),
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
    state.spawn_report_task(
        format!("submitted run --step --workflow {workflow_id}: {request}"),
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
    state.spawn_report_task(format!("submitted step: {run_id}"), async move {
        runtime
            .step_run(&run_id)
            .await
            .map_err(|err| err.to_string())
    });
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
            }
            details.push(format!(
                "resolve with: /resolve {} <status> [fields-json]",
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

fn show_runs(state: &mut AppState, runtime: &WorkflowRuntime) -> Result<()> {
    let runs = runtime.list_runs()?;
    state.set_status(format!("{} run(s)", runs.len()));
    if runs.is_empty() {
        state.push_card("Runs", ["known runs: 0".to_string()]);
    } else {
        for run in runs {
            state.push_card("Run", render_run_summary_lines(&run));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use chrono::Utc;
    use cowboy_workflow_core::{ResumeCallback, RunHead, RunStatus, WorkflowRun};
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};
    use cowboy_workflow_store::RedbRunStore;
    use serde_json::Value;

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

    fn rendered_entries(state: &AppState) -> String {
        state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
            .collect::<Vec<_>>()
            .join("\n")
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
            status,
            current_step: current_step.to_string(),
            head: head.map(ToString::to_string),
            resume: Value::Null,
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
    #[test]
    fn help_uses_generated_slash_command_rows() {
        let mut state = test_state();

        show_help(&mut state);
        let rendered = state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
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
    fn runs_command_renders_structured_runtime_summaries() {
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

        show_runs(&mut state, &runtime).unwrap();

        assert_eq!(state.status(), "3 run(s)");
        let run_cards = state
            .event_entries()
            .iter()
            .map(|entry| entry.plain_text())
            .collect::<Vec<_>>();
        assert_eq!(
            run_cards.len(),
            3,
            "expected one card per run: {run_cards:#?}"
        );

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

    #[test]
    fn runs_command_renders_empty_state_card() {
        let (_dir, runtime, mut state) = test_runtime_state();

        show_runs(&mut state, &runtime).unwrap();

        assert_eq!(state.status(), "0 run(s)");
        assert_eq!(state.event_entries().len(), 1);
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
        assert!(suggestions.contains(&"/runs".to_string()));

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
            max_steps_per_run: 5,
            max_visits_per_step: 5,
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
            ("/resolve", "/resolve <run-id> [status] [fields-json]"),
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
            ("/run --step do work", "submitted run --step: do work"),
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
    async fn bare_resume_shows_required_run_id_usage_without_spawning_task() {
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
