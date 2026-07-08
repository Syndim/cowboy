## Plan

Update the TUI chrome so the top row becomes an agent-generated run-topic title and the bottom status row becomes the compact runtime metadata row. Keep UI rendering in the TUI app crate, and put agent-backed topic generation in the workflow engine where existing agent selector/summarizer adapters already live.

The title bar should render `Cowboy - <topic>` once an agent-generated topic is available for the initial user request submitted through plain input, `/run`, `/run-step`, or `/run-workflow`. The topic must be produced by the configured agent from the initial user input, not by local keyword truncation or deterministic summarization. Local code may only normalize and validate the agent response: trim whitespace, reject empty/multiline results, cap display width for rendering, and keep `Cowboy` when no valid agent topic is available for the active run.

Move the current header metadata items except the Cowboy name into the status bar: run-state icon, current step, short run id, workflow name, and background task count. The status bar should replace its current help/status-copy strings with only this metadata row, using the existing compact symbols and width-aware dropping/truncation behavior.

Topic lifecycle contract:

- A topic belongs to one run id.
- A `RunStarted` event with `request_topic: Some(topic)` sets the active topic for that event's run id.
- A `RunStarted` event with `request_topic: None` preserves the existing topic only when the event run id matches the currently tracked topic run id; this covers later `/step`, `/resume`, answer, and resolve paths that re-enter the same run without regenerating the title topic.
- A `RunStarted` event with `request_topic: None` for a different run id clears the title topic and renders `Cowboy` until that run supplies a topic.
- Step/progress/waiting/completed/failed events never clear the topic by themselves; the topic is cleared only by a different run without a topic, explicit topic replacement, or full app state reset.

## Changes

- `crates/workflow/engine/src/workflow.rs`
  - Add an agent-backed request-topic generator next to `AgentWorkflowSelector` and `AgentWorkflowSummarizer`.
  - Prompt the configured agent with the initial user request as a one-shot summarization task: do not run tools, do not ask questions, return only JSON such as `{ "topic": "short topic" }`.
  - Validate the parsed topic as non-empty, single-line, and short enough for UI chrome before returning it.
  - Add fake-client unit coverage for valid JSON, surrounding prose with JSON extraction if supported by the shared parser, invalid JSON, empty topics, and multiline topics.

- `crates/workflow/engine/src/runtime.rs`
  - Generate the request topic from the default configured agent for new-run entry points before handing the run to the runner: `start_run`, `start_run_stepwise`, `start_run_with_workflow`, and `start_run_with_workflow_stepwise`.
  - Use best-effort semantics for UI chrome: a topic-generation failure should be logged and should leave the topic absent, but it should not fail the workflow run.
  - Thread `Option<String>` topic data into `run_existing` / `run_existing_with_events` only for new-run calls. Existing-run paths (`step_run`, `resume_run`, `answer_run`, `resolve_run`) should pass `None` and rely on the TUI same-run preservation contract.
  - Do not emit a second synthetic `RunStarted` event from runtime. The runner remains the single source of `RunStarted` events.

- `crates/workflow/engine/src/runner.rs`
  - Add runner plumbing for the optional request topic, for example a `with_request_topic(Option<String>)` builder or equivalent field on `WorkflowRunner`.
  - Emit the existing first `RunStarted` event in both `run_until_blocked` and `step_once` with the optional topic attached.
  - Preserve current event ordering: `RunStarted` remains the first runner-emitted event, followed by step lifecycle events.

- `crates/workflow/engine/src/events.rs`
  - Extend `WorkflowEventKind::RunStarted` with `request_topic: Option<String>`, using serde defaults and skip-serializing when absent so older persisted event logs still deserialize.
  - Update `WorkflowEvent::run_started` or add `WorkflowEvent::run_started_with_topic` so runner code can attach the topic to the actual `RunStarted` event.
  - Keep `run_started_at` behavior unchanged.

- `crates/tui/app/src/app/state.rs`
  - Add app-state storage for both the current topic run id and current run topic received from runtime events.
  - Apply the topic lifecycle contract exactly when handling `WorkflowEventKind::RunStarted`.
  - Do not derive a semantic topic locally from slash-command text; only store the runtime-provided agent topic.
  - Preserve the topic across same-run step/progress/waiting/completed/failed events.

- `crates/tui/app/src/app/controls/header.rs`
  - Change `text`/`line` rendering to produce `Cowboy - <topic>` when `AppState` has an agent topic, otherwise `Cowboy`.
  - Reuse Unicode display-width truncation so long and wide-character topics stay within the header width.
  - Remove step/run/workflow/tasks metadata from the title bar.

