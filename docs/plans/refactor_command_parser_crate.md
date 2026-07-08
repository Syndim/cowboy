# Plan

Refactor `cowboy-command-parser` so clap owns the command grammar for both process CLI input and TUI slash input, with one shared clap subcommand enum flattened into thin CLI/TUI wrapper enums.

Current state from inspection:

- `crates/tui/command-parser/src/lib.rs` already contains both `CliCommand` and `SlashCommand` plus a private clap-backed `SlashCli` parser.
- The parser crate still depends on `shlex` and uses it in `tokenize_slash_input`; this is the remaining non-clap parsing dependency for slash input.
- Shared runtime actions are duplicated across `CliCommand` and `SlashCommand`: run, step, resume, answer, improve, runs, and resolve.
- TUI-only actions are cancel, exit, help, and workflows. CLI-only action is launching the TUI with no subcommand or `tui`.
- Slash-only run forms `/run-workflow` and `/run-step` duplicate CLI `run --workflow` and `run --step`, which prevents the command list from lining up cleanly.
- Help, usage, suggestions, and completion currently come from a handwritten `SlashCommandMetadata`/`SLASH_COMMANDS` registry that can drift from the clap command definitions.

Target command model:

- Add a shared command enum in `cowboy-command-parser`, tentatively `SharedCommand`, that derives `clap::Subcommand` and represents runtime actions both entry points can express:
  - `Run { step: bool, workflow: Option<String>, request: Vec<String> }`, normalized by parser helpers into a request string for dispatch
  - `Step { run_id: String }`
  - `Resume { run_id: String }`
  - `Answer { run_id: String, prompt_id: String, answer: Vec<String> }`, normalized by parser helpers into an answer string for dispatch
  - `Improve { run_id: String }`
  - `Runs`
  - `Resolve { run_id: String, status: Option<String>, fields: Option<String>, body: Option<String> }`
- Keep shell-specific wrappers small and use clap flattening, not conversion traits, to share command definitions:
  - `CliCommand` keeps only CLI-only variants such as `Tui`, plus a newtype variant like `#[command(flatten)] Shared(SharedCommand)`.
  - `SlashCommand` or `TuiCommand` keeps only TUI-only variants such as `Workflows`, `Cancel`, `Exit`, and `Help`, plus a newtype variant like `#[command(flatten)] Shared(SharedCommand)`.
  - Do not implement `TryFrom<CliCommand>` / `TryFrom<SlashCommand>` as the sharing mechanism; the shared runtime commands should be declared once and flattened into both parser surfaces.
- Align argument shapes for shared commands so the flattened enum can be reused directly. In particular, `/resume` should align with `cowboy resume <run-id>` as `/resume <run-id>`; if the active-run resume shortcut is still desired, keep it as an explicitly TUI-only command rather than weakening the shared `resume` command.
- Redesign the advertised command list around aligned command definitions, not a separate metadata table:
  - Shared in both CLI and TUI: `run`, `step`, `resume`, `answer`, `runs`, `improve`, `resolve`.
  - Canonical TUI slash run form: `/run [--step] [--workflow <workflow-id>] <request>` to match `cowboy run [--step] [--workflow <workflow-id>] <request...>`.
  - Remove `/run-step` and `/run-workflow` from slash help, suggestions, parser tests, and product docs as separate command names.
  - Keep TUI-only commands documented separately in the TUI command enum: `/workflows`, `/cancel`, `/exit`, `/help`.
  - Delete `SlashCommandMetadata` and `SLASH_COMMANDS`; command names, usage, aliases, help text, and completion rows should be derived from the clap command definitions via doc comments/`#[command(...)]` attributes and clap `Command` introspection.
  - Keep any helper returned by `slash_suggestions` as a generated view of clap command definitions, not as the source of truth.

# Changes

- Update `crates/tui/command-parser/Cargo.toml` to remove the direct `shlex` dependency.
- Update `crates/tui/command-parser/src/lib.rs`:
  - introduce the shared `SharedCommand` enum deriving `clap::Subcommand`, plus shared clap `Args` structs for repeated argument groups where useful;
  - flatten `SharedCommand` into `CliCommand` with `#[command(flatten)] Shared(SharedCommand)` while keeping `Tui` as CLI-only;
  - flatten the same `SharedCommand` into the TUI slash command enum with `#[command(flatten)] Shared(SharedCommand)` while keeping `Workflows`, `Cancel`, `Exit`, and `Help` as TUI-only;
  - change slash parsing so `/run --step ...` and `/run --workflow <id> ...` are accepted and routed through the same flattened `SharedCommand::Run` variant as CLI `run`;
  - remove slash-only `RunWorkflow` and `RunStep` variants from the public TUI command type;
  - delete `SlashCommandMetadata` and the handwritten `SLASH_COMMANDS` registry;
  - move TUI command descriptions, usage-facing names, aliases, and completion eligibility into clap command definitions using doc comments and `#[command(...)]`/`#[arg(...)]` attributes;
  - implement help, usage-error lookup, slash suggestions, and tab completion from clap's generated command graph, so the command definition remains the single source of truth;
  - replace `shlex::split` and `escape_comment_start_hashes` with an internal tokenizer that does not treat `#` as a comment marker.
