use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use cowboy_workflow_engine::WorkflowRuntime;

use super::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SlashCommand {
    pub(super) name: &'static str,
    pub(super) usage: &'static str,
    pub(super) description: &'static str,
    pub(super) takes_arguments: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedSlashCommand {
    Run {
        request: String,
    },
    RunWorkflow {
        workflow_id: String,
        request: String,
    },
    RunStep {
        request: String,
    },
    Step {
        run_id: String,
    },
    Resume {
        run_id: Option<String>,
    },
    Answer {
        run_id: String,
        prompt_id: String,
        answer: String,
    },
    Runs,
    Workflows,
    Improve {
        run_id: String,
    },
    Resolve {
        run_id: String,
        status: Option<String>,
        fields_json: Option<String>,
    },
    Cancel,
    Exit,
    Help,
}

#[derive(Debug, Parser)]
#[command(
    name = "cowboy-tui",
    no_binary_name = true,
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true
)]
struct SlashCli {
    #[command(subcommand)]
    command: SlashSubcommand,
}

#[derive(Debug, Subcommand)]
enum SlashSubcommand {
    #[command(name = "run")]
    Run(TrailingRequestArgs),
    #[command(name = "run-workflow")]
    RunWorkflow(RunWorkflowArgs),
    #[command(name = "run-step")]
    RunStep(TrailingRequestArgs),
    #[command(name = "step")]
    Step(RunIdArgs),
    #[command(name = "resume")]
    Resume(ResumeArgs),
    #[command(name = "answer")]
    Answer(AnswerArgs),
    #[command(name = "runs")]
    Runs,
    #[command(name = "workflows")]
    Workflows,
    #[command(name = "improve")]
    Improve(RunIdArgs),
    #[command(name = "resolve")]
    Resolve(ResolveArgs),
    #[command(name = "cancel")]
    Cancel,
    #[command(name = "exit")]
    Exit,
    #[command(name = "help")]
    Help,
}

#[derive(Debug, Args)]
struct TrailingRequestArgs {
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
    request: Vec<String>,
}

#[derive(Debug, Args)]
struct RunWorkflowArgs {
    workflow_id: String,
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
    request: Vec<String>,
}

#[derive(Debug, Args)]
struct RunIdArgs {
    run_id: String,
}

#[derive(Debug, Args)]
struct ResumeArgs {
    run_id: Option<String>,
}

#[derive(Debug, Args)]
struct AnswerArgs {
    run_id: String,
    prompt_id: String,
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
    answer: Vec<String>,
}

