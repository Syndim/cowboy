# Plan

Replace the workflow transcript header's UTC wall-clock stamp with elapsed run time. The durable baseline is `WorkflowRun::created_at`; each newly emitted `WorkflowEvent` should carry that baseline so `crates/tui/src/app/events.rs` can render an elapsed stamp without external state.

Render elapsed time in the same header slot that currently uses `event.timestamp.format("%H:%M:%S")`. Format as `HH:MM:SS`, with hours allowed to exceed `23` for long runs. Clamp negative elapsed values to `00:00:00` for clock-skew safety. Existing persisted event logs that do not include the new baseline must still deserialize and should render `00:00:00` rather than UTC.

Scope this to workflow transcript events. Do not change diagnostic log timestamps, persisted `WorkflowRun::created_at` / `updated_at`, step/turn timestamps, or run-list summaries.

# Changes

- Update `crates/workflow/engine/src/events.rs` `WorkflowEvent`:
  - add `run_started_at: Option<DateTime<Utc>>` with `#[serde(default)]` to preserve compatibility with existing event JSON;
  - keep `timestamp` as the event emission timestamp;
  - keep `WorkflowEvent::new` for generic callers/tests, with `run_started_at: None`;
  - add run-aware constructors or builder helpers, such as `for_run(run, kind)`, `run_status_for_run(run, status)`, and `step_completed_for_run(run, record)`, that set `run_started_at = Some(run.created_at)`.
- Update `crates/workflow/engine/src/runner.rs` event emission so runner-created events use the run-aware constructor/helper:
  - `RunStarted` in `run_until_blocked` and `step_once`;
  - `StepStarted` and `RunFailed` in `execute_one`;
  - `StepCompleted` after a new head is saved;
  - `RunStatusChanged`, `WaitingForInput`, `RunCompleted`, and `RunCancelled` status events.
- Update `crates/workflow/engine/src/runtime.rs` event emission outside the runner:
  - in `answer_run`, use the waiting run's original `created_at` for resume-completion and status events before continuing execution;
  - in `run_existing_with_events`, capture the run's original `created_at` beside `run_id` and use it when converting `AgentProgress` into workflow events;
  - keep persisted prefix events and collected events in one chronological list.
- Update `crates/tui/src/app/events.rs`:
  - replace the UTC stamp with an `elapsed_stamp(event)` helper;
  - compute elapsed from `event.timestamp - event.run_started_at.unwrap_or(event.timestamp)`;
  - use integer seconds for display so subsecond differences render as `00:00:00`;
  - keep event titles, metadata, markdown body rendering, styles, and active-event coalescing unchanged.
- Add a direct `chrono` dependency to `crates/tui/Cargo.toml` only if TUI tests or helper signatures need to construct deterministic chrono timestamps directly.
- Leave `crates/workflow/engine/src/bin/engine-cli.rs` unchanged unless compilation requires field construction updates; the feature request targets the TUI transcript display, not diagnostic CLI text.

# Tests to be added/updated

- Add deterministic tests in `crates/tui/src/app/events.rs` for elapsed rendering:
  - an event at `12:34:56Z` with `run_started_at = 12:30:00Z` renders a header beginning with `00:04:56`;
  - the same rendered output does not contain the UTC wall-clock string `12:34:56`;
  - an event more than twenty-four hours after the baseline renders an hour value greater than `23`;
  - an event with `run_started_at = None` renders `00:00:00` and does not panic.
- Update existing TUI renderer tests only where they construct `WorkflowEvent` literals or assert timestamp text; keep assertions for event titles, metadata, bodies, markdown rendering, truncation, and styles.
- Add `crates/workflow/engine/src/events.rs` serialization tests:
  - new events round-trip with `run_started_at` present;
  - legacy JSON without `run_started_at` deserializes with `None`.
- Update `crates/workflow/engine/src/runner.rs` tests to assert all runner-emitted events for a run have `run_started_at == Some(run.created_at)`.
- Add or update `crates/workflow/engine/src/runtime.rs` tests for non-runner event paths:
  - `answer_run` events preserve the waiting run's original `created_at`;
  - runtime-created agent progress events preserve the current run's original `created_at`.

# How to verify

- Run `cargo test -p cowboy app::events::tests`.
- Run `cargo test -p cowboy-workflow-engine events::tests`.
- Run `cargo test -p cowboy-workflow-engine runner::tests`.
- Run the focused `cowboy-workflow-engine` runtime test(s) added for `answer_run` and agent progress event baselines.
- Run `cargo test -p cowboy -p cowboy-workflow-engine` as the focused regression pass.
- Manual TUI smoke check with a workflow that emits multiple events over at least one second:
  - confirm transcript headers show elapsed values such as `00:00:00`, `00:00:01`, and later durations;
  - confirm no workflow transcript header shows the current UTC wall-clock time;
  - confirm prompt, response, thought, tool, step completion, and terminal event metadata/body layout remains unchanged.

# Verification evidence

- Focused/package tests passed: `cargo test -p cowboy -p cowboy-workflow-engine` reported 144 passed.
- Manual TUI smoke check completed with a real `target/debug/cowboy` TUI process in a pty, a temporary Lua workflow, and a fake ACP agent backend. Scenario: submit `/run smoke elapsed`; fake agent emits prompt/session, thought, tool call, waits 1.2 seconds, emits tool update, response, step completion, and run completion.
- Observed elapsed transcript headers: `00:00:00  Run started`, `00:00:00  Prompt sent to agent`, `00:00:00  Agent thinking`, `00:00:00  Agent tool call`, `00:00:01  Agent tool update`, `00:00:01  Agent response`, `00:00:01  Step completed`, and `00:00:01  Run completed`.
- Confirmed no workflow transcript header used UTC wall-clock time, and prompt, response, thought, tool, step completion, and terminal event metadata/body layout remained visible.

# TODO

- [x] Add optional `run_started_at` to `WorkflowEvent` with serde default compatibility.
- [x] Add run-aware `WorkflowEvent` constructor/helper APIs using `WorkflowRun::created_at`.
- [x] Update runner-created events to populate `run_started_at` from the active run.
- [x] Update `answer_run` resume/status events to populate `run_started_at` from the waiting run.
- [x] Update runtime-created agent progress events to populate `run_started_at` from the active run.
- [x] Replace UTC stamp formatting in `crates/tui/src/app/events.rs` with elapsed `HH:MM:SS` formatting.
- [x] Add TUI renderer tests for elapsed time, UTC omission, long durations, and missing-baseline fallback.
- [x] Add engine event serialization tests for new and legacy event JSON.
- [x] Add runner/runtime tests proving emitted events carry the original run creation timestamp.
- [x] Run focused `cowboy` and `cowboy-workflow-engine` test commands.
- [x] Manually smoke test a live TUI run and confirm transcript headers show elapsed run time.
