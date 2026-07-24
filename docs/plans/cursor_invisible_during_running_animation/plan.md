# Plan

Use the confirmed RCA at
`docs/plans/cursor_invisible_during_running_animation/rca.md` as the source of
truth. Keep the investigator-added regression test
`crates/tui/app/src/app/tests.rs::running_animation_keeps_composer_cursor_visible`
as an unchanged input to the fix: do not rename it, replace it, weaken its
running-animation precondition, or weaken its visible-cursor assertion.

Restore the composer cursor request while the running animation is active by
removing the animation-specific suppression from
`AppState::composer_shows_cursor`. Continue to use the existing
`composer::render` and `set_input_cursor` path so the cursor is restored at the
actual composer input position rather than through a second running-only
placement path.

Preserve `draw_cursor_safe_production_frame` and its pre-paint
`Terminal::hide_cursor()` call. That ordering protects against the prior cursor
flash over changed animation cells: each frame must hide the cursor while
Ratatui paints its diff, then restore it at the composer position after painting.

Change the terminal-session input cursor style from
`SetCursorStyle::BlinkingBlock` to `SetCursorStyle::SteadyBlock`. Cowboy
currently configures one cursor style when entering the TUI, and a steady block
meets the accepted non-blinking behavior while avoiding repeated blink-phase
resets during animation redraws. Terminal teardown must continue restoring
`SetCursorStyle::DefaultUserShape` and emitting `Show`.

# Changes

- In `crates/tui/app/src/app/state.rs`, change
  `AppState::composer_shows_cursor` so an editable composer remains cursor-visible
  during `status_animation_active()`. Keep editability, submission gating,
  animation activation, and animation cadence unchanged.
- Keep `crates/tui/app/src/app/controls/composer.rs::render` using
  `composer_shows_cursor()` and `set_input_cursor`; do not duplicate cursor
  positioning logic.
- Keep `crates/tui/app/src/app.rs::draw_cursor_safe_production_frame` hiding the
  cursor before diff painting and allowing Ratatui to restore the requested
  cursor after rendering.
- In `crates/tui/terminal/src/lib.rs`, make
  `tui_input_cursor_style()` return `SetCursorStyle::SteadyBlock`. Preserve
  terminal teardown with `SetCursorStyle::DefaultUserShape` and `Show`.
- Reconcile tests that encode the superseded hidden-running-cursor behavior.
  Require a visible cursor at the composer position after each completed
  running-animation frame while retaining the no-visible-cursor-during-diff-paint
  assertion.

# Tests to be added/updated

- Keep
  `app::tests::running_animation_keeps_composer_cursor_visible` unchanged and
  make it pass.
- Keep
  `app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell`
  unchanged as the pre-paint cursor-safety regression contract.
- Rename
  `app::state::tests::composer_hides_cursor_only_during_running_animation` to
  `composer_shows_cursor_while_editable_during_running_animation`; assert cursor
  visibility in idle, active running-animation, and waiting-for-input states.
- Rename `app::tests::draw_hides_composer_cursor_during_active_run_animation` to
  `draw_shows_composer_cursor_during_active_run_animation`; retain the active-run
  setup and assert visibility at the expected composer input position.
- Replace the superseded expectations in
  `app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames`
  with a renamed multi-frame test,
  `running_animation_redraws_restore_visible_cursor_after_safe_paint`. Extend
  `CursorVisibilityProbeBackend` only as needed to record restored cursor
  positions. Assert that animation ticks continue to dirty frames, changed cells
  are painted while the cursor is hidden, and every completed frame leaves the
  cursor visible at the composer position.
- Rename
  `tests::tui_input_cursor_style_uses_unix_block_cursor` in
  `cowboy-tui-terminal` to
  `tests::tui_input_cursor_style_uses_steady_block_cursor` and assert
  `SetCursorStyle::SteadyBlock`.
- Extend
  `tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste` with
  explicit serialized-command assertions for
  `SetCursorStyle::DefaultUserShape` (`"\u{1b}[0 q"`) and `Show`
  (`"\u{1b}[?25h"`), while preserving its existing mouse-capture and bracketed
  paste assertions.

# How to verify

Run from the repository root:

