//! Clap-backed command parsing for Cowboy entry points.
//!
//! This crate owns the grammar for both product CLI commands and interactive TUI
//! slash commands. Callers pass process-style argv or a raw slash-command input
//! string and receive runtime-agnostic typed command values plus generated help
//! and completion UI. The crate intentionally has no dependency on the workflow
//! runtime, ratatui, terminal input, config loading, or app state.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, Args, Command, CommandFactory, Parser, Subcommand};

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

    #[command(flatten)]
    Shared(SharedCommand),
}

/// Runtime commands shared by the product CLI and TUI slash command surfaces.
#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
pub enum SharedCommand {
    /// Start a workflow run.
    #[command(about = "start a workflow run")]
    Run(RunArgs),

    /// Execute exactly one further step of an existing workflow run.
    #[command(about = "execute one more step")]
    Step(RunIdArgs),

    /// Continue an existing workflow run until it blocks, fails, or completes.
    #[command(about = "continue a run until blocked")]
    Resume(RunIdArgs),

    /// Answer a workflow input prompt and continue the run.
    #[command(about = "answer a waiting prompt")]
    Answer(AnswerArgs),

    /// Summarize a completed run and apply proposed workflow file updates.
    #[command(about = "improve workflow source")]
    Improve(RunIdArgs),

    /// List workflow runs.
    #[command(about = "list workflow runs")]
    Runs,

    /// Resolve a failed run. Without <status>, lists resolvable statuses.
    #[command(about = "list or resolve a failed step")]
    Resolve(ResolveArgs),
}

/// Arguments for starting a workflow run.
#[derive(Debug, Clone, Args, PartialEq, Eq)]
pub struct RunArgs {
    /// Execute only the first workflow step instead of running until blocked.
    #[arg(long)]
    pub step: bool,

    /// Catalog workflow id to run, bypassing workflow selection.
    #[arg(long, value_name = "workflow-id")]
    pub workflow: Option<String>,

    #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true, value_name = "request")]
    pub request: Vec<String>,
}

impl RunArgs {
    /// Return the trailing request words normalized into the runtime request string.
    pub fn into_request(self) -> String {
        join_trailing_args(self.request)
    }
}

/// Arguments for commands that address one workflow run.
#[derive(Debug, Clone, Args, PartialEq, Eq)]
pub struct RunIdArgs {
    #[arg(value_name = "run-id")]
    pub run_id: String,
}

/// Arguments for answering a workflow prompt.
#[derive(Debug, Clone, Args, PartialEq, Eq)]
pub struct AnswerArgs {
    #[arg(value_name = "run-id")]
    pub run_id: String,

    #[arg(value_name = "prompt-id")]
    pub prompt_id: String,

    #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true, value_name = "answer")]
    pub answer: Vec<String>,
}

impl AnswerArgs {
    /// Return the trailing answer words normalized into the runtime answer string.
    pub fn into_answer(self) -> String {
        join_trailing_args(self.answer)
    }
}

/// Arguments for resolving a failed run.
#[derive(Debug, Clone, Args, PartialEq, Eq)]
pub struct ResolveArgs {
    #[arg(value_name = "run-id")]
    pub run_id: String,

    /// Status to resolve the failed step to. Omit to list options.
    #[arg(value_name = "status", allow_hyphen_values = true)]
    pub status: Option<String>,

    /// JSON object of output fields for the chosen status.
    #[arg(long, value_name = "json", conflicts_with = "fields_json")]
    pub fields: Option<String>,

    /// Human-readable body for the synthesized step record.
    #[arg(long, value_name = "text")]
    pub body: Option<String>,

    #[arg(
        value_name = "fields-json",
        allow_hyphen_values = true,
        conflicts_with = "fields",
        hide = true
    )]
    pub fields_json: Option<String>,
}

impl ResolveArgs {
    /// Return fields JSON supplied through either the CLI option or slash positional form.
    pub fn into_fields(self) -> Option<String> {
        self.fields.or(self.fields_json)
    }
}

/// Parsed TUI slash command.
#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
pub enum SlashCommand {
    #[command(flatten)]
    Shared(SharedCommand),

    /// List known workflows.
    #[command(about = "list known workflows")]
    Workflows,

    /// Cancel active background tasks.
    #[command(about = "cancel active background tasks")]
    Cancel,

    /// Quit Cowboy.
    #[command(about = "quit Cowboy")]
    Exit,

    /// Show built-in commands.
    #[command(about = "show built-in commands")]
    Help,
}

/// Generated slash command row used by help, usage rendering, and completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandInfo {
    pub name: String,
    pub usage: String,
    pub description: String,
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
    command: SlashCommand,
}

