## Plan

Improve the `/runs` and `cowboy runs` output by making run summaries user-facing instead of debug-shaped. Each listed run should show the run topic/title and a structured status block derived from `RunStatus`, not Rust `Debug` output.

Repository facts grounding the change:

- `WorkflowRuntime::list_runs` currently returns `RunSummaryLine` from `crates/workflow/engine/src/runtime.rs` with `run_id`, `workflow_name`, `status`, `current_step`, and `head_step` only.
- CLI `cowboy runs` currently formats status with `{:?}` in `crates/tui/app/src/main.rs`.
- TUI `/runs` currently formats status with `{:?}` in `crates/tui/app/src/app/commands.rs`.
- `WorkflowRun` already stores the full initial request as `original_request`; generated short topics currently exist only on `RunStarted.request_topic` events and TUI active-run state.

Use a clean data boundary: the workflow engine should expose a complete run summary, including topic metadata and structured status metadata. The CLI and TUI should only render that summary; they should not inspect private storage details or re-derive topics locally.

## Changes

- `crates/workflow/core/src/state.rs`
  - Add a backward-compatible optional run-topic field to `WorkflowRun`, using the existing `request_topic` naming to match `WorkflowEventKind::RunStarted`.
  - Use `#[serde(default, skip_serializing_if = "Option::is_none")]` so existing stored runs still deserialize.
  - Keep `original_request` unchanged; it remains the durable full request, not the display topic.

- `crates/workflow/engine/src/runtime.rs`
  - Extend `RunSummaryLine` with:
    - `topic: Option<String>` for the short run topic shown by `/runs`.
    - `status_detail: RunStatusDetail` or an equivalent serializable summary type that flattens `RunStatus` into explicit fields.
  - Add a small status projection type near `RunSummaryLine`, for example:
    - `state: "running" | "waiting_for_input" | "completed" | "failed" | "cancelled"`.
    - `reason: Option<String>` for failed runs.
    - `waiting_step`, `prompt_id`, `message`, and `choices` for waiting runs.
  - Populate `WorkflowRun.request_topic` when `generate_request_topic` succeeds for new-run entry points, persist the updated run before workflow execution continues, and continue to pass the same topic into `WorkflowRunner` for the first `RunStarted` event.
  - Preserve best-effort topic semantics: topic generation failure should still log and leave `topic` absent, not fail the run.
  - In `list_runs`, load each run as it already does, return `topic` from `run.request_topic`, and optionally fall back to the first persisted `RunStarted.request_topic` event for existing runs created after topic events existed but before the new run field existed.
  - Do not fall back from topic to `original_request` unless the product decision is to show a request when no short topic exists; if used, label it distinctly as `request` so users are not misled.

- `crates/tui/app/src/main.rs`
  - Replace the one-line `cowboy runs` debug output with user-facing output built from `RunSummaryLine`.
  - Include run id, topic when present, workflow, current step, head, and structured status fields.
  - Remove all `{:?}` status formatting from the `runs` path.

- `crates/tui/app/src/app/commands.rs`
  - Replace the `/runs` card details with the same user-facing fields as the CLI path.
  - Show `topic: <topic>` when present.
  - Render status as structured lines, such as `status: waiting_for_input`, `status.prompt_id: approval`, and `status.choices: yes, no`, instead of `status: WaitingForInput { ... }`.
  - Keep the status-bar summary (`N run(s)`) behavior unchanged unless the new summary count wording needs to match CLI output.

- `crates/workflow/engine/src/bin/engine-cli.rs`
  - Update the diagnostic `engine-cli runs` output to use the same status projection, or deliberately keep it diagnostic but remove any duplicated status-formatting logic that would drift from product behavior.

- Shared rendering helpers
  - Prefer a small pure helper for run-summary rendering in the `cowboy` crate if both CLI and TUI need identical text lines.
  - Keep engine types presentation-neutral; they should expose structured summary data, not preformatted terminal strings.

## Tests to be added/updated

- Add workflow-engine runtime tests proving:
  - a new run with a generated request topic persists that topic on `WorkflowRun`;
  - `WorkflowRuntime::list_runs` includes the persisted topic;
  - `RunSummaryLine.status_detail` for `Running`, `Completed`, `Failed`, `Cancelled`, and `WaitingForInput` contains explicit fields and does not depend on `Debug` formatting;
  - legacy runs without the new field still deserialize and list successfully;
  - existing persisted `RunStarted.request_topic` events can backfill summary topic when the run field is absent, if that fallback is implemented.

- Add or update `crates/tui/app/src/app/commands.rs` tests for `/runs` card rendering:
  - completed run with topic shows `topic:` and structured `status: completed`;
  - waiting run shows prompt id, message, choices, and waiting step as separate lines;
  - failed run shows `status: failed` and a separate reason line;
  - rendered output does not contain Rust debug fragments such as `WaitingForInput {`, `Failed {`, or `resume_callback:`.

- Add or update CLI-facing tests or pure rendering-helper tests for `cowboy runs` output:
  - output includes topic, workflow, current step, head, and structured status fields;
  - output never formats `RunStatus` with `{:?}`-style enum payloads.

- Update `engine-cli runs` tests if that binary has direct unit coverage after extracting shared status projection helpers.

## How to verify

Run the narrow checks that cover the changed crates and paths:

```bash
cargo test -p cowboy-workflow-engine runtime::
cargo test -p cowboy app::commands
cargo test -p cowboy --bin cowboy
```

Manual smoke check after implementation:

```bash
cargo run -- runs
cargo run
```

In the TUI, run `/runs` after creating at least one completed run and one waiting or failed run. Verify:

- each run shows a topic when one was generated;
- status output is structured and readable;
- waiting status shows prompt metadata without exposing resume callback internals;
- failed status shows the failure reason as data, not enum debug text;
- no `/runs` or `cowboy runs` output contains Rust debug fragments such as `{`, `resume_callback`, or enum variant payload syntax for normal status display.

## TODO

- [x] Add backward-compatible optional `request_topic` storage to `WorkflowRun`.
- [x] Persist generated request topics on new workflow runs without changing best-effort topic failure semantics.
- [x] Add structured run-status summary types near `RunSummaryLine` in the workflow engine.
- [x] Extend `RunSummaryLine` with topic and structured status data.
- [x] Update `WorkflowRuntime::list_runs` to populate topic and structured status details.
- [x] Add legacy event-log topic fallback for runs missing the persisted topic field, if implemented.
- [x] Replace `cowboy runs` debug status formatting with user-facing summary rendering.
- [x] Replace TUI `/runs` debug status formatting with user-facing summary card rendering.
- [x] Align or consciously isolate `engine-cli runs` status rendering to avoid duplicated stale formatting.
- [x] Add runtime tests for topic persistence, summary projection, and legacy compatibility.
- [x] Add TUI `/runs` rendering tests for completed, waiting, and failed statuses.
- [x] Add CLI or pure-rendering tests that reject Rust debug-shaped status output.
- [x] Run the targeted verification commands and record the results.
