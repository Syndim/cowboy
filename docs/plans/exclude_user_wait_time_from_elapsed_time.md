# Plan

Replace the transcript elapsed-time baseline with a persisted active-run clock. The current renderer computes each header stamp as `event.timestamp - event.run_started_at`, so it counts every wall-clock gap after the initial request. The desired value is time Cowboy was actively driving the run: starting the workflow, executing steps, handling accepted answers/resolutions, retrying, and collecting agent progress. It must exclude inactive gaps: waiting for user input, time between `run --step` / `step` invocations while the run is merely persisted as `Running`, and any other period where no runtime operation is actively executing that run.

Model this as accumulated active execution duration on `WorkflowRun`, not as a special-case subtraction for `WaitingForInput`. Each runtime operation that actually drives a run opens an active window, emits events with `previous_active_duration + current_window_elapsed`, then closes the window and persists the accumulated active duration when the operation stops because the run completed, failed, blocked for input, was cancelled, or returned from single-step mode while still `Running`.

Keep the scope to workflow transcript elapsed stamps. Do not change diagnostic log timestamps, `created_at` / `updated_at` semantics, run-list summaries, step/turn timestamps, or agent-reported step durations unless a focused test proves one of those values feeds the transcript header.

# Changes

- Update `crates/workflow/core/src/state.rs` `WorkflowRun`:
  - add a serde-defaulted cumulative field such as `active_duration_ms: u64`;
  - document it as persisted milliseconds spent actively executing Cowboy runtime work for the run;
  - preserve compatibility with existing persisted runs by defaulting missing values to `0`.
- Update all `WorkflowRun` test fixtures and helper constructors across workflow/core, workflow/engine, workflow/store, workflow/agent as needed, and TUI tests to set the new field or use serde defaults where a JSON fixture is intentionally legacy.
- Add a small active-clock helper in `cowboy-workflow-engine` rather than duplicating timestamp arithmetic:
  - capture `base_active_duration_ms` from the loaded run;
  - capture an `active_window_started_at: DateTime<Utc>` when a runtime operation begins actively driving the run;
  - compute event elapsed with saturating arithmetic as `base_active_duration_ms + max(event_timestamp - active_window_started_at, 0)`;
  - close the window by adding `max(end_timestamp - active_window_started_at, 0)` to `run.active_duration_ms` and saving the run/head before returning.
- Update `crates/workflow/engine/src/events.rs` `WorkflowEvent`:
  - add a serde-defaulted optional field such as `run_active_duration_ms: Option<u64>`;
  - keep `timestamp` and `run_started_at` for compatibility and fallback rendering;
  - keep `WorkflowEvent::new` for generic callers/tests with `run_active_duration_ms: None`;
  - add constructors or builder methods that set both `timestamp` and `run_active_duration_ms` from the active-clock helper without calling `Utc::now()` more than once per event.
- Update active runtime entry points in `crates/workflow/engine/src/runtime.rs`:
  - `start_catalog_workflow` opens the first active window after the run is created and before topic generation / runner execution;
  - `resume_with` opens an active window only when the loaded run is `RunStatus::Running`; returning a non-running run with no events must not add active time;
  - `answer_run` opens an active window only after `ResumeRouter::answer` validates the prompt and answer, so prior waiting time remains inactive while applying the accepted answer and any continuation counts as active;
  - `resolve_run` opens an active window only after resolution inputs validate and before synthesizing/emitting the manual-resolution step;
  - invalid prompt ids, invalid choices, invalid resolution fields, and no-op commands do not mutate `active_duration_ms`.
- Update `crates/workflow/engine/src/runner.rs` event emission to carry the active clock through runner-created events:
  - `RunStarted`, `StepStarted`, `StepCompleted`, retry events, failure give-up events, and status events all use active elapsed from the current active window;
  - when `step_once` returns while the run is still `Running`, close and persist the active window so idle time until the next `/step` or `resume` command is excluded;
  - when the runner stops at `WaitingForInput`, close and persist active time at the block point so time spent reading/answering the prompt is excluded.
- Update `crates/workflow/engine/src/runtime.rs` agent progress conversion:
  - capture the same active-clock base/window in the progress callback used by `run_existing_with_events`;
  - emit prompt, thought, response, tool, and plan progress events with `run_active_duration_ms` populated from the current active window.
