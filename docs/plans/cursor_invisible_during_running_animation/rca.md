## Bug behavior

When a workflow is running and the status animation is active, the composer remains editable but its cursor is not visible. The cursor returns when the run leaves the animated `running` state. The requested behavior is a cursor that remains visible at the composer input position during the animation; it may be steady rather than blinking.

## Root cause

The cursor visibility policy explicitly disables the composer cursor whenever the running status animation is active. `AppState::composer_shows_cursor` returns false for every animated running frame, and `composer::render` therefore skips Ratatui's `Frame::set_cursor_position` call. Production drawing also hides the terminal cursor before every frame, so no later frame cursor request restores it while the animation remains active.

This policy was introduced to prevent rapid blinking during animation redraws, but it suppresses visibility entirely instead of preserving a visible, non-blinking cursor.

## Root cause evidence

No runtime log is needed for this rendering defect because the trigger-to-failure flow is directly observable in the state and render path:

1. A workflow start reaches `AppState::apply_workflow_event_metadata` in `crates/tui/app/src/app/state.rs:1257-1281`. The `WorkflowEventKind::RunStarted` branch assigns `self.run_state = "running"` at line 1279.
2. `AppState::status_animation_active` in `crates/tui/app/src/app/state.rs:593-595` returns true when there is no pending prompt and `run_state == "running"`. The regression test confirms this precondition after applying `RunStarted`.
3. `AppState::composer_accepts_edits` in `crates/tui/app/src/app/state.rs:572-574` still returns true, so the running composer remains an editable input surface.
4. `AppState::composer_shows_cursor` in `crates/tui/app/src/app/state.rs:581-583` combines editability with `!self.status_animation_active()`. Because the animation is active, this expression returns false.
5. `composer::render` in `crates/tui/app/src/app/controls/composer.rs:79-93` renders the input text, but only calls `set_input_cursor` when `composer_shows_cursor()` is true. The false result skips line 92, so the frame contains no cursor position request.
6. The production draw seam `draw_cursor_safe_production_frame` in `crates/tui/app/src/app.rs:155-170` calls `terminal.hide_cursor()` before drawing. Since the composer does not request a cursor in step 5, Ratatui has no reason to show it after the frame.
7. Each animation tick advances the frame and marks another redraw through `tick_status_animation` in `crates/tui/app/src/app.rs:151-153`, repeating the same hide-without-restore sequence for the duration of the running animation.

The focused regression test follows steps 1-5 with Ratatui's test backend and observes `cursor_visible() == false`, proving that the explicit animation-dependent cursor policy causes the reported invisible cursor.

## Reproduction steps

1. Launch the Cowboy TUI.
2. Enter text in the composer so the cursor position is easy to see.
3. Start a workflow and wait until the running status animation is visible.
4. Observe that the composer text remains present and editable, but the cursor disappears.
5. Run the focused repository test below to reproduce the same state/render path without a real terminal.

## Regression test

- Test file path: `crates/tui/app/src/app/tests.rs`
- Test name: `app::tests::running_animation_keeps_composer_cursor_visible`
- Command: `cargo test -p cowboy --lib app::tests::running_animation_keeps_composer_cursor_visible -- --exact --nocapture`
- Expected failure before the fix: the running-animation precondition passes, but the final assertion fails because the rendered backend cursor is hidden rather than visible.

## Current failing result

The in-progress cursor fix was removed to restore the pre-fix product baseline. Running the exact regression selector on that current baseline produced:

```text
running 1 test

thread 'app::tests::running_animation_keeps_composer_cursor_visible' (...) panicked at crates/tui/app/src/app/tests.rs:415:5:
the composer cursor must remain visible while the running status animation is active
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test app::tests::running_animation_keeps_composer_cursor_visible ... FAILED

failures:
    app::tests::running_animation_keeps_composer_cursor_visible

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 338 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Keep the composer cursor visible at the input position while the running status animation is active.
- A steady, non-blinking cursor during animation is acceptable.
- Do not reintroduce the previous rapid blink or visible cursor movement over changed animation cells.
- Preserve editable draft input during active workflow execution.
- Preserve the running animation cadence and status updates.
- Keep the pre-paint cursor safety behavior needed to prevent the cursor flashing at changed cells, or replace it only with behavior that provides the same protection.
- Reconcile the existing tests that intentionally require a hidden running cursor (`running_state_does_not_toggle_cursor_visibility_across_animation_frames`, `draw_hides_composer_cursor_during_active_run_animation`, and `composer_hides_cursor_only_during_running_animation`) because they encode the superseded behavior.
- Make `crates/tui/app/src/app/tests.rs::running_animation_keeps_composer_cursor_visible` pass without weakening its running-state precondition or visible-cursor assertion.