- Update `crates/tui/app/src/main.rs` to dispatch shared runtime commands through one helper match, leaving only CLI-only TUI launch outside that path.
- Update `crates/tui/app/src/app/commands.rs` to dispatch shared runtime commands through the same helper logic where practical, and keep only TUI-only actions in a TUI-specific match.
- Update TUI suggestion and help rendering callers to consume generated parser help/completion rows instead of `SLASH_COMMANDS`, `SlashCommandMetadata`, or `slash_command_metadata`.
- Update command-line rendering recognition in `crates/tui/app/src/app/markup.rs` so canonical aligned forms such as `/run --workflow review do work` and `/run --step do work` render as commands, and removed slash command names are no longer treated as canonical commands.
- Update user-facing command documentation in `README.md`, `docs/architecture.md`, and root `AGENTS.md` to show the aligned command list and remove separate `/run-workflow` and `/run-step` entries.
- Run `cargo update -p cowboy-command-parser` or the equivalent lockfile refresh if removing the direct `shlex` dependency changes `Cargo.lock`; do not require `shlex` to disappear globally if another transitive dependency still needs it.

# Tests to be added/updated

- Update parser unit tests in `crates/tui/command-parser/src/lib.rs`:
  - flattened shared command parsing from CLI `cowboy run`, `cowboy run --step`, and `cowboy run --workflow <id>`;
  - flattened shared command parsing from TUI `/run`, `/run --step`, and `/run --workflow <id>`;
  - `/run "do work"` and `/run do work` preserve the same request string;
  - `#123` remains ordinary payload text in `/run` and `/answer`;
  - hyphen-leading requests such as `/run -- --dry-run now` or the chosen canonical clap-compatible form preserve request text;
  - `/resume <run-id>` parses through the flattened shared command for both CLI and TUI, and bare `/resume` fails with clap usage instead of relying on a TUI-specific shared-command shape;
  - `/resolve <run-id>`, `/resolve <run-id> <status>`, and `/resolve <run-id> <status> <fields-json>` keep their current TUI behavior;
  - malformed quotes return `SlashParseError::UnmatchedQuote`;
  - removed command names `/run-workflow` and `/run-step` are not advertised and fail as unknown commands;
  - command descriptions, usage text, and completion rows come from the clap command definitions, with no `SlashCommandMetadata` or `SLASH_COMMANDS` registry left in the parser crate;
  - the parser crate manifest guard confirms no direct dependency on `shlex`, workflow runtime, ratatui, crossterm, or tui-input.
- Update TUI command tests in `crates/tui/app/src/app/commands.rs` and composer tests:
  - slash suggestions show canonical `/run [--step] [--workflow <workflow-id>] <request>` usage derived from the parser command definitions;
  - suggestions no longer include `/run-workflow` or `/run-step`;
  - `/run --workflow review do work` spawns the named-workflow runtime task;
  - `/run --step do work` spawns the stepwise runtime task;
  - `/run do work`, plain text submission, prompt answers, `/resume <run-id>`, `/answer`, `/resolve`, `/cancel`, `/exit`, and `/help` keep their behavior.
- Update markup tests in `crates/tui/app/src/app/markup.rs` for the canonical aligned slash command forms.
- Update any README/docs snapshot-style tests if present after documentation changes.

# How to verify

- Run focused parser tests: `cargo test -p cowboy-command-parser`.
- Run focused TUI/app tests that cover command dispatch, suggestions, and markup: `cargo test -p cowboy app::commands app::controls::composer app::markup` or the closest valid filtered commands after implementation.
- Run package-level verification for affected crates: `cargo test -p cowboy-command-parser -p cowboy`.
- Inspect dependency output with `cargo tree -p cowboy-command-parser` and confirm `shlex` is no longer a direct or transitive dependency of `cowboy-command-parser` unless introduced by clap itself.
- Manual TUI smoke check:
  - `/run --workflow <workflow-id> <request>` starts a named workflow;
  - `/resume <run-id>` resumes the requested run;
  - `/run <request>` still starts selector-backed execution;
  - malformed quoted input shows a usage/error card and does not start a run;
  - `/help` shows the aligned command list.

# TODO

- [x] Remove the direct `shlex` dependency from `crates/tui/command-parser`.
- [x] Add the shared `SharedCommand` enum deriving `clap::Subcommand` and any shared `Args` structs needed for repeated argument groups.
- [x] Flatten `SharedCommand` into the CLI command enum with `#[command(flatten)]` instead of implementing conversion traits.
- [x] Flatten `SharedCommand` into the TUI slash command enum with `#[command(flatten)]` instead of implementing conversion traits.
- [x] Replace slash command tokenization with an internal minimal argv lexer that preserves current payload behavior.
- [x] Delete `SlashCommandMetadata`, `SLASH_COMMANDS`, and `slash_command_metadata` from `cowboy-command-parser`.
- [x] Move slash command help, usage, aliases, and completion eligibility into clap command definitions.
- [x] Generate slash suggestions, usage-error messages, and help rows from clap command definitions.
- [x] Replace `/run-workflow` and `/run-step` with canonical `/run --workflow` and `/run --step` parser behavior.
- [x] Align `/resume` with CLI `resume` by requiring `<run-id>` for the shared command, or add any active-run shortcut only as a separate TUI-only command.
- [x] Refactor CLI dispatch in `crates/tui/app/src/main.rs` to consume shared commands for runtime actions.
- [x] Refactor TUI dispatch in `crates/tui/app/src/app/commands.rs` to consume shared commands for runtime actions.
- [x] Update slash suggestions, help rendering, and command-line markup for the redesigned command list.
- [x] Update README, architecture docs, and root agent guide command lists for the aligned command names.
- [x] Update parser tests for shared command conversion, tokenizer behavior, canonical slash run forms, and removed slash command names.
- [x] Update TUI dispatch, suggestion, and markup tests for the redesigned command list.
- [x] Refresh `Cargo.lock` if dependency removal changes it.
- [x] Run focused parser and TUI command tests.
- [x] Run affected package-level verification.

## Review feedback TODO

- [x] Make `cowboy resolve --help` advertise `--fields` and `--body` while keeping slash usage positional.
- [x] Remove unused `style_warning` import from the composer control.
- [x] Verify focused feedback fixes.