- Update `crates/tui/app/src/app/events.rs`:
  - change `elapsed_stamp` to prefer `event.run_active_duration_ms` when present;
  - keep the existing fallback `event.timestamp - event.run_started_at.unwrap_or(event.timestamp)` for legacy persisted event logs and bare tests;
  - format active elapsed as `HH:MM:SS`, clamp fallback negatives to zero, and allow hours above `23`.

# Tests to be added/updated

- Add or update `crates/workflow/engine/src/events.rs` tests:
  - new events round-trip with `run_active_duration_ms` present;
  - legacy JSON missing `run_active_duration_ms` and/or `run_started_at` still deserializes safely;
  - active-clock event construction uses one timestamp and computes cumulative active duration with saturating arithmetic.
- Add `crates/workflow/engine/src/runtime.rs` tests for active window accounting:
  - starting a run emits events whose active duration advances while Cowboy is executing;
  - answering a waiting run does not include the time between the persisted `WaitingForInput` state and the accepted answer, but does include applying the answer and subsequent execution;
  - `step_run` / single-step execution closes the active window even when the persisted run remains `Running`, and a later `step_run` resumes from the previous active total without counting idle wall time between commands;
  - invalid prompt id, invalid choice, and invalid manual resolution do not increment `active_duration_ms`.
- Update `crates/workflow/engine/src/runner.rs` tests so runner-emitted events still carry the run start baseline and now also carry active elapsed duration from the injected active clock.
- Add deterministic `crates/tui/app/src/app/events.rs` renderer tests:
  - an event with `run_active_duration_ms = 296000` renders `00:04:56` even when wall-clock elapsed from `run_started_at` is larger;
  - an event with active duration over twenty-four hours renders hours above `23`;
  - an event missing `run_active_duration_ms` keeps the legacy fallback behavior;
  - a sequence fixture proves post-answer and second-step transcript stamps exclude inactive gaps while preserving active increments.
- Update existing tests that construct `WorkflowEvent` or `WorkflowRun` literals for the new fields without weakening assertions about event titles, metadata, body rendering, styles, coalescing, ordering, run status summaries, or prompt handling.

# How to verify

- Run `cargo test -p cowboy-workflow-engine events::tests`.
- Run the focused `cowboy-workflow-engine` runtime tests added for active-window accounting.
- Run `cargo test -p cowboy-workflow-engine runner::tests`.
- Run `cargo test -p cowboy app::events::tests`.
- Run `cargo test -p cowboy -p cowboy-workflow-engine -p cowboy-workflow-core`.
- Run `cargo clippy -p cowboy -p cowboy-workflow-engine -p cowboy-workflow-core --all-targets -- -D warnings`.
- Manual TUI smoke check with a workflow that asks for confirmation: start the run, wait several seconds before answering, then confirm post-answer transcript headers do not jump by the time spent waiting for the answer.
- Manual CLI/TUI smoke check with `--step` or `/step`: execute one step, wait several seconds before the next step, then confirm the next transcript stamp resumes from the previous active total instead of including idle time.

# TODO

- [x] Add persisted cumulative active execution duration to `WorkflowRun` with serde compatibility.
- [x] Update run fixtures and constructors for the new `WorkflowRun` field.
- [x] Add an engine active-clock helper for opening, computing, and closing active execution windows.
- [x] Add active elapsed duration support to `WorkflowEvent` and active-clock-aware constructors.
- [x] Update workflow start, resume, answer, and manual-resolution paths to open active windows only for accepted run-driving operations.
- [x] Update runner-created workflow events to populate active elapsed duration from the current active window.
- [x] Close and persist active windows when runs block, complete, fail, cancel, or return from single-step mode still `Running`.
- [x] Update runtime-created agent-progress events to preserve active elapsed duration.
- [x] Update the TUI transcript elapsed renderer to prefer active elapsed duration with legacy fallback.
- [x] Add engine event serialization and active-clock unit tests.
- [x] Add runtime tests for prompt waiting, single-step idle gaps, and invalid command no-op accounting.
- [x] Add TUI renderer tests for active elapsed stamps and legacy fallback.
- [x] Run focused cargo tests and clippy verification commands.
- [x] Smoke test confirmation and single-step workflows in the TUI/CLI and verify inactive gaps are excluded.
