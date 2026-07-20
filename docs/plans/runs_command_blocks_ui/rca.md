## Bug behavior

Submitting `/runs` from the TUI can take a long time before any output appears when the store has a large or slow persisted run history. During that interval the TUI does not process key events, so cancellation, typing, and navigation appear frozen until the command finishes.

## Root cause

The TUI event loop handles Enter by awaiting command submission inline before returning to event polling. The `/runs` slash command is dispatched to `show_runs`, and `show_runs` calls `WorkflowRuntime::list_runs()` synchronously on the TUI task instead of spawning background work.

`WorkflowRuntime::list_runs()` does more than read run heads: it opens the store, iterates every run head, loads each full `WorkflowRun`, and, when `request_topic` is absent, reads and parses that run's persisted event JSON to backfill the topic. With many runs or slow storage, this blocks the same loop that must draw the UI and process keys.

The existing long-running workflow commands (`/run`, `/step`, `/resume`, `/answer`, and resolving with a status) enqueue background tasks. `/runs` is the outlier: the regression test observes that submitting `/runs` leaves `background_task_count()` at `0`, proving it ran inline on the UI path.

## Reproduction steps

1. From the repository root, run the focused regression test:

   ```bash
   cargo test -p cowboy app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive -- --exact
   ```

2. The test submits `/runs` through the same `submit_input` path used by the TUI Enter key.
3. It asserts that `/runs` should enqueue background work so the event loop can continue polling and handling keys while run summaries are loaded.

## Regression test

- Test file path: `crates/tui/app/src/app/commands.rs`
- Test name: `app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive`
- Command: `cargo test -p cowboy app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive -- --exact`
- Expected failure before the fix: the assertion fails because `/runs` creates zero background tasks instead of one, showing it still runs inline on the TUI command path.

## Current failing result

```text
running 1 test
failures:

---- app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive stdout ----

thread 'app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive' panicked at crates/tui/app/src/app/commands.rs:736:9:
assertion `left == right` failed: /runs must run in a background task so the TUI event loop can keep processing keys
  left: 0
 right: 1

failures:
    app::commands::tests::runs_submission_is_dispatched_as_background_task_to_keep_ui_responsive

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 281 filtered out; finished in 0.01s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Keep this investigation limited to tests and documentation until a fix plan is approved.
- Preserve the existing `/runs` rendered summary content and structured status fields.
- Move the expensive run-listing work off the TUI event loop; do not mask the symptom with timing thresholds or by dropping run details.
- Keep workflow runtime and store semantics in the workflow crates; the TUI should only dispatch and render results.
- Preserve `user_feedback` output fields exactly when present; do not append agent or reviewer comments to that field.
