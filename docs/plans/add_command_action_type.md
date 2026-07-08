# Plan

Add a fifth workflow action, `action.command`, that runs one command-line program with an explicit argument vector and records the result as a normal completed step output. Keep the interface narrow and deterministic: workflows provide `program`, optional `args`, and optional routing/timeout settings; Cowboy executes the program directly without a shell, from the runtime working directory, captures bounded stdout/stderr, and routes by exit success.

The action should live behind the existing action seam. `cowboy-workflow-core` owns the declarative `CommandAction` model, `cowboy-workflow-lua` converts Lua action tables into that model, and `cowboy-workflow-actions` owns process execution through a new `CommandActionRunner`. The engine should only wire runtime configuration such as `cwd` into the dispatcher; UI and CLI surfaces should continue consuming generic step events and records without command-specific branching.

Initial semantics:

- Lua shape: `action.command { program = "git", args = { "status", "--short" } }`.
- `program` is required and must be a non-empty string.
- `args` is optional and defaults to an empty string array; each arg must be a string.
- `success_status` and `failure_status` are optional and default to `"success"` and `"failed"`.
- `timeout_ms` is optional; when present and exceeded, the child is killed and the step completes with `failure_status` plus `timed_out = true`.
- The runner must invoke `tokio::process::Command` directly, not a shell, so quoting and interpolation are never interpreted by Cowboy.
- The command runs in `RuntimeConfig.cwd`; do not add workflow-authored cwd/env/stdin support in this feature.
- The completed `StepRecord` action is `"command"`. `StepOutput.status` is `success_status` for zero exit status, otherwise `failure_status`.
- `StepOutput.fields` includes at least `program`, `args`, `success`, `exit_code`, `stdout`, `stderr`, `timed_out`, and truncation flags for captured streams.
- `StepOutput.body` should be useful human-readable command output: stdout on success; stderr if present on failure, otherwise stdout.
- Spawn errors should complete with `failure_status` and a `spawn_error` field so workflows can route them; malformed action definitions remain `InvalidAction`/Lua conversion errors.

# Changes

- In `crates/workflow/core/src/action.rs`, add `StepAction::Command(CommandAction)`, a serializable `CommandAction` struct, default status helpers, and the `"command"` branch in `StepAction::action_name`.
- Update core action tests in `crates/workflow/core/src/action.rs` and core engine test dispatchers in `crates/workflow/core/src/engine.rs` so action variant coverage includes `command`.
- In `crates/workflow/lua/src/api.rs`, register `command` in the `action` helper table alongside `agent`, `status`, `ask_user`, and `fail`.
- In `crates/workflow/lua/src/convert.rs`, add a `"command"` conversion branch that validates `program`, converts `args` with the existing string-array pattern, accepts optional `success_status`, `failure_status`, and `timeout_ms`, and reports invalid values through `Error::InvalidActionField`.
- Update Lua runtime/conversion tests in `crates/workflow/lua/src/runtime.rs` for successful command conversion and validation failures for missing program, non-string args, and invalid timeout values.
- In `crates/workflow/actions/src/command.rs`, add `CommandActionRunner` that uses `tokio::process::Command`, `kill_on_drop(true)`, explicit `args`, configured `cwd`, bounded stdout/stderr capture, optional timeout handling, and conversion into a completed `StepRecord`.
- In `crates/workflow/actions/src/lib.rs`, add the `command` module/export, store a `CommandActionRunner` in `EngineActionDispatcher`, dispatch `StepAction::Command`, and adjust the dispatcher constructor to receive the runtime cwd or a small command-runner config.
- In `crates/workflow/actions/Cargo.toml`, add the required `tokio` features for async process execution and timeout support.
- In `crates/workflow/engine/src/runtime.rs`, pass `RuntimeConfig.cwd` into `EngineActionDispatcher` when building the workflow runner.
- Update engine/runtime test fixtures that construct `EngineActionDispatcher` or enumerate action variants so they use the new constructor and cover `command` where appropriate.
- Update `crates/workflow/engine/src/runner.rs` test `NoopDispatcher` handling so command actions do not get mislabeled as agent actions in tests.
- Update `docs/workflow-authoring.md` to document `action.command`, its fields, non-shell execution rule, current-working-directory behavior, output fields, timeout behavior, and transition routing.
- Update `docs/architecture.md` and `docs/module-map.md` only if their action lists or crate responsibility descriptions enumerate the full set of action types.