```bash
cargo test -p cowboy --lib app::tests::running_animation_keeps_composer_cursor_visible -- --exact --nocapture
cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell -- --exact
cargo test -p cowboy --lib app::tests::running_animation_redraws_restore_visible_cursor_after_safe_paint -- --exact
cargo test -p cowboy --lib app::tests::draw_shows_composer_cursor_during_active_run_animation -- --exact
cargo test -p cowboy --lib app::state::tests::composer_shows_cursor_while_editable_during_running_animation -- --exact
cargo test -p cowboy-tui-terminal tests::tui_input_cursor_style_uses_steady_block_cursor -- --exact
cargo test -p cowboy-tui-terminal tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste -- --exact
cargo test -p cowboy --lib
cargo test -p cowboy-tui-terminal
cargo clippy -p cowboy -p cowboy-tui-terminal --all-targets -- -D warnings
rustfmt --edition 2024 crates/tui/app/src/app/state.rs crates/tui/app/src/app/tests.rs crates/tui/terminal/src/lib.rs
rustfmt --edition 2024 --check crates/tui/app/src/app/state.rs crates/tui/app/src/app/tests.rs crates/tui/terminal/src/lib.rs
cargo fmt --all -- --check
git diff --check
```

Observable pass criteria:

- Every exact selector reports `running 1 test` and exits successfully.
- The investigator repro observes an active running animation and a visible
  rendered cursor.
- The multi-frame test observes a visible cursor at the composer input after
  each frame while animation ticks continue.
- The cursor-safety repro observes zero changed cells painted while the cursor
  is visible.
- Terminal tests prove Cowboy selects a steady block and teardown emits
  `DefaultUserShape` and `Show`.
- Both crate suites and Clippy exit successfully without warnings.
- Focused `rustfmt --edition 2024 --check` exits successfully for
  `state.rs`, `tests.rs`, and the terminal `lib.rs`; `git diff --check` exits
  successfully.
- The read-only workspace `cargo fmt --all -- --check` result is reported
  separately. Any output for one of the three bug files is a failed focused
  validation; output limited to unrelated files is recorded without rewriting
  those files.

Manual verification:

1. Run `cargo run`, enter draft text in the composer, and start a workflow.
2. While the running status animation advances, observe the composer.
3. Confirm the cursor remains visible at the draft input position, remains
   steady rather than rapidly blinking, does not flash over status-animation
   cells, draft editing still works, and the animation cadence is unchanged.
4. Exit Cowboy and confirm the terminal restores its normal user cursor shape.

# TODO

- [x] TODO-01: Restore composer cursor visibility during the running animation without changing editability or animation state.
  - Procedure: In `crates/tui/app/src/app/state.rs`, change
    `AppState::composer_shows_cursor` to return the editable-composer cursor
    policy without excluding `status_animation_active()`. Leave
    `composer_accepts_edits`, `status_animation_active`, and
    `advance_status_animation` unchanged. Run
    `cargo test -p cowboy --lib app::tests::running_animation_keeps_composer_cursor_visible -- --exact --nocapture`.
  - Expected result: The command reports `running 1 test` and exits successfully;
    its running-animation precondition remains true and the rendered backend
    cursor is visible at the composer input.
  - Implementer-observed result: `AppState::composer_shows_cursor` now returns
    the existing editable-composer policy directly; editability and animation
    methods remain unchanged. The exact investigator repro reported `running 1
    test` and passed with its active-animation and visible rendered cursor
    assertions unchanged.

- [x] TODO-02: Use a steady block cursor for the TUI session while preserving terminal restoration.
  - Procedure: In `crates/tui/terminal/src/lib.rs`, change
    `tui_input_cursor_style()` from `SetCursorStyle::BlinkingBlock` to
    `SetCursorStyle::SteadyBlock`; rename and update its focused unit test.
    Extend
    `tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste` to
    assert that teardown serialization contains `"\u{1b}[0 q"` for
    `SetCursorStyle::DefaultUserShape` and `"\u{1b}[?25h"` for `Show`. Run
    `cargo test -p cowboy-tui-terminal tests::tui_input_cursor_style_uses_steady_block_cursor -- --exact`
    and
    `cargo test -p cowboy-tui-terminal tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste -- --exact`.
  - Expected result: Both commands report `running 1 test` and exit successfully;
    terminal entry selects a steady block, and teardown explicitly restores and
    shows the user's default cursor.
  - Implementer-observed result: `tui_input_cursor_style()` now selects
    `SteadyBlock`, and the teardown test contains explicit serialized assertions
    for `DefaultUserShape` and `Show`. Both exact terminal selectors reported
    `running 1 test` and passed.

