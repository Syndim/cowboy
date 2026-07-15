//! Clap-backed command parsing for Cowboy entry points.
//!
//! This crate owns the grammar for both product CLI commands and interactive TUI
//! slash commands. Callers pass process-style argv or a raw slash-command input
//! string and receive runtime-agnostic typed command values plus generated help
//! and completion UI. The crate intentionally has no dependency on the workflow
//! runtime, ratatui, terminal input, config loading, or app state.

use std::ffi::OsString;
use std::fmt;
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

/// Validation error for resolve field pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveFieldError {
    message: String,
}

impl ResolveFieldError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ResolveFieldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ResolveFieldError {}

fn parse_field_value(name: &str, raw_value: &str) -> Result<serde_json::Value, ResolveFieldError> {
    match serde_json::from_str(raw_value) {
        Ok(value) => Ok(value),
        Err(err) if looks_like_structured_json(raw_value) => Err(ResolveFieldError::new(format!(
            "field {name:?} has malformed JSON value: {err}"
        ))),
        Err(_) => Ok(serde_json::Value::String(raw_value.to_string())),
    }
}

/// Assemble raw name/value pairs into the object expected by the runtime.
pub fn resolve_fields_object(
    field_values: Vec<String>,
) -> Result<Option<serde_json::Value>, ResolveFieldError> {
    if field_values.is_empty() {
        return Ok(None);
    }

    let mut fields = serde_json::Map::new();
    let mut pairs = field_values.chunks_exact(2);
    for pair in &mut pairs {
        let name = &pair[0];
        if fields.contains_key(name) {
            return Err(ResolveFieldError::new(format!(
                "field {name:?} was provided more than once"
            )));
        }

        fields.insert(name.clone(), parse_field_value(name, &pair[1])?);
    }

    debug_assert!(pairs.remainder().is_empty());
    Ok(Some(serde_json::Value::Object(fields)))
}

fn looks_like_structured_json(raw_value: &str) -> bool {
    matches!(raw_value.trim_start().chars().next(), Some('[' | '{' | '"'))
}

/// Arguments for resolving a failed run.
#[derive(Debug, Clone, Args, PartialEq, Eq)]
pub struct ResolveArgs {
    #[arg(value_name = "run-id")]
    pub run_id: String,

    /// Status to resolve the failed step to. Omit to list options.
    #[arg(value_name = "status", allow_hyphen_values = true)]
    pub status: Option<String>,

    /// Output field name and value. Repeat for multiple fields.
    #[arg(
        long = "field",
        value_names = ["name", "value"],
        num_args = 2,
        action = ArgAction::Append,
        allow_hyphen_values = true,
        requires = "status"
    )]
    pub fields: Vec<String>,

    /// Human-readable body for the synthesized step record.
    #[arg(long, value_name = "text", requires = "status")]
    pub body: Option<String>,
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
    if arg.is_hide_set() {
        return None;
    }

    if let Some(long) = arg.get_long() {
        let usage = if matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse) {
            format!("--{long}")
        } else {
            format!("--{long} {}", value_names(arg))
        };
        let mut usage = optional_usage(arg, usage);
        if matches!(arg.get_action(), ArgAction::Append) {
            usage.push_str("...");
        }

        return Some(usage);
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

