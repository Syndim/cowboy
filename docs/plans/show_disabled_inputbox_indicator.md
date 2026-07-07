# Plan

Make the disabled TUI input box communicate its state inside the composer itself, using the existing `AppState::composer_enabled()` gate as the single source of truth. The current input lock is enforced in `crates/tui/src/app/input.rs`, paste handling is gated in `crates/tui/src/app.rs`, and disabled-state copy already appears in the composer title and status line. Add an in-box disabled notice so users can see the input area is intentionally locked even when the cursor is hidden and the input body is otherwise empty.

Keep the behavior unchanged: while a background run task is active and no prompt is pending, input edits/submission remain ignored; `Esc` still cancels active background tasks; transcript scrolling and exit controls still work; `WaitingForInput` keeps the composer enabled so the user can answer the prompt.

# Changes

- Update `crates/tui/src/app/controls/composer.rs` to reserve one visible composer body row for disabled-state copy when `!state.composer_enabled()`.
- Render a styled, explicit notice in the input box body, for example `Input disabled while run active. Press Esc to cancel.`, using existing warning or muted styles from `crates/tui/src/app/styles.rs` rather than introducing a new palette.
- Keep the disabled composer title focused on the active run/cancel affordance while the status-line copy and in-box notice explain that input is disabled.
- Continue suppressing slash-command suggestions while disabled.
- Do not change prompt-answer routing, workflow runtime state, background task lifecycle, or the existing input-lock semantics.
- Avoid adding README or workflow documentation changes; this is a TUI affordance change only.

# Tests to be added/updated

- Update `crates/tui/src/app/controls/composer.rs` tests so the disabled composer case asserts the in-box disabled notice is rendered and slash suggestions remain hidden.
- Update `crates/tui/src/app/tests.rs` rendering coverage so a locked composer screen includes the disabled notice in the composer area, the disabled title, and the active-run status copy.
- Keep the existing input-handler tests in `crates/tui/src/app/input.rs` unchanged unless copy changes require test name or assertion updates; they already cover that disabled composer input ignores edits/submission while allowing global controls.
- Keep `crates/tui/src/app/state.rs` enabled/disabled transition tests unchanged unless the helper name changes; they already cover active task, prompt, cancellation, and completion transitions.

# How to verify

- Run `cargo test -p cowboy app::controls::composer::tests` to cover composer body rendering and slash-suggestion suppression.
- Run `cargo test -p cowboy app::tests::draw_locked_composer_shows_disabled_copy_without_slash_suggestions` to cover the full TUI render output for the disabled state.
- Run `cargo test -p cowboy app::input::tests::locked_composer_ignores_submission_edits_navigation_and_history app::input::tests::locked_composer_allows_scroll_follow_latest_and_exit_keys` to confirm the visual change did not alter disabled input behavior.
- Optional manual smoke: launch `cargo run -p cowboy`, submit a run that stays active long enough to observe the UI, confirm the composer body shows disabled copy, typed characters and `Enter` are ignored, `Esc` cancels and re-enables editing, and a `WaitingForInput` prompt still enables answer entry.

# TODO

- [x] Add disabled-state body copy rendering to `crates/tui/src/app/controls/composer.rs`.
- [x] Reserve composer body space for the disabled notice without breaking wrapping or clipping of existing input text.
- [x] Keep disabled status copy and in-box notice text consistent while title only shows run/cancel state.
- [x] Preserve hidden cursor and suppressed slash suggestions while disabled.
- [x] Update composer unit tests for the disabled in-box notice.
- [x] Update full TUI render test coverage for the locked composer screen.
- [x] Run focused composer rendering tests.
- [x] Run focused locked-input behavior tests.
- [ ] Perform an optional manual TUI smoke test if visual confirmation is needed.
