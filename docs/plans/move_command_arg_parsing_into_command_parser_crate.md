# Plan

Split the current `cowboy` TUI package into two workspace crates under `crates/tui`: an app crate that keeps package name `cowboy`, and a command-parser crate that owns all command argument parsing for both non-interactive CLI commands and interactive slash commands.

The command-parser crate should be a deep module at the command parsing seam: callers provide either process argv or a raw slash-command composer string, and receive typed command enums plus command metadata. It must not depend on `cowboy-workflow-engine`, ratatui, app state, config loading, terminal input, or runtime dispatch. The app crate remains responsible for config loading, logging setup, runtime calls, terminal rendering, slash suggestions display, and command execution.

Current grounding:

- `crates/tui/src/main.rs` defines the product CLI `Cli` and `Command` clap parser, then dispatches parsed commands to `WorkflowRuntime`.
- `crates/tui/src/app/commands.rs` defines slash command metadata, slash clap parser structs, raw slash input tokenization, parse errors, suggestion helpers, and runtime dispatch.
- `crates/tui/Cargo.toml` currently has both `clap` and `shlex`; `shlex` is only used by slash input tokenization in `app/commands.rs`.
- `docs/module-map.md` and `docs/architecture.md` currently document `cowboy` at `crates/tui` as the only TUI crate.

# Changes

- Restructure the workspace layout:
  - move the existing app package from `crates/tui` to `crates/tui/app` while keeping `[package] name = "cowboy"`;
  - add a new `crates/tui/command-parser` package, named `cowboy-command-parser`;
  - update root `Cargo.toml` `members` and `default-members` from `crates/tui` to `crates/tui/app` and add `crates/tui/command-parser`;
  - update path dependencies affected by the app crate move, especially dependencies in `crates/tui/app/Cargo.toml` that currently point to sibling workflow/log crates.
- Put the command parser interface in `cowboy-command-parser`:
  - expose typed product CLI structs/enums, e.g. `Cli` and `CliCommand`, with `parse`, `try_parse_from`, and config-path accessors usable by `cowboy`'s `main.rs`;
  - expose typed slash command enums, e.g. `SlashCommand`, `SlashParseError`, `SlashCommandMetadata`, and parsing helpers such as `parse_slash_command(input: &str)`;
  - expose slash completion/query helpers or metadata enough for the app crate to implement suggestions without duplicating the registry;
  - keep parser outputs runtime-agnostic strings/options only, not closures or workflow-runtime calls.
- Move CLI clap definitions out of `crates/tui/app/src/main.rs` into `cowboy-command-parser`:
  - preserve current user-facing CLI behavior: `cowboy`, `cowboy tui`, `cowboy run [--step] [--workflow <id>] <request...>`, `cowboy step <run-id>`, `cowboy resume <run-id>`, `cowboy answer <run-id> <prompt-id> <answer>`, `cowboy improve <run-id>`, `cowboy runs`, and `cowboy resolve <run-id> [status] [--fields <json>] [--body <text>]`;
  - keep the global `--config` option defaulting through a callback supplied by, or re-exported from, the app crate without making the parser crate depend on app config loading.
- Move slash command parsing out of `crates/tui/app/src/app/commands.rs` into `cowboy-command-parser`:
  - preserve current slash behavior for `/run`, `/run-workflow`, `/run-step`, `/step`, `/resume`, `/answer`, `/runs`, `/workflows`, `/improve`, `/resolve`, `/cancel`, `/exit`, and `/help`;
  - preserve required-argument usage strings shown by the TUI;
  - preserve quoted multi-word arguments, issue-style `#123` text, hyphen-leading requests such as `/run-step --dry-run now`, optional `/resume`, and optional `/resolve` fields-json behavior;
  - use clap for the command grammar for both CLI and slash commands;
  - keep any raw slash-string lexing as a small internal tokenizer in the parser crate, and remove `shlex` from the app crate. If the implementation can remove the workspace `shlex` dependency without behavior loss, do so; otherwise keep it private to the parser crate and document it as lexing only, not command argument parsing.
- Refactor the app crate to consume parser outputs:
  - in `main.rs`, parse via `cowboy-command-parser` and keep only config loading, logging setup, runtime construction, and command dispatch;
  - in `app/commands.rs`, remove local parser structs/enums/tokenization and dispatch on `cowboy-command-parser::SlashCommand`;
  - keep `show_help`, slash suggestions, completion, and parse-error rendering behavior identical from the user perspective, sourcing metadata from the parser crate;
  - keep plain-text request routing and pending-prompt fallback in the app crate.
- Update package docs and repository docs:
  - update `crates/tui/app/src/lib.rs` crate-level docs to describe app responsibilities after parser extraction;
  - add crate-level docs for `cowboy-command-parser` explaining its inputs, typed outputs, and no-runtime-dependency rule;
  - update `docs/module-map.md` and `docs/architecture.md` so the TUI section shows `crates/tui/app` and `crates/tui/command-parser` with separate responsibilities.

