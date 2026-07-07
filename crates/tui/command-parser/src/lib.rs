//! Clap-backed command parsing for Cowboy entry points.
//!
//! This crate owns the grammar for both product CLI commands and interactive TUI
//! slash commands. Callers pass process-style argv or a raw slash-command input
//! string and receive runtime-agnostic typed command values plus metadata for
//! help and completion UI. The crate intentionally has no dependency on the
//! workflow runtime, ratatui, terminal input, config loading, or app state.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

/// Cowboy product CLI parser.
#[derive(Debug, Parser)]
#[command(
    name = "cowboy",
    version,
    about = "Workflow-first AI agent orchestrator"
)]
pub struct Cli {
    /// Path to config file.
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

impl Cli {
    /// Parse CLI arguments from the current process.
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }

    /// Parse CLI arguments from an explicit argv iterator.
    pub fn parse_from<I, T>(itr: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        <Self as Parser>::parse_from(itr)
    }

    /// Try to parse CLI arguments from an explicit argv iterator.
    pub fn try_parse_from<I, T>(itr: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        <Self as Parser>::try_parse_from(itr)
    }

    /// Return the configured path, or compute the application default lazily.
    pub fn config_path_or_else<F>(&self, default: F) -> PathBuf
    where
        F: FnOnce() -> PathBuf,
    {
        self.config.clone().unwrap_or_else(default)
    }

    /// Return the configured path, or a supplied default path.
    pub fn config_path_or(&self, default: impl AsRef<Path>) -> PathBuf {
        self.config
            .clone()
            .unwrap_or_else(|| default.as_ref().to_path_buf())
    }
}

/// Non-interactive Cowboy CLI commands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum CliCommand {
    /// Launch the interactive terminal UI.
    #[command(alias = "start")]
    Tui,
    /// Start a new workflow run. With --step, execute only the first step.
    Run {
        /// Execute only the first workflow step instead of running until blocked.
        #[arg(long)]
        step: bool,
        /// Catalog workflow id to run, bypassing workflow selection.
        #[arg(long)]
        workflow: Option<String>,
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

/// Parsed TUI slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
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

/// Metadata used by help, usage rendering, and completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommandMetadata {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
    pub takes_arguments: bool,
}

/// Errors produced while parsing slash commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashParseError {
    UnmatchedQuote,
    Validation {
        command: Option<String>,
        message: String,
    },
}

/// Advertised slash command registry.
pub const SLASH_COMMANDS: &[SlashCommandMetadata] = &[
    SlashCommandMetadata {
        name: "/run",
        usage: "/run <request>",
        description: "start a workflow run",
        takes_arguments: true,
    },
    SlashCommandMetadata {
        name: "/run-workflow",
        usage: "/run-workflow <workflow-id> <request>",
        description: "start a named workflow run",
        takes_arguments: true,
    },
    SlashCommandMetadata {
        name: "/run-step",
        usage: "/run-step <request>",
        description: "run only the first workflow step",
        takes_arguments: true,
    },
    SlashCommandMetadata {
        name: "/step",
        usage: "/step <run-id>",
        description: "execute one more step",
        takes_arguments: true,
    },
    SlashCommandMetadata {
        name: "/resume",
        usage: "/resume [run-id]",
        description: "continue a run until blocked",
        takes_arguments: true,
    },
    SlashCommandMetadata {
        name: "/answer",
        usage: "/answer <run> <id> <answer>",
        description: "answer a waiting prompt",
        takes_arguments: true,
    },
    SlashCommandMetadata {
        name: "/runs",
        usage: "/runs",
        description: "list workflow runs",
        takes_arguments: false,
    },
    SlashCommandMetadata {
        name: "/workflows",
        usage: "/workflows",
        description: "list known workflows",
        takes_arguments: false,
    },
    SlashCommandMetadata {
        name: "/improve",
        usage: "/improve <run-id>",
        description: "improve workflow source",
        takes_arguments: true,
    },
    SlashCommandMetadata {
        name: "/resolve",
        usage: "/resolve <run-id> [status] [fields-json]",
        description: "list or resolve a failed step",
        takes_arguments: true,
    },
    SlashCommandMetadata {
        name: "/cancel",
        usage: "/cancel",
        description: "cancel active background tasks",
        takes_arguments: false,
    },
    SlashCommandMetadata {
        name: "/exit",
        usage: "/exit",
        description: "quit Cowboy",
        takes_arguments: false,
    },
    SlashCommandMetadata {
        name: "/help",
        usage: "/help",
        description: "show built-in commands",
        takes_arguments: false,
    },
];

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

