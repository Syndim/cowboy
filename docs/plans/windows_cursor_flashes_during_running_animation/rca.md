## Bug behavior

During an active run, Cowboy advances the running status animation every scheduler tick. The TUI redraw changes the animated status cell while the text composer still owns a visible cursor. On Windows, the console cursor visibly jumps to the animated status cell while that changed cell is painted, then jumps back to the composer input cursor position. This presents as a flashing cursor on the running animation.

The bug is grounded by a focused test that renders a running frame, advances the status animation, renders the next frame, and records that one changed animation cell is painted while the cursor is visible.

## Root cause

`crates/tui/app/src/app.rs` redraws when `tick_status_animation` advances `AppState::status_animation` and then calls `Terminal::draw` through `draw_production_frame`. `crates/tui/app/src/app/controls/composer.rs` always calls `frame.set_cursor_position(...)` when the composer accepts edits, so each completed frame leaves Ratatui's terminal cursor visible at the input box.

On the next animation tick, Ratatui flushes changed cells before restoring the requested frame cursor position. Its crossterm backend paints diffs by moving the terminal cursor to each changed cell and printing the new symbol; only after diff painting does `Terminal::try_draw` call `show_cursor` and `set_cursor_position` for the frame cursor. Cowboy does not hide the cursor before these diff writes. Windows exposes that intermediate cursor movement during the animation redraw, so the cursor flashes at the animated status item before returning to the input box.

The failing regression test instruments this draw order with a test backend: after the first frame leaves the cursor visible, the second frame paints the status-animation diff while the cursor is still visible.

## Reproduction steps

1. Start a Cowboy TUI run on Windows so the run state is `running` and the status animation is active.
2. Keep focus in the composer input box.
3. Observe the animated running status item as it changes frame.
4. The visible cursor briefly moves from the composer input to the animated status item and then returns to the composer input.

Repository-grounded reproduction:

1. Render a running app frame with a composer cursor.
2. Advance `AppState::status_animation`.
3. Render the next frame with a backend that records whether changed cells are painted while the cursor is visible.
4. The backend records one visible-cursor changed-cell paint during the animation redraw.

## Regression test

- Test file path: `crates/tui/app/src/app/tests.rs`
- Test name: `status_animation_redraw_hides_cursor_before_painting_changed_cell`
- Command: `cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell`
- Expected failure before the fix: the assertion fails because the animation redraw paints a changed cell while the cursor is visible. The observed failing value is `visible_redraw_painted_cells == 1`, but the expected fixed behavior is `0`.

## Current failing result

Command run:

```text
cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell
```

Observed output:

```text
running 1 test
failures:

---- app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell stdout ----

thread 'app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell' (1335854) panicked at crates/tui/app/src/app/tests.rs:397:5:
assertion `left == right` failed: animation redraw painted 1 changed cells while the cursor was visible; redraws must hide the composer cursor before painting changed cells and show it again only after restoring the composer cursor position
  left: 1
 right: 0
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 309 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Do not stop or slow the running status animation just to avoid the symptom.
- Keep the composer cursor visible at the input position after each completed frame when the composer accepts edits.
- Hide the cursor before painting redraw diffs, then restore visibility and position after the frame is painted.
- Keep terminal lifecycle policy in the terminal seam where possible; avoid embedding Windows-specific terminal behavior in unrelated TUI widgets.
- Preserve existing cursor-position behavior covered by `draw_places_cursor_at_input_end`, `draw_places_cursor_in_active_run_draft_input`, and wrapped-input cursor tests.
- The fix must make `crates/tui/app/src/app/tests.rs::status_animation_redraw_hides_cursor_before_painting_changed_cell` pass without changing the test intent.