/// Return the active slash query when input is completing a command name.
pub fn slash_query(input: &str) -> Option<&str> {
    let query = input.strip_prefix('/')?;
    (!query.chars().any(char::is_whitespace)).then_some(query)
}

/// Return all generated command rows matching the current slash command prefix.
pub fn slash_suggestions(input: &str) -> Vec<SlashCommandInfo> {
    let Some(query) = slash_query(input) else {
        return Vec::new();
    };

    slash_command_infos()
        .into_iter()
        .filter(|command| command.name[1..].starts_with(query))
        .collect()
}

/// Return generated rows for every advertised slash command.
pub fn slash_help_rows() -> Vec<SlashCommandInfo> {
    slash_command_infos()
}

/// Return the slash usage string for a command name without its leading slash.
pub fn slash_command_usage(command_name: &str) -> Option<String> {
    slash_command_infos()
        .into_iter()
        .find(|command| command.name.strip_prefix('/') == Some(command_name))
        .map(|command| command.usage)
}

/// Return generated slash command names without their leading slash.
pub fn slash_command_names() -> Vec<String> {
    slash_command_infos()
        .into_iter()
        .map(|command| command.name.trim_start_matches('/').to_string())
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

    Ok(cli.command)
}

fn slash_command_infos() -> Vec<SlashCommandInfo> {
    let command = SlashCli::command();
    command.get_subcommands().map(slash_command_info).collect()
}

fn slash_command_info(command: &Command) -> SlashCommandInfo {
    SlashCommandInfo {
        name: format!("/{}", command.get_name()),
        usage: slash_usage(command),
        description: command
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default(),
        takes_arguments: command.get_arguments().next().is_some(),
    }
}

fn slash_usage(command: &Command) -> String {
    let mut parts = vec![format!("/{}", command.get_name())];
    parts.extend(command.get_arguments().filter_map(arg_usage));
    parts.join(" ")
}

fn arg_usage(arg: &Arg) -> Option<String> {
    if arg.is_hide_set() && arg.get_id() != "fields_json" {
        return None;
    }

    if matches!(arg.get_id().as_str(), "fields" | "body") {
        return None;
    }

    if let Some(long) = arg.get_long() {
        let usage = if matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse) {
            format!("--{long}")
        } else {
            format!("--{long} <{}>", value_name(arg))
        };

        return Some(optional_usage(arg, usage));
    }

    let usage = match arg.get_id().as_str() {
        "request" => "<request>".to_string(),
        "answer" => "<answer>".to_string(),
        id => format!("<{}>", id.replace('_', "-")),
    };

    Some(optional_usage(arg, usage))
}

fn optional_usage(arg: &Arg, usage: String) -> String {
    if arg.is_required_set() {
        usage
    } else if usage.starts_with('<') && usage.ends_with('>') {
        format!("[{}]", usage.trim_start_matches('<').trim_end_matches('>'))
    } else {
        format!("[{usage}]")
    }
}

fn value_name(arg: &Arg) -> String {
    arg.get_value_names()
        .and_then(|names| names.first())
        .map(ToString::to_string)
        .unwrap_or_else(|| arg.get_id().as_str().replace('_', "-"))
}

fn tokenize_slash_input(input: &str) -> Result<Vec<String>, SlashParseError> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut in_token = false;
    let mut quote = None;
    let mut chars = input.chars();

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    token.push(ch);
                }
            }
            Some('"') => match ch {
                '"' => quote = None,
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        token.push(escaped);
                    } else {
                        token.push(ch);
                    }
                }
                _ => token.push(ch),
            },
            Some(_) => unreachable!("unsupported quote delimiter"),
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    in_token = true;
                }
                '\\' => {
                    in_token = true;
                    if let Some(escaped) = chars.next() {
                        token.push(escaped);
                    } else {
                        token.push(ch);
                    }
                }
                ch if ch.is_whitespace() => {
                    if in_token {
                        tokens.push(std::mem::take(&mut token));
                        in_token = false;
                    }
                }
                _ => {
                    token.push(ch);
                    in_token = true;
                }
            },
        }
    }

    if quote.is_some() {
        return Err(SlashParseError::UnmatchedQuote);
    }

    if in_token {
        tokens.push(token);
    }

    Ok(tokens)
}