/// Return the active slash query when input is completing a command name.
pub fn slash_query(input: &str) -> Option<&str> {
    let query = input.strip_prefix('/')?;
    (!query.chars().any(char::is_whitespace)).then_some(query)
}

/// Return all command metadata entries matching the current slash command prefix.
pub fn slash_suggestions(input: &str) -> Vec<&'static SlashCommandMetadata> {
    let Some(query) = slash_query(input) else {
        return Vec::new();
    };

    SLASH_COMMANDS
        .iter()
        .filter(|command| command.name[1..].starts_with(query))
        .collect()
}

/// Parse a raw slash-command composer string.
pub fn parse_slash_command(input: &str) -> Result<SlashCommand, SlashParseError> {
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

/// Return metadata for a command name without its leading slash.
pub fn slash_command_metadata(command_name: &str) -> Option<&'static SlashCommandMetadata> {
    SLASH_COMMANDS
        .iter()
        .find(|command| command.name.strip_prefix('/') == Some(command_name))
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

fn parsed_slash_command(command: SlashSubcommand) -> SlashCommand {
    match command {
        SlashSubcommand::Run(args) => SlashCommand::Run {
            request: join_trailing_args(args.request),
        },
        SlashSubcommand::RunWorkflow(args) => SlashCommand::RunWorkflow {
            workflow_id: args.workflow_id,
            request: join_trailing_args(args.request),
        },
        SlashSubcommand::RunStep(args) => SlashCommand::RunStep {
            request: join_trailing_args(args.request),
        },
        SlashSubcommand::Step(args) => SlashCommand::Step {
            run_id: args.run_id,
        },
        SlashSubcommand::Resume(args) => SlashCommand::Resume {
            run_id: args.run_id,
        },
        SlashSubcommand::Answer(args) => SlashCommand::Answer {
            run_id: args.run_id,
            prompt_id: args.prompt_id,
            answer: join_trailing_args(args.answer),
        },
        SlashSubcommand::Runs => SlashCommand::Runs,
        SlashSubcommand::Workflows => SlashCommand::Workflows,
        SlashSubcommand::Improve(args) => SlashCommand::Improve {
            run_id: args.run_id,
        },
        SlashSubcommand::Resolve(args) => SlashCommand::Resolve {
            run_id: args.run_id,
            status: args.status,
            fields_json: (!args.fields_json.is_empty())
                .then(|| join_trailing_args(args.fields_json)),
        },
        SlashSubcommand::Cancel => SlashCommand::Cancel,
        SlashSubcommand::Exit => SlashCommand::Exit,
        SlashSubcommand::Help => SlashCommand::Help,
    }
}

fn join_trailing_args(args: Vec<String>) -> String {
    args.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_with_workflow_parses_workflow_override() {
        let cli = Cli::parse_from(["cowboy", "run", "--workflow", "review", "do", "work"]);
        match cli.command {
            Some(CliCommand::Run {
                workflow, request, ..
            }) => {
                assert_eq!(workflow.as_deref(), Some("review"));
                assert_eq!(request, vec!["do".to_string(), "work".to_string()]);
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn run_step_with_workflow_parses_first_step_override() {
        let cli = Cli::parse_from([
            "cowboy",
            "run",
            "--step",
            "--workflow",
            "review",
            "do",
            "work",
        ]);
        match cli.command {
            Some(CliCommand::Run {
                step,
                workflow,
                request,
            }) => {
                assert!(step);
                assert_eq!(workflow.as_deref(), Some("review"));
                assert_eq!(request, vec!["do".to_string(), "work".to_string()]);
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn run_without_workflow_keeps_selector_backed_form() {
        let cli = Cli::parse_from(["cowboy", "run", "do", "work"]);
        match cli.command {
            Some(CliCommand::Run {
                workflow, request, ..
            }) => {
                assert_eq!(workflow, None);
                assert_eq!(request, vec!["do".to_string(), "work".to_string()]);
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn resolve_without_status_parses_as_list_form() {
        let cli = Cli::parse_from(["cowboy", "resolve", "run-1"]);
        match cli.command {
            Some(CliCommand::Resolve {
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
            Some(CliCommand::Resume { run_id }) => assert_eq!(run_id, "run-1"),
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
            Some(CliCommand::Resolve {
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

    #[test]
    fn quoted_and_unquoted_run_requests_parse_equivalently() {
        let expected = SlashCommand::Run {
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
            SlashCommand::RunWorkflow {
                workflow_id: "review".to_string(),
                request: "do work".to_string(),
            }
        );
    }

    #[test]
    fn hash_issue_references_are_preserved() {
        assert_eq!(
            parse_slash_command("/run fix #123 regression").unwrap(),
            SlashCommand::Run {
                request: "fix #123 regression".to_string(),
            }
        );
        assert_eq!(
            parse_slash_command("/run-workflow review fix #123 regression").unwrap(),
            SlashCommand::RunWorkflow {
                workflow_id: "review".to_string(),
                request: "fix #123 regression".to_string(),
            }
        );
        assert_eq!(
            parse_slash_command("/answer run-1 prompt-1 see #123").unwrap(),
            SlashCommand::Answer {
                run_id: "run-1".to_string(),
                prompt_id: "prompt-1".to_string(),
                answer: "see #123".to_string(),
            }
        );
    }

    #[test]
    fn run_step_accepts_hyphen_leading_request() {
        assert_eq!(
            parse_slash_command("/run-step --dry-run now").unwrap(),
            SlashCommand::RunStep {
                request: "--dry-run now".to_string(),
            }
        );
    }

    #[test]
    fn resume_parses_bare_and_explicit_run_id_forms() {
        assert_eq!(
            parse_slash_command("/resume").unwrap(),
            SlashCommand::Resume { run_id: None }
        );
        assert_eq!(
            parse_slash_command("/resume run-1").unwrap(),
            SlashCommand::Resume {
                run_id: Some("run-1".to_string()),
            }
        );
    }

    #[test]
    fn answer_parses_unquoted_and_quoted_multi_word_answers() {
        let expected = SlashCommand::Answer {
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
    fn resolve_parses_list_status_and_quoted_fields_forms() {
        assert_eq!(
            parse_slash_command("/resolve run-1").unwrap(),
            SlashCommand::Resolve {
                run_id: "run-1".to_string(),
                status: None,
                fields_json: None,
            }
        );
        assert_eq!(
            parse_slash_command("/resolve run-1 failed").unwrap(),
            SlashCommand::Resolve {
                run_id: "run-1".to_string(),
                status: Some("failed".to_string()),
                fields_json: None,
            }
        );
        assert_eq!(
            parse_slash_command(r#"/resolve run-1 failed '{"reason":"needs work","retry":false}'"#)
                .unwrap(),
            SlashCommand::Resolve {
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
                    SlashCommand::Run {
                        request: "do work".to_string(),
                    },
                ),
                "/run-workflow" => (
                    "/run-workflow review do work",
                    SlashCommand::RunWorkflow {
                        workflow_id: "review".to_string(),
                        request: "do work".to_string(),
                    },
                ),
                "/run-step" => (
                    "/run-step do work",
                    SlashCommand::RunStep {
                        request: "do work".to_string(),
                    },
                ),
                "/step" => (
                    "/step run-1",
                    SlashCommand::Step {
                        run_id: "run-1".to_string(),
                    },
                ),
                "/resume" => (
                    "/resume run-1",
                    SlashCommand::Resume {
                        run_id: Some("run-1".to_string()),
                    },
                ),
                "/answer" => (
                    "/answer run-1 prompt-1 ship it",
                    SlashCommand::Answer {
                        run_id: "run-1".to_string(),
                        prompt_id: "prompt-1".to_string(),
                        answer: "ship it".to_string(),
                    },
                ),
                "/runs" => ("/runs", SlashCommand::Runs),
                "/workflows" => ("/workflows", SlashCommand::Workflows),
                "/improve" => (
                    "/improve run-1",
                    SlashCommand::Improve {
                        run_id: "run-1".to_string(),
                    },
                ),
                "/resolve" => (
                    "/resolve run-1 failed",
                    SlashCommand::Resolve {
                        run_id: "run-1".to_string(),
                        status: Some("failed".to_string()),
                        fields_json: None,
                    },
                ),
                "/cancel" => ("/cancel", SlashCommand::Cancel),
                "/exit" => ("/exit", SlashCommand::Exit),
                "/help" => ("/help", SlashCommand::Help),
                name => panic!("advertised slash command {name} has no parser sample"),
            };

            assert_eq!(parse_slash_command(input).unwrap(), expected);
            assert!(slash_command_metadata(command.name.trim_start_matches('/')).is_some());
        }
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

    #[test]
    fn parser_crate_stays_runtime_and_ui_independent() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in [
            "cowboy-workflow-engine",
            "ratatui",
            "crossterm",
            "tui-input",
        ] {
            assert!(
                !manifest.contains(forbidden),
                "forbidden dependency: {forbidden}"
            );
        }
    }
}