# Tests to be added/updated

- Add core serialization/deserialization tests proving `StepAction::Command` serializes with `action = "command"`, defaults missing optional fields, and returns `"command"` from `action_name`.
- Add core execution/dispatcher tests proving command actions pass through `execute_step` like other completed action types and route by returned status.
- Add Lua conversion tests proving `action.command { program, args }` produces `StepAction::Command` with defaults.
- Add Lua conversion tests for invalid command action tables: missing/empty `program`, non-array or non-string `args`, empty status overrides, and zero/invalid `timeout_ms`.
- Add action-runner tests using the current test binary as a helper command so tests do not depend on external programs.
- Add action-runner tests for zero exit, non-zero exit, spawn error, timeout kill, stdout/stderr capture, stream truncation flags, and working-directory inheritance from dispatcher config.
- Add dispatcher tests proving `EngineActionDispatcher` routes `StepAction::Command` to `CommandActionRunner` without invoking the agent handler.
- Add an engine runtime integration test with a temporary Lua workflow whose first step returns `action.command`, then verify the run completes or transitions based on the command exit status and exposes stdout/stderr through `ctx.prev.fields`.
- Update documentation-adjacent tests or examples if any enumerate available Lua actions.

# How to verify

- Run `cargo test -p cowboy-workflow-core command`.
- Run `cargo test -p cowboy-workflow-lua command`.
- Run `cargo test -p cowboy-workflow-actions command`.
- Run `cargo test -p cowboy-workflow-engine command`.
- Run `cargo test -p cowboy-workflow-engine runtime` if the command runtime test is not filterable by `command`.
- Run `cargo test -p cowboy-workflow-actions` after adding the new process runner dependency and dispatcher constructor changes.
- Run `cargo test -p cowboy-workflow-lua` after updating the authoring API action table.
- Run a manual `engine-cli` smoke workflow that executes a harmless command with args, confirms a zero exit routes to `success`, confirms a non-zero exit routes to `failed`, and confirms `ctx.prev.fields.stdout`/`stderr` are available to a following status step.

# TODO

- [x] Add the core `CommandAction` model and `StepAction::Command` variant.
- [x] Add command action defaults and `action_name` coverage in core.
- [x] Update core action serialization and execution tests for the command variant.
- [x] Register `action.command` in the Lua authoring API.
- [x] Add Lua conversion for command action tables.
- [x] Add Lua validation tests for valid and invalid command action definitions.
- [x] Add the async command runner module in `cowboy-workflow-actions`.
- [x] Implement direct non-shell process spawning with explicit args and runtime cwd.
- [x] Implement bounded stdout/stderr capture and truncation metadata.
- [x] Implement optional timeout handling that kills the child and returns failure output.
- [x] Map exit status, spawn errors, stdout, stderr, and timeout state into `StepOutput`.
- [x] Wire `CommandActionRunner` into `EngineActionDispatcher`.
- [x] Add required `tokio` process/time dependencies to the actions crate.
- [x] Pass `RuntimeConfig.cwd` from the engine runtime into the dispatcher.
- [x] Update dispatcher, runner, and runtime tests/fixtures for the new constructor and action variant.
- [x] Add action-runner tests for success, failure, spawn error, timeout, truncation, and cwd behavior.
- [x] Add an engine runtime Lua workflow integration test for command routing and `ctx.prev.fields`.
- [x] Document `action.command` in workflow authoring docs.
- [x] Update architecture/module docs if they enumerate the complete action set.
- [x] Run the focused verification commands and manual engine-cli smoke test.

## Review follow-up TODO

- [x] Define and test explicit command environment policy.
- [x] Remove persisted cwd from command step input context.
- [x] Stop duplicating captured streams into `StepOutput.raw`.
- [x] Add Lua validation coverage for empty `failure_status`.
- [x] Add stderr truncation coverage.
- [x] Update AGENTS.md action lists and step-execution diagram.
- [x] Use deterministic absolute missing path in dispatcher command route test.
- [x] Re-run focused verification after reviewer fixes.

## Documentation review follow-up TODO

- [x] Document that `action.command` is not a sandbox and should be used only with trusted workflows/commands.
- [x] Document that child processes can access filesystem and network resources available to the Cowboy process.
- [x] Include persisted `program` metadata in the sensitive-data warning and recommend bare executable names or non-sensitive paths.