fn join_trailing_args(args: Vec<String>) -> String {
    args.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_cli_command(args: impl IntoIterator<Item = &'static str>) -> SharedCommand {
        match Cli::parse_from(args).command {
            Some(CliCommand::Shared(command)) => command,
            other => panic!("expected shared command, got {other:?}"),
        }
    }

    fn shared_slash_command(input: &str) -> SharedCommand {
        match parse_slash_command(input).unwrap() {
            SlashCommand::Shared(command) => command,
            other => panic!("expected shared slash command, got {other:?}"),
        }
    }

    #[test]
    fn cli_run_parses_through_shared_command() {
        match shared_cli_command(["cowboy", "run", "do", "work"]) {
            SharedCommand::Run(args) => {
                assert!(!args.step);
                assert_eq!(args.workflow, None);
                assert_eq!(args.request, vec!["do".to_string(), "work".to_string()]);
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn cli_run_step_and_workflow_parse_through_shared_command() {
        match shared_cli_command([
            "cowboy",
            "run",
            "--step",
            "--workflow",
            "review",
            "do",
            "work",
        ]) {
            SharedCommand::Run(args) => {
                assert!(args.step);
                assert_eq!(args.workflow.as_deref(), Some("review"));
                assert_eq!(args.request, vec!["do".to_string(), "work".to_string()]);
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn slash_run_parses_through_shared_command() {
        match shared_slash_command("/run do work") {
            SharedCommand::Run(args) => {
                assert!(!args.step);
                assert_eq!(args.workflow, None);
                assert_eq!(args.into_request(), "do work");
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn slash_run_step_and_workflow_parse_through_shared_command() {
        match shared_slash_command("/run --step --workflow review do work") {
            SharedCommand::Run(args) => {
                assert!(args.step);
                assert_eq!(args.workflow.as_deref(), Some("review"));
                assert_eq!(args.into_request(), "do work");
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn quoted_and_unquoted_run_requests_parse_equivalently() {
        let expected = SharedCommand::Run(RunArgs {
            step: false,
            workflow: None,
            request: vec!["do work".to_string()],
        });

        assert_eq!(shared_slash_command("/run \"do work\""), expected);
        assert_eq!(
            shared_slash_command("/run do work").into_request(),
            "do work"
        );
    }

    #[test]
    fn hash_issue_references_are_preserved() {
        match shared_slash_command("/run fix #123 regression") {
            SharedCommand::Run(args) => assert_eq!(args.into_request(), "fix #123 regression"),
            other => panic!("expected run command, got {other:?}"),
        }

        match shared_slash_command("/answer run-1 prompt-1 see #123") {
            SharedCommand::Answer(args) => {
                assert_eq!(args.run_id, "run-1");
                assert_eq!(args.prompt_id, "prompt-1");
                assert_eq!(args.into_answer(), "see #123");
            }
            other => panic!("expected answer command, got {other:?}"),
        }
    }

    #[test]
    fn run_accepts_clap_delimited_hyphen_leading_request() {
        match shared_slash_command("/run -- --dry-run now") {
            SharedCommand::Run(args) => assert_eq!(args.into_request(), "--dry-run now"),
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn resume_requires_run_id_for_cli_and_slash() {
        match shared_cli_command(["cowboy", "resume", "run-1"]) {
            SharedCommand::Resume(args) => assert_eq!(args.run_id, "run-1"),
            other => panic!("expected resume command, got {other:?}"),
        }

        match shared_slash_command("/resume run-1") {
            SharedCommand::Resume(args) => assert_eq!(args.run_id, "run-1"),
            other => panic!("expected resume command, got {other:?}"),
        }

        assert_eq!(
            Cli::try_parse_from(["cowboy", "resume"])
                .unwrap_err()
                .kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert!(matches!(
            parse_slash_command("/resume"),
            Err(SlashParseError::Validation { .. })
        ));
    }

    #[test]
    fn answer_parses_unquoted_and_quoted_multi_word_answers() {
        let expected = SharedCommand::Answer(AnswerArgs {
            run_id: "run-1".to_string(),
            prompt_id: "prompt-1".to_string(),
            answer: vec!["ship it".to_string()],
        });

        assert_eq!(
            shared_slash_command("/answer run-1 prompt-1 \"ship it\""),
            expected
        );
        match shared_slash_command("/answer run-1 prompt-1 ship it") {
            SharedCommand::Answer(args) => assert_eq!(args.into_answer(), "ship it"),
            other => panic!("expected answer command, got {other:?}"),
        }
    }

    #[test]
    fn resolve_parses_cli_options_and_slash_positional_fields() {
        match shared_cli_command([
            "cowboy",
            "resolve",
            "run-1",
            "approved",
            "--fields",
            r#"{"summary":"done"}"#,
            "--body",
            "looks good",
        ]) {
            SharedCommand::Resolve(args) => {
                assert_eq!(args.run_id, "run-1");
                assert_eq!(args.status.as_deref(), Some("approved"));
                assert_eq!(args.fields.as_deref(), Some(r#"{"summary":"done"}"#));
                assert_eq!(args.body.as_deref(), Some("looks good"));
                assert_eq!(args.fields_json, None);
            }
            other => panic!("expected resolve command, got {other:?}"),
        }

        match shared_slash_command(
            r#"/resolve run-1 failed '{"reason":"needs work","retry":false}'"#,
        ) {
            SharedCommand::Resolve(args) => {
                assert_eq!(args.run_id, "run-1");
                assert_eq!(args.status.as_deref(), Some("failed"));
                assert_eq!(
                    args.fields_json.as_deref(),
                    Some("{\"reason\":\"needs work\",\"retry\":false}")
                );
            }
            other => panic!("expected resolve command, got {other:?}"),
        }
    }

    #[test]
    fn cli_resolve_help_advertises_cli_options() {
        let mut command = Cli::command();
        let resolve = command
            .find_subcommand_mut("resolve")
            .expect("resolve subcommand exists");
        let mut help = Vec::new();
        resolve.write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();

        assert!(help.contains("--fields <json>"), "{help}");
        assert!(help.contains("--body <text>"), "{help}");
    }

    #[test]
    fn resolve_list_and_status_forms_parse() {
        match shared_slash_command("/resolve run-1") {
            SharedCommand::Resolve(args) => {
                assert_eq!(args.run_id, "run-1");
                assert_eq!(args.status, None);
                assert_eq!(args.fields_json, None);
            }
            other => panic!("expected resolve command, got {other:?}"),
        }

        match shared_slash_command("/resolve run-1 failed") {
            SharedCommand::Resolve(args) => {
                assert_eq!(args.run_id, "run-1");
                assert_eq!(args.status.as_deref(), Some("failed"));
                assert_eq!(args.fields_json, None);
            }
            other => panic!("expected resolve command, got {other:?}"),
        }
    }

    #[test]
    fn malformed_quote_is_a_parse_error() {
        assert_eq!(
            parse_slash_command("/run \"unterminated"),
            Err(SlashParseError::UnmatchedQuote)
        );
    }

    #[test]
    fn tui_only_commands_parse_to_tui_variants() {
        assert_eq!(
            parse_slash_command("/workflows").unwrap(),
            SlashCommand::Workflows
        );
        assert_eq!(
            parse_slash_command("/cancel").unwrap(),
            SlashCommand::Cancel
        );
        assert_eq!(parse_slash_command("/exit").unwrap(), SlashCommand::Exit);
        assert_eq!(parse_slash_command("/help").unwrap(), SlashCommand::Help);
    }

    #[test]
    fn removed_run_command_names_are_unknown_and_not_advertised() {
        assert!(matches!(
            parse_slash_command("/run-workflow review do work"),
            Err(SlashParseError::Validation { .. })
        ));
        assert!(matches!(
            parse_slash_command("/run-step do work"),
            Err(SlashParseError::Validation { .. })
        ));

        let names = slash_command_names();
        assert!(!names.contains(&"run-workflow".to_string()));
        assert!(!names.contains(&"run-step".to_string()));
    }

    #[test]
    fn slash_suggestions_are_generated_from_clap_commands() {
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
    fn slash_suggestions_include_required_resume_usage() {
        let suggestions = slash_suggestions("/res")
            .into_iter()
            .map(|command| command.usage)
            .collect::<Vec<_>>();

        assert!(suggestions.contains(&"/resume <run-id>".to_string()));
    }

    #[test]
    fn slash_help_rows_include_descriptions_from_clap_commands() {
        let rows = slash_help_rows();
        assert!(rows.iter().any(|row| {
            row.name == "/run"
                && row.usage == "/run [--step] [--workflow <workflow-id>] <request>"
                && row.description == "start a workflow run"
                && row.takes_arguments
        }));
        assert!(rows.iter().any(|row| {
            row.name == "/resolve"
                && row.usage == "/resolve <run-id> [status] [fields-json]"
                && !row.usage.contains("--fields")
                && !row.usage.contains("--body")
        }));
        assert!(rows.iter().any(|row| {
            row.name == "/help"
                && row.usage == "/help"
                && row.description == "show built-in commands"
        }));
    }

    #[test]
    fn parser_crate_stays_runtime_and_ui_independent() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in [
            "shlex",
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

    trait SharedCommandTestExt {
        fn into_request(self) -> String;
    }

    impl SharedCommandTestExt for SharedCommand {
        fn into_request(self) -> String {
            match self {
                SharedCommand::Run(args) => args.into_request(),
                other => panic!("expected run command, got {other:?}"),
            }
        }
    }
}
