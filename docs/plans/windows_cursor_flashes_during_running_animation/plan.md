# Plan

Use the RCA at `docs/plans/windows_cursor_flashes_during_running_animation/rca.md` as the source of truth. The investigator-added repro test is `crates/tui/app/src/app/tests.rs::status_animation_redraw_hides_cursor_before_painting_changed_cell`; keep that test as the fix contract and do not rewrite or replace it.

Fix the bug at the terminal redraw seam. `crates/tui/app/src/app.rs::run_loop` currently calls `Terminal::draw` directly after `tick_status_animation` marks the frame dirty. Ratatui then paints changed cells while the cursor remains visible from the previous frame, and only restores the composer cursor after diff painting. Add a small app-level draw helper that hides the terminal cursor immediately before each `Terminal::draw` call, then lets Ratatui restore the requested cursor position and visibility after the frame is painted.

This should not affect the blinking cursor style in the input box. Cowboy sets the style once during terminal entry through `cowboy_tui_terminal::tui_input_cursor_style()` as `crossterm::cursor::SetCursorStyle::BlinkingBlock`; `Terminal::hide_cursor()` and Ratatui's post-frame `show_cursor`/`set_cursor_position` path toggle visibility and position only. The draw helper must not issue `SetCursorStyle` or reset the cursor shape.

Do not slow, stop, or special-case the running status animation. Do not move cursor policy into the status animation or composer widgets. The composer must still request the input cursor through `crates/tui/app/src/app/controls/composer.rs::render`, and completed frames must still leave the cursor visible at the input position with the configured blinking block style when the composer accepts edits.

# Changes

- Update `crates/tui/app/src/app.rs` to add a cursor-safe production draw helper around `Terminal::draw`.
- The helper should call `terminal.hide_cursor()` before the draw closure runs, then call the existing `draw_production_frame` path so layout reconciliation and rendering order stay unchanged.
- The helper should not import, call, or emit `SetCursorStyle`; hiding the cursor is a visibility operation, not a style reset.
- Replace the direct `terminal.draw(|frame| { current_layout = draw_production_frame(...) })` call in `run_loop` with the new helper and keep existing draw-error logging behavior.
- Update only the draw invocation inside `status_animation_redraw_hides_cursor_before_painting_changed_cell` as needed so the repro exercises the same cursor-safe draw seam used by production. Preserve the test setup, animation advance, assertion, test name, and observable intent.
- Leave `crates/tui/app/src/app/controls/composer.rs` cursor placement behavior unchanged unless a narrow compiler issue requires an import-only adjustment.
- Leave `cowboy_tui_animation` status frames unchanged; the animation should continue advancing normally.
- Leave `cowboy_tui_terminal::tui_input_cursor_style()` as `SetCursorStyle::BlinkingBlock`; terminal entry/exit remains the only place that sets or resets cursor shape.
- Do not add Windows-specific branches. The safe behavior is platform-neutral: hide before diff painting, restore after frame cursor placement.

# Tests to be added/updated

- Keep `app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell` as the regression contract and make it pass without changing its assertion that no changed animation cells are painted while the cursor is visible.
- Preserve existing cursor placement coverage in `app::tests::draw_places_cursor_at_input_end`, `app::tests::draw_places_cursor_in_active_run_draft_input`, `app::tests::draw_places_cursor_at_wrapped_input_end`, and `app::tests::draw_places_cursor_at_moved_wrapped_input_position`.
- Preserve cursor-style coverage in `cowboy_tui_terminal::tests::tui_input_cursor_style_uses_unix_block_cursor` so the input cursor remains a blinking block during TUI sessions.
- No new broad UI test is required because the RCA repro directly instruments the terminal cursor visibility ordering that caused the Windows-only flash, and the cursor-style risk is controlled by keeping style setup out of the per-frame draw helper.

# How to verify

Run these commands from the repository root after implementation:

```bash
cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell
cargo test -p cowboy --lib app::tests::draw_places_cursor_at_input_end
cargo test -p cowboy --lib app::tests::draw_places_cursor_in_active_run_draft_input
cargo test -p cowboy --lib app::tests::draw_places_cursor_at_wrapped_input_end
cargo test -p cowboy --lib app::tests::draw_places_cursor_at_moved_wrapped_input_position
cargo test -p cowboy-tui-terminal tui_input_cursor_style_uses_unix_block_cursor -- --exact
```

Observable pass criteria:

- The RCA repro test exits successfully because redraw diff painting occurs after a cursor-hide call, so `visible_redraw_painted_cells == 0`.
- Single-line, active-run, and wrapped-input cursor placement tests still exit successfully, proving the cursor is restored to the composer input position after completed frames.
- The terminal cursor-style test exits successfully, proving Cowboy's configured TUI cursor style remains `SetCursorStyle::BlinkingBlock`.
- Code inspection confirms the per-frame draw helper uses `Terminal::hide_cursor()`/Ratatui frame cursor restoration only and does not call `SetCursorStyle`, so it cannot change the input cursor shape or blinking style.
- The running status animation remains active and continues to dirty frames through `tick_status_animation`; the fix changes cursor visibility ordering, not animation cadence.