- `crates/tui/app/src/app/controls/status.rs`
  - Replace the current status/help text branches with the compact metadata currently owned by the header: icon, step, run id, workflow, and task count.
  - Keep the existing priority-drop behavior from the header so narrow terminals preserve the highest-value metadata.
  - Apply the same run-state styling already used by the current status bar.

- `crates/tui/app/src/app/controls/header.rs` and `crates/tui/app/src/app/controls/status.rs`
  - Share or duplicate only minimal private helpers as needed; prefer a small local helper module if moving `HeaderPart`, `status_icon`, short run id, and display-width truncation avoids two competing implementations.

## Tests to be added/updated

- Add workflow-engine unit tests for the new agent-backed topic generator, including prompt shape, JSON parsing, validation failures, and no-tool/no-question instruction text.
- Add runtime tests proving new-run paths pass a generated topic into the runner while existing-run paths do not regenerate or replace the topic.
- Add runner tests proving `run_until_blocked` and `step_once` emit a single first `RunStarted` event with the supplied topic and do not require runtime to emit a duplicate event.
- Update event tests so `RunStarted` can carry an optional agent topic and older `RunStarted` JSON without the topic still deserializes.
- Update TUI state tests for exact topic lifecycle semantics: set on same event with topic, preserve on same-run `RunStarted` without topic, clear on different-run `RunStarted` without topic, and ignore later step/progress/completion events for clearing.
- Update header-control tests so an active run renders `Cowboy - <agent topic>` and no longer renders the state icon, step, run id, workflow name, or task count in the header.
- Add or update header tests for idle state (`Cowboy`), long topic truncation, and Unicode-width truncation.
- Update status-control tests so active, waiting, idle, and background-task states render the compact metadata row instead of the old help/status copy.
- Add a status-control narrow-width test proving lower-priority metadata drops before high-priority metadata.
- Update full TUI draw tests that currently search for old title/status strings such as `draft allowed`, `Enter waits for active run`, or compact header metadata.
- Add command/runtime-facing tests showing plain input, `/run`, `/run-step`, and `/run-workflow` request paths can receive and display the agent-generated topic without local topic derivation.

## How to verify

Run the narrow engine and TUI test suite after implementation:

```bash
cargo test -p cowboy-workflow-engine workflow::
cargo test -p cowboy-workflow-engine runner::
cargo test -p cowboy-workflow-engine events::
cargo test -p cowboy-workflow-engine runtime::
cargo test -p cowboy app::controls::header
cargo test -p cowboy app::controls::status
cargo test -p cowboy app::tests
cargo test -p cowboy app::commands
```

Manual smoke check with a configured agent backend:

```bash
cargo run
```

Then submit a request such as `add health check route` and verify:

- The top row initially remains `Cowboy` or a loading-safe equivalent until the agent topic is available.
- After the topic event/report arrives, the top row shows `Cowboy - <agent-generated topic>` or a width-truncated equivalent.
- The top row does not show the status icon, step id, run id, workflow name, or background task count.
- The bottom status row shows those compact metadata items and no longer shows the previous help/status shortcut copy.
- `/run-workflow <workflow-id> <request>` uses the agent-generated topic from `<request>`, not the workflow id.
- Running `/step`, `/resume`, answering a prompt, or resolving the same run preserves the existing title topic when subsequent `RunStarted` events do not carry topic data.
- Starting or loading a different run without topic data clears the title topic back to `Cowboy`.
- A narrow terminal truncates or drops metadata without corrupting borders or composer layout.

## TODO

- [x] Add an agent-backed request-topic generator in the workflow engine using the configured agent client.
- [x] Define and validate the topic JSON response contract as a non-empty single-line topic.
- [x] Wire best-effort topic generation into all new-run runtime paths without changing workflow execution semantics.
- [x] Thread optional topic data from runtime into `WorkflowRunner` for new runs only.
- [x] Update runner `RunStarted` emission so the actual first runner event carries the optional topic without duplicate runtime events.
- [x] Extend `WorkflowEventKind::RunStarted` with a backward-compatible optional topic field.
- [x] Add current-topic run id storage and exact preserve/clear semantics in TUI state.
- [x] Simplify header rendering to `Cowboy` or `Cowboy - <agent topic>` only.
- [x] Move compact run metadata rendering from the header to the status bar and remove old status/help strings.
- [x] Share width/drop helpers between header and status rendering, or keep a single implementation to avoid drift.
- [x] Update and add engine, runner, event, header, status, command/state, and full draw tests.
- [x] Run the targeted engine and TUI tests and record the exact verification commands and results.
