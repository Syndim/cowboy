## Plan

Base the fix on `docs/plans/runs_command_blocks_ui/rca.md` and the investigator-added regression test `crates/tui/app/src/app/commands.rs::app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive`. Keep that repro test as an input to the fix; do not rewrite or replace it.

Fix the `/runs` freeze at the TUI command boundary. Submitting `/runs` must return control to the event loop immediately, enqueue the expensive `WorkflowRuntime::list_runs()` work in a cancellable background task, and render the same run summary cards when that task completes. Do not change `WorkflowRuntime::list_runs()`, redb store behavior, CLI `cowboy runs` output, or run-summary formatting semantics for this bug fix.

The background task should be a non-workflow UI command task, not a workflow execution task. While `/runs` is loading, `workflow_execution_running()` must remain false unless an actual workflow task is also active, so composer gating, pending prompt behavior, workflow conflict checks, and header state do not regress.

## Changes

- In `crates/tui/app/src/app/state.rs`, generalize background task completion so `AppState` can drain both existing workflow `RunReport` tasks and non-workflow UI command tasks that produce transcript cards/status updates.
- Keep `BackgroundTaskKind::WorkflowExecution` reserved for actual workflow execution reports; add a separate kind or equivalent representation for `/runs` list work so `workflow_execution_running()` ignores run-list tasks.
- Add a runs-list completion payload that applies `Vec<RunSummaryLine>` by setting status to `<N> run(s)` and pushing the existing `Runs` empty-state card or one `Run` card per summary using `render_run_summary_lines`.
- In `crates/tui/app/src/app/commands.rs`, replace the inline `show_runs(state, runtime)?` dispatch for `SharedCommand::Runs` with a spawn helper that clones `WorkflowRuntime`, immediately records submission feedback, and runs `runtime.list_runs()` off the TUI event loop, preferably through `tokio::task::spawn_blocking` because the method performs synchronous store and event-log I/O.
- Preserve the current synchronous rendering helper as a pure apply/render helper if useful for tests, but ensure the slash-command path used by `submit_input` never calls `runtime.list_runs()` before returning.
- Leave `crates/tui/app/src/main.rs` unchanged for this bug unless implementation uncovers a compile-time signature change; the reported freeze is specific to the interactive TUI event loop, not the non-interactive CLI command.

## Tests to be added/updated

- Keep `app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive` unchanged and make it pass through product code changes.
- Update the existing `/runs` rendering tests so they exercise the background path by submitting `/runs`, yielding/draining background tasks, and asserting the same structured cards and empty-state output currently asserted after direct `show_runs` calls.
- Add or update a focused state/command test proving a pending `/runs` background task does not make `workflow_execution_running()` true and does not put the composer into `ExecutionBlocked` when no workflow execution is active.
- Keep existing run-summary unit tests in `crates/tui/app/src/run_summary.rs` unchanged unless the implementation moves code without changing output.

## How to verify

1. Confirm the existing repro test fails before the fix and passes after the fix:
   `cargo test -p cowboy app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive -- --exact`
2. Verify the background `/runs` task renders populated run summaries after completion:
   `cargo test -p cowboy app::commands::tests::runs_submission_eventually_renders_structured_runtime_summaries_after_background_drain -- --exact`
3. Verify the background `/runs` task renders the empty-state card after completion:
   `cargo test -p cowboy app::commands::tests::runs_submission_eventually_renders_empty_state_card_after_background_drain -- --exact`
4. Verify `/runs` background work does not masquerade as workflow execution:
   `cargo test -p cowboy app::commands::tests::runs_submission_does_not_mark_workflow_execution_running -- --exact`
5. Run the focused command module after updating tests:
   `cargo test -p cowboy app::commands::tests::runs_`
6. Run the TUI crate test suite required for this UI command-path change:
   `cargo test -p cowboy`
7. Run Clippy for the changed crate and fail on diagnostics:
   `cargo clippy -p cowboy --all-targets -- -D warnings`

## TODO