fn value_names(arg: &Arg) -> String {
    arg.get_value_names()
        .map(|names| {
            names
                .iter()
                .map(|name| format!("<{name}>"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| format!("<{}>", arg.get_id().as_str().replace('_', "-")))
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
    fn resolve_parses_identically_for_cli_and_slash_commands() {
        assert_eq!(
            shared_cli_command(["cowboy", "resolve", "run-1"]),
            shared_slash_command("/resolve run-1")
        );
        assert_eq!(
            shared_cli_command([
                "cowboy",
                "resolve",
                "run-1",
                "approved",
                "--field",
                "summary",
                "work completed",
                "--body",
                "looks good",
            ]),
            shared_slash_command(
                r#"/resolve run-1 approved --field summary "work completed" --body "looks good""#,
            )
        );

        let command = shared_cli_command([
            "cowboy",
            "resolve",
            "run-1",
            "failed",
            "--field",
            "reason",
            "needs work",
            "--field",
            "link",
            "https://example.test?a=b=c",
        ]);
        assert_eq!(
            command,
            shared_slash_command(
                r#"/resolve run-1 failed --field reason "needs work" --field link https://example.test?a=b=c"#,
            )
        );

        let SharedCommand::Resolve(args) = command else {
            panic!("expected resolve command");
        };
        assert_eq!(
            resolve_fields_object(args.fields).unwrap(),
            Some(serde_json::json!({
                "link": "https://example.test?a=b=c",
                "reason": "needs work",
            }))
        );
    }

    #[test]
    fn resolve_field_pairs_preserve_json_types_and_plain_strings() {
        let command = shared_cli_command([
            "cowboy",
            "resolve",
            "run-1",
            "success",
            "--field",
            "summary",
            "done",
            "--field",
            "retry",
            "false",
            "--field",
            "count",
            "3",
            "--field",
            "files",
            r#"["src/a.rs"]"#,
            "--field",
            "metadata",
            r#"{"owner":"dev"}"#,
            "--field",
            "note",
            "null",
        ]);
        let SharedCommand::Resolve(args) = command else {
            panic!("expected resolve command");
        };

        assert_eq!(
            resolve_fields_object(args.fields).unwrap(),
            Some(serde_json::json!({
                "summary": "done",
                "retry": false,
                "count": 3,
                "files": ["src/a.rs"],
                "metadata": {"owner": "dev"},
                "note": null,
            }))
        );
    }

    #[test]
    fn resolve_preserves_boundary_names_and_hyphen_values() {
        let arguments = [
            "cowboy",
            "resolve",
            "run-1",
            "success",
            "--field",
            "foo=bar",
            "value=with=equals",
            "--field",
            "-review",
            "-declined",
            "--field",
            " review ",
            " spaced value ",
            "--field",
            "",
            "empty name",
            "--field",
            "--body",
            "--field",
        ];
        let command = shared_cli_command(arguments);
        assert_eq!(
            command,
            shared_slash_command(
                r#"/resolve run-1 success --field foo=bar value=with=equals --field -review -declined --field " review " " spaced value " --field "" "empty name" --field --body --field"#,
            )
        );
        let SharedCommand::Resolve(args) = command else {
            panic!("expected resolve command");
        };

        assert_eq!(
            resolve_fields_object(args.fields).unwrap(),
            Some(serde_json::json!({
                "foo=bar": "value=with=equals",
                "-review": "-declined",
                " review ": " spaced value ",
                "": "empty name",
                "--body": "--field",
            }))
        );
    }

    #[test]
    fn resolve_rejects_malformed_values_and_exact_duplicate_names() {
        for (name, value) in [
            ("files", "[\"private-file-name\""),
            ("credentials", "{\"token\":\"private-token\""),
            ("quoted", "\"private-content"),
        ] {
            let SharedCommand::Resolve(args) = shared_cli_command([
                "cowboy", "resolve", "run-1", "success", "--field", name, value,
            ]) else {
                panic!("expected resolve command");
            };
            let err = resolve_fields_object(args.fields).unwrap_err();
            let message = err.to_string();
            assert!(message.contains(&format!("field {name:?} has malformed JSON value:")));
            assert!(message.contains("line 1 column"), "{message}");
            assert!(!message.contains(value), "{message}");
            assert!(!message.contains("private-"), "{message}");
        }

        let SharedCommand::Resolve(args) = shared_cli_command([
            "cowboy", "resolve", "run-1", "success", "--field", "summary", "first", "--field",
            "summary", "second",
        ]) else {
            panic!("expected resolve command");
        };
        let err = resolve_fields_object(args.fields).unwrap_err();
        assert_eq!(
            err.to_string(),
            "field \"summary\" was provided more than once"
        );

        assert_eq!(
            resolve_fields_object(vec![
                " summary".to_string(),
                "one".to_string(),
                "summary".to_string(),
                "two".to_string(),
            ])
            .unwrap(),
            Some(serde_json::json!({" summary": "one", "summary": "two"}))
        );
    }

    #[test]
    fn resolve_fields_and_body_require_status_on_both_surfaces() {
        for args in [
            vec!["cowboy", "resolve", "run-1", "--field", "summary", "one"],
            vec!["cowboy", "resolve", "run-1", "--body", "details"],
        ] {
            let err = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
            assert!(err.to_string().contains("<status>"), "{err}");
        }

        for input in [
            "/resolve run-1 --field summary one",
            "/resolve run-1 --body details",
        ] {
            let Err(SlashParseError::Validation { message, .. }) = parse_slash_command(input)
            else {
                panic!("resolve payload without status unexpectedly parsed: {input}");
            };
            assert!(message.contains("<status>"), "{message}");
        }
    }

    #[test]
    fn resolve_help_and_removed_forms_match_clean_cutover() {
        let mut command = Cli::command();
        let resolve = command
            .find_subcommand_mut("resolve")
            .expect("resolve subcommand exists");
        let mut help = Vec::new();
        resolve.write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();

        assert!(help.contains("--field <name> <value>"), "{help}");
        assert!(help.contains("--body <text>"), "{help}");
        assert!(!help.contains("name=value"), "{help}");
        assert!(!help.contains("--fields"), "{help}");
        assert!(!help.contains("fields-json"), "{help}");

        for args in [
            vec![
                "cowboy",
                "resolve",
                "run-1",
                "success",
                "--field",
                "summary=value",
            ],
            vec!["cowboy", "resolve", "run-1", "success", "--fields", "{}"],
            vec!["cowboy", "resolve", "run-1", "success", "{}"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }

        for input in [
            "/resolve run-1 success --field summary=value",
            "/resolve run-1 success --fields '{}'",
            "/resolve run-1 success '{}'",
        ] {
            assert!(
                matches!(
                    parse_slash_command(input),
                    Err(SlashParseError::Validation { .. })
                ),
                "removed syntax unexpectedly parsed: {input}"
            );
        }
        assert_eq!(
            slash_command_usage("resolve").as_deref(),
            Some("/resolve <run-id> [status] [--field <name> <value>]... [--body <text>]")
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
    fn slash_suggestions_include_resolve_and_resume_usage() {
        let suggestions = slash_suggestions("/res")
            .into_iter()
            .map(|command| command.usage)
            .collect::<Vec<_>>();

        assert!(suggestions.contains(&"/resume <run-id>".to_string()));
        assert!(suggestions.contains(
            &"/resolve <run-id> [status] [--field <name> <value>]... [--body <text>]".to_string()
        ));
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
                && row.usage
                    == "/resolve <run-id> [status] [--field <name> <value>]... [--body <text>]"
                && row.description == "list or resolve a failed step"
                && row.takes_arguments
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
