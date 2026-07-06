# Plan

Add a first-class resume command that continues an existing `Running` workflow run until it blocks, fails, or completes. The workflow engine already exposes this behavior as `WorkflowRuntime::resume_run`, and the diagnostic `engine-cli` already has a `resume <run-id>` command. The product surfaces are missing it: the `cowboy` CLI only exposes `step`, and the TUI slash commands only expose `/step`.

Keep the engine behavior unchanged. Reuse the existing `WorkflowRuntime::resume_run` interface and `RunReport` rendering so resume has the same event persistence, run locking, status handling, and output formatting as `run`, `answer`, `resolve`, and `step`. Do not add a new workflow state, store table, Lua action, or run-selection policy.

Expose resume in both user-facing interfaces:

- CLI: `cowboy resume <run-id>` continues that run until blocked or terminal.
- TUI: `/resume [run-id]` continues the provided run id, or the current active run when no run id is provided.

The optional TUI run id matches the phrase "current workflow" without inventing global CLI state. If no TUI active run is known, `/resume` should show a usage/status card instead of starting a new workflow or panicking.

# Changes

- Update `crates/tui/src/main.rs`:
  - add a `Resume { run_id: String }` `clap` subcommand near `Step`;
  - document it as continuing a workflow run until it blocks, fails, or completes;
  - route it to `WorkflowRuntime::resume_run(&run_id).await?` and reuse `print_report(&report)`.
- Update `crates/tui/src/app/commands.rs`:
  - add `/resume` to `SLASH_COMMANDS` with usage `/resume [run-id]` and a description such as `continue a run until blocked`;
  - add dispatch for both exact `/resume` and `/resume <run-id>` before the plain-text fallback;
  - implement a `spawn_resume_run` helper mirroring `spawn_step_run`, but calling `runtime.resume_run(&run_id)` and using a status label such as `submitted resume: <run-id>`;
  - when `/resume` has no explicit run id, use `state.active_run_id()` if present;
  - when `/resume` has no explicit run id and no active run exists, set status and push a usage card, for example `usage: /resume [run-id]`.
- Preserve existing behavior:
  - `/step <run-id>` still executes exactly one step;
  - `/run-step <request>` still starts only the first step;
  - plain text still starts a new workflow unless the TUI is waiting for prompt input;
  - prompt answers still take precedence after explicit slash-command handling.
- Update `crates/tui/src/app/markup.rs` so `/resume` and `/resume <run-id>` are treated as command lines where command-line rendering is applied.
- Update `README.md`:
  - add `cowboy resume <run-id>` to the CLI quick-start/command section near `cowboy step <run-id>`;
  - add `/resume [run-id]` to the TUI command list;
  - describe the difference between `step` and `resume`: `step` advances one workflow step, `resume` runs until the workflow blocks, fails, or completes.
- Update docs only where user-facing command lists are maintained. Do not change workflow runtime docs except to mention the newly exposed command if the same command list is duplicated there.

# Tests to be added/updated

- Add or update `crates/workflow/engine/src/runtime.rs` coverage for the existing engine behavior if it is not already covered by a focused test:
  - create a temporary two-step status workflow;
  - call `start_run_stepwise` and assert the run remains `Running` at the second step;
  - call `resume_run` and assert it completes by executing the remaining step;
  - assert persisted events include the resumed step lifecycle.
- Add `crates/tui/src/main.rs` parser/routing coverage for the product CLI:
  - verify `cowboy resume <run-id>` parses as the new `Resume` subcommand;
  - verify the subcommand requires a run id.
- Add `crates/tui/src/app/commands.rs` or `crates/tui/src/app/tests.rs` TUI command coverage:
  - slash suggestions include `/resume [run-id]`;
  - submitting `/resume <run-id>` spawns a background report task that calls the resume path;
  - submitting `/resume` with an active run uses `AppState::active_run_id()`;
  - submitting `/resume` without an active run produces a usage/status card and does not spawn a background task.
- Add or update `crates/tui/src/app/markup.rs` tests so `/resume` and `/resume <run-id>` are recognized as command lines, while prose containing the word `resume` is not.
- Update README-facing expectations in any snapshot-like TUI help tests if the help command output is asserted.

# How to verify

- Run `cargo test -p cowboy-workflow-engine runtime::tests::resume_run_continues_stepwise_run_until_blocked` or the exact focused runtime test name added for this change.
- Run `cargo test -p cowboy app::commands` if command-level tests are added there.
- Run `cargo test -p cowboy app::markup` after updating command-line recognition.
- Run `cargo test -p cowboy` after focused TUI tests pass.
- Run `cargo test -p cowboy-workflow-engine` after focused engine tests pass.
- Manual CLI smoke test with a temporary config/workflow directory:
  - start a multi-step workflow with `cargo run -p cowboy -- --config <temp-config> run --step <request>`;
  - copy the printed run id;
  - run `cargo run -p cowboy -- --config <temp-config> resume <run-id>`;
  - confirm the report reaches the next blocking or terminal state without requiring repeated `step` calls.
- Manual TUI smoke test with the same temporary state:
  - launch `cargo run -p cowboy -- --config <temp-config>`;
  - start a stepwise run with `/run-step <request>`;
  - submit `/resume` and confirm the active run continues until blocked or terminal;
  - start or select another running run if needed and submit `/resume <run-id>` to confirm explicit run ids still work;
  - submit `/resume` in a fresh TUI with no active run and confirm it shows usage instead of starting a new workflow.