- [x] TODO-03: Reconcile superseded hidden-cursor tests with visible, cursor-safe running-animation behavior.
  - Procedure: Update the named state and app tests described in
    `Tests to be added/updated`; do not modify or replace
    `running_animation_keeps_composer_cursor_visible` or
    `status_animation_redraw_hides_cursor_before_painting_changed_cell`. If
    required, extend `CursorVisibilityProbeBackend` to record cursor positions.
    Run the five exact `cowboy` selectors listed first under `How to verify`.
  - Expected result: All five commands report `running 1 test` and exit
    successfully; running frames end with a visible cursor at the composer input,
    animation ticks continue to trigger redraws, and zero changed cells are
    painted while the cursor is visible.
  - Implementer-observed result: The superseded state and app tests now require
    visible cursor-safe running frames, and `CursorVisibilityProbeBackend`
    records final cursor positions. The two investigator repro tests remain
    unchanged. All five exact Cowboy selectors reported `running 1 test` and
    passed, observing restored composer positions, continued animation redraws,
    and zero changed cells painted while the cursor was visible.

- [x] TODO-04: Validate the complete TUI cursor fix and its crate boundaries.
  - Procedure: Execute these seven steps in order and preserve their indices in
    implementer and tester command/evidence records:
    1. Run
       `cargo test -p cowboy-tui-terminal tests::tui_input_cursor_style_uses_steady_block_cursor -- --exact`
       and
       `cargo test -p cowboy-tui-terminal tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste -- --exact`.
    2. Run `cargo test -p cowboy --lib`.
    3. Run `cargo test -p cowboy-tui-terminal`.
    4. Run
       `cargo clippy -p cowboy -p cowboy-tui-terminal --all-targets -- -D warnings`.
    5. Format only the files changed for this bug with
       `rustfmt --edition 2024 crates/tui/app/src/app/state.rs crates/tui/app/src/app/tests.rs crates/tui/terminal/src/lib.rs`,
       then require
       `rustfmt --edition 2024 --check crates/tui/app/src/app/state.rs crates/tui/app/src/app/tests.rs crates/tui/terminal/src/lib.rs`
       to exit successfully. Run `cargo fmt --all -- --check` afterward only as a
       read-only workspace diagnostic: do not run `cargo fmt --all` and do not
       rewrite unrelated files. Report unrelated workspace-format findings
       separately; any reported difference in the three bug files is a failed
       validation.
    6. Run `git diff --check` and inspect `git status --short` plus
       `git diff --stat` to confirm the resulting patch is whitespace-clean and
       all formatter changes are visible for review.
    7. Perform the ordered live-terminal verification under `How to verify`,
       including running-animation cursor visibility, steady behavior, draft
       editing, animation advancement, absence of cursor flashes over changed
       status cells, and cursor restoration after exit. Map the live-terminal
       command to procedure index 7 in both implementer and tester records.
  - Expected result: Both exact terminal selectors report `running 1 test`;
    tests and Clippy exit successfully without warnings;
    focused `rustfmt --edition 2024 --check` and `git diff --check` exit
    successfully; the workspace formatting diagnostic reports no differences in
    the three bug files and any unrelated findings are reported separately;
    during a live running animation the cursor is visible and steady at the
    composer input, changed status cells do not display the cursor, editing and
    animation remain functional, and exiting restores the normal terminal
    cursor.
  - Implementer-observed result: Both exact terminal selectors, all 339 Cowboy
    library tests (337 passed and 2 ignored), all 11 terminal tests, and Clippy
    with warnings denied passed. Focused formatting and `git diff --check`
    passed. The read-only workspace formatting diagnostic exited 1 only for
    unrelated unchanged files and reported no difference in the three bug files;
    those baseline findings were not applied. Isolated tmux verification sampled
    `cursor_flag=1` across advancing running frames, observed the cursor move
    from x=3 to x=6 after editing draft `abc`, and captured steady-block setup,
    default-shape restoration, and cursor-show terminal sequences after clean
    `/exit`.