#[derive(Debug, Args)]
struct ResolveArgs {
    run_id: String,
    #[arg(allow_hyphen_values = true)]
    status: Option<String>,
    #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
    fields_json: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SlashParseError {
    UnmatchedQuote,
    Validation {
        command: Option<String>,
        message: String,
    },
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
        name: "/run-workflow",
        usage: "/run-workflow <workflow-id> <request>",
        description: "start a named workflow run",
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
        name: "/resume",
        usage: "/resume [run-id]",
        description: "continue a run until blocked",
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
        usage: "/resolve <run-id> [status] [fields-json]",
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

fn parse_slash_command(input: &str) -> Result<ParsedSlashCommand, SlashParseError> {
    let mut tokens = tokenize_slash_input(input)?;
    let command = tokens
        .first_mut()
        .and_then(|token| token.strip_prefix('/').map(str::to_string));
    let Some(command) = command.filter(|command| !command.is_empty()) else {
        return Err(SlashParseError::Validation {
            command: None,
            message: "missing slash command".to_string(),
        });
    };

    tokens[0] = command.clone();
    let cli = SlashCli::try_parse_from(tokens).map_err(|err| SlashParseError::Validation {
        command: Some(command),
        message: err.to_string(),
    })?;

    Ok(parsed_slash_command(cli.command))
}

fn tokenize_slash_input(input: &str) -> Result<Vec<String>, SlashParseError> {
    let input = escape_comment_start_hashes(input);
    shlex::split(&input).ok_or(SlashParseError::UnmatchedQuote)
}

fn escape_comment_start_hashes(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    let mut quote = None;
    let mut backslash = false;
    let mut word_started = false;

    for ch in input.chars() {
        match quote {
            Some('\'') => {
                escaped.push(ch);
                word_started = true;
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                escaped.push(ch);
                word_started = true;
                if backslash {
                    backslash = false;
                } else if ch == '\\' {
                    backslash = true;
                } else if ch == '"' {
                    quote = None;
                }
            }
            _ if backslash => {
                escaped.push(ch);
                backslash = false;
                word_started = true;
            }
            _ => match ch {
                '\\' => {
                    escaped.push(ch);
                    backslash = true;
                    word_started = true;
                }
                '\'' | '"' => {
                    escaped.push(ch);
                    quote = Some(ch);
                    word_started = true;
                }
                ' ' | '\t' | '\n' => {
                    escaped.push(ch);
                    word_started = false;
                }
                '#' if !word_started => {
                    escaped.push('\\');
                    escaped.push(ch);
                    word_started = true;
                }
                _ => {
                    escaped.push(ch);
                    word_started = true;
                }
            },
        }
    }

    escaped
}

fn parsed_slash_command(command: SlashSubcommand) -> ParsedSlashCommand {
    match command {
        SlashSubcommand::Run(args) => ParsedSlashCommand::Run {
            request: join_trailing_args(args.request),
        },
        SlashSubcommand::RunWorkflow(args) => ParsedSlashCommand::RunWorkflow {
            workflow_id: args.workflow_id,
            request: join_trailing_args(args.request),
        },
        SlashSubcommand::RunStep(args) => ParsedSlashCommand::RunStep {
            request: join_trailing_args(args.request),
        },
        SlashSubcommand::Step(args) => ParsedSlashCommand::Step {
            run_id: args.run_id,
        },
        SlashSubcommand::Resume(args) => ParsedSlashCommand::Resume {
            run_id: args.run_id,
        },
        SlashSubcommand::Answer(args) => ParsedSlashCommand::Answer {
            run_id: args.run_id,
            prompt_id: args.prompt_id,
            answer: join_trailing_args(args.answer),
        },
        SlashSubcommand::Runs => ParsedSlashCommand::Runs,
        SlashSubcommand::Workflows => ParsedSlashCommand::Workflows,
        SlashSubcommand::Improve(args) => ParsedSlashCommand::Improve {
            run_id: args.run_id,
        },
        SlashSubcommand::Resolve(args) => ParsedSlashCommand::Resolve {
            run_id: args.run_id,
            status: args.status,
            fields_json: (!args.fields_json.is_empty())
                .then(|| join_trailing_args(args.fields_json)),
        },
        SlashSubcommand::Cancel => ParsedSlashCommand::Cancel,
        SlashSubcommand::Exit => ParsedSlashCommand::Exit,
        SlashSubcommand::Help => ParsedSlashCommand::Help,
    }
}

fn join_trailing_args(args: Vec<String>) -> String {
    args.join(" ")
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

fn slash_command_metadata(command_name: &str) -> Option<&'static SlashCommand> {
    SLASH_COMMANDS
        .iter()
        .find(|command| command.name.strip_prefix('/') == Some(command_name))
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
    command: ParsedSlashCommand,
) -> Result<()> {
    match command {
        ParsedSlashCommand::Run { request } => spawn_start_run(state, runtime, request),
        ParsedSlashCommand::RunWorkflow {
            workflow_id,
            request,
        } => spawn_start_run_with_workflow(state, runtime, workflow_id, request),
        ParsedSlashCommand::RunStep { request } => {
            spawn_start_run_stepwise(state, runtime, request)
        }
        ParsedSlashCommand::Step { run_id } => spawn_step_run(state, runtime, run_id),
        ParsedSlashCommand::Resume { run_id } => submit_resume_run(state, runtime, run_id),
        ParsedSlashCommand::Answer {
            run_id,
            prompt_id,
            answer,
        } => spawn_answer_task(state, runtime, run_id, prompt_id, answer),
        ParsedSlashCommand::Runs => show_runs(state, runtime)?,
        ParsedSlashCommand::Workflows => show_workflows(state, runtime)?,
        ParsedSlashCommand::Improve { run_id } => improve_run(state, runtime, &run_id).await?,
        ParsedSlashCommand::Resolve {
            run_id,
            status,
            fields_json,
        } => resolve_run(state, runtime, run_id, status, fields_json).await?,
        ParsedSlashCommand::Cancel => state.cancel_background_tasks(),
        ParsedSlashCommand::Exit => {
            state.mark_exit_requested();
            state.set_status("exiting");
            state.push_card("Exit", ["exiting".to_string()]);
        }
        ParsedSlashCommand::Help => show_help(state),
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
    fn quoted_and_unquoted_run_requests_parse_equivalently() {
        let expected = ParsedSlashCommand::Run {
            request: "do work".to_string(),
        };

        assert_eq!(
            parse_slash_command("/run do work").unwrap(),
            expected.clone()
        );
        assert_eq!(parse_slash_command("/run \"do work\"").unwrap(), expected);
    }

    #[test]
    fn run_workflow_parses_workflow_id_and_trailing_request() {
        assert_eq!(
            parse_slash_command("/run-workflow review do work").unwrap(),
            ParsedSlashCommand::RunWorkflow {
                workflow_id: "review".to_string(),
                request: "do work".to_string(),
            }
        );
    }

    #[test]
    fn run_preserves_hash_issue_references() {
        assert_eq!(
            parse_slash_command("/run fix #123 regression").unwrap(),
            ParsedSlashCommand::Run {
                request: "fix #123 regression".to_string(),
            }
        );
    }

    #[test]
    fn run_workflow_preserves_hash_issue_references() {
        assert_eq!(
            parse_slash_command("/run-workflow review fix #123 regression").unwrap(),
            ParsedSlashCommand::RunWorkflow {
                workflow_id: "review".to_string(),
                request: "fix #123 regression".to_string(),
            }
        );
    }

    #[test]
    fn run_step_accepts_hyphen_leading_request() {
        assert_eq!(
            parse_slash_command("/run-step --dry-run now").unwrap(),
            ParsedSlashCommand::RunStep {
                request: "--dry-run now".to_string(),
            }
        );
    }

    #[test]
    fn resume_parses_bare_and_explicit_run_id_forms() {
        assert_eq!(
            parse_slash_command("/resume").unwrap(),
            ParsedSlashCommand::Resume { run_id: None }
        );
        assert_eq!(
            parse_slash_command("/resume run-1").unwrap(),
            ParsedSlashCommand::Resume {
                run_id: Some("run-1".to_string()),
            }
        );
    }

    #[test]
    fn answer_parses_unquoted_and_quoted_multi_word_answers() {
        let expected = ParsedSlashCommand::Answer {
            run_id: "run-1".to_string(),
            prompt_id: "prompt-1".to_string(),
            answer: "ship it".to_string(),
        };

        assert_eq!(
            parse_slash_command("/answer run-1 prompt-1 ship it").unwrap(),
            expected.clone()
        );
        assert_eq!(
            parse_slash_command("/answer run-1 prompt-1 \"ship it\"").unwrap(),
            expected
        );
    }

    #[test]
    fn answer_preserves_hash_issue_references() {
        assert_eq!(
            parse_slash_command("/answer run-1 prompt-1 see #123").unwrap(),
            ParsedSlashCommand::Answer {
                run_id: "run-1".to_string(),
                prompt_id: "prompt-1".to_string(),
                answer: "see #123".to_string(),
            }
        );
    }

    #[test]
    fn resolve_parses_run_status_and_quoted_fields() {
        assert_eq!(
            parse_slash_command("/resolve run-1").unwrap(),
            ParsedSlashCommand::Resolve {
                run_id: "run-1".to_string(),
                status: None,
                fields_json: None,
            }
        );
        assert_eq!(
            parse_slash_command("/resolve run-1 failed").unwrap(),
            ParsedSlashCommand::Resolve {
                run_id: "run-1".to_string(),
                status: Some("failed".to_string()),
                fields_json: None,
            }
        );
        assert_eq!(
            parse_slash_command(r#"/resolve run-1 failed '{"reason":"needs work","retry":false}'"#)
                .unwrap(),
            ParsedSlashCommand::Resolve {
                run_id: "run-1".to_string(),
                status: Some("failed".to_string()),
                fields_json: Some("{\"reason\":\"needs work\",\"retry\":false}".to_string()),
            }
        );
    }

    #[test]
    fn malformed_quote_is_a_parse_error() {
        assert_eq!(
            parse_slash_command("/run \"unterminated"),
            Err(SlashParseError::UnmatchedQuote)
        );
    }

    #[test]
    fn advertised_slash_commands_parse_to_their_variants() {
        for command in SLASH_COMMANDS {
            let (input, expected) = match command.name {
                "/run" => (
                    "/run do work",
                    ParsedSlashCommand::Run {
                        request: "do work".to_string(),
                    },
                ),
                "/run-workflow" => (
                    "/run-workflow review do work",
                    ParsedSlashCommand::RunWorkflow {
                        workflow_id: "review".to_string(),
                        request: "do work".to_string(),
                    },
                ),
                "/run-step" => (
                    "/run-step do work",
                    ParsedSlashCommand::RunStep {
                        request: "do work".to_string(),
                    },
                ),
                "/step" => (
                    "/step run-1",
                    ParsedSlashCommand::Step {
                        run_id: "run-1".to_string(),
                    },
                ),
                "/resume" => (
                    "/resume run-1",
                    ParsedSlashCommand::Resume {
                        run_id: Some("run-1".to_string()),
                    },
                ),
                "/answer" => (
                    "/answer run-1 prompt-1 ship it",
                    ParsedSlashCommand::Answer {
                        run_id: "run-1".to_string(),
                        prompt_id: "prompt-1".to_string(),
                        answer: "ship it".to_string(),
                    },
                ),
                "/runs" => ("/runs", ParsedSlashCommand::Runs),
                "/workflows" => ("/workflows", ParsedSlashCommand::Workflows),
                "/improve" => (
                    "/improve run-1",
                    ParsedSlashCommand::Improve {
                        run_id: "run-1".to_string(),
                    },
                ),
                "/resolve" => (
                    "/resolve run-1 failed",
                    ParsedSlashCommand::Resolve {
                        run_id: "run-1".to_string(),
                        status: Some("failed".to_string()),
                        fields_json: None,
                    },
                ),
                "/cancel" => ("/cancel", ParsedSlashCommand::Cancel),
                "/exit" => ("/exit", ParsedSlashCommand::Exit),
                "/help" => ("/help", ParsedSlashCommand::Help),
                name => panic!("advertised slash command {name} has no parser sample"),
            };

            assert_eq!(parse_slash_command(input).unwrap(), expected);
        }
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