- [x] TODO-01: Add a non-workflow background task result path in `AppState`.
  - Procedure: implement the state changes in `crates/tui/app/src/app/state.rs`, then run `cargo test -p cowboy app::commands::tests::runs_submission_does_not_mark_workflow_execution_running -- --exact`.
  - Expected result: the test passes and shows a pending `/runs` task increases `background_task_count()` without making `workflow_execution_running()` true or blocking ordinary composer submission when no workflow execution is active.
  - Implementer observed result: added a `RunsList` background task kind/result path in `AppState`; `cargo test -p cowboy app::commands::tests::runs_submission_does_not_mark_workflow_execution_running -- --exact` exited 0 and verified `/runs` creates one non-workflow background task, leaves `workflow_execution_running()` false, keeps composer mode `Idle`, and allows an ordinary request to spawn a workflow task.

- [x] TODO-02: Dispatch `/runs` as background work from `submit_input`.
  - Procedure: replace the `SharedCommand::Runs` inline call in `crates/tui/app/src/app/commands.rs` with a spawn helper, then run `cargo test -p cowboy app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive -- --exact`.
  - Expected result: the repro test passes without changing the test body; immediately after submitting `/runs`, `background_task_count()` is `1` and the command has returned to the caller.
  - Implementer observed result: replaced inline `/runs` dispatch with `spawn_runs_list`, which offloads `WorkflowRuntime::list_runs()` through `tokio::task::spawn_blocking`; `cargo test -p cowboy app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive -- --exact` exited 0 with the investigator-added test body unchanged.

- [x] TODO-03: Preserve populated `/runs` summary rendering after background completion.
  - Procedure: update the populated `/runs` rendering test to submit `/runs`, yield/drain the background task, and run `cargo test -p cowboy app::commands::tests::runs_submission_eventually_renders_structured_runtime_summaries_after_background_drain -- --exact`.
  - Expected result: the test passes with three run cards containing the same run ids, topics, workflow names, current steps, heads, structured statuses, and no Rust debug fragments.
  - Implementer observed result: updated the populated rendering test to use `/runs` submission plus background drain; the first command run exited 101 because the test helper timed out before `spawn_blocking` completed, then the helper was made to wait deterministically and the same command exited 0 with three structured run cards and no Rust debug fragments.

- [x] TODO-04: Preserve empty `/runs` rendering after background completion.
  - Procedure: update the empty `/runs` rendering test to submit `/runs`, yield/drain the background task, and run `cargo test -p cowboy app::commands::tests::runs_submission_eventually_renders_empty_state_card_after_background_drain -- --exact`.
  - Expected result: the test passes with status `0 run(s)`, exactly one `Runs` card, `known runs: 0`, and no per-run card text.
  - Implementer observed result: updated the empty rendering test to use `/runs` submission plus background drain; `cargo test -p cowboy app::commands::tests::runs_submission_eventually_renders_empty_state_card_after_background_drain -- --exact` exited 0 with status `0 run(s)`, one `Runs` card, `known runs: 0`, and no per-run card text.

- [x] TODO-05: Run focused and crate-level verification for the TUI command change.
  - Procedure: run `cargo test -p cowboy app::commands::tests::runs_`, then `cargo test -p cowboy`, then `cargo clippy -p cowboy --all-targets -- -D warnings` from the repository root.
  - Expected result: the two test commands pass with no Rust compiler warnings, and the Clippy command passes with no Clippy diagnostics.
  - Implementer observed result: `cargo test -p cowboy app::commands::tests::runs_` exited 0; the first `cargo test -p cowboy` exposed test expectations that still assumed inline `/runs` rendering and resolve fixtures without declared `required_fields`, which were updated; `cargo clippy -p cowboy --all-targets -- -D warnings` first reported `large_enum_variant`, then `BackgroundTaskResult::WorkflowReport` was boxed and Clippy exited 0; after final formatting-only tidy edits, `cargo test -p cowboy` exited 0 with 282 passed and 2 ignored, and `cargo clippy -p cowboy --all-targets -- -D warnings` exited 0 with no diagnostics.
