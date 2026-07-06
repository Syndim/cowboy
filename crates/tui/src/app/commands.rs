use anyhow::Result;
use cowboy_workflow_engine::WorkflowRuntime;

use super::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SlashCommand {
    pub(super) name: &'static str,
    pub(super) usage: &'static str,
    pub(super) description: &'static str,
    pub(super) takes_arguments: bool,
}

pub(super) const MAX_SLASH_SUGGESTIONS: usize = 6;

pub(super) const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/run",
        usage: "/run <request>",
        description: "start a workflow run",
        takes_arguments: true,
    },
    SlashCommand {
        name: "/run-step",
        usage: "/run-step <request>",
        description: "run only the first workflow step",
        takes_arguments: true,
    },
    SlashCommand {
        name: "/step",
        usage: "/step <run-id>",
        description: "execute one more step",
        takes_arguments: true,
    },
    SlashCommand {
        name: "/answer",
        usage: "/answer <run> <id> <answer>",
        description: "answer a waiting prompt",
        takes_arguments: true,
    },
    SlashCommand {
        name: "/runs",
        usage: "/runs",
        description: "list workflow runs",
        takes_arguments: false,
    },
    SlashCommand {
        name: "/workflows",
        usage: "/workflows",
        description: "list known workflows",
        takes_arguments: false,
    },
    SlashCommand {
        name: "/improve",
        usage: "/improve <run-id>",
        description: "improve workflow source",
        takes_arguments: true,
    },
    SlashCommand {
        name: "/resolve",
        usage: "/resolve <run> [status]",
        description: "list or resolve a failed step",
        takes_arguments: true,
    },
    SlashCommand {
        name: "/cancel",
        usage: "/cancel",
        description: "cancel active background tasks",
        takes_arguments: false,
    },
    SlashCommand {
        name: "/exit",
        usage: "/exit",
        description: "quit Cowboy",
        takes_arguments: false,
    },
    SlashCommand {
        name: "/help",
        usage: "/help",
        description: "show built-in commands",
        takes_arguments: false,
    },
];

pub(super) fn slash_query(input: &str) -> Option<&str> {
    let query = input.strip_prefix('/')?;
    (!query.chars().any(char::is_whitespace)).then_some(query)
}

pub(super) fn slash_suggestions(input: &str) -> Vec<&'static SlashCommand> {
    let Some(query) = slash_query(input) else {
        return Vec::new();
    };
    SLASH_COMMANDS
        .iter()
        .filter(|command| command.name[1..].starts_with(query))
        .collect()
}

pub(super) fn slash_suggestion_line_count(input: &str) -> usize {
    if slash_query(input).is_none() {
        return 0;
    }

    let suggestions = slash_suggestions(input);
    if suggestions.is_empty() {
        1
    } else {
        let hidden = suggestions.len().saturating_sub(MAX_SLASH_SUGGESTIONS);
        1 + suggestions.len().min(MAX_SLASH_SUGGESTIONS) + usize::from(hidden > 0)
    }
}

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
    if let Some(rest) = input.strip_prefix("/run ") {
        spawn_start_run(state, runtime, rest.trim().to_string());
    } else if let Some(rest) = input.strip_prefix("/run-step ") {
        spawn_start_run_stepwise(state, runtime, rest.trim().to_string());
    } else if let Some(rest) = input.strip_prefix("/step ") {
        spawn_step_run(state, runtime, rest.trim().to_string());
    } else if let Some(rest) = input.strip_prefix("/answer ") {
        submit_explicit_answer(state, runtime, rest);
    } else if input == "/cancel" {
        state.cancel_background_tasks();
    } else if let Some(rest) = input.strip_prefix("/improve ") {
        improve_run(state, runtime, rest.trim()).await?;
    } else if let Some(rest) = input.strip_prefix("/resolve ") {
        resolve_run(state, runtime, rest.trim()).await?;
    } else if input == "/exit" {
        state.mark_exit_requested();
        state.set_status("exiting");
        state.push_card("Exit", ["exiting".to_string()]);
    } else if input == "/help" {
        show_help(state);
    } else if input == "/workflows" {
        show_workflows(state, runtime)?;
    } else if input == "/runs" {
        show_runs(state, runtime)?;
    } else if let Some((run_id, prompt_id)) = state.pending_prompt_answer_target() {
        spawn_answer_task(state, runtime, run_id, prompt_id, input.to_string());
    } else {
        spawn_start_run(state, runtime, input.to_string());
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

fn spawn_step_run(state: &mut AppState, runtime: &WorkflowRuntime, run_id: String) {
    let runtime = runtime.clone();
    state.spawn_report_task(format!("submitted step: {run_id}"), async move {
        runtime
            .step_run(&run_id)
            .await
            .map_err(|err| err.to_string())
    });
}

fn submit_explicit_answer(state: &mut AppState, runtime: &WorkflowRuntime, rest: &str) {
    let mut parts = rest.splitn(3, ' ');
    if let (Some(run_id), Some(prompt_id), Some(answer)) =
        (parts.next(), parts.next(), parts.next())
    {
        spawn_answer_task(
            state,
            runtime,
            run_id.to_string(),
            prompt_id.to_string(),
            answer.to_string(),
        );
    } else {
        state.set_status("usage: /answer <run-id> <prompt-id> <answer>");
        state.push_card("Usage", [state.status().to_string()]);
    }
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

async fn resolve_run(state: &mut AppState, runtime: &WorkflowRuntime, rest: &str) -> Result<()> {
    let mut parts = rest.splitn(3, ' ');
    let Some(run_id) = parts.next().filter(|id| !id.is_empty()) else {
        state.set_status("usage: /resolve <run-id> [status] [fields-json]");
        state.push_card("Usage", [state.status().to_string()]);
        return Ok(());
    };
    let status = parts.next().map(str::to_string);
    let fields_raw = parts.next().map(str::to_string);

    match status {
        None => {
            let options = runtime.resolution_options(run_id)?;
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
            let run_id = run_id.to_string();
            state.spawn_report_task(format!("submitted resolve: {run_id} {status}"), async move {
                runtime
                    .resolve_run(&run_id, &status, fields, None)
                    .await
                    .map_err(|err| err.to_string())
            });
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
        assert!(!suggestions.contains(&"/answer <run> <id> <answer>"));
    }

    #[test]
    fn complete_slash_suggestion_updates_input() {
        let mut state = test_state();
        state.push_input("/ru");

        complete_slash_suggestion(&mut state);

        assert_eq!(state.input(), "/run ");
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