# TODO

- [x] Add the `cowboy resume <run-id>` CLI subcommand in `crates/tui/src/main.rs`.
- [x] Route the new CLI subcommand through `WorkflowRuntime::resume_run` and existing `print_report` output.
- [x] Add `/resume [run-id]` to TUI slash command metadata and help output.
- [x] Dispatch `/resume <run-id>` to a new TUI background resume task.
- [x] Dispatch `/resume` with no argument to the current `AppState::active_run_id()` when available.
- [x] Show a non-fatal TUI usage/status card for `/resume` when no run id is available.
- [x] Keep `/step`, `/run-step`, plain request submission, and prompt answer routing behavior unchanged.
- [x] Update command-line rendering recognition for `/resume` in `crates/tui/src/app/markup.rs`.
- [x] Update README CLI and TUI command documentation for `resume` and the `step` versus `resume` distinction.
- [x] Add focused engine coverage proving `resume_run` continues a stepwise running workflow until blocked or terminal.
- [x] Add focused CLI parser coverage for `resume <run-id>` and missing run-id rejection.
- [x] Add focused TUI slash-command coverage for explicit run id, active-run fallback, and no-active-run usage behavior.
- [x] Add focused markup coverage for `/resume` command-line recognition.
- [x] Run the focused engine and TUI tests.
- [x] Run the full `cowboy` and `cowboy-workflow-engine` crate test suites.
- [x] Manually smoke-test CLI resume with a temporary workflow state.
- [x] Manually smoke-test TUI `/resume` with explicit, active-run, and no-active-run cases.

# Manual smoke-test evidence

Primary temporary root used for CLI, explicit TUI, and active-run TUI checks: `/tmp/cowboy-resume-review-tf_pbzyk`.

Setup:

- Created `config.toml` pointing at a temporary `state/` directory and `workflows/aaa.lua`.
- Used a deterministic status-only workflow so resume behavior could be exercised without an ACP backend.
- Used `engine-cli run-step` only to seed a `Running` workflow state; the product CLI/TUI resume surfaces were then exercised against that persisted state.

CLI resume smoke:

- Seed command: `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- run-step cli resume smoke`
  - Observed: `status=Running step=finish steps_executed=1` for `run-08712adb-54fb-42a0-ba2a-909b5e8d0991`.
- Product command: `cargo run -p cowboy --quiet -- --config /tmp/cowboy-resume-review-tf_pbzyk/config.toml resume run-08712adb-54fb-42a0-ba2a-909b5e8d0991`
  - Observed: `status=Completed step=finish`.
  - Observed events included `StepStarted { step_id: "finish" }`, `StepCompleted { step_id: "finish", action: "status", status: Some("success"), body: "finished" }`, and `RunCompleted`.

TUI `/resume` no-active-run smoke:

- Temporary root: `/tmp/cowboy-resume-noactive-cmmhprgb`.
- Before command: `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- runs`
  - Observed: `runs (0)`.
- Launch command: `cargo run -p cowboy --quiet -- --config /tmp/cowboy-resume-noactive-cmmhprgb/config.toml`
- Typed `/resume`, then exited with `Ctrl-C`.
  - Observed: usage card/status containing `usage: /resume [run-id]`.
- After command: `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- runs`
  - Observed: `runs (0)`, confirming the bare command did not start a workflow.

TUI `/resume <run-id>` explicit-run smoke:

- Seed command: `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- run-step tui explicit smoke`
  - Observed: `status=Running step=finish steps_executed=1` for `run-c46d8e93-0da1-4b80-85d8-def1e84a01cd`.
- Launch command: `cargo run -p cowboy --quiet -- --config /tmp/cowboy-resume-review-tf_pbzyk/config.toml`
- Typed `/resume run-c46d8e93-0da1-4b80-85d8-def1e84a01cd`, then exited with `Ctrl-C`.
  - Observed TUI transcript included `submitted resume: run-c46d8e93-0da1-4b80-85d8-def1e84a01cd`, `Step started  step=finish`, and `Run completed`.
- Verification command: `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- show run-c46d8e93-0da1-4b80-85d8-def1e84a01cd`
  - Observed persisted run state: `status: Completed`, `current_step: finish`, `steps_executed: 2`.

TUI bare `/resume` active-run smoke:

- Seeded a three-step workflow and command: `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- run-step tui active smoke`
  - Observed: `status=Running step=review steps_executed=1` for `run-c3f97855-a676-4565-a966-e89bc5c94715`.
- Launch command: `cargo run -p cowboy --quiet -- --config /tmp/cowboy-resume-review-tf_pbzyk/config.toml`
- Typed `/step run-c3f97855-a676-4565-a966-e89bc5c94715` to make that run the active TUI run, then typed bare `/resume`, then exited with `Ctrl-C`.
  - Observed TUI transcript included `submitted resume: run-c3f97855-a676-4565-a966-e89bc5c94715`, `Step started  step=finish`, and `Run completed`.
- Verification command: `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- show run-c3f97855-a676-4565-a966-e89bc5c94715`
  - Observed persisted run state: `status: Completed`, `current_step: finish`, `steps_executed: 3`.
