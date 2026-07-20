# Plan

Implement the confirmed fix from [the RCA](./rca.md): vertical mouse-wheel events over the composer must preserve the current composer input, while keyboard `Up`/`Down` remains the only submitted-input history navigation path.

The current checkout is the `d7daba2` revert of the mouse-enabled implementation. Restore the non-defective region-aware mouse scrolling from `6bf5212` onto the current branch, adapting it to intervening TUI changes, then omit the two composer match arms that called `AppState::history_previous()` and `AppState::history_next()`. Preserve transcript wheel scrolling, shared layout hit-testing, scrollbar rendering, mouse-capture cleanup, and the existing keyboard composer navigation behavior. Whether `handle_mouse_event` reports a composer wheel event as handled is not part of this fix; input immutability is the contract.

Use `docs/plans/mouse_scrolling_switches_composer_input_history/regression_test.patch` as the unchanged regression-test input. Do not rewrite or replace `app::input::tests::composer_mouse_wheel_does_not_switch_input_history`.

# Changes

- Restore the non-defective parts of `6bf5212` in `crates/tui/app/src/app.rs`: paired Crossterm mouse capture/restore, `Event::Mouse` dispatch, and the shared `AppLayout` rectangles used for both rendering and hit-testing. Resolve against the current branch rather than reverting later unrelated TUI work.
- Restore the bounded transcript viewport and scrollbar behavior in `crates/tui/app/src/app/controls/transcript.rs`, plus the minimal scroll-limit/change-reporting state in `crates/tui/app/src/app/state.rs`, so wheel events over the transcript continue to scroll visual rows without unbounded history work.
- Restore `handle_key_press_with_layout` and `handle_mouse_event` in `crates/tui/app/src/app/input.rs`, but route vertical wheel events only when the pointer is inside `layout.transcript`. Do not add composer wheel arms and do not call `history_previous()`, `history_next()`, `composer::move_input_up()`, `composer::move_input_down()`, or any equivalent input-replacement path from mouse handling.
- Leave `handle_key_press` and `crates/tui/app/src/app/controls/composer.rs` as the keyboard-owned history path: `Up`/`Down` may continue crossing history entries at the existing visual boundaries and submit-availability guard.
- Restore the non-defective terminal lifecycle, shared-layout, transcript scrolling, scrollbar, resize, and draw-level coverage from `6bf5212` in `crates/tui/app/src/app/tests.rs` and the touched module tests.
- Do not restore the stale `composer_wheel_changes_only_guarded_input_history` expectation from `6bf5212`; it encodes the rejected behavior. Keep the investigator regression as the single focused composer-wheel contract.

# Tests to be added/updated

- Apply `docs/plans/mouse_scrolling_switches_composer_input_history/regression_test.patch` unchanged so `crates/tui/app/src/app/input.rs::app::input::tests::composer_mouse_wheel_does_not_switch_input_history` dispatches real `ScrollUp` and `ScrollDown` events inside `layout.composer` and verifies that both selected history inputs remain byte-for-byte unchanged.
- Restore the transcript-region mouse tests proving wheel up/down changes transcript offset/follow state without mutating composer input, including empty/short transcript boundaries and viewport-resize behavior.
- Restore no-op coverage for header, status, outside-layout, horizontal-wheel, movement, and click events. Composer wheel events may be included in no-op state coverage, but no test should require a particular `handle_mouse_event` boolean for them.
- Keep the existing keyboard history tests, including `history_entries_change_only_at_visual_boundaries`, passing to prove that removal of mouse history routing does not remove the supported `Up`/`Down` behavior.
- Restore terminal mouse-capture cleanup, shared-layout boundary, scrollbar rendering/position, narrow-area safety, and large-history bounded-redraw tests needed by the reintroduced mouse feature.

# How to verify

- Run the focused regression in the implementation checkout:
  - `cargo test -p cowboy composer_mouse_wheel_does_not_switch_input_history`
- Recheck the RCA baseline by applying `regression_test.patch` and the minimal handler fix to a detached `6bf5212` worktree, then run:
  - `cargo test -p cowboy composer_mouse_wheel_does_not_switch_input_history`
- Run focused TUI suites covering all restored behavior:
  - `cargo test -p cowboy app::input::tests`
  - `cargo test -p cowboy app::controls::transcript::tests`
  - `cargo test -p cowboy app::tests`
- Run formatting and warning checks:
  - `cargo fmt -p cowboy -- --check`
  - `cargo clippy -p cowboy --all-targets -- -D warnings`
- Manually run `cargo run -p cowboy` with at least two submitted inputs and an overflowing transcript. Verify wheel up/down over the composer never changes the current input; keyboard `Up`/`Down` still navigates history; transcript wheel scrolling and scrollbar movement still work; header/status wheel events do nothing; resizing remains usable; and exit restores normal terminal mouse behavior.

# TODO

- [x] Reapply the non-defective `6bf5212` mouse-capture, shared-layout, transcript viewport, scrollbar, and state changes onto the current checkout without restoring stale documentation or unrelated reverted behavior.
- [x] Restore mouse event dispatch and transcript-only wheel routing in `crates/tui/app/src/app.rs` and `crates/tui/app/src/app/input.rs`.
- [x] Remove composer-to-history routing so vertical composer wheel events cannot replace `AppState.input`, while preserving keyboard `Up`/`Down` history navigation.
- [x] Apply `regression_test.patch` unchanged and omit the obsolete `composer_wheel_changes_only_guarded_input_history` test.
- [x] Restore and adapt the non-defective mouse lifecycle, layout, transcript, scrollbar, resize, no-op region, and bounded-rendering tests.
- [x] Run the focused regression on the implementation checkout and on the patched `6bf5212` baseline with the minimal fix.
- [x] Run the focused TUI test suites, `cargo fmt -p cowboy -- --check`, and warning-free Clippy.
- [x] Manually verify composer input immutability, keyboard history navigation, transcript scrolling, scrollbar movement, region no-ops, resize behavior, and terminal restoration.
- [x] Add focused coverage for `Up` on the oldest history entry after moving its cursor.
- [x] Rewrite the stale RCA checkout wording as historical investigation context.
