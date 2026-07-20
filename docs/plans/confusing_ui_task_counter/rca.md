# Bug behavior

During an active TUI workflow run, the status strip renders metadata like:

```text
● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature · ◷ 1
```

The `◷ 1` suffix is hard to understand because it is unlabeled and resembles elapsed/progress/status metadata rather than an internal task count. In the normal run path it also usually stays at `1`, so it does not behave like visible workflow progress, step count, retry count, elapsed time, or queued work.

## Root cause

The confusing suffix is emitted by `status_metadata_text` in `crates/tui/app/src/app/controls/chrome.rs`. When `AppState::background_task_count() > 0`, it appends `format!("◷ {}", state.background_task_count())` to the user-facing status metadata.

`background_task_count()` in `crates/tui/app/src/app/state.rs` returns `self.background.len()`. That vector stores internal Tokio background tasks spawned by `spawn_report_task_with_entry`, not workflow-domain progress.

The main run/step/resume command paths in `crates/tui/app/src/app/commands.rs` call `spawn_card_report_task`, which pushes one `BackgroundTaskKind::WorkflowExecution` handle for the foreground workflow operation. Normal TUI command conflict handling prevents starting another conflicting workflow command while one is active. Therefore a normal active workflow shows `◷ 1` for the single wrapper task and rarely, if ever, increments above `1` in the scenario the user is watching.

The UI leaks an implementation detail: count of active async wrapper tasks. The glyph has no label and the count is not the user-visible workflow metric implied by its placement beside step/run/workflow metadata.

## Reproduction steps

1. Construct an `AppState` with a started run event for workflow `agent/00-feature`, step `implement`, and run id `run-170dc431-7a35-49a5-b4db-9f1219431a1d`.
2. Spawn one pending workflow report task with `spawn_test_card_report_task`, matching the normal active-run state where the foreground workflow execution is held in `AppState.background`.
3. Render the status strip line at width `160` via `crates/tui/app/src/app/controls/status.rs::line`.
4. Observe that the rendered line includes the ambiguous internal task counter suffix `◷ 1`.

## Regression test

- Test file path: `crates/tui/app/src/app/controls/status.rs`
- Test name: `app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count`
- Command: `cargo test -p cowboy app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count -- --exact`
- Expected failure before the fix: the test expects an active workflow status line without the ambiguous `◷` suffix, but current code renders `● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature · ◷ 1`.

## Current failing result

```text
running 1 test
failures:

---- app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count stdout ----

thread 'app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count' (1001456) panicked at crates/tui/app/src/app/controls/status.rs:131:9:
assertion `left == right` failed
  left: "● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature · ◷ 1"
 right: "● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature"
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 281 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Preserve the useful status metadata: run state icon, current step, short run id, and workflow name.
- Do not present `AppState::background_task_count()` as a workflow progress indicator; it is an internal async task count.
- If a replacement indicator is added, base it on a user-facing workflow concept and label or icon it clearly enough that the meaning is discoverable.
- Update existing status/card metadata tests that currently assert `◷ 1` once product behavior changes.
- Keep runtime orchestration out of the TUI rendering layer; UI should consume existing workflow/event state rather than duplicating workflow runtime logic.