# Tests to be added/updated

- Add parser-crate unit tests for the existing CLI parser cases currently in `crates/tui/src/main.rs`:
  - `run --workflow` preserves workflow id and trailing request words;
  - `run --step --workflow` parses both options;
  - `run` without workflow keeps the selector-backed form;
  - `resolve` without status parses as list form;
  - `resume <run-id>` parses and missing run id is rejected;
  - `resolve <run-id> <status> --fields <json> --body <text>` parses fields and body.
- Add parser-crate unit tests for existing slash parser behavior currently in `crates/tui/src/app/commands.rs`:
  - quoted and unquoted `/run` requests parse equivalently;
  - `/run-workflow` captures workflow id and trailing request;
  - `#123` text is preserved in `/run`, `/run-workflow`, and `/answer`;
  - `/run-step --dry-run now` preserves the hyphen-leading request;
  - bare and explicit `/resume` parse correctly;
  - `/answer` preserves quoted/unquoted multi-word answers;
  - `/resolve` parses list, status-only, and quoted fields-json forms;
  - malformed quotes return `SlashParseError::UnmatchedQuote`;
  - every advertised slash command has a parse sample and metadata.
- Update app-crate tests that currently reach private parser internals so they assert behavior through the parser crate or through public app dispatch:
  - slash help uses parser metadata;
  - slash suggestions filter by command prefix and include `/resume [run-id]`;
  - missing slash args show usage without spawning background tasks;
  - parser errors show usage without starting a plain-text run;
  - dispatch tests for `/run`, `/run-workflow`, `/resume`, `/answer`, `/resolve`, `/cancel`, `/exit`, and plain text still pass.
- Add a compile-level guard if practical: `cowboy-command-parser` should not depend on `cowboy-workflow-engine`, `ratatui`, `crossterm`, or `tui-input`.

# How to verify

- Run parser-crate tests:

```bash
cargo test -p cowboy-command-parser
```

- Run focused app parser/dispatch tests after the move:

```bash
cargo test -p cowboy --lib app::commands
```

- Run focused CLI parser tests if any remain in the app package after extraction:

```bash
cargo test -p cowboy --bin cowboy
```

- Run the app package test suite:

```bash
cargo test -p cowboy
```

- Run a workspace check to catch moved-path dependency issues:

```bash
cargo check --workspace
```

- Manually smoke the unchanged entry points after the package path move:

```bash
cargo run -p cowboy -- --help
cargo run -p cowboy -- run --step --workflow default smoke request
```

# TODO

- [x] Create `crates/tui/app` and move the existing `cowboy` app package there without changing package name or binary name.
- [x] Create `crates/tui/command-parser` as package `cowboy-command-parser`.
- [x] Update workspace `members` and `default-members` for the new app and command-parser crate paths.
- [x] Fix moved app-crate path dependencies in `crates/tui/app/Cargo.toml`.
- [x] Move product CLI clap types and parse helpers from app `main.rs` into `cowboy-command-parser`.
- [x] Move slash command clap types, metadata, parse errors, slash query, suggestions, and completion metadata from app `commands.rs` into `cowboy-command-parser`.
- [x] Preserve current CLI command shapes, aliases, global config flag, and parse errors.
- [x] Preserve current slash command parsing behavior for quotes, hash references, hyphen-leading trailing args, optional `/resume`, and optional `/resolve` fields-json.
- [x] Remove direct command parsing dependencies and parser internals from the app crate where the parser crate now owns them.
- [x] Refactor app `main.rs` to consume `cowboy-command-parser` CLI outputs and keep runtime dispatch local.
- [x] Refactor app `app/commands.rs` and composer controls to consume `cowboy-command-parser` slash outputs and metadata while keeping runtime dispatch local.
- [x] Keep plain text submission and pending prompt fallback in the app crate.
- [x] Move or recreate existing CLI parser tests in `cowboy-command-parser`.
- [x] Move or recreate existing slash parser tests in `cowboy-command-parser`.
- [x] Update app dispatch, suggestion, usage, and help tests to use the parser crate interface.
- [x] Add or update dependency-boundary coverage proving the parser crate stays runtime/UI independent.
- [x] Update crate-level docs for both app and parser crates.
- [x] Update `docs/module-map.md` and `docs/architecture.md` for the split TUI crate layout.
- [x] Run `cargo test -p cowboy-command-parser`.
- [x] Run `cargo test -p cowboy --lib app::commands`.
- [x] Run `cargo test -p cowboy`.
- [x] Run `cargo check --workspace`.
- [x] Move TUI suggestion row/max display policy out of `cowboy-command-parser` and back into the app composer.
- [x] Fix `docs/module-map.md` to state that default command and `tui` subcommand launch the TUI.
