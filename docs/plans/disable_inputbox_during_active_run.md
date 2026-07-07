# Plan

Disable the TUI composer while a submitted workflow operation is actively running, then re-enable it when the operation reaches a user-actionable or non-running state. The current TUI accepts new characters, paste, history navigation, slash completion, and `Enter` while `AppState` has background run tasks (`crates/tui/src/app/state.rs:491-499`), so a user can queue or edit another request before the active run blocks. Add one derived input gate on `AppState` and use it consistently in event handling, key handling, rendering, and status copy.

Treat `WaitingForInput` as explicitly enabled so users can answer the pending prompt. Treat `cancelled`, failed, completed, or other non-running states as enabled once no background task remains. There is no separate `Paused` or `Blocked` `RunStatus` variant in `crates/workflow/core/src/state.rs`; today "blocked" is represented by `RunStatus::WaitingForInput`, failed resolution states, or a returned `RunReport` after `run_until_blocked`.

# Changes

- Add an `AppState` query such as `composer_enabled()` / `input_enabled()` in `crates/tui/src/app/state.rs`.
  - Return enabled when `pending_prompt().is_some()` so prompt answers can be typed.
  - Return enabled when `background_task_count() == 0` so idle, completed, failed, and cancelled runs accept new input.
  - Return disabled while one or more `spawn_report_task` futures are active and no prompt is pending.
- Route all input mutations through the same gate.
  - In `crates/tui/src/app.rs`, ignore `Event::Paste` while the composer is disabled.
  - In `crates/tui/src/app/input.rs`, make disabled-state handling allow only global controls that do not edit or submit the composer: `Ctrl+C` exits, `Esc` cancels active background tasks, `Ctrl+U`/`Ctrl+D` scroll, and `End` follows latest.
  - While disabled, printable characters, newline insertion, `Enter`, `Tab`, history navigation, cursor movement, `Backspace`, and `Delete` should be no-ops that return `KeyHandling::Continue`.
- Keep prompt answers working.
  - Do not change `commands::dispatch_submitted_input` prompt-answer routing through `pending_prompt_answer_target()`.
  - After `spawn_answer_task` clears the prompt and starts the answer/resume background task, the composer becomes disabled again until that task blocks, completes, fails, or is cancelled.
- Update rendering to make the disabled state visible.
  - In `crates/tui/src/app/controls/composer.rs`, show a disabled title such as `Run active ─ input disabled ─ Esc cancels` when `composer_enabled()` is false.
  - Suppress slash suggestions while disabled.
  - Avoid placing the input cursor in the composer while disabled, or otherwise render an unmistakable disabled affordance so the box does not look editable.
- Update status/help copy.
  - In `crates/tui/src/app/controls/status.rs`, mention that active background tasks disable input and that `Esc` cancels.
  - README docs are intentionally left unchanged per `confirm_result_answer` feedback.
- Preserve existing cancellation behavior.
  - Keep `/cancel` unchanged for enabled states.
  - Keep `Esc` as the available cancellation path while the composer is disabled, because `/cancel` cannot be typed during the locked interval.

# Tests to be added/updated

- Add focused input-handler tests in `crates/tui/src/app/input.rs` for the disabled composer state:
  - printable characters are ignored;
  - plain `Enter` does not return `KeyHandling::Submit`;
  - modified `Enter`, `Tab`, history navigation, cursor movement, `Backspace`, and `Delete` do not mutate the input;
  - `Esc` still cancels active background tasks;
  - `Ctrl+C`, transcript scrolling, and `End` still work.
- Add state-level coverage in `crates/tui/src/app/state.rs` for the derived enabled/disabled rules:
  - idle state is enabled;
  - an active background task disables composer input;
  - `WorkflowEventKind::WaitingForInput` re-enables prompt-answer input;
  - drained/cancelled background tasks re-enable input.
- Add rendering tests in `crates/tui/src/app/controls/composer.rs` and/or `crates/tui/src/app/tests.rs` proving the disabled title/status is visible and slash-command suggestions are hidden while disabled.
- Add or update status rendering assertions if existing tests cover the relevant status line text.

# How to verify

- Run `cargo test -p cowboy app::input::tests` for focused key-handling coverage.
- Run `cargo test -p cowboy app::state::tests` for the input-gate state transitions.
- Run `cargo test -p cowboy app::controls::composer::tests app::tests` for rendering/status coverage.
- Run `cargo test -p cowboy` if the focused test filters are not accepted or if the status/doc-related assertions land outside those modules.
- Manual TUI smoke test:
  - launch `cargo run -p cowboy`;
  - submit a normal workflow request;
  - while the run is active, type characters, paste text, press `Tab`, `↑`, `Backspace`, and `Enter`, and confirm the composer stays unchanged and no second run is submitted;
  - press `Esc` during an active run and confirm the background task is cancelled and input becomes editable again;
  - start a workflow that asks for input, confirm the composer re-enables at `WaitingForInput`, type an answer, and confirm it submits to the pending prompt;
  - after the answer resumes the run, confirm the composer disables again until the run blocks or finishes.

# Manual smoke result

- 2026-07-07: Passed scripted TUI smoke using `cargo run -q -p cowboy -- --config <temp-config>` with a temporary `ask-agent` workflow and a `sleep 20` agent command to hold active runs.
  - Active-run input lock: disabled copy appeared; typed text, `Tab`, and `Enter` were ignored.
  - `Esc` cancellation unlock: `Esc` cancelled the active task; `/help` was accepted afterward.
  - Prompt-answer unlock: `WaitingForInput` enabled answer entry and showed the answer-prompt title.
  - Post-answer re-lock: submitting `yes` resumed the run; composer locked again and ignored new typed text.

# TODO

- [x] Add a derived composer-enabled/input-enabled query to `AppState`.
- [x] Gate paste handling in the TUI event loop on the new query.
- [x] Gate key handling so disabled composer input ignores edits/submission but keeps global controls.
- [x] Preserve prompt-answer submission and re-disable after answer submission starts a background task.
- [x] Render a visible disabled composer state and hide slash suggestions while disabled.
- [x] Update status-line copy for active-run input locking.
- [x] Leave README unchanged per `confirm_result_answer` feedback.
- [x] Add disabled-state input-handler unit tests.
- [x] Add AppState enabled/disabled transition unit tests.
- [x] Add composer/status rendering tests for disabled state.
- [x] Run focused TUI tests and, if needed, the full `cowboy` crate test suite.
- [x] Manually smoke-test active-run locking, cancellation unlock, prompt-answer unlock, and post-answer re-lock.
