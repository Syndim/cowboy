# Plan

Add an explicit workflow-selection path for starting new runs so users can bypass the agent-backed selector when they already know the workflow to use.

Expose the feature as a named workflow run command:

- CLI: `cowboy run --workflow <workflow-id> <request...>` starts the requested catalog workflow and runs until blocked, failed, suspended, or completed.
- CLI: `cowboy run --step --workflow <workflow-id> <request...>` starts the requested catalog workflow and executes only its first step, matching existing `--step` behavior.
- TUI: `/run-workflow <workflow-id> <request>` starts the requested catalog workflow from the composer.

Use the catalog workflow id as the accepted `workflow-id`. This is the stable id shown by the catalog, such as the built-in `default` id or a filesystem id derived from the `.lua` entry path. Do not resolve by compiled Lua `workflow("name", ...)` alone because multiple catalog entries can declare the same workflow name and resolving names would require compiling candidates before selection.

Keep the default path unchanged: `cowboy run <request>`, `/run <request>`, `/run-step <request>`, and plain TUI text still use the existing agent-backed workflow selector.

# Changes

- Update `crates/workflow/engine/src/runtime.rs`:
  - add public methods such as `start_run_with_workflow(workflow_id, request)` and `start_run_with_workflow_stepwise(workflow_id, request)`;
  - factor the current `start_with` flow so both agent-selected and user-selected starts share catalog loading, source compilation, run creation, event persistence, locking, and `run_existing` behavior;
  - for the user-selected path, look up `workflow_id` directly in `catalog.workflows` and return a `WorkflowError::InvalidAction`-style error when it is missing;
  - keep `select_workflow` and `SelectorMode` behavior untouched for existing run paths.
- Update `crates/tui/src/main.rs`:
  - add an optional `#[arg(long)] workflow: Option<String>` field to the existing `Run` subcommand;
  - route `cowboy run --workflow <workflow-id> <request...>` to the new runtime method;
  - make `--step` compose with `--workflow` by routing to the stepwise named-workflow runtime method;
  - preserve current `cowboy run <request...>` and `cowboy run --step <request...>` behavior when `--workflow` is absent.
- Update `crates/tui/src/app/commands.rs`:
  - add `/run-workflow <workflow-id> <request>` to `SLASH_COMMANDS` and help output;
  - dispatch `/run-workflow ...` before the existing `/run ...` branch;
  - parse the first non-empty argument as `workflow_id` and the remaining text as the initial prompt;
  - show a usage/status card instead of starting a run when either the workflow id or prompt is missing;
  - spawn a background report task that calls the new runtime named-workflow start method.
- Update `crates/tui/src/app/markup.rs` so `/run-workflow` and `/run-workflow <workflow-id> <request>` render as command lines.
- Update `README.md` and `AGENTS.md` command lists only where they document product CLI/TUI usage:
  - document `cowboy run --workflow <workflow-id> <request...>`;
  - document `/run-workflow <workflow-id> <request>`;
  - state that `<workflow-id>` is the catalog id shown by `/workflows` or other catalog listings, not necessarily the Lua-declared workflow name.
- Do not add workflow runtime logic to the TUI crate; TUI and CLI should only parse command input and delegate to `WorkflowRuntime`.

# Tests to be added/updated

- Add focused `cowboy-workflow-engine` runtime tests in `crates/workflow/engine/src/runtime.rs`:
  - create two temporary status-only workflows with distinct catalog ids;
  - call `start_run_with_workflow` for the second workflow and assert the persisted run uses that workflow rather than the deterministic first workflow;
  - call the stepwise named-workflow start and assert it stops after the first step with the selected workflow persisted;
  - call the named-workflow start with an unknown id and assert it returns an actionable error without creating a run.
- Add CLI parser/routing tests in `crates/tui/src/main.rs`:
  - `cowboy run --workflow review "do work"` parses the workflow id and request;
  - `cowboy run --step --workflow review "do work"` parses both options;
  - `cowboy run "do work"` still parses with no workflow override.
- Add TUI command tests in `crates/tui/src/app/commands.rs` or the existing app test module:
  - slash suggestions include `/run-workflow <workflow-id> <request>`;
  - `/run-workflow review do work` spawns a named-workflow background task and uses a status label that includes the workflow id;
  - `/run-workflow` and `/run-workflow review` show usage/status cards and do not spawn background tasks;
  - `/run <request>` and plain text still dispatch to the selector-backed start path.
- Add markup tests in `crates/tui/src/app/markup.rs`:
  - `/run-workflow review do work` is classified as a command line;
  - prose containing `run-workflow` is not classified as a command line.
- Update README-facing snapshot or help tests if they assert command lists.

# How to verify

- Run `cargo test -p cowboy-workflow-engine runtime::tests::start_run_with_workflow_uses_requested_catalog_id` or the exact focused test name added for the runtime path.
- Run `cargo test -p cowboy-workflow-engine runtime::tests::start_run_with_workflow_rejects_unknown_catalog_id` or the exact focused error-path test name.
- Run `cargo test -p cowboy main::tests` after updating CLI parser tests.
- Run `cargo test -p cowboy app::commands` after updating TUI command tests.
- Run `cargo test -p cowboy app::markup` after updating command-line rendering tests.
- Run `cargo test -p cowboy-workflow-engine` after focused engine tests pass.
- Run `cargo test -p cowboy` after focused TUI/CLI tests pass.
- Manual CLI smoke test with a temporary config and two workflows:
  - run `cowboy run --workflow <second-workflow-id> <request>`;
  - confirm printed `workflow=<selected-workflow>` and persisted events come from the requested workflow, not the selector's default choice.
- Manual TUI smoke test with the same config:
  - launch the TUI;
  - submit `/workflows` to confirm the catalog id;
  - submit `/run-workflow <workflow-id> <request>`;
  - confirm the transcript starts a run for the requested workflow.

# TODO

- [x] Add named-workflow start methods to `WorkflowRuntime`.
- [x] Factor shared run-start creation logic so selected and explicit workflows use the same persistence path.
- [x] Validate explicit workflow ids against `catalog.workflows` with an actionable missing-workflow error.
- [x] Preserve selector-backed behavior for existing `start_run` and `start_run_stepwise` methods.
- [x] Add `--workflow <workflow-id>` parsing to the CLI `run` subcommand.
- [x] Route CLI `run --workflow` and `run --step --workflow` through the new runtime methods.
- [x] Add `/run-workflow <workflow-id> <request>` slash-command metadata and help text.
- [x] Dispatch TUI `/run-workflow` inputs through a new named-workflow background task.
- [x] Show TUI usage/status cards for missing `/run-workflow` workflow ids or prompts.
- [x] Update command-line rendering recognition for `/run-workflow`.
- [x] Update README and command documentation for explicit workflow selection.
- [x] Add focused runtime tests for selected workflow, stepwise behavior, and unknown workflow id.
- [x] Add focused CLI parser tests for `--workflow` with and without `--step`.
- [x] Add focused TUI command tests for valid and invalid `/run-workflow` inputs.
- [x] Add focused markup tests for `/run-workflow` recognition.
- [x] Run the focused engine, CLI, TUI command, and markup tests.
- [x] Run the full `cowboy-workflow-engine` and `cowboy` crate test suites.
- [x] Smoke-test CLI explicit workflow selection with a temporary multi-workflow catalog.
- [x] Smoke-test TUI `/run-workflow` explicit workflow selection with a temporary multi-workflow catalog.
