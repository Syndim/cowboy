# Plan

Fix the intermittent non-blinking input cursor by stopping the TUI from redrawing the composer continuously while nothing changed. The current loop in `crates/tui/src/app.rs` calls `terminal.draw(...)` before every `event::poll(Duration::from_millis(100))`. Each draw calls `composer::render`, which calls `Frame::set_cursor_position`; Ratatui then shows and moves the terminal cursor after the frame. Many terminals reset the cursor blink timer when the cursor is shown or moved, so a 10 Hz idle redraw can make the cursor appear steady or inconsistently blinking.

Keep the cursor owned by Ratatui frame cursor positioning, but make rendering event-driven: draw once initially, draw after input/workflow/background state changes, and do not redraw on idle poll timeouts. Also set a blinking cursor style once when entering the alternate screen and restore the user's default cursor style on exit, so Cowboy does not inherit a stale steady cursor style from a previous terminal program.

# Changes

- Update `crates/tui/src/app.rs` to track a `needs_draw` flag in `run_loop`:
  - initialize it to `true` so the first frame renders immediately;
  - draw only when `needs_draw` is true;
  - set it back to `false` after a successful draw;
  - leave idle poll timeouts as state-drain opportunities, not forced redraws.
- Change state-draining APIs so the loop can tell whether anything visible changed:
  - make `AppState::drain_workflow_events` return `true` when it applies at least one event or handles lag;
  - make `AppState::drain_background_tasks` return `true` when it consumes at least one finished task or changes status/events;
  - keep existing behavior for event ordering, streaming events, and background task retention.
- Mark `needs_draw = true` after every user input event that can affect visible state:
  - paste input;
  - key handling that mutates input, history position, scroll, cancellation status, or submits a command;
  - command submission results and spawned background tasks.
- Keep `composer::set_input_cursor` as the single per-frame cursor-positioning path; do not add a second manual cursor move outside drawing.
- Update `TerminalModeGuard` in `crates/tui/src/app.rs` to set a blinking input-friendly cursor style once on enter, preferably `crossterm::cursor::SetCursorStyle::BlinkingBar`, and restore `SetCursorStyle::DefaultUserShape` on exit before showing the cursor.
- Avoid runtime logic changes outside `crates/tui`; this is terminal rendering policy, not workflow engine behavior.

# Tests to be added/updated

- Add unit coverage for the changed state drain return values in `crates/tui/src/app/state.rs`:
  - no pending workflow events returns `false` and leaves visible state unchanged;
  - applying a workflow event returns `true`;
  - no finished background task returns `false`;
  - a finished background task returns `true` and updates status/event log as before.
- Add focused draw-invalidation coverage in `crates/tui/src/app.rs` if the loop logic is extracted into a small helper, asserting:
  - the first tick draws;
  - an idle poll timeout after a clean frame does not draw again;
  - a workflow event, background completion, paste, or key press marks the next frame dirty.
- Keep existing cursor placement tests in `crates/tui/src/app/tests.rs` passing, especially `draw_places_cursor_at_input_end` and `draw_places_cursor_at_wrapped_input_end`.
- Add or update a terminal-mode test only if the enter/restore cursor-style commands are testable without a real terminal; otherwise cover this with manual smoke verification because cursor blink behavior is terminal-emulator behavior.

# How to verify

- Run `cargo test -p cowboy app::state` to verify visible-state drain return values.
- Run `cargo test -p cowboy app::tests` to verify composer rendering and cursor positioning still work.
- Run `cargo test -p cowboy` to catch any TUI command/input regressions.
- Manually smoke-test with `cargo run -p cowboy`: leave the TUI idle with focus in the input box for several seconds and confirm the cursor blinks steadily; type text, paste text, run a workflow, and confirm the UI still updates promptly after each visible state change.
- In the manual smoke test, confirm exiting restores the terminal cursor style to the user's default shape/blink setting.

# TODO

- [x] Add dirty-render scheduling to `crates/tui/src/app.rs` so idle poll timeouts do not redraw the frame.
- [x] Make workflow-event draining report whether visible state changed.
- [x] Make background-task draining report whether visible state changed.
- [x] Mark the frame dirty after paste, handled key input, command submission, workflow events, and finished background tasks.
- [x] Preserve existing Ratatui `Frame::set_cursor_position` cursor placement in `composer::render`.
- [x] Set a blinking cursor style once when entering terminal mode and restore the default user cursor style on exit.
- [x] Add state-drain tests for changed and unchanged drain paths.
- [x] Add draw-invalidation tests if the scheduling logic is extracted into a testable helper.
- [x] Run targeted and package-level tests.
- [x] Manually smoke-test cursor blinking and terminal cursor restoration in the TUI.

# Smoke test evidence

Recorded PTY smoke test for the manual terminal path:

- Command: `cargo run -p cowboy -- --config /tmp/cowboy-tui-smoke-se6_m1qg/config.toml`.
- Launched the TUI in an alternate-screen PTY with a fake ACP selector that chooses the status-only `00-demo` workflow.
- Confirmed `SetCursorStyle::BlinkingBar` was emitted on enter and `SetCursorStyle::DefaultUserShape` was emitted on exit.
- Left the input idle for 2.25 seconds after the first frame; observed `0` output bytes during the idle window, which confirms no idle redraws were resetting the terminal cursor blink timer.
- Typed `typed` and confirmed the composer re-rendered promptly.
- Sent a bracketed paste of `/run smoke workflow`, pressed Enter, and confirmed the submitted run rendered promptly.
- Confirmed the workflow reached `Waiting for input` with `Apply the plan?`.
- Typed `yes`, pressed Enter, and confirmed the workflow completed with `applied the plan` / completed status.
- Pressed Ctrl-C; process exited with code `0`, left the alternate screen, and restored the default user cursor shape.