# TODO

- [x] TODO-01: Add a cursor-safe terminal draw helper in the TUI app.
  - Procedure: In `crates/tui/app/src/app.rs`, add a helper that accepts the mutable `Terminal`, `AppState`, and previous `AppLayout`, calls `terminal.hide_cursor()` before invoking `Terminal::draw`, uses `draw_production_frame` inside the draw closure, and returns the updated layout or the original draw error.
  - Expected result: The helper keeps the existing `draw_production_frame` render path intact and guarantees the backend receives `hide_cursor` before any changed cells are painted on each production redraw.
  - Observed result: `crates/tui/app/src/app.rs` now has `draw_cursor_safe_production_frame`, which calls `terminal.hide_cursor()?` before `terminal.draw`, renders through `draw_production_frame`, and returns the updated layout on success. The helper is exercised by the passing repro test command recorded for TODO-02.

- [x] TODO-02: Route production redraws and the repro draw scenario through the cursor-safe helper.
  - Procedure: Replace the direct `Terminal::draw` call in `crates/tui/app/src/app.rs::run_loop` with the helper from TODO-01, preserving `current_layout` updates, `draw_scheduler.mark_clean()`, and the existing `tracing::error!("TUI draw failed")` path; update only the draw calls in `crates/tui/app/src/app/tests.rs::status_animation_redraw_hides_cursor_before_painting_changed_cell` to use the same helper; then run `cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell`.
  - Expected result: The command exits successfully, and the running-animation redraw paints zero changed cells while the probe backend cursor is visible.
  - Observed result: `run_loop` now calls `draw_cursor_safe_production_frame` while preserving layout updates, `draw_scheduler.mark_clean()`, and the `tracing::error!("TUI draw failed")` error path. The repro draw scenario uses the same helper. `cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell` exited successfully with 1 test passed, proving zero visible-cursor changed-cell paints.

- [x] TODO-03: Keep the investigator repro test as the cursor-ordering contract.
  - Procedure: Inspect `crates/tui/app/src/app/tests.rs::status_animation_redraw_hides_cursor_before_painting_changed_cell` after TODO-02 and confirm the test name, run setup, `state.advance_status_animation()` step, `visible_redraw_painted_cells == 0` assertion, and failure message are still present; rerun `cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell`.
  - Expected result: The command exits successfully and the test still proves the original Windows symptom is prevented by hiding the cursor before animation diff painting.
  - Observed result: Inspection confirmed the test name, running workflow setup, `state.advance_status_animation()` step, `visible_redraw_painted_cells == 0` assertion, and original failure message remain present. Rerunning `cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell` exited successfully with 1 test passed.

- [x] TODO-04: Preserve composer cursor restoration behavior.
  - Procedure: Leave `crates/tui/app/src/app/controls/composer.rs::render` requesting the cursor when `state.composer_accepts_edits()` is true, then run `cargo test -p cowboy --lib app::tests::draw_places_cursor_at_input_end`, `cargo test -p cowboy --lib app::tests::draw_places_cursor_in_active_run_draft_input`, `cargo test -p cowboy --lib app::tests::draw_places_cursor_at_wrapped_input_end`, and `cargo test -p cowboy --lib app::tests::draw_places_cursor_at_moved_wrapped_input_position`.
  - Expected result: All four commands exit successfully, and the terminal cursor remains positioned at the expected composer input cell after completed frames.
  - Observed result: `crates/tui/app/src/app/controls/composer.rs::render` was left unchanged. `cargo test -p cowboy --lib app::tests::draw_places_cursor_at_input_end`, `cargo test -p cowboy --lib app::tests::draw_places_cursor_in_active_run_draft_input`, `cargo test -p cowboy --lib app::tests::draw_places_cursor_at_wrapped_input_end`, and `cargo test -p cowboy --lib app::tests::draw_places_cursor_at_moved_wrapped_input_position` all exited successfully with 1 test passed each.

- [x] TODO-05: Preserve the configured blinking block cursor style while hiding redraws.
  - Procedure: Keep per-frame draw code in `crates/tui/app/src/app.rs` free of `SetCursorStyle`, `tui_input_cursor_style`, `SetCursorStyle::DefaultUserShape`, `EnableBlinking`, and `DisableBlinking`; leave cursor style setup in `crates/tui/terminal/src/lib.rs`; then run `cargo test -p cowboy-tui-terminal tui_input_cursor_style_uses_unix_block_cursor -- --exact`.
  - Expected result: The command exits successfully, `tui_input_cursor_style()` still returns `SetCursorStyle::BlinkingBlock`, and no per-frame draw helper changes cursor shape or blinking behavior.
  - Observed result: `crates/tui/app/src/app.rs` contains no `SetCursorStyle`, `tui_input_cursor_style`, `SetCursorStyle::DefaultUserShape`, `EnableBlinking`, or `DisableBlinking` references in the per-frame draw helper; `crates/tui/terminal/src/lib.rs` still returns `SetCursorStyle::BlinkingBlock` from `tui_input_cursor_style()`. `cargo test -p cowboy-tui-terminal tui_input_cursor_style_uses_unix_block_cursor -- --exact` exited successfully.
